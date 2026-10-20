//! P-17, semantic gates and risk tiers (round 33): the summary of a change, path globs, and the tier Policy
//! derives from them. Policy is pure over log entries, so it reads the summary a `vcs.land` proposal carries in
//! its body and never a tier the proposer states.

use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;

use super::Ctx;
use super::doc::{ActorKind, GatesDoc, PathClass, PolicyDoc};
use super::state::Effect;
use crate::types::{ActorId, EntryId};

/// Files a change summary lists.
pub const MAX_CHANGE_FILES: usize = 64;
/// The JCS of a change summary, so the proposal's header stays under 16 KiB.
pub const MAX_CHANGE_BYTES: usize = 12_288;
/// Path-class rules, and path-owner rules, per policy.
pub const MAX_GATE_RULES: usize = 64;
/// Path owners an effect keeps (the first by file order, each once).
pub const MAX_PATH_OWNERS: usize = 16;

/// The approvers entry for the authors of an effect's `gate.owners`.
pub const PATH_OWNER: &str = "@path-owner";

/// Paths no rule can let a machine approve: CI, HAN's own policy and gate config, dependency manifests and
/// lockfiles, credentials, and who may approve what.
pub const NEVER_AUTO: &[&str] = &[
    ".github/**",
    ".gitlab/**",
    ".gitlab-ci.yml",
    ".circleci/**",
    "**/Jenkinsfile",
    "azure-pipelines.yml",
    ".han/**",
    "**/Cargo.toml",
    "**/Cargo.lock",
    "**/package.json",
    "**/package-lock.json",
    "**/pnpm-lock.yaml",
    "**/yarn.lock",
    "**/bun.lock",
    "**/bun.lockb",
    "**/go.mod",
    "**/go.sum",
    "**/requirements*.txt",
    "**/pyproject.toml",
    "**/poetry.lock",
    "**/uv.lock",
    "**/Pipfile",
    "**/Pipfile.lock",
    "**/Gemfile",
    "**/Gemfile.lock",
    "**/pom.xml",
    "**/build.gradle",
    "**/build.gradle.kts",
    "**/deny.toml",
    "**/rust-toolchain.toml",
    "**/.env",
    "**/.env.*",
    "**/*.pem",
    "**/*.key",
    "**/*.p12",
    "**/*.pfx",
    "**/*.keystore",
    "**/id_rsa*",
    "**/id_ed25519*",
    "**/.secrets/**",
    "**/secrets/**",
    "**/credentials",
    "**/credentials.*",
    "**/.netrc",
    "**/.npmrc",
    "**/.pypirc",
    "**/CODEOWNERS",
    "**/sudoers",
    "**/sudoers.d/**",
    "**/.ssh/**",
];

/// What a change did to a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    /// New path.
    Add,
    /// Content, type or mode changed.
    Modify,
    /// Gone.
    Delete,
    /// Moved from `fromPath`, the content the same.
    Rename,
}

/// One path of a change summary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ChangeFile {
    /// The path (for a rename, the new one).
    pub path: String,
    /// What happened.
    pub kind: FileKind,
    /// Lines added and removed; for a file git treats as binary, its larger byte size.
    pub lines: u64,
    /// For a rename, the old path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_path: Option<String>,
}

/// The summary of a change (`effect.propose.change`, the plan's `args.change`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Change {
    /// The repository, as the plan's `target` names it.
    pub repo: String,
    /// The delta digest of the change.
    pub delta_digest: String,
    /// The paths the delta has.
    pub total: u64,
    /// The first paths in byte order, at most [`MAX_CHANGE_FILES`] and within [`MAX_CHANGE_BYTES`].
    pub files: Vec<ChangeFile>,
    /// Present (true) when `files` leaves paths out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    /// A revert (A2): the `vcs.land` effect of this space whose delta this change inverts. Policy checks that
    /// it is a succeeded `vcs.land`; the executor checks the inverse (X-13).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revert_of: Option<EntryId>,
}

impl Change {
    /// `sha256` of the summary's JCS: what the plan's copy must equal.
    pub fn digest(&self) -> String {
        let bytes = crate::json::to_value(self)
            .and_then(|v| crate::json::canonicalize(&v))
            .unwrap_or_default();
        crate::ids::sha256_hex(&bytes)
    }
}

/// The evidence an approval cites (`effect.approve.evidence`, and what the state keeps of it): the kind of
/// record and the delta digest it is over. The record itself is the approval's content `evidence` blob.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    /// The record's schema, for example `han.review/1`.
    pub kind: String,
    /// The delta digest the record is over.
    pub over: String,
}

/// The gate of an effect: fixed at the proposal from the policy in force.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Gate {
    /// Repository bound by the proposal's change summary; absent without a summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The risk tier, 0 to 3.
    pub tier: u8,
    /// The change's delta digest; absent when the proposal carried no change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_digest: Option<String>,
    /// `sha256(JCS(change))`; absent when the proposal carried no change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_digest: Option<String>,
    /// The path owners, or the proposer's confirmed operator when none matched.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owners: Vec<ActorId>,
    /// The `vcs.land` effect this change reverts (A2); fixed at the proposal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revert_of: Option<EntryId>,
    /// The reverted effect's delta digest: what a revert rule's evidence must be over (A2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverted_digest: Option<String>,
}

// ───────────────────────── globs and paths ─────────────────────────

/// A path of a change: UTF-8 of 1 to 255 bytes, `/`-separated, no empty, `.` or `..` segment, no control
/// character.
pub(crate) fn is_repo_path(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && !s.chars().any(|c| (c as u32) < 0x20 || c as u32 == 0x7f)
        && s.split('/').all(|seg| !matches!(seg, "" | "." | ".."))
}

/// A repository name: 1 to 128 printable ASCII characters.
pub(crate) fn is_repo_name(s: &str) -> bool {
    (1..=128).contains(&s.len()) && s.bytes().all(|b| (b'!'..=b'~').contains(&b))
}

/// An evidence kind: `[a-z0-9][a-z0-9./_-]{0,63}`.
pub(crate) fn is_evidence_kind(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=64).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b[1..].iter().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'/' | b'_' | b'-')
        })
}

/// A path glob: `*` and `?` inside a segment, `**` as a whole segment (zero or more segments), at most four of
/// them.
pub(crate) fn is_glob(s: &str) -> bool {
    if !(1..=256).contains(&s.len())
        || !s.bytes().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(
                    c,
                    b'.' | b'_'
                        | b'/'
                        | b'*'
                        | b'?'
                        | b'@'
                        | b'+'
                        | b'='
                        | b','
                        | b'~'
                        | b':'
                        | b'-'
                )
        })
    {
        return false;
    }
    let segs: Vec<&str> = s.split('/').collect();
    segs.iter().all(|seg| !matches!(*seg, "" | "." | ".."))
        && segs.iter().filter(|seg| **seg == "**").count() <= 4
        && segs.iter().all(|seg| *seg == "**" || !seg.contains("**"))
}

fn seg_match(pat: &str, s: &str) -> bool {
    let s: Vec<char> = s.chars().collect();
    let mut row = vec![false; s.len() + 1];
    row[0] = true;
    for c in pat.chars() {
        let mut next = vec![false; s.len() + 1];
        if c == '*' {
            let mut ok = false;
            for j in 0..=s.len() {
                ok = ok || row[j];
                next[j] = ok;
            }
        } else {
            for j in 1..=s.len() {
                next[j] = row[j - 1] && (c == '?' || c == s[j - 1]);
            }
        }
        row = next;
    }
    row[s.len()]
}

/// Whether `pat` matches the whole of `path`: anchored, case-sensitive, over Unicode scalar values.
pub fn glob_match(pat: &str, path: &str) -> bool {
    let ps: Vec<&str> = pat.split('/').collect();
    let ss: Vec<&str> = path.split('/').collect();
    let mut m = vec![vec![false; ss.len() + 1]; ps.len() + 1];
    m[ps.len()][ss.len()] = true;
    for i in (0..ps.len()).rev() {
        for j in (0..=ss.len()).rev() {
            m[i][j] = if ps[i] == "**" {
                m[i + 1][j] || (j < ss.len() && m[i][j + 1])
            } else {
                j < ss.len() && seg_match(ps[i], ss[j]) && m[i + 1][j + 1]
            };
        }
    }
    m[0][0]
}

fn never_auto(path: &str) -> bool {
    NEVER_AUTO.iter().any(|g| glob_match(g, path))
}

fn class_tier(c: PathClass) -> u8 {
    match c {
        PathClass::Docs | PathClass::Tests | PathClass::Generated => 0,
        PathClass::Code => 1,
        PathClass::Sensitive => 2,
        PathClass::Critical => 3,
    }
}

fn in_repo(rule_repo: Option<&String>, repo: &str) -> bool {
    rule_repo.is_none_or(|r| r == repo)
}

fn paths_of(f: &ChangeFile) -> impl Iterator<Item = &str> {
    std::iter::once(f.path.as_str()).chain(f.from_path.as_deref())
}

/// The tier of a summary under `gates`: the highest of its paths' floors (built-in never-auto paths 2, else
/// the first matching rule's class, else code), 2 for more than `large` lines, 1 for a tier-0 change of more
/// than `small` lines; 3 for a summary that leaves files out.
fn tier_of(gates: &GatesDoc, ch: &Change) -> u8 {
    if ch.truncated == Some(true) {
        return 3;
    }
    let (mut tier, mut lines) = (0u8, 0u64);
    for f in &ch.files {
        lines = lines.saturating_add(f.lines);
        for path in paths_of(f) {
            let class = gates
                .paths
                .iter()
                .find(|r| in_repo(r.repo.as_ref(), &ch.repo) && glob_match(&r.glob, path))
                .map_or(PathClass::Code, |r| r.class);
            tier = tier
                .max(class_tier(class))
                .max(if never_auto(path) { 2 } else { 0 });
        }
    }
    if lines > gates.large {
        tier = tier.max(2);
    } else if tier == 0 && lines > gates.small {
        tier = 1;
    }
    tier
}

/// The tier Policy derives for a change summary under `gates`, with no revert floor (P-17). A node reads it to
/// show a landing's tier before a person or a reviewer looks; the gate pinned in the log stays Policy's.
pub fn change_tier(gates: &GatesDoc, ch: &Change) -> u8 {
    tier_of(gates, ch)
}

/// P-17: the gate of a proposal of `action` with `change`, derived from `policy` and never from a tier the
/// proposer states; `None` when the policy has no `gates` or the action is not `vcs.land`.
pub(super) fn gate_of(
    policy: &PolicyDoc,
    action: &str,
    change: Option<&Change>,
    effects: &BTreeMap<EntryId, Effect>,
    ctx: &Ctx,
    author: &str,
) -> Option<Gate> {
    let gates = policy.gates.as_ref()?;
    if action != "vcs.land" {
        return None;
    }
    // what is not shown is not known: no summary fails closed, like a truncated one
    let mut tier = change.map_or(3, |ch| tier_of(gates, ch));
    // A2: a revert is at least the tier of the change it reverts (one with no gate is not known: tier 3)
    let reverted = change
        .and_then(|ch| ch.revert_of.as_ref())
        .and_then(|id| effects.get(id));
    if let Some(r) = reverted {
        tier = tier.max(r.gate.as_ref().map_or(3, |g| g.tier));
    }
    let mut owners: Vec<ActorId> = Vec::new();
    if let Some(ch) = change {
        for f in &ch.files {
            for path in paths_of(f) {
                let rule = gates
                    .owners
                    .iter()
                    .find(|r| in_repo(r.repo.as_ref(), &ch.repo) && glob_match(&r.glob, path));
                for o in rule.into_iter().flat_map(|r| &r.owners) {
                    if !owners.contains(o) {
                        owners.push(o.clone());
                    }
                }
            }
        }
    }
    if owners.is_empty()
        && let Some(p) = ctx.profile(author)
        && p.actor_kind != ActorKind::Human
        && p.operator_confirmed
        && let Some(op) = p.operator.as_ref().filter(|o| !o.is_empty())
    {
        owners.push(op.clone()); // no path owner: the proposer's confirmed operator
    }
    owners.truncate(MAX_PATH_OWNERS);
    Some(Gate {
        repo: change.map(|c| c.repo.clone()),
        tier,
        delta_digest: change.map(|c| c.delta_digest.clone()),
        change_digest: change.map(Change::digest),
        owners,
        revert_of: change.and_then(|c| c.revert_of.clone()),
        reverted_digest: reverted
            .and_then(|r| r.gate.as_ref())
            .and_then(|g| g.delta_digest.clone()),
    })
}
