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
    /// A status item was sent (`StatusPublish`).
    Status {
        /// The item's number on the session's stream.
        n: u64,
    },
    /// Done; nothing to return.
    Done,
}
