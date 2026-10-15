//! `han-vcs`: the version-control seam of HAN (PLAN §10 "Version control is pluggable").
//!
//! han-host, han-exec and overlap tier 3 reach version control only through the [`Vcs`] trait:
//!
//! * [`Vcs::create_workspace`]: a session workspace at a base commit, on its own branch, with its own refs and
//!   index (SR-26), sharing a read-only object store per device;
//! * [`Vcs::changeset`]: the workspace's changes against its base as a [`ChangeSet`] (paths, kinds, content
//!   hashes, hunks, and a digest the executor checks before landing);
//! * [`Vcs::land`]: a compare-and-swap ref move on a remote (`expected` to `new`), desired state so a retry is
//!   safe (SR-69);
//! * [`Vcs::merge_simulate`]: whether two commits merge cleanly, and which paths conflict.
//!
//! Git is the first backend ([`git::GitVcs`]); it shells out to `git` with repo hooks and repo-supplied
//! credential, ssh and protocol settings disabled (SR-72).

pub mod git;
mod merge;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use git::{GitVcs, remove_tree};

/// The one ref of a bundle made by [`Vcs::bundle`].
pub const BUNDLE_REF: &str = "refs/han/bundle";

/// The media type of a bundle made by [`Vcs::bundle`] (the `type` of its `BlobRef`).
pub const BUNDLE_MEDIA_TYPE: &str = "application/x-git-bundle";

/// Why a version-control operation failed.
#[derive(Debug, thiserror::Error)]
pub enum VcsError {
    /// The request names something that is not valid (a ref, a path, a sha).
    #[error("invalid: {0}")]
    Invalid(String),
    /// Something it needs does not exist (a repository, a commit, a ref).
    #[error("not found: {0}")]
    NotFound(String),
    /// The backend tool failed.
    #[error("{tool} failed: {message}")]
    Tool {
        /// The tool, for example `git`.
        tool: String,
        /// What it said (never a credential).
        message: String,
    },
    /// An I/O error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// A result of this crate.
pub type Result<T> = std::result::Result<T, VcsError>;

/// Where a session workspace comes from and where it goes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSpec {
    /// The repository: a URL (`https://…`) or a local path to a repository.
    pub source: String,
    /// What to start from: a branch name, `HEAD`, or a full commit id. Resolved once to the base sha.
    pub base: String,
    /// The session's branch, for example `han/123-lars-coder`.
    pub branch: String,
    /// The directory to create; must not exist yet.
    pub dir: PathBuf,
    /// Sparse-checkout cone patterns (directories); empty means the whole tree.
    #[serde(default)]
    pub sparse: Vec<String>,
}

/// A created workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    /// Its working directory.
    pub dir: PathBuf,
    /// The base commit id (full hex).
    pub base: String,
    /// Its branch.
    pub branch: String,
    /// The shared object store it borrows objects from (read-only to the session), if any.
    pub shared_store: Option<PathBuf>,
}

/// A commit named by its short id and subject: what a report of unlanded work lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    /// The first 8 hex digits.
    pub id: String,
    /// The first line of the message.
    pub subject: String,
}

/// The arguments of `git cherry -v` that list the commits of `tip` the `target` lacks by patch id. With
/// `after`, only the commits after it are judged (a squash merge never matches the commits of the change it
/// landed, so the commits the change itself carried are not looked at). Shared by the host's workspace pruning
/// ([`Vcs::unlanded`]) and the `han dev prune` command, which run the same question over different checkouts.
pub fn cherry_args<'a>(target: &'a str, tip: &'a str, after: Option<&'a str>) -> Vec<&'a str> {
    let mut args = vec!["cherry", "-v", target, tip];
    args.extend(after);
    args
}

/// The commits `git cherry -v` marks `+` (not on the target), as [`Commit`]s.
pub fn parse_cherry(text: &str) -> Vec<Commit> {
    text.lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("+ ")?;
            let (id, subject) = rest.split_once(' ').unwrap_or((rest, ""));
            Some(Commit {
                id: id.chars().take(8).collect(),
                subject: subject.to_owned(),
            })
        })
        .collect()
}

/// What happened to one path.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// New.
    Added,
    /// Content changed.
    Modified,
    /// Removed.
    Deleted,
    /// Moved from another path (content may also have changed).
    Renamed,
    /// Only the mode changed (for example the executable bit).
    ModeChanged,
    /// The type changed (file, symlink, submodule).
    TypeChanged,
}

/// One changed path of a [`ChangeSet`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    /// The path, relative, `/`-separated, as git writes it.
    pub path: String,
    /// For a rename, the old path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// What happened.
    pub kind: ChangeKind,
    /// The base content's blob id, if the path existed at the base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_hash: Option<String>,
    /// The new content's blob id, if the path exists now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_hash: Option<String>,
    /// The new mode (`100644`, `100755`, `120000`, `160000`), if the path exists now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Changed line ranges `[start, len]` in the new file (1-based start; `len` 0 for a pure deletion at
    /// `start`), as `git diff -U0` reports them. Empty for binary files.
    #[serde(default)]
    pub hunks: Vec<(u32, u32)>,
    /// Whether git treats the file as binary.
    #[serde(default)]
    pub binary: bool,
    /// Lines added and removed (0 for binary files).
    #[serde(default)]
    pub added: u32,
    /// Lines removed.
    #[serde(default)]
    pub removed: u32,
}

/// The unit of landed work (PLAN §10 "Changesets"): the files changed against the base, with content hashes.
/// Semantic gates read its [`ChangeSummary`]; the executor checks the landed change against the approved
/// [`ChangeSet::digest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSet {
    /// The base commit.
    pub base: String,
    /// The commit the changes are in, when they are committed (`None` for a dirty working tree).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// The changed paths, sorted by path.
    pub files: Vec<FileChange>,
    /// [`changeset_digest`] of `base` and `files`.
    pub digest: String,
}

/// What tiers and gates read: paths and kinds, nothing else (no contents).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSummary {
    /// The digest of the change set it summarises.
    pub digest: String,
    /// `(path, kind)` pairs, sorted by path.
    pub paths: Vec<(String, ChangeKind)>,
}

impl ChangeSet {
    /// The summary semantic gates read.
    pub fn summary(&self) -> ChangeSummary {
        ChangeSummary {
            digest: self.digest.clone(),
            paths: self
                .files
                .iter()
                .map(|f| (f.path.clone(), f.kind.clone()))
                .collect(),
        }
    }
}

/// The digest of a change set: lowercase hex SHA-256 over a canonical line form that covers the base and,
/// for each file in path order, its path, old path, kind, old and new blob ids and mode. Hunks and line
/// counts are derived from the blobs, so they are not hashed.
pub fn changeset_digest(base: &str, files: &[FileChange]) -> String {
    use sha2::{Digest, Sha256};
    let mut sorted: Vec<&FileChange> = files.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut h = Sha256::new();
    h.update(b"han.changeset/1\n");
    h.update(base.as_bytes());
    h.update(b"\n");
    for f in sorted {
        let kind = serde_json::to_value(&f.kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        for part in [
            f.path.as_str(),
            f.from.as_deref().unwrap_or(""),
            kind.as_str(),
            f.old_hash.as_deref().unwrap_or(""),
            f.new_hash.as_deref().unwrap_or(""),
            f.mode.as_deref().unwrap_or(""),
        ] {
            h.update((part.len() as u64).to_be_bytes());
            h.update(part.as_bytes());
        }
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// A credential for a remote, held only in memory. Its `Debug` never shows the secret.
#[derive(Clone)]
pub struct RemoteAuth {
    /// The user name git sends (for GitHub installation tokens, `x-access-token`).
    pub username: String,
    /// The secret.
    secret: String,
}

impl RemoteAuth {
    /// A credential.
    pub fn new(username: impl Into<String>, secret: impl Into<String>) -> Self {
        RemoteAuth {
            username: username.into(),
            secret: secret.into(),
        }
    }

    /// The secret (for the backend only).
    pub fn secret(&self) -> &str {
        &self.secret
    }
}

impl std::fmt::Debug for RemoteAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteAuth")
            .field("username", &self.username)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// A compare-and-swap ref move: make `refname` on `remote` point at `new`, only if it points at `expected`
/// now (`None`: only if it does not exist).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandSpec {
    /// The remote URL or path.
    pub remote: String,
    /// The full ref name, for example `refs/heads/han/123-lars-coder`.
    pub refname: String,
    /// What the ref must point at before the move.
    pub expected: Option<String>,
    /// The commit to move it to.
    pub new: String,
}

/// How a land ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum LandOutcome {
    /// The ref moved from `expected` to `new`.
    Landed,
    /// The ref already pointed at `new` (a retry after a lost reply): done, nothing moved.
    AlreadyThere,
    /// The ref points somewhere else; nothing was forced.
    Conflict {
        /// Where it points (`None`: it does not exist).
        actual: Option<String>,
    },
}

/// The result of a merge simulation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeSim {
    /// Whether the merge is clean.
    pub clean: bool,
    /// Paths with conflicts, sorted.
    pub conflicts: Vec<String>,
}

impl From<&ConflictRecord> for MergeSim {
    fn from(r: &ConflictRecord) -> Self {
        let mut conflicts: Vec<String> = r.conflicts.iter().map(|c| c.path.clone()).collect();
        conflicts.sort();
        conflicts.dedup();
        MergeSim {
            clean: conflicts.is_empty(),
            conflicts,
        }
    }
}

/// What a [`Delta`] entry did to its path.
///
/// Serialised flat into the entry as `kind` (`add`, `modify`, `delete`, `rename`) plus, for a rename,
/// `fromPath`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum DeltaKind {
    /// The path did not exist at the base.
    Add,
    /// The path exists on both sides with another content id, type or mode.
    Modify,
    /// The path does not exist at the head.
    Delete,
    /// The content moved: the path is new, `from_path` is gone. Detected by [`Vcs::delta`] (exact content
    /// only).
    Rename {
        /// The path the content had at the base.
        from_path: String,
    },
}

/// One path of a [`Delta`] (R33 §1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeltaEntry {
    /// The path (for a rename, the new one), relative, `/`-separated.
    pub path: String,
    /// What happened to it.
    #[serde(flatten)]
    pub kind: DeltaKind,
    /// The backend's object id of the content at the base (for a rename, at `from_path`); absent for an add.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The mode at the base (`100644`, `100755`, `120000`, `160000`); absent for an add. Not in R33 §1's
    /// list, but without it a mode change landed on the target in the meantime would be undone silently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_mode: Option<String>,
    /// The object id of the content at the head; absent for a delete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// The mode at the head; absent for a delete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

impl DeltaEntry {
    /// The paths this entry reads or writes: its own and, for a rename, the old one.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        let from = match &self.kind {
            DeltaKind::Rename { from_path } => Some(from_path.as_str()),
            _ => None,
        };
        std::iter::once(self.path.as_str()).chain(from)
    }
}

/// The changes between a base and a head as a path-sorted list that does not name the base commit, so it is
/// the same wherever the change is rebased to. Its [`delta_digest`] is what an approval and its evidence
/// are over; [`changeset_digest`] stays the digest of the landed result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Delta {
    /// The entries, sorted by (new) path in byte order. At most one per path.
    pub entries: Vec<DeltaEntry>,
}

/// `sha256(JCS(delta))` as lowercase hex: the digest of the entry list, with absent fields left out.
pub fn delta_digest(delta: &Delta) -> Result<String> {
    use sha2::{Digest, Sha256};
    let value = han_core::json::to_value(delta).map_err(|e| VcsError::Invalid(e.to_string()))?;
    let bytes =
        han_core::json::canonicalize(&value).map_err(|e| VcsError::Invalid(e.to_string()))?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

impl Delta {
    /// [`delta_digest`] of this delta.
    pub fn digest(&self) -> Result<String> {
        delta_digest(self)
    }
}

/// The answer of [`Vcs::path_disjoint`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathDisjoint {
    /// Whether the delta applies to the new base without changing: every path it reads or writes is as it
    /// was at the old base, and so is the kind (file or directory) of every ancestor directory.
    pub ok: bool,
    /// The paths that broke the condition, sorted: delta paths whose content differs in the new base, and
    /// ancestor directories that are no longer directories (or no longer files).
    pub touched: Vec<String>,
}

/// A change squash-rebased onto a new base.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rebased {
    /// The new commit: one commit on `onto`.
    pub commit: String,
    /// The delta from `onto` to `commit`. Equal to the original delta when `path_disjoint`.
    pub delta: Delta,
    /// Whether the original delta applied unchanged (see [`PathDisjoint`]). When false the change merged
    /// cleanly but its result is not what was approved, so it must be gated again (R33 §2).
    pub path_disjoint: bool,
}

/// The result of [`Vcs::squash_rebase`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RebaseOutcome {
    /// The change is on the new base.
    Rebased(Rebased),
    /// It does not merge; nothing was written but the objects the record names.
    Conflict(ConflictRecord),
}

/// Why a path conflicts (R33 §5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
    /// Both sides changed the same text.
    Content,
    /// Both sides added the path with different content.
    AddAdd,
    /// One side deleted what the other changed.
    ModifyDelete,
    /// One side has a file where the other has a directory.
    FileDirectory,
    /// Both sides changed a binary file (a submodule pointer counts as opaque content too).
    Binary,
    /// A symbolic link is involved.
    Symlink,
}

/// One side of a conflicted path: a git tree entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictSide {
    /// The mode: `100644`, `100755`, `120000`, `160000`, or `040000` for a directory.
    pub mode: String,
    /// The object id (a blob, or a tree for a directory).
    pub id: String,
}

/// A line range, 1-based: `len` lines from `start`. An empty range (`len` 0) sits before line `start`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineRange {
    /// The first line.
    pub start: u32,
    /// How many lines.
    pub len: u32,
}

/// Where a resolution candidate comes from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CandidateSource {
    /// The side being rebased onto.
    Ours,
    /// The change being rebased.
    Theirs,
    /// An external structural-merge tool (for example mergiraf). Nothing in this crate fills it yet.
    Tool {
        /// The tool.
        name: String,
        /// Its version.
        version: String,
    },
}

/// One proposed resolution of a hunk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    /// An id for the candidate, unique within the record.
    pub id: String,
    /// Where it comes from.
    pub source: CandidateSource,
    /// The git blob that holds the resolved text of the hunk.
    pub content_id: String,
}

/// A region of a text file where the two sides disagree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictHunk {
    /// The region in the merge base.
    pub base_range: LineRange,
    /// The region in the side being rebased onto.
    pub ours_range: LineRange,
    /// The region in the change being rebased.
    pub theirs_range: LineRange,
    /// Resolutions to choose from: ours and theirs, plus any tool's.
    pub candidates: Vec<Candidate>,
}

/// A path that does not merge.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
    /// The path, relative, `/`-separated.
    pub path: String,
    /// Why it conflicts.
    pub kind: ConflictKind,
    /// The merge base's entry, if the path existed there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<ConflictSide>,
    /// The entry on the side rebased onto.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ours: Option<ConflictSide>,
    /// The entry of the change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theirs: Option<ConflictSide>,
    /// The disagreeing regions of a `content` conflict; empty for the other kinds.
    #[serde(default)]
    pub hunks: Vec<ConflictHunk>,
}

/// A merge or rebase that does not go through, as data (R33 §5): never conflict markers. Every content it
/// names is a git blob (or tree) in the repository that was asked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictRecord {
    /// The commit merged into.
    pub onto: String,
    /// The paths the change touches that merge cleanly, sorted.
    pub clean: Vec<String>,
    /// The paths that do not, sorted.
    pub conflicts: Vec<Conflict>,
}

/// The version-control seam.
pub trait Vcs: Send + Sync + 'static {
    /// The backend's name, for example `git`.
    fn name(&self) -> &'static str;

    /// Creates a session workspace at `spec.base` on `spec.branch`.
    fn create_workspace(&self, spec: &WorkspaceSpec) -> Result<Workspace>;

    /// The workspace's changes against `base`: committed and uncommitted, untracked files that are not
    /// ignored included.
    fn changeset(&self, dir: &Path, base: &str) -> Result<ChangeSet>;

    /// The changes between two commits of the workspace's repository (`head` committed).
    fn changeset_between(&self, dir: &Path, base: &str, head: &str) -> Result<ChangeSet>;

    /// Commits everything in the workspace (as `git add -A` would) on its branch and returns the commit id;
    /// with nothing to commit, returns the current head.
    fn commit_all(&self, dir: &Path, message: &str, author: &str) -> Result<String>;

    /// Where `refname` points on `remote` now (`None`: it does not exist).
    fn remote_ref(
        &self,
        dir: &Path,
        remote: &str,
        refname: &str,
        auth: Option<&RemoteAuth>,
    ) -> Result<Option<String>>;

    /// Moves a ref on a remote by compare-and-swap, pushing the objects `new` needs from `dir`.
    fn land(&self, dir: &Path, spec: &LandSpec, auth: Option<&RemoteAuth>) -> Result<LandOutcome>;

    /// The commits `base..head` of the repository at `dir` as one self-describing file (for git, a bundle
    /// whose one ref, [`BUNDLE_REF`], is `head` and whose prerequisite is `base`). The host hands it to an
    /// executor that can't read `dir` (R15).
    fn bundle(&self, dir: &Path, base: &str, head: &str) -> Result<Vec<u8>> {
        let _ = (dir, base, head);
        Err(VcsError::Invalid(format!(
            "{} makes no bundles",
            self.name()
        )))
    }

    /// Imports a [`Vcs::bundle`] into a repository of this backend's own for `remote`, fetching `base` from
    /// `remote` (with `auth`) when the repository lacks it, and returns that repository, which then holds
    /// `head`. Fails if the bundle's head is not `head` or its prerequisite is not `base`.
    fn import_bundle(
        &self,
        remote: &str,
        auth: Option<&RemoteAuth>,
        bundle: &[u8],
        base: &str,
        head: &str,
    ) -> Result<PathBuf> {
        let _ = (remote, auth, bundle, base, head);
        Err(VcsError::Invalid(format!(
            "{} imports no bundles",
            self.name()
        )))
    }

    /// Fetches the tip of `refname` from `remote` (with `auth`) into the repository of this backend's own for
    /// `remote` (the one [`Vcs::import_bundle`] returns), and returns that repository with the tip's commit id:
    /// the commit exactly as fetched. `None` when the ref does not exist. A landing lane squash-rebases onto it.
    fn fetch_ref(
        &self,
        remote: &str,
        auth: Option<&RemoteAuth>,
        refname: &str,
    ) -> Result<Option<(PathBuf, String)>> {
        let _ = (remote, auth, refname);
        Err(VcsError::Invalid(format!(
            "{} fetches no refs",
            self.name()
        )))
    }

    /// Simulates merging `theirs` into `ours` in the repository at `dir`, without touching its working tree,
    /// index or refs.
    fn merge_simulate(&self, dir: &Path, ours: &str, theirs: &str) -> Result<MergeSim>;

    /// [`Vcs::merge_simulate`] with the conflicts as data (R33 §5). The record has no conflicts when the
    /// merge is clean. Contents it references are written to the repository's object store.
    fn merge_record(&self, dir: &Path, ours: &str, theirs: &str) -> Result<ConflictRecord> {
        let _ = (dir, ours, theirs);
        Err(VcsError::Invalid(format!(
            "{} makes no conflict records",
            self.name()
        )))
    }

    /// The parents of `commit` in the repository at `dir`, in order (empty for a root commit). A landing
    /// executor checks with it that the commit it moves a ref to sits directly on the tip it was tested on.
    fn parents(&self, dir: &Path, commit: &str) -> Result<Vec<String>> {
        let _ = (dir, commit);
        Err(VcsError::Invalid(format!(
            "{} lists no commit parents",
            self.name()
        )))
    }

    /// The [`Delta`] from `base` to `head`, both commits of the repository at `dir`.
    ///
    /// Entries come from a tree-to-tree comparison (no content or similarity heuristics of the backend).
    /// A rename is a path deleted and a path added with the same content id and the same type (regular file
    /// or symbolic link); several candidates with one content id pair up in path order, the n-th deleted
    /// path with the n-th added path. A rename that also edits the file is a delete and an add.
    fn delta(&self, dir: &Path, base: &str, head: &str) -> Result<Delta> {
        let _ = (dir, base, head);
        Err(VcsError::Invalid(format!(
            "{} makes no deltas",
            self.name()
        )))
    }

    /// The unified text diff from `base` to `head`, both commits of the repository at `dir`: no colour, no
    /// renames, no external diff or text conversion, so any implementation reads the same bytes. Binary files
    /// appear as a one-line notice. A reviewer reads it; nothing is applied from it.
    fn diff_text(&self, dir: &Path, base: &str, head: &str) -> Result<String> {
        let _ = (dir, base, head);
        Err(VcsError::Invalid(format!(
            "{} makes no text diffs",
            self.name()
        )))
    }

    /// Whether `delta` applies to `new_base` unchanged (R33 §1), and the paths that say otherwise.
    fn path_disjoint(&self, dir: &Path, delta: &Delta, new_base: &str) -> Result<PathDisjoint> {
        let _ = (dir, delta, new_base);
        Err(VcsError::Invalid(format!(
            "{} checks no path-disjointness",
            self.name()
        )))
    }

    /// Squashes `base..head` into one commit on `onto` (R33 §2), without touching any checkout.
    ///
    /// When the delta is path-disjoint from `onto`, the commit's tree is `onto`'s with exactly the approved
    /// entries written, so its delta equals the original. Otherwise the change is merged three-way
    /// (`base`, `onto`, `head`): a clean merge returns `path_disjoint: false`, a conflict a record.
    ///
    /// The commit is a function of the arguments: `author` (`Name <email>` or a bare name) is author and
    /// committer, `committer_time` (seconds since the epoch, UTC) is both dates.
    #[allow(clippy::too_many_arguments)]
    fn squash_rebase(
        &self,
        dir: &Path,
        base: &str,
        head: &str,
        onto: &str,
        message: &str,
        author: &str,
        committer_time: i64,
    ) -> Result<RebaseOutcome> {
        let _ = (dir, base, head, onto, message, author, committer_time);
        Err(VcsError::Invalid(format!(
            "{} makes no squash-rebases",
            self.name()
        )))
    }

    /// Whether `ancestor` is `descendant` or one of its ancestors, in the repository at `dir`. Both are
    /// commit ids (the descendant may be `HEAD`). A host asks it before it bases new work on a commit it already proposed.
    fn is_ancestor(&self, dir: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
        let _ = (dir, ancestor, descendant);
        Err(VcsError::Invalid(format!(
            "{} answers no ancestry",
            self.name()
        )))
    }

    /// The commit the workspace is on (`HEAD`), committed or not.
    fn head(&self, dir: &Path) -> Result<String> {
        let _ = dir;
        Err(VcsError::Invalid(format!("{} names no head", self.name())))
    }

    /// Whether the workspace has changes that are not committed (untracked files that are not ignored
    /// included). It runs no program of the workspace's configuration.
    fn is_dirty(&self, dir: &Path) -> Result<bool> {
        let _ = dir;
        Err(VcsError::Invalid(format!(
            "{} tells no dirty workspaces",
            self.name()
        )))
    }

    /// Fetches the tip of `refname` from `remote` (with `auth`) into the workspace at `dir`, where it can be
    /// compared with the workspace's own commits ([`Vcs::unlanded`]), and returns its commit id (`None`: the
    /// ref does not exist).
    fn fetch_target(
        &self,
        dir: &Path,
        remote: &str,
        refname: &str,
        auth: Option<&RemoteAuth>,
    ) -> Result<Option<String>> {
        let _ = (dir, remote, refname, auth);
        Err(VcsError::Invalid(format!(
            "{} fetches no targets",
            self.name()
        )))
    }

    /// The commits of `tip` that `target` lacks by patch id (R33 §7), oldest first, in the repository at
    /// `dir`. With `after`, only the commits after it (an ancestor of `tip`) are judged. All three are commit
    /// ids the repository holds.
    fn unlanded(
        &self,
        dir: &Path,
        target: &str,
        tip: &str,
        after: Option<&str>,
    ) -> Result<Vec<Commit>> {
        let _ = (dir, target, tip, after);
        Err(VcsError::Invalid(format!(
            "{} lists no unlanded commits",
            self.name()
        )))
    }

    /// Removes a workspace (after merge or cancel).
    fn remove_workspace(&self, ws: &Workspace) -> Result<()>;
}
