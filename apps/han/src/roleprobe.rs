//! `han role-probe`: what a host or an executor can send through the API its role works through. It starts a
//! role node the way `han host` and `han exec` do (`rolenode::start`, which holds the node to the role's
//! class), tries the commands that approve and govern, and says for each whether the class refused it. The
//! end-to-end tests run it to prove that a role cannot approve or govern on the real path.

use std::path::PathBuf;

use clap::{Args, ValueEnum};
use han_node::api::{Command, ConnectionClass, NodeError};
use han_registry::HomeRef;

use crate::rolenode;

#[derive(Clone, Copy, ValueEnum)]
enum Role {
    Host,
    Exec,
}

/// Options of `han role-probe`.
#[derive(Args)]
pub struct ProbeArgs {
    /// The class to probe as.
    role: Role,
    /// The role's actor.
    #[arg(long)]
    actor: String,
    /// The id of the role's key in the actor's profile.
    #[arg(long, default_value = "k1")]
    key_id: String,
    /// Where the key store is (`<keys>/keys` for file keys).
    #[arg(long)]
    keys: PathBuf,
    /// The role node's data directory.
    #[arg(long)]
    data: PathBuf,
    /// The space.
    #[arg(long)]
    space: String,
    /// The space's home: a home ticket or a ServerName.
    #[arg(long)]
    home: HomeRef,
    /// Relays: `n0`, `none`, or URLs.
    #[arg(long, default_value = "n0")]
    relay: String,
}

pub async fn run(args: ProbeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let (class, allowed) = match args.role {
        Role::Host => (
            ConnectionClass::Host,
            Command::TaskUpdate {
                space: args.space.clone(),
                task: "none".into(),
                state: "done".into(),
            },
        ),
        Role::Exec => (
            ConnectionClass::Executor,
            Command::EffectOutcome {
                space: args.space.clone(),
                claim: "none".into(),
                result: "failed".into(),
                started: "no".into(),
                cost: None,
                evidence: None,
            },
        ),
    };
    let space = args.space.clone();
    let mut tries = vec![
        (
            "approve",
            Command::EffectApprove {
                space: space.clone(),
                proposal: "none".into(),
                expires_at: None,
                note: None,
                evidence: None,
            },
        ),
        (
            "revert-approve",
            Command::RevertApprove {
                space: space.clone(),
                proposal: "none".into(),
                note: None,
                evidence: han_node::api::ApprovalEvidence {
                    kind: "han.ci/1".into(),
                    over: "0".repeat(64),
                    record: han_node::api::EvidenceRef {
                        hash: "0".repeat(64),
                        size: 1,
                        media_type: "application/json".into(),
                    },
                },
            },
        ),
        (
            "cancel",
            Command::EffectCancel {
                space: space.clone(),
                proposal: "none".into(),
                reason: None,
                evidence: None,
            },
        ),
        (
            "member",
            Command::MemberSet {
                space: space.clone(),
                actor: args.actor.clone(),
                role: "owner".into(),
            },
        ),
        (
            "invite",
            Command::Invite {
                space: space.clone(),
                ttl_secs: None,
                expires_at: None,
                auto_admit: false,
            },
        ),
        (
            "policy",
            Command::PolicySet {
                space: space.clone(),
                policy: "{}".into(),
            },
        ),
        (
            "revoke",
            Command::DeviceRevoke {
                key_id: args.key_id.clone(),
            },
        ),
    ];
    if class == ConnectionClass::Host {
        tries.push((
            "claim",
            Command::EffectClaim {
                space: space.clone(),
                proposal: "none".into(),
                attempt: 1,
                window: 60,
            },
        ));
    }
    let role = rolenode::start(
        &args.actor,
        &args.key_id,
        &rolenode::key_name(&args.actor),
        &args.keys,
        &args.data,
        &args.relay,
        &args.home,
        &space,
        class,
    )
    .await?;
    let mut leaked = false;
    for (name, cmd) in tries {
        // An executor refuses an effect it will not run (P-07a) and approves its own revert (A2): its class lets
        // EffectCancel and RevertApprove through to the node.
        let may = class == ConnectionClass::Executor && matches!(name, "cancel" | "revert-approve");
        match role.api.call(cmd).await {
            Err(NodeError::Denied { message }) if message.contains("may not send") => {
                leaked |= may;
                println!("{name}: refused by class");
            }
            _ if may => println!("{name}: reached the node"),
            other => {
                leaked = true;
                println!("{name}: NOT refused by class: {other:?}");
            }
        }
    }
    // A command of the role's own class reaches the node (whatever the node then says of it).
    match role.api.call(allowed).await {
        Err(NodeError::Denied { message }) if message.contains("may not send") => {
            leaked = true;
            println!("own: refused by class: {message}");
        }
        _ => println!("own: reached the node"),
    }
    role.node.shutdown();
    role.transport.shutdown().await;
    if leaked {
        return Err("the role's API let a command through".into());
    }
    Ok(())
}
