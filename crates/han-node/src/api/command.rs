//! `call(Command)`: what a face asks the node to do, and what comes back.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::rows::StatusBody;

pub use han_registry::HomeRef;

/// A reference to a blob already uploaded (`BlobPut` answers with one), as the `evidence` of an
/// `effect.outcome` or `effect.cancel`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct EvidenceRef {
    /// `sha256` of the bytes, lowercase hex.
    pub hash: String,
    /// The size in bytes.
    pub size: u64,
    /// The media type.
    pub media_type: String,
}

/// The record an approval rests on (architecture.md P-17): the body carries `kind` and `over`, the content the
/// `record`, a blob already uploaded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ApprovalEvidence {
    /// The record's schema, for example `han.review/1`.
    pub kind: String,
    /// The delta digest the record is over (the plan's `deltaDigest`).
    pub over: String,
    /// The record, a blob already uploaded (content `evidence`, readable under G-01).
    pub record: EvidenceRef,
}

/// Something a face asks of the device. Commands that write the log end as a receipt (or are queued in the
/// outbox when the home cannot be reached); the verdict of Policy comes back as a [`super::NodeError`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// Read-only home Policy check before remote lane preparation; consumes no approval or claim.
    EffectReady {
        /// The space.
        space: String,
        /// The proposal.
        proposal: String,
    },
    /// Creates a space at `home` with this device's person as its owner, and follows it.
    SpaceCreate {
        /// The home that will sequence it (default: the home of this device's actor).
        #[serde(default)]
        #[ts(optional)]
        home: Option<HomeRef>,
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
        /// The home of the space. A ticket is pinned first. Default: the server the space id names.
        #[serde(default)]
        #[ts(optional)]
        home: Option<HomeRef>,
        /// The space.
        space: String,
    },
    /// Waits until a followed space has its first records from the home, so that what depends on its state
    /// (a profile's operator, a member change) can go on. The node waits, not the caller: at most
    /// `timeout_ms` (default and cap: the node's `sync_wait`). `not_found` if the space is not followed;
    /// `unavailable` with `pending: space` if the records have not arrived by then.
    SpaceAwait {
        /// The space.
        space: String,
        /// How long to wait, at most.
        #[serde(default)]
        #[ts(optional)]
        timeout_ms: Option<u64>,
    },
    /// Waits until this node knows a session of a followed space and its watch of the session's stream is up,
    /// so that a view of the session can be opened and the items sent from now on are seen. The node waits,
    /// not the caller: at most `timeout_ms` (default and cap: the node's `sync_wait`). `not_found` if the
    /// space is not followed; `unavailable` with `pending: session` if that has not happened by then.
    SessionAwait {
        /// The space.
        space: String,
        /// The session.
        session: String,
        /// How long to wait, at most.
        #[serde(default)]
        #[ts(optional)]
        timeout_ms: Option<u64>,
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
    },
    /// Starts a session of an agent through the host role (not before M4).
    AgentStart {
        /// The space.
        space: String,
        /// The agent actor.
        agent: String,
        /// The harness (default: the one recorded for the agent).
        #[serde(default)]
        #[ts(optional)]
        harness: Option<String>,
        /// The repository, `github:owner/repo`.
        #[serde(default)]
        #[ts(optional)]
        repo: Option<String>,
        /// The issue, `github:owner/repo#123`. The session works the open task for the issue that is addressed
        /// to the agent (han-github creates one for each agent-ready issue; its data block carries the issue's
        /// title and body). With no such task the start is refused, since the agent has no network to fetch
        /// the text.
        #[serde(default)]
        #[ts(optional)]
        issue: Option<String>,
        /// The task to work. Without `issue`, the task's own issue is used; with it, the task must be for
        /// that issue.
        #[serde(default)]
        #[ts(optional)]
        task: Option<String>,
        /// The agent's home, when the node does not follow the agent's profile yet: the node pins it (a
        /// ticket), follows `profile:<agent>` there, and takes the operator from it.
        #[serde(default)]
        #[ts(optional)]
        home: Option<HomeRef>,
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
    /// Uploads bytes as a blob (`blobPut`, in parts) to the home of the space and returns its reference, for a
    /// record of this node's actor to refer to (an effect's `argsBlob`): the home supplies it for the space
    /// once such a record is in its log (G-01). At most 4 MiB. The agent's host hands a `git_push`'s changeset
    /// to the executor this way (R15).
    BlobPut {
        /// The space.
        space: String,
        /// The media type of the bytes.
        media_type: String,
        /// The bytes, base64url.
        data: String,
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
        /// The record the approval rests on, for a rule that asks for evidence (P-17).
        #[serde(default)]
        #[ts(optional)]
        evidence: Option<ApprovalEvidence>,
    },
    /// The land lane's own approval of the revert it proposed (A2, P-17 "Revert"), an **executor** command: no
    /// person is asked. The node signs an `effect.approve` as its own actor only when the proposal is open, was
    /// made by this node's actor, carries `change.revertOf`, and the evidence is `han.ci/1` over the delta of
    /// the change it reverts (the gate's `revertedDigest`). Policy still decides whether the approval counts, so
    /// a revert of tier 2 or more, or on a never-auto path, stays with a person. A face or an agent's host may
    /// not send it, and `EffectApprove` stays refused to an executor (SR-06, SR-106).
    RevertApprove {
        /// The space.
        space: String,
        /// The revert's proposal (entry id).
        proposal: String,
        /// A note.
        #[serde(default)]
        #[ts(optional)]
        note: Option<String>,
        /// The failing run the revert rests on: `han.ci/1` over the reverted change's delta digest.
        evidence: ApprovalEvidence,
    },
    /// The tier-1 machine reviewer's approval of a landing it reviewed, an **executor-connection** command like
    /// [`Command::RevertApprove`]: no person is asked. The node signs an `effect.approve` as its own actor only
    /// when the proposal is an open `vcs.land` that another actor proposed, whose derived tier is at most 1, and
    /// the evidence is `han.review/1` over the delta digest of the change summary the proposal carries (the
    /// gate's `deltaDigest`). Policy still decides whether the approval counts: a non-human approval needs a
    /// rule that lists this actor with `evidence: "han.review/1"`, never counts at tier 2 or more, and
    /// `separateApprover` still applies. A face or an agent's host may not send it, and `EffectApprove` stays
    /// refused to an executor connection (SR-06, SR-106).
    ReviewApprove {
        /// The space.
        space: String,
        /// The landing's proposal (entry id).
        proposal: String,
        /// A note.
        #[serde(default)]
        #[ts(optional)]
        note: Option<String>,
        /// The review record: `han.review/1` over the landing's delta digest.
        evidence: ApprovalEvidence,
    },
    /// Cancels a proposed effect. The effect's executor may cancel it too while it is `open` (round 33,
    /// P-07a): it refuses an effect for good, and says why.
    EffectCancel {
        /// The space.
        space: String,
        /// The proposal's entry id.
        proposal: String,
        /// Why (content `reason`, at most 1,024 bytes).
        #[serde(default)]
        #[ts(optional)]
        reason: Option<String>,
        /// The proof, a blob already uploaded (content `evidence`, readable under G-01).
        #[serde(default)]
        #[ts(optional)]
        evidence: Option<EvidenceRef>,
    },
    /// Proposes an effect (`effect.propose`): the only way an agent's work reaches the world. The node
    /// computes `planDigest = sha256(JCS(plan))` and puts the plan in the content. For a `vcs.land` it copies
    /// the plan's `args.change` into the body, where Policy reads it (P-17); the executor checks that the two
    /// are the same (X-13).
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
        /// What the attempt showed, a blob already uploaded (content `evidence`, readable under G-01).
        #[serde(default)]
        #[ts(optional)]
        evidence: Option<EvidenceRef>,
    },
    /// Queues an outbox entry that `failed` (delivery gave up) again and tries to send it at once. The answer
    /// is that of the entry: its receipt, its refusal, or `Queued`.
    OutboxRetry {
        /// The outbox id (`Queued` and the `Outbox` view carry it).
        outbox_id: u64,
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

/// What a connection to the node API is, which the node knows from the endpoint it came in on and not from
/// anything the peer says. It bounds the commands the connection may send (SR-06, SR-106): only a
/// [`ConnectionClass::Face`] may send a [`CommandClass::Face`] command, so an agent's host or an executor can
/// never approve, however it asks. A face connection is also **locked** for the commands that govern or steer
/// ([`Command::needs_presence`]) until the person confirms their presence on that connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionClass {
    /// A face acting for the device's person: every command; the ones that govern or steer once the person
    /// has confirmed their presence on the connection.
    Face,
    /// An agent's host and tools, and any other local client that is not a face (an integration): its own
    /// sessions, tasks, status, leases and proposals ([`CommandClass::Agent`] commands) and reads. It can never
    /// approve, govern or steer, and it has no way to unlock. Its endpoint is `role_endpoint(.., Host)`.
    Host,
    /// An executor: [`CommandClass::Agent`] and [`CommandClass::Executor`] commands.
    Executor,
}

impl ConnectionClass {
    /// Whether a connection of this class may send `cmd`: its class permits the command's class, and a
    /// command only an executor sends ([`Command::executor_only`]) is not sent on a face connection, which would
    /// otherwise skip the person's confirmation it stands for.
    pub fn permits_command(self, cmd: &Command) -> bool {
        self.permits(cmd.class()) && !(self == ConnectionClass::Face && cmd.executor_only())
    }

    /// Whether a connection of this class may send a command of class `c`.
    pub fn permits(self, c: CommandClass) -> bool {
        match self {
            ConnectionClass::Face => true,
            ConnectionClass::Host => c == CommandClass::Agent,
            ConnectionClass::Executor => c != CommandClass::Face,
        }
    }
}

impl Command {
    /// The class of the command. The match is exhaustive with no wildcard arm, so a new variant must be
    /// classified before the crate builds (SR-06).
    pub fn class(&self) -> CommandClass {
        match self {
            Command::SpaceCreate { .. }
            | Command::SpaceJoin { .. }
            | Command::SpaceFollow { .. }
            | Command::SpaceAwait { .. }
            | Command::SessionAwait { .. }
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
            // the agent's host calls it in process; an agent's tools never upload blobs
            | Command::BlobPut { .. }
            | Command::EffectApprove { .. }
            | Command::EffectCancel { .. }
            | Command::OutboxRetry { .. }
            | Command::PolicySet { .. }
            // Steering is a face command: `<endpoint>-host` signs as the node's actor, so a command there
            // could steer as the person. An agent's own in-process node (`ClassApi`) may send it, and the
            // policy decides (session rights, guarding).
            | Command::SessionInput { .. } => CommandClass::Face,
            Command::TaskCreate { .. }
            | Command::TaskUpdate { .. }
            | Command::SessionOpen { .. }
            | Command::SessionClose { .. }
            | Command::StatusPublish { .. }
            | Command::StreamPublish { .. }
            | Command::LeaseSet { .. }
            | Command::EffectPropose { .. } => CommandClass::Agent,
            Command::EffectReady { .. }
            | Command::EffectClaim { .. }
            | Command::EffectOutcome { .. }
            | Command::RevertApprove { .. }
            | Command::ReviewApprove { .. } => CommandClass::Executor,
        }
    }

    /// Whether the command governs (membership, policy, invites, devices, agent keys, trust in a home) or steers
    /// a session. A face connection sends these only after the person confirmed their presence on it
    /// (SR-106): any process of the person's OS user can open the face pipe, so the pipe alone proves nothing.
    /// Approvals are not listed: they ask for a person at each action, bound to the plan. The match has no
    /// wildcard arm, so a new variant must be decided before the crate builds.
    pub fn needs_presence(&self) -> bool {
        match self {
            Command::SpaceJoin { .. }
            | Command::HomePin { .. }
            | Command::Invite { .. }
            | Command::MemberSet { .. }
            | Command::AgentCreate { .. }
            // starts a tool-using agent on any repository or harness
            | Command::AgentStart { .. }
            | Command::DeviceOffer
            | Command::DevicePair { .. }
            | Command::DeviceRevoke { .. }
            | Command::PolicySet { .. }
            | Command::SessionInput { .. } => true,
            // a home given by ticket is pinned (its node and receipt keys become the trust anchor of its
            // server), which is `HomePin` by another door
            Command::SpaceCreate { home, .. } | Command::SpaceFollow { home, .. } => {
                matches!(home, Some(HomeRef::Ticket(_)))
            }
            // Tasks stay open to a face connection on purpose: a task is data, not a prompt (SR-54), and the
            // policy decides who may create or cancel one.
            Command::SpaceAwait { .. }
            | Command::SessionAwait { .. }
            | Command::AgentStop { .. }
            | Command::TaskCreate { .. }
            | Command::TaskUpdate { .. }
            | Command::TaskCancel { .. }
            | Command::BlobFetch { .. }
            | Command::BlobPut { .. }
            | Command::SessionOpen { .. }
            | Command::SessionClose { .. }
            | Command::StatusPublish { .. }
            | Command::LeaseSet { .. }
            | Command::EffectApprove { .. }
            | Command::EffectCancel { .. }
            | Command::EffectPropose { .. }
            | Command::EffectReady { .. }
            | Command::EffectClaim { .. }
            | Command::EffectOutcome { .. }
            | Command::RevertApprove { .. }
            | Command::ReviewApprove { .. }
            | Command::OutboxRetry { .. }
            | Command::StreamPublish { .. } => false,
        }
    }

    /// Whether only an executor connection may send it: it signs as the node's actor with no prompt, so a face
    /// connection (any process of the person's OS user can open it) must not.
    pub fn executor_only(&self) -> bool {
        matches!(
            self,
            Command::RevertApprove { .. } | Command::ReviewApprove { .. }
        )
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
    /// A blob was uploaded (`BlobPut`): its reference.
    BlobRef {
        /// `sha256` of the bytes, lowercase hex.
        hash: String,
        /// The size in bytes.
        size: u64,
        /// The media type.
        media_type: String,
    },
    /// A status item was sent (`StatusPublish`).
    Status {
        /// The item's number on the session's stream.
        n: u64,
    },
    /// Done; nothing to return.
    Done,
}
