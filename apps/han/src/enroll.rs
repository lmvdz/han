//! Enrolling an actor that is not a person (an agent or an executor): the steps after its key exists, shared by
//! `han agent add` and `han executor add`.
//!
//! 1. register its profile at the home (the home's admin);
//! 2. the operator confirms it (§4.3: the operator holds the owner role in the profile space);
//! 3. an owner of the space makes it a member.
//!
//! Each step is done here when this device may do it, and otherwise left as a line saying exactly what the
//! other party runs. Every step first looks at what is already so, so a second run changes nothing.

use std::path::Path;
use std::time::Duration;

use han_node::NodeApi;
use han_node::api::{CallResult, Command, NodeError, Query, Row, ViewEvent, Window};
use han_node::rpc::RpcClient;
use han_registry::HomeRef;

use crate::seqadmin::{self, Register};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// Who to enrol and where.
pub struct Enroll<'a> {
    /// The actor, `@name:server`.
    pub actor: &'a str,
    /// `agent` or `service`.
    pub kind: &'a str,
    /// The key's id in the profile.
    pub key_id: &'a str,
    /// The public key, base64url.
    pub public_key: &'a str,
    /// The operator.
    pub operator: &'a str,
    /// The home, as the command line named it.
    pub home: Option<&'a HomeRef>,
    /// The home's data directory, when this machine runs the home (so its admin socket is reachable).
    pub home_data: Option<&'a Path>,
    /// The space to make the actor a member of.
    pub space: Option<&'a str>,
    /// The running node of this device, if there is one.
    pub client: Option<&'a RpcClient>,
}

/// What is left for other parties.
#[derive(Default)]
pub struct Outcome {
    /// Lines of what somebody else runs.
    pub todo: Vec<String>,
}

fn changed(what: &str) {
    println!("  changed:   {what}");
}

fn same(what: &str) {
    println!("  unchanged: {what}");
}

fn todo(out: &mut Outcome, who: &str, what: &str, run: &str) {
    println!("  to do:     {what}");
    println!("             {who} runs: {run}");
    out.todo.push(format!("{who} runs: {run}"));
}

/// The actor of the running node.
async fn node_actor(client: &RpcClient) -> Option<String> {
    let mut view = client.view(Query::NodeStatus {
        window: Window::default(),
    });
    match view.next().await {
        Some(ViewEvent::Snapshot { rows, .. }) => rows.into_iter().find_map(|r| match r {
            Row::NodeStatus(n) => Some(n.actor),
            _ => None,
        }),
        _ => None,
    }
}

/// Runs the steps.
pub async fn run(e: &Enroll<'_>) -> Res<Outcome> {
    let mut out = Outcome::default();
    let ticket = e
        .home
        .map_or_else(|| "<home ticket>".to_owned(), ToString::to_string);

    // 1. the profile
    let registered = match e.home_data {
        Some(data) => {
            let (seq, already) = seqadmin::ask(
                data,
                &Register {
                    actor: e.actor.to_owned(),
                    kind: e.kind.to_owned(),
                    key_id: e.key_id.to_owned(),
                    public_key: e.public_key.to_owned(),
                    operator: Some(e.operator.to_owned()),
                },
            )
            .await?;
            if already {
                same(&format!("{} is registered (profile at seq {seq})", e.actor));
            } else {
                changed(&format!("registered {} (profile at seq {seq})", e.actor));
            }
            true
        }
        None => {
            // Maybe the admin did it already: the confirm below finds out.
            false
        }
    };
    let register_cmd = format!(
        "han home register {} --kind {} --key-id {} --public-key {} --operator {} --data <the home's data dir>",
        e.actor, e.kind, e.key_id, e.public_key, e.operator
    );

    // 2. the operator's confirmation
    let Some(client) = e.client else {
        if !registered {
            todo(
                &mut out,
                "the home's admin",
                "register the profile",
                &register_cmd,
            );
        }
        todo(
            &mut out,
            "the operator",
            "confirm the profile",
            &format!(
                "han actor confirm {} --operator {} --home {ticket}",
                e.actor, e.operator
            ),
        );
        if let Some(space) = e.space {
            todo(
                &mut out,
                &format!("an owner of {space}"),
                "make it a member",
                &format!("han space member {space} {} member", e.actor),
            );
        }
        return Ok(out);
    };
    let me = node_actor(client).await;
    let i_am_operator = me.as_deref() == Some(e.operator);
    let confirm_cmd = format!(
        "han actor confirm {} --operator {} --home {ticket}",
        e.actor, e.operator
    );
    // Whether the operator's confirmation is in: done here when this node is the operator's; otherwise the
    // home says so when it runs on this machine, and when it does not, the operator is told again each time.
    let mut confirmed = false;
    if !i_am_operator
        && let Some(data) = e.home_data
        && let Ok((_, true)) = seqadmin::profile(data, e.actor).await
    {
        same(&format!("{} is confirmed by {}", e.actor, e.operator));
        confirmed = true;
    }
    if !registered && !i_am_operator {
        todo(
            &mut out,
            "the home's admin",
            "register the profile, if not done",
            &register_cmd,
        );
    }
    if i_am_operator {
        // Give the profile time to arrive when we just made it; a short look when somebody else did.
        let wait = Duration::from_secs(if registered { 60 } else { 3 });
        match confirm(client, e.actor, e.home, e.operator, wait).await {
            Ok(true) => {
                changed(&format!("{} confirmed {}", e.operator, e.actor));
                confirmed = true;
            }
            Ok(false) => {
                same(&format!("{} is confirmed by {}", e.actor, e.operator));
                confirmed = true;
            }
            Err(Pending::Profile) => {
                todo(
                    &mut out,
                    "the home's admin",
                    "register the profile",
                    &register_cmd,
                );
            }
            Err(Pending::Other(m)) => return Err(m.into()),
        }
    }

    // 3. membership
    let member_cmd = |space: &str| format!("han space member {space} {} member", e.actor);
    match e.space {
        Some(space) if confirmed => {
            match client
                .call(Command::MemberSet {
                    space: space.to_owned(),
                    actor: e.actor.to_owned(),
                    role: "member".into(),
                })
                .await
            {
                Ok(CallResult::Done) => same(&format!("{} is a member of {space}", e.actor)),
                Ok(_) => changed(&format!("{} is now a member of {space}", e.actor)),
                Err(NodeError::Denied { .. }) => todo(
                    &mut out,
                    &format!("an owner of {space}"),
                    "make it a member",
                    &member_cmd(space),
                ),
                Err(err) => return Err(err.into()),
            }
        }
        Some(space) => {
            if !i_am_operator && !confirmed {
                todo(
                    &mut out,
                    "the operator",
                    &format!(
                        "confirm the profile (this node acts as {})",
                        me.as_deref().unwrap_or("?")
                    ),
                    &confirm_cmd,
                );
            }
            todo(
                &mut out,
                &format!("an owner of {space}"),
                "make it a member, once confirmed",
                &member_cmd(space),
            );
        }
        None if !i_am_operator && !confirmed => {
            todo(
                &mut out,
                "the operator",
                &format!(
                    "confirm the profile (this node acts as {})",
                    me.as_deref().unwrap_or("?")
                ),
                &confirm_cmd,
            );
        }
        None => {}
    }
    Ok(out)
}

/// Why a confirmation could not be made yet.
pub enum Pending {
    /// The profile has not arrived: the home does not have it.
    Profile,
    /// Anything else.
    Other(String),
}

/// The operator's confirmation of `actor`: `member.set {operator: owner}` in the actor's profile space. Returns
/// whether it changed anything. `wait` is how long the node may take for the profile to arrive.
pub async fn confirm(
    client: &RpcClient,
    actor: &str,
    home: Option<&HomeRef>,
    operator: &str,
    wait: Duration,
) -> Result<bool, Pending> {
    let profile = format!("profile:{actor}");
    let _ = client
        .call(Command::SpaceFollow {
            home: home.cloned(),
            space: profile.clone(),
        })
        .await;
    // The node waits for the space to sync, bounded by `wait`; the answer says what it waits for.
    match client
        .call(Command::SpaceAwait {
            space: profile.clone(),
            timeout_ms: Some(u64::try_from(wait.as_millis()).unwrap_or(u64::MAX)),
        })
        .await
    {
        Ok(_) => {}
        Err(e) if matches!(e.waiting_for(), Some(han_node::api::Pending::Space { .. })) => {
            return Err(Pending::Profile);
        }
        Err(e) => return Err(Pending::Other(e.to_string())),
    }
    // The operator holds no role in the profile space; the peer filter admits this member.set because the
    // profile names it as the operator (N-02, P-03).
    match client
        .call(Command::MemberSet {
            space: profile,
            actor: operator.to_owned(),
            role: "owner".into(),
        })
        .await
    {
        Ok(CallResult::Done) => Ok(false),
        Ok(_) => Ok(true),
        Err(e) => Err(Pending::Other(e.to_string())),
    }
}
