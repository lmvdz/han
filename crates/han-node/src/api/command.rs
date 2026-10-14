//! `call(Command)`: what a face asks the node to do, and what comes back.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::rows::StatusBody;

/// A home and the node to dial for it (the same facts a `/.well-known/han` document or a ticket carries).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct HomeRef {
    /// The ServerName, as put in `dialed` (G-08).
    pub server: String,
    /// The node id of the home's sequencer.
    pub node: String,
    /// Direct addresses, `ip:port`.
    #[serde(default)]
    pub addrs: Vec<String>,
    /// Relay URLs.
    #[serde(default)]
    pub relays: Vec<String>,
}

/// Something a face asks of the device. Commands that write the log end as a receipt (or are queued in the
/// outbox when the home cannot be reached); the verdict of Policy comes back as a [`super::NodeError`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// Creates a space at `home` with this device's person as its owner, and follows it.
    SpaceCreate {
        /// The home that will sequence it.
        home: HomeRef,
        /// The space's name (content of `space.create`).
        name: String,
        /// `members` or `public`.
        visibility: String,
        /// `invite`, `open` or `closed`.
        join: String,
    },
    /// Uses an invite ticket (`han1...`): dials its node, joins, and follows the space.
    SpaceJoin {
        /// The ticket text.
        ticket: String,
    },
    /// Joins a home by a home ticket (`han1home.`, `han seq` prints it) instead of its HTTPS well-known: pins
    /// the home's node and receipt keys as **ticket-pinned** trust, keeps them in `node.db`, and links to it.
    HomePin {
        /// The home ticket text.
        ticket: String,
    },
    /// Follows a space this person already belongs to (for example on a new device).
    SpaceFollow {
        /// The home of the space.
        home: HomeRef,
        /// The space.
        space: String,
    },
    /// Registers an invite at the space's home and returns the ticket.
    Invite {
        /// The space.
        space: String,
        /// Seconds the ticket lives (default a day, at most 7 days).
        #[serde(default)]
        #[ts(optional)]
        ttl_secs: Option<u64>,
        /// The time the ticket ends, instead of `ttl_secs` (at most 7 days ahead); giving both is `invalid`.
        #[serde(default)]
        #[ts(optional)]
        expires_at: Option<String>,
        /// Admit whoever joins with it as a member while it is valid, if the node can tell they are a person
        /// or an agent whose operator confirmed it; anyone else waits for an owner. Only while this node
        /// runs.
        #[serde(default)]
        auto_admit: bool,
    },
    /// Gives an actor a role: `owner`, `member`, `guest`, `banned` or `none`.
    MemberSet {
        /// The space.
        space: String,
        /// The actor.
        actor: String,
        /// Its new role.
        role: String,
    },
    /// Makes the key of a new agent actor in this device's key store and returns what its home must register.
    AgentCreate {
        /// The agent's name, without `@` and server.
        name: String,
        /// The server it lives on.
        server: String,
        /// Its operator.
        operator: String,
    },
    /// Starts a session of an agent through the host role (not before M4).
    AgentStart {
        /// The space.
        space: String,
        /// The agent actor.
        agent: String,
        /// The harness.
        harness: String,
        /// The repository, `github:owner/repo`.
        #[serde(default)]
        #[ts(optional)]
        repo: Option<String>,
        /// The issue, `github:owner/repo#123`.
        #[serde(default)]
        #[ts(optional)]
        issue: Option<String>,
    },
    /// Stops the host role of a session (not before M4).
    AgentStop {
        /// The session, or the agent actor.
        target: String,
    },
    /// Asks actors to do something: a `task` entry; with `external` it is a claim on an issue.
    TaskCreate {
        /// The space.
        space: String,
        /// The assignees.
        to: Vec<String>,
        /// The ask (content).
        ask: String,
        /// The work's external reference, `github:owner/repo#123`.
        #[serde(default)]
        #[ts(optional)]
        external: Option<String>,
    },
    /// Reports a task's state.
    TaskUpdate {
        /// The space.
        space: String,
        /// The task's entry id.
        task: String,
        /// `accepted`, `progress`, `done`, `failed` or `declined`.
        state: String,
    },
    /// Fetches a blob a record of the space refers to (an effect's `argsBlob`), checks its hash and size, and
    /// returns it. A face never gets bytes that do not hash to what was named.
    BlobFetch {
        /// The space.
        space: String,
        /// `sha256` of the bytes, lowercase hex.
        hash: String,
        /// The size the reference names.
        size: u64,
        /// The media type the reference names.
        media_type: String,
    },
    /// Calls a task off.
    TaskCancel {
        /// The space.
        space: String,
        /// The task's entry id.
        task: String,
    },
    /// Opens a session of this device's actor.
    SessionOpen {
        /// The space.
        space: String,
        /// `auto` (default) or `guarded` (R-24: an agent that chooses guarding).
        #[serde(default)]
        #[ts(optional)]
        steer_mode: Option<String>,
        /// The task it works.
        #[serde(default)]
        #[ts(optional)]
        task: Option<String>,
        /// A title.
        #[serde(default)]
        #[ts(optional)]
        title: Option<String>,
    },
    /// Closes a session.
    SessionClose {
        /// The space.
        space: String,
        /// The `session.open` entry id.
        session: String,
    },
    /// Steers a session: a signed `session.input`.
    SessionInput {
        /// The space.
        space: String,
        /// The session.
        session: String,
        /// The text.
        text: String,
    },
    /// Publishes a `han.status/1` item on a session's stream (the host's job; the node numbers and sends it).
    StatusPublish {
        /// The space.
        space: String,
        /// The status; its `seq` is set by the node when it is 0.
        status: Box<StatusBody>,
    },
    /// Takes an advisory lease on paths (an extension entry; it lapses after `ttl_secs`).
    LeaseSet {
        /// The space.
        space: String,
        /// The paths, relative to the repository; a path ending in `/` covers what is under it.
        paths: Vec<String>,
        /// Seconds it lasts.
        ttl_secs: u64,
        /// The issue it is for.
        #[serde(default)]
        #[ts(optional)]
        issue: Option<String>,
    },
    /// On the new device: makes the pairing code an existing device of the same actor needs to add it.
    DeviceOffer,
    /// On an existing device: adds the device a pairing code names (a `key.add` in the actor's profile).
    DevicePair {
        /// The code the new device showed.
        code: String,
    },
    /// Revokes a device's key (a `key.revoke` in the actor's profile).
    DeviceRevoke {
        /// The key id.
        key_id: String,
    },
    /// Approves a proposed effect.
    EffectApprove {
        /// The space.
        space: String,
        /// The proposal's entry id.
        proposal: String,
        /// The approval lapses at this time (a future HAN Time).
        #[serde(default)]
        #[ts(optional)]
        expires_at: Option<String>,
        /// A note.
        #[serde(default)]
        #[ts(optional)]
        note: Option<String>,
    },
    /// Cancels a proposed effect.
    EffectCancel {
        /// The space.
        space: String,
        /// The proposal's entry id.
        proposal: String,
    },
    /// Proposes an effect (`effect.propose`): the only way an agent's work reaches the world. The node
    /// computes `planDigest = sha256(JCS(plan))` and puts the plan in the content.
    EffectPropose {
        /// The space.
        space: String,
        /// Who may claim and run it.
        executor: String,
        /// The plan as JSON text: `{action, target, args?}` (han.ts `Plan`; `action` is also the body's).
        plan: String,
        /// The task it is charged to (R-24: the driving task, when a task drives the turn).
        #[serde(default)]
        #[ts(optional)]
        task: Option<String>,
        /// The cost unit and amount, in a budgeted space.
        #[serde(default)]
        #[ts(optional)]
        cost: Option<(String, u64)>,
        /// Whether a failed attempt may be retried.
        #[serde(default)]
        repeatable: bool,
        /// No claim at or after this time.
        #[serde(default)]
        #[ts(optional)]
        expires_at: Option<String>,
    },
    /// Claims an effect for this node's executor instance (`effect.claim`, X-03). `attempt` and `window` come
    /// from the state the executor read (the `Effects` view); a stale value is `denied` and is re-read.
    EffectClaim {
        /// The space.
        space: String,
        /// The proposal.
        proposal: String,
        /// One more than the number of earlier claims.
        attempt: u64,
        /// The space's `launchWindow` in seconds.
        window: u64,
    },
    /// Reports how a claimed attempt ended (`effect.outcome`, X-05).
    EffectOutcome {
        /// The space.
        space: String,
        /// The claim's entry id.
        claim: String,
        /// `succeeded`, `failed`, `cancelled` or `unknown`.
        result: String,
        /// `yes`, `no` or `unknown`.
        started: String,
        /// What it cost, with `started: yes` only.
        #[serde(default)]
        #[ts(optional)]
        cost: Option<(String, u64)>,
    },
    /// Replaces the space's policy (`policy.set`); an owner's command.
    PolicySet {
        /// The space.
        space: String,
        /// The whole policy as JSON text.
        policy: String,
    },
    /// Publishes a profile item other than `han.status/1` on a session's stream (for example a
    /// `han.timeline/1` batch). Only the session's actor may publish (T-07).
    StreamPublish {
        /// The space.
        space: String,
        /// The session.
        session: String,
        /// The profile's name, such as `han.timeline/1`.
        name: String,
        /// The item's body as JSON text (at most 64 KiB with its envelope).
        body: String,
    },
}

/// Who may send a command (SR-06). The node API's local pipe serves faces; han-mcp's connection (an agent's
/// tools) gets only [`CommandClass::Agent`] commands, and an executor only [`CommandClass::Agent`] and
/// [`CommandClass::Executor`] ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandClass {
    /// Only a face acting for the device's person: approvals, membership, devices, policy, agents.
    Face,
    /// Also an agent's host and tools: its own sessions, tasks, status, leases and proposals.
    Agent,
    /// Also an executor: claims and outcomes.
    Executor,
}

impl Command {
    /// The class of the command. The match is exhaustive with no wildcard arm, so a new variant must be
    /// classified before the crate builds (SR-06).
    pub fn class(&self) -> CommandClass {
        match self {
            Command::SpaceCreate { .. }
            | Command::SpaceJoin { .. }
            | Command::SpaceFollow { .. }
            | Command::HomePin { .. }
            | Command::Invite { .. }
            | Command::MemberSet { .. }
            | Command::AgentCreate { .. }
            | Command::AgentStart { .. }
            | Command::AgentStop { .. }
            | Command::TaskCancel { .. }
            | Command::DeviceOffer
            | Command::DevicePair { .. }
            | Command::DeviceRevoke { .. }
            | Command::BlobFetch { .. }
            | Command::EffectApprove { .. }
            | Command::EffectCancel { .. }
            | Command::PolicySet { .. } => CommandClass::Face,
            Command::TaskCreate { .. }
            | Command::TaskUpdate { .. }
            | Command::SessionOpen { .. }
            | Command::SessionClose { .. }
            | Command::SessionInput { .. }
            | Command::StatusPublish { .. }
            | Command::StreamPublish { .. }
            | Command::LeaseSet { .. }
            | Command::EffectPropose { .. } => CommandClass::Agent,
            Command::EffectClaim { .. } | Command::EffectOutcome { .. } => CommandClass::Executor,
        }
    }

    /// Whether an agent's tools may send it (SR-06).
    pub fn is_agent_allowed(&self) -> bool {
        self.class() == CommandClass::Agent
    }
}

/// How a call ended, when it did not fail.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CallResult {
    /// The home ordered the entry and the node checked its receipt.
    Receipt {
        /// The space.
        space: String,
        /// The entry's position in the log.
        seq: u64,
        /// The entry's id.
        id: String,
        /// The receipt's time.
        time: String,
    },
    /// The home could not be reached: the entry is signed and waits in the outbox.
    Queued {
        /// The outbox row.
        outbox_id: u64,
        /// The space.
        space: String,
        /// The entry's id.
        id: String,
    },
    /// A space was created, joined or followed.
    Space {
        /// The space.
        space: String,
    },
    /// An invite ticket.
    Ticket {
        /// The text to pass on.
        text: String,
    },
    /// A new agent key.
    Agent {
        /// The actor id.
        actor: String,
        /// The key id to register.
        key_id: String,
        /// The public key (also the node id of the host that runs it).
        public_key: String,
    },
    /// A device pairing code.
    DeviceCode {
        /// The code to type or scan on the existing device.
        code: String,
        /// This device's node id.
        node_id: String,
        /// Short check both screens show, derived from the code.
        check: String,
    },
    /// A blob, checked against its hash and size.
    Blob {
        /// The hash.
        hash: String,
        /// The media type the reference named.
        media_type: String,
        /// The bytes, base64url.
        data: String,
    },
    /// A status item was sent (`StatusPublish`).
    Status {
        /// The item's number on the session's stream.
        n: u64,
    },
    /// Done; nothing to return.
    Done,
}
