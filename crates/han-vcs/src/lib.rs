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

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use git::GitVcs;

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

    /// Simulates merging `theirs` into `ours` in the repository at `dir`, without touching its working tree,
    /// index or refs.
    fn merge_simulate(&self, dir: &Path, ours: &str, theirs: &str) -> Result<MergeSim>;

    /// Removes a workspace (after merge or cancel).
    fn remove_workspace(&self, ws: &Workspace) -> Result<()>;
}
