//! The git backend: shells out to `git` (PLAN §6.7: gix can't push yet). One shared, read-only object store per
//! device (a bare cache repository per remote), and per-session clones that borrow its objects through
//! alternates, each with its own refs and index (SR-26).
//!
//! **No code from the worktree runs (SR-72).** Every invocation clears the `GIT_*` environment, ignores the
//! system and global configuration (`GIT_CONFIG_NOSYSTEM`, `GIT_CONFIG_GLOBAL` = an empty file), and
//! overrides on the command line (which beats repository configuration) the settings that run programs or
//! reach the network: hooks (`core.hooksPath` = an empty directory), credential helpers, `core.askPass`,
//! `core.sshCommand`, `core.fsmonitor`, `diff.external`, and the allowed protocols. Settings that name
//! drivers by an arbitrary name (`filter.<x>.clean`, `diff.<x>.textconv`, `merge.<x>.driver`) cannot be
//! overridden that way, so this backend never runs a git command that would apply them in a session's
//! repository: file contents are hashed with `hash-object --no-filters`, trees are built in a temporary
//! index with `update-index --index-info`, diffs are tree-to-tree with `--no-ext-diff --no-textconv`, and
//! merges are simulated in a scratch bare repository whose configuration is this backend's own.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use sha2::{Digest, Sha256};

use crate::merge::{self, parse_changes, parse_merge_tree};
use crate::{
    BUNDLE_REF, Candidate, CandidateSource, ChangeKind, ChangeSet, Conflict, ConflictHunk,
    ConflictKind, ConflictRecord, ConflictSide, Delta, DeltaEntry, DeltaKind, FileChange,
    LandOutcome, LandSpec, LineRange, MergeSim, PathDisjoint, RebaseOutcome, Rebased, RemoteAuth,
    Result, Vcs, VcsError, Workspace, WorkspaceSpec, changeset_digest,
};

/// What the backend remembers about a file it hashed: size, modification time and mode, and the blob id.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    mtime: Option<SystemTime>,
    mode: String,
    blob: String,
}

/// The git backend.
#[derive(Clone, Debug)]
pub struct GitVcs {
    /// Where the shared object stores live (one bare repository per remote).
    store_root: PathBuf,
    /// The `git` program.
    git: PathBuf,
    /// Protocols remotes may use (`protocol.<name>.allow=always`); every other one is `never`.
    protocols: Vec<String>,
    /// Blob ids of worktree files by workspace and path, so unchanged files are not hashed again.
    stamps: Arc<Mutex<HashMap<PathBuf, HashMap<String, Stamp>>>>,
    /// Serialises fetches into the shared stores within this process.
    fetch_lock: Arc<Mutex<()>>,
}

static UNIQUE: AtomicU64 = AtomicU64::new(0);

fn unique(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{prefix}-{}-{}-{nanos}",
        std::process::id(),
        UNIQUE.fetch_add(1, Ordering::Relaxed)
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn is_hex_id(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Checks a git bundle's header: version 2 or 3, one ref, [`BUNDLE_REF`] at `head`, and no prerequisite but
/// `base`.
fn check_bundle(bundle: &[u8], base: &str, head: &str) -> Result<()> {
    let bad = |why: &str| VcsError::Invalid(format!("not the bundle of {base}..{head}: {why}"));
    let end = bundle
        .windows(2)
        .take(1 << 16)
        .position(|w| w == b"\n\n")
        .ok_or_else(|| bad("no header"))?;
    let header = std::str::from_utf8(&bundle[..end]).map_err(|_| bad("its header is not text"))?;
    let mut lines = header.lines();
    if !matches!(lines.next(), Some("# v2 git bundle" | "# v3 git bundle")) {
        return Err(bad("not a v2 or v3 bundle"));
    }
    let mut refs = Vec::new();
    for line in lines {
        if line.starts_with('@') {
            continue; // a v3 capability
        }
        if let Some(pre) = line.strip_prefix('-') {
            if pre.split(' ').next() != Some(base) {
                return Err(bad("it needs a commit other than the base"));
            }
            continue;
        }
        refs.push(line.split_once(' ').ok_or_else(|| bad("a malformed ref"))?);
    }
    if refs != [(head, BUNDLE_REF)] {
        return Err(bad("its ref is not the head"));
    }
    Ok(())
}

/// A path as git config and alternates want it: forward slashes.
fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Removes a credential from text: the secret itself, its Basic forms, and any `scheme://user:pass@` userinfo.
fn scrub(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_owned();
    for s in secrets.iter().filter(|s| !s.is_empty()) {
        out = out.replace(s.as_str(), "<redacted>");
    }
    // userinfo in URLs
    let mut result = String::with_capacity(out.len());
    let mut rest = out.as_str();
    while let Some(i) = rest.find("://") {
        let (head, tail) = rest.split_at(i + 3);
        result.push_str(head);
        let end = tail
            .find(|c: char| c == '/' || c.is_whitespace() || c == '\'' || c == '"')
            .unwrap_or(tail.len());
        match tail[..end].rfind('@') {
            Some(at) => {
                result.push_str("<redacted>@");
                rest = &tail[at + 1..];
            }
            None => rest = tail,
        }
    }
    result.push_str(rest);
    result
}

/// Undoes git's C-style quoting of a path (`"a\tb"`), if it is quoted.
fn unquote(s: &str) -> String {
    let Some(inner) = s.strip_prefix('"').and_then(|x| x.strip_suffix('"')) else {
        return s.to_owned();
    };
    let mut bytes = Vec::new();
    let b = inner.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            i += 1;
            match b[i] {
                b'n' => bytes.push(b'\n'),
                b't' => bytes.push(b'\t'),
                b'r' => bytes.push(b'\r'),
                b'a' => bytes.push(7),
                b'b' => bytes.push(8),
                b'f' => bytes.push(12),
                b'v' => bytes.push(11),
                b'0'..=b'7' if i + 2 < b.len() => {
                    let oct = &inner[i..i + 3];
                    bytes.push(u8::from_str_radix(oct, 8).unwrap_or(b'?'));
                    i += 2;
                }
                c => bytes.push(c),
            }
        } else {
            bytes.push(b[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Removes a directory tree, clearing read-only bits (git's object files) when the first attempt fails.
fn remove_tree(p: &Path) -> std::io::Result<()> {
    if !p.exists() {
        return Ok(());
    }
    if std::fs::remove_dir_all(p).is_ok() {
        return Ok(());
    }
    fn writable(p: &Path) {
        if let Ok(meta) = std::fs::symlink_metadata(p) {
            if meta.is_dir()
                && let Ok(rd) = std::fs::read_dir(p)
            {
                for e in rd.flatten() {
                    writable(&e.path());
                }
            }
            if !meta.file_type().is_symlink() {
                let mut perm = meta.permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                perm.set_readonly(false);
                let _ = std::fs::set_permissions(p, perm);
            }
        }
    }
    writable(p);
    std::fs::remove_dir_all(p)
}

/// A git command under construction, with the secrets its errors must not show.
struct Git {
    cmd: Command,
    secrets: Vec<String>,
    input: Option<Vec<u8>>,
}

impl Git {
    fn arg(mut self, a: impl AsRef<OsStr>) -> Self {
        self.cmd.arg(a);
        self
    }

    fn args<I, S>(mut self, a: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.cmd.args(a);
        self
    }

    fn env(mut self, k: &str, v: impl AsRef<OsStr>) -> Self {
        self.cmd.env(k, v);
        self
    }

    fn index(self, file: &Path) -> Self {
        self.env("GIT_INDEX_FILE", file)
    }

    fn stdin(mut self, input: Vec<u8>) -> Self {
        self.input = Some(input);
        self
    }

    /// Runs it; `Ok` with the output when the exit code is one of `ok` (0 always is).
    fn run_codes(mut self, ok: &[i32]) -> Result<Output> {
        let what = format!("{:?}", self.cmd.get_args().take(12).collect::<Vec<_>>());
        self.cmd
            .stdin(if self.input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = self.cmd.spawn().map_err(|e| VcsError::Tool {
            tool: "git".into(),
            message: format!("cannot run git: {e}"),
        })?;
        let writer = match (self.input.take(), child.stdin.take()) {
            (Some(input), Some(mut stdin)) => Some(std::thread::spawn(move || {
                let _ = stdin.write_all(&input);
            })),
            _ => None,
        };
        let out = child.wait_with_output()?;
        if let Some(w) = writer {
            let _ = w.join();
        }
        let code = out.status.code().unwrap_or(-1);
        if code == 0 || ok.contains(&code) {
            return Ok(out);
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let message = scrub(
            &format!(
                "{} (exit {code}): {}",
                scrub(&what, &self.secrets),
                stderr.trim()
            ),
            &self.secrets,
        );
        Err(VcsError::Tool {
            tool: "git".into(),
            message,
        })
    }

    fn run(self) -> Result<Output> {
        self.run_codes(&[])
    }

    /// Runs it and returns stdout as text, trimmed.
    fn text(self) -> Result<String> {
        let out = self.run()?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }
}

/// One raw `diff-tree` entry.
struct RawEntry {
    old_mode: String,
    new_mode: String,
    old_sha: String,
    new_sha: String,
    status: char,
    path: String,
    from: Option<String>,
}

impl GitVcs {
    /// A backend whose shared object stores live under `store_root`. Remotes may use `https` and `file`.
    pub fn new(store_root: impl Into<PathBuf>) -> Self {
        GitVcs {
            store_root: store_root.into(),
            git: PathBuf::from("git"),
            protocols: vec!["https".into(), "file".into()],
            stamps: Arc::default(),
            fetch_lock: Arc::default(),
        }
    }

    /// Uses another `git` program.
    #[must_use]
    pub fn with_git(mut self, git: impl Into<PathBuf>) -> Self {
        self.git = git.into();
        self
    }

    /// Sets the protocols remotes may use (for example `["https"]` in production).
    #[must_use]
    pub fn with_protocols<I: IntoIterator<Item = S>, S: Into<String>>(
        mut self,
        protocols: I,
    ) -> Self {
        self.protocols = protocols.into_iter().map(Into::into).collect();
        self
    }

    /// The directory of the shared stores.
    pub fn store_root(&self) -> &Path {
        &self.store_root
    }

    /// The `git` program in use.
    pub fn git_program(&self) -> &Path {
        &self.git
    }

    /// The protocols remotes may use.
    pub fn protocols(&self) -> &[String] {
        &self.protocols
    }

    /// The repository a bundle for `remote` is imported into: the executor's own, one per remote (R15).
    pub fn landing_for(&self, remote: &str) -> PathBuf {
        let key = normalise_source(remote);
        let digest = hex(&Sha256::digest(key.as_bytes()));
        self.store_root
            .join(format!("landing-{}.git", &digest[..16]))
    }

    /// The shared store (a bare repository) for a source.
    pub fn store_for(&self, source: &str) -> PathBuf {
        let key = normalise_source(source);
        let digest = hex(&Sha256::digest(key.as_bytes()));
        self.store_root.join(format!("{}.git", &digest[..16]))
    }

    fn hooks_dir(&self) -> PathBuf {
        self.store_root.join("no-hooks")
    }

    fn empty_config(&self) -> PathBuf {
        self.store_root.join("empty.gitconfig")
    }

    fn template_dir(&self) -> PathBuf {
        self.store_root.join("no-template")
    }

    fn tmp_dir(&self) -> PathBuf {
        self.store_root.join("tmp")
    }

    fn ensure_root(&self) -> Result<()> {
        for d in [self.hooks_dir(), self.template_dir(), self.tmp_dir()] {
            std::fs::create_dir_all(&d)?;
        }
        let cfg = self.empty_config();
        if !cfg.exists() {
            std::fs::write(&cfg, b"")?;
        }
        Ok(())
    }

    /// A git command with the isolation every invocation gets (SR-72), run in `cwd`.
    fn git(&self, cwd: Option<&Path>) -> Result<Git> {
        self.ensure_root()?;
        let mut cmd = Command::new(&self.git);
        for (k, _) in std::env::vars_os() {
            let name = k.to_string_lossy().to_ascii_uppercase();
            if name.starts_with("GIT_") || name == "SSH_ASKPASS" || name.starts_with("GCM_") {
                cmd.env_remove(&k);
            }
        }
        cmd.env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.empty_config())
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GCM_INTERACTIVE", "never");
        let mut c = vec![
            format!("core.hooksPath={}", slash(&self.hooks_dir())),
            "credential.helper=".into(),
            "core.askPass=".into(),
            "core.sshCommand=".into(),
            "core.gitProxy=".into(),
            "core.fsmonitor=false".into(),
            "core.untrackedCache=false".into(),
            "diff.external=".into(),
            "core.quotePath=false".into(),
            "core.autocrlf=false".into(),
            "core.longpaths=true".into(),
            "safe.directory=*".into(),
            "gc.auto=0".into(),
            "maintenance.auto=false".into(),
            "protocol.allow=never".into(),
        ];
        for p in &self.protocols {
            c.push(format!("protocol.{p}.allow=always"));
        }
        for kv in c {
            cmd.arg("-c").arg(kv);
        }
        match cwd {
            Some(d) => {
                cmd.current_dir(d);
            }
            None => {
                // Commands that need no repository run in the store root, and never discover one above it.
                cmd.current_dir(&self.store_root);
                if let Some(parent) = self.store_root.parent() {
                    cmd.env("GIT_CEILING_DIRECTORIES", parent);
                }
            }
        }
        Ok(Git {
            cmd,
            secrets: Vec::new(),
            input: None,
        })
    }

    /// A git command that authenticates to `remote` with `auth` through the environment only (SR-50).
    fn git_auth(&self, cwd: Option<&Path>, remote: &str, auth: Option<&RemoteAuth>) -> Result<Git> {
        let mut g = self.git(cwd)?;
        if let Some(a) = auth {
            let basic = base64::engine::general_purpose::STANDARD.encode(format!(
                "{}:{}",
                a.username,
                a.secret()
            ));
            g.secrets = vec![
                a.secret().to_owned(),
                basic.clone(),
                base64::engine::general_purpose::STANDARD.encode(a.secret()),
            ];
            if remote.starts_with("https://") || remote.starts_with("http://") {
                g = g
                    .env("GIT_CONFIG_COUNT", "2")
                    .env("GIT_CONFIG_KEY_0", format!("http.{remote}.extraHeader"))
                    .env(
                        "GIT_CONFIG_VALUE_0",
                        format!("Authorization: Basic {basic}"),
                    )
                    .env("GIT_CONFIG_KEY_1", "http.followRedirects")
                    .env("GIT_CONFIG_VALUE_1", "false");
            }
        }
        Ok(g)
    }

    fn git_dir(&self, dir: &Path) -> Result<PathBuf> {
        let out = self
            .git(Some(dir))?
            .args(["rev-parse", "--absolute-git-dir"])
            .text()?;
        Ok(PathBuf::from(out))
    }

    fn commit_of(&self, dir: &Path, rev: &str) -> Result<String> {
        self.git(Some(dir))?
            .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
            .arg(format!("{rev}^{{commit}}"))
            .text()
            .ok()
            .filter(|s| is_hex_id(s))
            .ok_or_else(|| VcsError::NotFound(format!("no commit {rev}")))
    }

    fn zero_id(&self, dir: &Path) -> Result<String> {
        let fmt = self
            .git(Some(dir))?
            .args(["rev-parse", "--show-object-format"])
            .text()
            .unwrap_or_else(|_| "sha1".into());
        Ok("0".repeat(if fmt == "sha256" { 64 } else { 40 }))
    }

    /// Takes the cross-process lock of a shared store (a lock directory), waiting up to two minutes; a lock
    /// older than ten minutes is taken over.
    fn store_lock(&self, store: &Path) -> Result<StoreLock> {
        let lock = store.with_extension("git.lock");
        for _ in 0..1200 {
            match std::fs::create_dir(&lock) {
                Ok(()) => return Ok(StoreLock(lock)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(&lock)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > Duration::from_secs(600));
                    if stale {
                        let _ = std::fs::remove_dir(&lock);
                        continue;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(VcsError::Tool {
            tool: "git".into(),
            message: format!("the shared store {} stays locked", store.display()),
        })
    }

    /// Creates or refreshes the shared store of `source`.
    fn ensure_store(&self, source: &str) -> Result<PathBuf> {
        let store = self.store_for(source);
        let _local = self.fetch_lock.lock().unwrap_or_else(|p| p.into_inner());
        let _lock = self.store_lock(&store)?;
        if !store.join("HEAD").exists() {
            remove_tree(&store)?;
            self.git(None)?
                .args(["init", "--bare", "--quiet"])
                .arg(format!("--template={}", slash(&self.template_dir())))
                .arg(&store)
                .run()?;
            for (k, v) in [
                ("remote.origin.url", normalise_source(source)),
                ("remote.origin.fetch", "+refs/heads/*:refs/heads/*".into()),
                ("gc.auto", "0".into()),
                ("gc.pruneExpire", "never".into()),
                ("core.logAllRefUpdates", "false".into()),
            ] {
                self.git(Some(&store))?.args(["config", k, &v]).run()?;
            }
            self.git(Some(&store))?
                .args([
                    "config",
                    "--add",
                    "remote.origin.fetch",
                    "+refs/tags/*:refs/tags/*",
                ])
                .run()?;
        }
        self.git(Some(&store))?
            .args([
                "fetch",
                "--prune",
                "--quiet",
                "--no-write-fetch-head",
                "origin",
            ])
            .run()?;
        Ok(store)
    }

    /// The commit `base` names in the shared store.
    fn resolve_base(&self, store: &Path, source: &str, base: &str) -> Result<String> {
        if is_hex_id(base) {
            return self.commit_of(store, base);
        }
        if base == "HEAD" {
            let out = self
                .git(Some(store))?
                .args(["ls-remote", "--symref", &normalise_source(source), "HEAD"])
                .text()?;
            if let Some(r) = out
                .lines()
                .find_map(|l| l.strip_prefix("ref: ").and_then(|x| x.split('\t').next()))
                && let Ok(c) = self.commit_of(store, r)
            {
                return Ok(c);
            }
            return self.commit_of(store, "HEAD");
        }
        for r in [
            format!("refs/heads/{base}"),
            format!("refs/tags/{base}"),
            base.into(),
        ] {
            if let Ok(c) = self.commit_of(store, &r) {
                return Ok(c);
            }
        }
        Err(VcsError::NotFound(format!("no commit {base} in {source}")))
    }

    /// The tree of the workspace's working state (tracked and untracked-not-ignored files as they are on
    /// disk, without filters), built in a fresh temporary index. Returns the tree, the temporary index (which
    /// the caller removes or installs) and the paths outside a sparse checkout.
    ///
    /// The session's own index is only read. The temporary index starts empty and gets every entry through
    /// `update-index --index-info`, so its entries carry no stat data: git never finds one "racily clean"
    /// and never re-reads a file through the repository's filters to check it (SR-72).
    fn worktree_tree(&self, dir: &Path) -> Result<(String, PathBuf, Vec<String>)> {
        let tmp = self.tmp_dir().join(unique("index"));
        let built = self.build_tree(dir, &tmp);
        if built.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        built.map(|(t, skip)| (t, tmp, skip))
    }

    fn build_tree(&self, dir: &Path, index: &Path) -> Result<(String, Vec<String>)> {
        // The session index's entries: mode and blob per path (stage 0, or the first stage of a conflict).
        let staged = self.git(Some(dir))?.args(["ls-files", "-z", "-s"]).run()?;
        let mut entries: BTreeMap<String, (String, String)> = BTreeMap::new();
        for rec in staged.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            let rec = String::from_utf8_lossy(rec);
            if let Some((meta, path)) = rec.split_once('\t') {
                let mut it = meta.split(' ');
                if let (Some(mode), Some(sha)) = (it.next(), it.next()) {
                    entries
                        .entry(path.to_owned())
                        .or_insert((mode.to_owned(), sha.to_owned()));
                }
            }
        }
        // Which of them are outside a sparse checkout, and the untracked files that are not ignored.
        let tagged = self
            .git(Some(dir))?
            .args([
                "ls-files",
                "-z",
                "-t",
                "--cached",
                "--others",
                "--exclude-standard",
            ])
            .run()?;
        let mut skip: Vec<String> = Vec::new();
        let mut paths: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for rec in tagged.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            let rec = String::from_utf8_lossy(rec);
            let (tag, path) = rec.split_at(rec.find(' ').map_or(0, |i| i + 1));
            if !seen.insert(path.to_owned()) {
                continue;
            }
            if tag.starts_with('S') {
                skip.push(path.to_owned());
            } else {
                paths.push(path.to_owned());
            }
        }
        let mut info: Vec<u8> = Vec::new();
        let push = |info: &mut Vec<u8>, mode: &str, blob: &str, path: &str| {
            info.extend_from_slice(format!("{mode} {blob}\t{path}").as_bytes());
            info.push(0);
        };
        // Paths outside the sparse checkout are as the index has them.
        for p in &skip {
            if let Some((mode, sha)) = entries.get(p) {
                push(&mut info, mode, sha, p);
            }
        }
        let mut to_hash: Vec<(String, String, u64, Option<SystemTime>)> = Vec::new();
        let mut links: Vec<(String, Vec<u8>)> = Vec::new();
        let key = dir.to_path_buf();
        let stamps = self
            .stamps
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&key)
            .cloned()
            .unwrap_or_default();
        let mut fresh: HashMap<String, Stamp> = HashMap::new();
        for path in paths {
            if path.ends_with('/') || path.contains('\n') {
                continue; // a nested repository, or a name hash-object cannot take
            }
            let indexed = entries.get(&path);
            match std::fs::symlink_metadata(dir.join(&path)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {} // deleted
                Err(e) => return Err(e.into()),
                Ok(meta) if meta.file_type().is_symlink() => {
                    let target = std::fs::read_link(dir.join(&path))?;
                    links.push((path, slash(&target).into_bytes()));
                }
                Ok(meta) if meta.is_dir() => {
                    // a submodule keeps its index entry
                    if let Some((mode, sha)) = indexed.filter(|(m, _)| m == "160000") {
                        push(&mut info, mode, sha, &path);
                    }
                }
                Ok(meta) => {
                    let mode = file_mode(&meta, indexed.map(|(m, _)| m.as_str()));
                    let mtime = meta.modified().ok();
                    match stamps.get(&path) {
                        Some(s) if s.len == meta.len() && s.mtime == mtime && s.mode == mode => {
                            push(&mut info, &mode, &s.blob, &path);
                            fresh.insert(path, s.clone());
                        }
                        _ => to_hash.push((path, mode, meta.len(), mtime)),
                    }
                }
            }
        }
        if !to_hash.is_empty() {
            let mut list = Vec::new();
            for (p, ..) in &to_hash {
                list.extend_from_slice(p.as_bytes());
                list.push(b'\n');
            }
            let out = self
                .git(Some(dir))?
                .args(["hash-object", "-w", "--no-filters", "--stdin-paths"])
                .stdin(list)
                .run()?;
            let blobs: Vec<String> = String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::to_owned)
                .collect();
            if blobs.len() != to_hash.len() {
                return Err(VcsError::Tool {
                    tool: "git".into(),
                    message: "hash-object returned a different number of ids".into(),
                });
            }
            for ((path, mode, len, mtime), blob) in to_hash.into_iter().zip(blobs) {
                push(&mut info, &mode, &blob, &path);
                fresh.insert(
                    path,
                    Stamp {
                        len,
                        mtime,
                        mode,
                        blob,
                    },
                );
            }
        }
        for (path, target) in links {
            let blob = self
                .git(Some(dir))?
                .args(["hash-object", "-w", "--no-filters", "--stdin"])
                .stdin(target)
                .text()?;
            push(&mut info, "120000", &blob, &path);
        }
        self.stamps
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key, fresh);
        if info.is_empty() {
            // an empty tree
            let tree = self.git(Some(dir))?.index(index).arg("write-tree").text()?;
            return Ok((tree, skip));
        }
        self.git(Some(dir))?
            .index(index)
            .args(["update-index", "-z", "--add", "--index-info"])
            .stdin(info)
            .run()?;
        let tree = self.git(Some(dir))?.index(index).arg("write-tree").text()?;
        Ok((tree, skip))
    }

    /// The change set between two tree-ish ids of the repository at `dir`.
    fn diff(&self, dir: &Path, base: &str, new: &str, head: Option<String>) -> Result<ChangeSet> {
        let common = ["-r", "-z", "-M", "--no-ext-diff", "--no-textconv"];
        let raw = self
            .git(Some(dir))?
            .arg("diff-tree")
            .args(common)
            .args(["--raw", "--no-abbrev", base, new])
            .run()?;
        let entries = parse_raw(&raw.stdout);
        let numstat = self
            .git(Some(dir))?
            .arg("diff-tree")
            .args(common)
            .args(["--numstat", base, new])
            .run()?;
        let counts = parse_numstat(&numstat.stdout);
        let patch = self
            .git(Some(dir))?
            .args([
                "diff-tree",
                "-r",
                "-M",
                "-p",
                "-U0",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                base,
                new,
            ])
            .run()?;
        let hunks = parse_hunks(&String::from_utf8_lossy(&patch.stdout));
        let mut files: Vec<FileChange> = entries
            .into_iter()
            .map(|e| {
                let kind = match e.status {
                    'A' => ChangeKind::Added,
                    'D' => ChangeKind::Deleted,
                    'R' => ChangeKind::Renamed,
                    'T' => ChangeKind::TypeChanged,
                    'M' if e.old_sha == e.new_sha => ChangeKind::ModeChanged,
                    _ => ChangeKind::Modified,
                };
                let deleted = kind == ChangeKind::Deleted;
                let added = kind == ChangeKind::Added;
                let (add, rem, binary) = counts.get(&e.path).copied().unwrap_or((0, 0, false));
                FileChange {
                    hunks: if deleted {
                        Vec::new()
                    } else {
                        hunks.get(&e.path).cloned().unwrap_or_default()
                    },
                    path: e.path,
                    from: e.from,
                    old_hash: (!added).then_some(e.old_sha),
                    new_hash: (!deleted).then_some(e.new_sha),
                    mode: (!deleted).then_some(e.new_mode),
                    binary,
                    added: add,
                    removed: rem,
                    kind,
                }
            })
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let base_commit = base.to_owned();
        let digest = changeset_digest(&base_commit, &files);
        Ok(ChangeSet {
            base: base_commit,
            head,
            files,
            digest,
        })
    }
}

/// A scratch bare repository of this backend's own that writes into the session repository's object store:
/// no session configuration (merge drivers, `i18n.commitEncoding`) and no session attributes apply to what
/// is computed in it, and what it writes is visible from the session. Removed when dropped.
struct Scratch {
    path: PathBuf,
    objects: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = remove_tree(&self.path);
    }
}

/// Splits `Name <email>` (or a bare name) into git's name and email.
fn identity(author: &str) -> (String, String) {
    let (name, email) = match author.split_once('<') {
        Some((n, e)) => (
            n.trim().to_owned(),
            e.trim_end_matches('>').trim().to_owned(),
        ),
        None => (
            author.trim().to_owned(),
            format!("{}@han.invalid", author.trim()),
        ),
    };
    let name = if name.is_empty() {
        "han".to_owned()
    } else {
        name
    };
    (name, email)
}

/// A tree mode as git prints it in `ls-tree`.
const TREE_MODE: &str = "040000";

/// How a delta entry's type is compared when pairing renames: regular files with each other, symbolic
/// links with each other, nothing else.
fn rename_class(mode: &str) -> Option<u8> {
    match mode {
        "100644" | "100755" => Some(0),
        "120000" => Some(1),
        _ => None,
    }
}

/// The deleted and the added entries (indexes) that share one content id and type.
type RenameCandidates = (Vec<usize>, Vec<usize>);

/// The ancestor directories of a path, outermost first.
fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/').map(move |(i, _)| &path[..i])
}

impl GitVcs {
    fn scratch(&self, dir: &Path) -> Result<Scratch> {
        let objects = self
            .git(Some(dir))?
            .args([
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "objects",
            ])
            .text()?;
        let path = self.tmp_dir().join(unique("scratch") + ".git");
        let s = Scratch {
            path,
            objects: PathBuf::from(objects),
        };
        self.git(None)?
            .args(["init", "--bare", "--quiet"])
            .arg(format!("--template={}", slash(&self.template_dir())))
            .arg(&s.path)
            .run()?;
        Ok(s)
    }

    /// A git command in the scratch repository, writing objects to the session's store.
    fn scratch_git(&self, s: &Scratch) -> Result<Git> {
        Ok(self
            .git(Some(&s.path))?
            .env("GIT_OBJECT_DIRECTORY", &s.objects)
            .env("LC_ALL", "C"))
    }

    /// Mode and object id of `paths` in `commit` (`ls-tree`, so a directory shows as mode `040000` with its
    /// tree id); a path that does not exist is missing from the map.
    fn tree_entries(
        &self,
        dir: &Path,
        commit: &str,
        paths: &[String],
    ) -> Result<HashMap<String, (String, String)>> {
        let mut found = HashMap::new();
        for chunk in paths.chunks(100) {
            let out = self
                .git(Some(dir))?
                .env("GIT_LITERAL_PATHSPECS", "1")
                .args(["ls-tree", "-z", "--full-tree", commit, "--"])
                .args(chunk)
                .run()?;
            for rec in out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
                let rec = String::from_utf8_lossy(rec);
                if let Some((meta, path)) = rec.split_once('\t') {
                    let mut f = meta.split(' ');
                    if let (Some(mode), Some(_), Some(id)) = (f.next(), f.next(), f.next()) {
                        found.insert(path.to_owned(), (mode.to_owned(), id.to_owned()));
                    }
                }
            }
        }
        Ok(found)
    }

    fn delta_between(&self, dir: &Path, base: &str, head: &str) -> Result<Delta> {
        let base = self.commit_of(dir, base)?;
        let head = self.commit_of(dir, head)?;
        let raw = self
            .git(Some(dir))?
            .arg("diff-tree")
            .args(["-r", "-z", "--no-renames", "--no-ext-diff", "--no-textconv"])
            .args(["--raw", "--no-abbrev", &base, &head])
            .run()?;
        let mut entries: Vec<DeltaEntry> = parse_raw(&raw.stdout)
            .into_iter()
            .map(|e| {
                let (kind, from, to) = match e.status {
                    'A' => (DeltaKind::Add, None, Some(e.new_sha)),
                    'D' => (DeltaKind::Delete, Some(e.old_sha), None),
                    _ => (DeltaKind::Modify, Some(e.old_sha), Some(e.new_sha)),
                };
                let added = kind == DeltaKind::Add;
                let deleted = kind == DeltaKind::Delete;
                DeltaEntry {
                    path: e.path,
                    kind,
                    from,
                    from_mode: (!added).then_some(e.old_mode),
                    to,
                    mode: (!deleted).then_some(e.new_mode),
                }
            })
            .collect();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        // Exact renames: a deleted and an added path with the same content id and type, paired in path order.
        let mut groups: BTreeMap<(u8, &str), RenameCandidates> = BTreeMap::new();
        for (i, e) in entries.iter().enumerate() {
            match (&e.kind, &e.from, &e.to) {
                (DeltaKind::Delete, Some(id), _) => {
                    if let Some(c) = e.from_mode.as_deref().and_then(rename_class) {
                        groups.entry((c, id)).or_default().0.push(i);
                    }
                }
                (DeltaKind::Add, _, Some(id)) => {
                    if let Some(c) = e.mode.as_deref().and_then(rename_class) {
                        groups.entry((c, id)).or_default().1.push(i);
                    }
                }
                _ => {}
            }
        }
        let mut renames: Vec<(usize, usize)> = Vec::new();
        for (dels, adds) in groups.values() {
            renames.extend(dels.iter().copied().zip(adds.iter().copied()));
        }
        let gone: HashSet<usize> = renames.iter().flat_map(|(d, a)| [*d, *a]).collect();
        let mut out: Vec<DeltaEntry> = Vec::with_capacity(entries.len());
        for (d, a) in &renames {
            out.push(DeltaEntry {
                path: entries[*a].path.clone(),
                kind: DeltaKind::Rename {
                    from_path: entries[*d].path.clone(),
                },
                from: entries[*d].from.clone(),
                from_mode: entries[*d].from_mode.clone(),
                to: entries[*a].to.clone(),
                mode: entries[*a].mode.clone(),
            });
        }
        out.extend(
            entries
                .into_iter()
                .enumerate()
                .filter(|(i, _)| !gone.contains(i))
                .map(|(_, e)| e),
        );
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Delta { entries: out })
    }

    fn path_disjoint_of(&self, dir: &Path, delta: &Delta, new_base: &str) -> Result<PathDisjoint> {
        let new_base = self.commit_of(dir, new_base)?;
        // What each path must be in the new base: `None` absent, `Some((mode, id))` as at the old base.
        let mut expect: BTreeMap<&str, Option<(&str, &str)>> = BTreeMap::new();
        for e in &delta.entries {
            let was = match (&e.from_mode, &e.from) {
                (Some(m), Some(id)) => Some((m.as_str(), id.as_str())),
                _ => None,
            };
            match &e.kind {
                DeltaKind::Add => {
                    expect.insert(&e.path, None);
                }
                DeltaKind::Modify | DeltaKind::Delete => {
                    expect.insert(&e.path, was);
                }
                DeltaKind::Rename { from_path } => {
                    expect.insert(&e.path, None);
                    expect.insert(from_path, was);
                }
            }
        }
        // An ancestor must be a directory (or absent), unless the delta itself replaces it.
        let mut dirs: BTreeSet<&str> = BTreeSet::new();
        for p in expect.keys() {
            dirs.extend(ancestors(p).filter(|a| !matches!(expect.get(a), Some(Some(_)))));
        }
        let query: Vec<String> = expect
            .keys()
            .copied()
            .chain(dirs.iter().copied())
            .map(str::to_owned)
            .collect();
        let now = self.tree_entries(dir, &new_base, &query)?;
        let mut touched: BTreeSet<String> = BTreeSet::new();
        for (path, want) in &expect {
            let got = now.get(*path).map(|(m, id)| (m.as_str(), id.as_str()));
            if got != *want {
                touched.insert((*path).to_owned());
            }
        }
        for d in &dirs {
            if now.get(*d).is_some_and(|(m, _)| m != TREE_MODE) {
                touched.insert((*d).to_owned());
            }
        }
        Ok(PathDisjoint {
            ok: touched.is_empty(),
            touched: touched.into_iter().collect(),
        })
    }

    /// `onto`'s tree with exactly the delta's entries written, built in a scratch index.
    fn apply_delta(&self, dir: &Path, s: &Scratch, onto: &str, delta: &Delta) -> Result<String> {
        let index = s.path.join("han-index");
        let zero = self.zero_id(dir)?;
        let mut info: Vec<u8> = Vec::new();
        let mut put = |mode: &str, id: &str, path: &str| {
            info.extend_from_slice(format!("{mode} {id}\t{path}").as_bytes());
            info.push(0);
        };
        // Removals first, so that a file can become a directory and the other way round.
        for e in &delta.entries {
            match &e.kind {
                DeltaKind::Delete => put("0", &zero, &e.path),
                DeltaKind::Rename { from_path } => put("0", &zero, from_path),
                _ => {}
            }
        }
        for e in &delta.entries {
            if let (Some(mode), Some(id)) = (&e.mode, &e.to) {
                put(mode, id, &e.path);
            }
        }
        self.scratch_git(s)?
            .index(&index)
            .args(["read-tree", &format!("{onto}^{{tree}}")])
            .run()?;
        self.scratch_git(s)?
            .index(&index)
            .args(["update-index", "-z", "--add", "--index-info"])
            .stdin(info)
            .run()?;
        self.scratch_git(s)?.index(&index).arg("write-tree").text()
    }

    fn commit_with(
        &self,
        s: &Scratch,
        tree: &str,
        parent: &str,
        message: &str,
        author: &str,
        time: i64,
    ) -> Result<String> {
        let (name, email) = identity(author);
        let date = format!("{time} +0000");
        self.scratch_git(s)?
            .env("GIT_AUTHOR_NAME", &name)
            .env("GIT_AUTHOR_EMAIL", &email)
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_NAME", &name)
            .env("GIT_COMMITTER_EMAIL", &email)
            .env("GIT_COMMITTER_DATE", &date)
            .args(["commit-tree", tree, "-p", parent, "-F", "-"])
            .stdin(message.as_bytes().to_vec())
            .text()
    }

    /// Runs `merge-tree --write-tree`. `--merge-base` needs git 2.40; before that, the change is given a
    /// synthetic parent, which is the same merge as long as `merge_base` is an ancestor of `ours`.
    fn merge_tree(
        &self,
        dir: &Path,
        s: &Scratch,
        merge_base: Option<&str>,
        ours: &str,
        theirs: &str,
    ) -> Result<Output> {
        let run = |theirs: &str, base_opt: Option<&str>| -> Result<Output> {
            let mut g = self
                .scratch_git(s)?
                .args(["merge-tree", "--write-tree", "-z"]);
            if let Some(b) = base_opt {
                g = g.arg(format!("--merge-base={b}"));
            }
            g.args([ours, theirs]).run_codes(&[1])
        };
        let Some(b) = merge_base else {
            return run(theirs, None);
        };
        match run(theirs, Some(b)) {
            Err(VcsError::Tool { message, .. }) if message.contains("unknown option") => {}
            other => return other,
        }
        let synthetic = self
            .scratch_git(s)?
            .env("GIT_AUTHOR_NAME", "han")
            .env("GIT_AUTHOR_EMAIL", "han@han.invalid")
            .env("GIT_AUTHOR_DATE", "1000000000 +0000")
            .env("GIT_COMMITTER_NAME", "han")
            .env("GIT_COMMITTER_EMAIL", "han@han.invalid")
            .env("GIT_COMMITTER_DATE", "1000000000 +0000")
            .args(["commit-tree", &format!("{theirs}^{{tree}}"), "-p", b])
            .args(["-m", "han: merge base"])
            .text()?;
        let found = self
            .git(Some(dir))?
            .args(["merge-base", ours, &synthetic])
            .run_codes(&[1])
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
        if found.as_deref() != Some(b) {
            return Err(VcsError::Invalid(
                "this git cannot merge with a base that is not an ancestor of the target (needs 2.40)"
                    .into(),
            ));
        }
        run(&synthetic, None)
    }

    /// Merges `theirs` into `ours` (with `merge_base`, or the one git finds) in the scratch repository and
    /// returns the merged tree and the record. The tree of a conflicted merge is not usable.
    fn three_way(
        &self,
        dir: &Path,
        s: &Scratch,
        merge_base: Option<&str>,
        ours: &str,
        theirs: &str,
    ) -> Result<(String, ConflictRecord)> {
        let out = self.merge_tree(dir, s, merge_base, ours, theirs)?;
        let parsed = parse_merge_tree(&out.stdout).ok_or_else(|| VcsError::Tool {
            tool: "git".into(),
            message: "merge-tree printed something unexpected".into(),
        })?;
        let base = match merge_base {
            Some(b) => Some(b.to_owned()),
            None => self
                .git(Some(dir))?
                .args(["merge-base", ours, theirs])
                .run_codes(&[1])
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .filter(|b| is_hex_id(b)),
        };
        let conflicts = if out.status.success() {
            Vec::new()
        } else {
            self.conflicts_of(dir, s, base.as_deref(), ours, theirs, &parsed)?
        };
        let mut clean: Vec<String> = Vec::new();
        if let Some(b) = &base {
            let changed = self
                .git(Some(dir))?
                .args(["diff-tree", "-r", "-z", "--no-renames", "--name-only"])
                .args([b.as_str(), theirs])
                .run()?;
            clean = changed
                .stdout
                .split(|b| *b == 0)
                .filter(|p| !p.is_empty())
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .filter(|p| {
                    !conflicts
                        .iter()
                        .any(|c| p == &c.path || p.starts_with(&format!("{}/", c.path)))
                })
                .collect();
            clean.sort();
            clean.dedup();
        }
        Ok((
            parsed.tree,
            ConflictRecord {
                onto: ours.to_owned(),
                clean,
                conflicts,
            },
        ))
    }

    fn conflicts_of(
        &self,
        dir: &Path,
        s: &Scratch,
        base: Option<&str>,
        ours: &str,
        theirs: &str,
        parsed: &merge::MergeTreeOut,
    ) -> Result<Vec<Conflict>> {
        let side = |e: &(String, String)| ConflictSide {
            mode: e.0.clone(),
            id: e.1.clone(),
        };
        let mut out: Vec<Conflict> = Vec::new();
        let mut consumed: HashSet<&str> = HashSet::new();
        // A file where the other side has a directory: git moves the file to `<path>~<side>` and says so.
        for (paths, kind) in &parsed.messages {
            if kind != "CONFLICT (file/directory)" {
                continue;
            }
            let moved = paths.iter().find(|p| parsed.stages.contains_key(*p));
            let logical = paths
                .iter()
                .find(|p| !parsed.stages.contains_key(*p))
                .or(paths.last());
            let (Some(logical), Some(moved)) = (logical, moved) else {
                continue;
            };
            consumed.insert(moved);
            let want = std::slice::from_ref(logical);
            let at = |c: Option<&str>| -> Result<Option<ConflictSide>> {
                Ok(match c {
                    Some(c) => self.tree_entries(dir, c, want)?.get(logical).map(side),
                    None => None,
                })
            };
            out.push(Conflict {
                path: logical.clone(),
                kind: ConflictKind::FileDirectory,
                base: at(base)?,
                ours: at(Some(ours))?,
                theirs: at(Some(theirs))?,
                hunks: Vec::new(),
            });
        }
        let binary: HashSet<&str> = parsed
            .messages
            .iter()
            .filter(|(_, k)| k == "CONFLICT (binary)")
            .flat_map(|(p, _)| p.iter().map(String::as_str))
            .collect();
        for (path, [b, o, t]) in &parsed.stages {
            if consumed.contains(path.as_str()) {
                continue;
            }
            let any_mode = |m: &str| {
                [b, o, t]
                    .iter()
                    .any(|x| x.as_ref().is_some_and(|x| x.mode == m))
            };
            let kind = if any_mode("120000") {
                ConflictKind::Symlink
            } else if b.is_some() && (o.is_some() != t.is_some()) {
                ConflictKind::ModifyDelete
            } else if b.is_none() && o.is_some() && t.is_some() {
                ConflictKind::AddAdd
            } else if any_mode("160000") || binary.contains(path.as_str()) {
                ConflictKind::Binary
            } else {
                ConflictKind::Content
            };
            let hunks = match (kind, b, o, t) {
                (ConflictKind::Content, Some(b), Some(o), Some(t)) => {
                    self.hunks_of(s, path, &b.id, &o.id, &t.id)?
                }
                _ => Vec::new(),
            };
            out.push(Conflict {
                path: path.clone(),
                kind,
                base: b.clone(),
                ours: o.clone(),
                theirs: t.clone(),
                hunks,
            });
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    /// The regions where `ours` and `theirs` (blob ids) disagree about `base`, each with its two candidates
    /// written as blobs. Empty when the files do not allow it.
    fn hunks_of(
        &self,
        s: &Scratch,
        path: &str,
        base: &str,
        ours: &str,
        theirs: &str,
    ) -> Result<Vec<ConflictHunk>> {
        let blob = |id: &str| -> Result<Vec<u8>> {
            Ok(self
                .scratch_git(s)?
                .args(["cat-file", "blob", id])
                .run()?
                .stdout)
        };
        let changes = |new: &str| -> Result<Vec<merge::Change>> {
            let out = self
                .scratch_git(s)?
                .args([
                    "diff",
                    "-U0",
                    "--no-color",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--diff-algorithm=myers",
                ])
                .args([base, new])
                .run()?;
            Ok(parse_changes(&String::from_utf8_lossy(&out.stdout)))
        };
        let (o_bytes, t_bytes) = (blob(ours)?, blob(theirs)?);
        let o_lines: Vec<&[u8]> = o_bytes.split_inclusive(|b| *b == b'\n').collect();
        let t_lines: Vec<&[u8]> = t_bytes.split_inclusive(|b| *b == b'\n').collect();
        let Some(regions) = merge::regions(&changes(ours)?, &changes(theirs)?, &o_lines, &t_lines)
        else {
            return Ok(Vec::new());
        };
        let range = |(start, len): (usize, usize)| LineRange {
            start: u32::try_from(start + 1).unwrap_or(u32::MAX),
            len: u32::try_from(len).unwrap_or(u32::MAX),
        };
        let mut hunks = Vec::new();
        for (n, r) in regions.iter().enumerate() {
            let mut candidates = Vec::new();
            for (name, source, lines, (start, len)) in [
                ("ours", CandidateSource::Ours, &o_lines, r.ours),
                ("theirs", CandidateSource::Theirs, &t_lines, r.theirs),
            ] {
                let text: Vec<u8> = lines[start..start + len].concat();
                let content_id = self
                    .scratch_git(s)?
                    .args(["hash-object", "-w", "--stdin"])
                    .stdin(text)
                    .text()?;
                candidates.push(Candidate {
                    id: format!("{path}#{n}/{name}"),
                    source,
                    content_id,
                });
            }
            hunks.push(ConflictHunk {
                base_range: range(r.base),
                ours_range: range(r.ours),
                theirs_range: range(r.theirs),
                candidates,
            });
        }
        Ok(hunks)
    }
}

/// Removes the lock directory of a shared store when dropped.
struct StoreLock(PathBuf);

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

/// A source as the stores key and fetch it: URLs as given, local paths absolute with forward slashes.
fn normalise_source(source: &str) -> String {
    if source.contains("://") {
        return source.to_owned();
    }
    let p = Path::new(source);
    let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = slash(&abs);
    // `\\?\C:\…` from canonicalize on Windows
    s.strip_prefix("//?/").map(str::to_owned).unwrap_or(s)
}

fn file_mode(meta: &std::fs::Metadata, indexed: Option<&str>) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = indexed;
        if meta.permissions().mode() & 0o111 != 0 {
            "100755".into()
        } else {
            "100644".into()
        }
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        match indexed {
            Some("100755") => "100755".into(),
            _ => "100644".into(),
        }
    }
}

fn parse_raw(out: &[u8]) -> Vec<RawEntry> {
    let parts: Vec<String> = out
        .split(|b| *b == 0)
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect();
    let mut entries = Vec::new();
    let mut i = 0;
    while i < parts.len() {
        let head = &parts[i];
        let Some(meta) = head.strip_prefix(':') else {
            i += 1;
            continue;
        };
        let f: Vec<&str> = meta.split(' ').collect();
        if f.len() < 5 || i + 1 >= parts.len() {
            break;
        }
        let status = f[4].chars().next().unwrap_or('M');
        let (path, from) = if matches!(status, 'R' | 'C') && i + 2 < parts.len() {
            let e = (parts[i + 2].clone(), Some(parts[i + 1].clone()));
            i += 3;
            e
        } else {
            let e = (parts[i + 1].clone(), None);
            i += 2;
            e
        };
        entries.push(RawEntry {
            old_mode: f[0].into(),
            new_mode: f[1].into(),
            old_sha: f[2].into(),
            new_sha: f[3].into(),
            status,
            path,
            from,
        });
    }
    entries
}

fn parse_numstat(out: &[u8]) -> HashMap<String, (u32, u32, bool)> {
    let parts: Vec<String> = out
        .split(|b| *b == 0)
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect();
    let mut map = HashMap::new();
    let mut i = 0;
    while i < parts.len() {
        let rec = &parts[i];
        let f: Vec<&str> = rec.splitn(3, '\t').collect();
        if f.len() < 3 {
            i += 1;
            continue;
        }
        let binary = f[0] == "-";
        let a = f[0].parse().unwrap_or(0);
        let r = f[1].parse().unwrap_or(0);
        let path = if f[2].is_empty() && i + 2 < parts.len() {
            // a rename: the old and new paths follow
            let p = parts[i + 2].clone();
            i += 3;
            p
        } else {
            i += 1;
            f[2].to_owned()
        };
        map.insert(path, (a, r, binary));
    }
    map
}

/// New-side hunks `[start, len]` per path from a `-U0` patch.
fn parse_hunks(patch: &str) -> HashMap<String, Vec<(u32, u32)>> {
    let mut map: HashMap<String, Vec<(u32, u32)>> = HashMap::new();
    let mut cur: Option<String> = None;
    for line in patch.lines() {
        if line.starts_with("diff --git ") {
            cur = None;
        } else if let Some(p) = line.strip_prefix("+++ ") {
            let p = unquote(p.trim_end_matches('\t'));
            cur = p.strip_prefix("b/").map(str::to_owned);
        } else if let Some(rest) = line.strip_prefix("@@ ") {
            let Some(path) = &cur else { continue };
            let Some(new) = rest.split(' ').find(|t| t.starts_with('+')) else {
                continue;
            };
            let new = &new[1..];
            let (start, len) = match new.split_once(',') {
                Some((s, l)) => (s.parse().unwrap_or(0), l.parse().unwrap_or(0)),
                None => (new.parse().unwrap_or(0), 1),
            };
            map.entry(path.clone()).or_default().push((start, len));
        }
    }
    map
}

impl Vcs for GitVcs {
    fn name(&self) -> &'static str {
        "git"
    }

    fn create_workspace(&self, spec: &WorkspaceSpec) -> Result<Workspace> {
        if spec.dir.exists() {
            return Err(VcsError::Invalid(format!("{} exists", spec.dir.display())));
        }
        self.git(None)?
            .args(["check-ref-format", "--branch", &spec.branch])
            .run()
            .map_err(|_| VcsError::Invalid(format!("not a branch name: {}", spec.branch)))?;
        if spec
            .sparse
            .iter()
            .any(|d| d.is_empty() || d.contains("..") || d.contains('\n') || d.contains('*'))
        {
            return Err(VcsError::Invalid(
                "sparse patterns are plain directories".into(),
            ));
        }
        let store = self.ensure_store(&spec.source)?;
        let base = self.resolve_base(&store, &spec.source, &spec.base)?;
        if let Some(parent) = spec.dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // A clone that borrows the store's objects (alternates) and has its own refs and index.
        let made = (|| -> Result<()> {
            self.git(None)?
                .args(["-c", "protocol.file.allow=always"])
                .args(["clone", "--quiet", "--no-checkout", "--shared"])
                .arg(format!("--template={}", slash(&self.template_dir())))
                .arg(&store)
                .arg(&spec.dir)
                .run()?;
            let dir = spec.dir.as_path();
            self.git(Some(dir))?
                .args([
                    "remote",
                    "set-url",
                    "origin",
                    &normalise_source(&spec.source),
                ])
                .run()?;
            if !spec.sparse.is_empty() {
                self.git(Some(dir))?
                    .args(["config", "core.sparseCheckout", "true"])
                    .run()?;
                self.git(Some(dir))?
                    .args(["config", "core.sparseCheckoutCone", "true"])
                    .run()?;
                let gd = self.git_dir(dir)?;
                std::fs::create_dir_all(gd.join("info"))?;
                std::fs::write(
                    gd.join("info").join("sparse-checkout"),
                    cone_patterns(&spec.sparse),
                )?;
            }
            self.git(Some(dir))?
                .args(["checkout", "--quiet", "-b", &spec.branch, &base])
                .run()?;
            Ok(())
        })();
        if let Err(e) = made {
            let _ = remove_tree(&spec.dir);
            return Err(e);
        }
        Ok(Workspace {
            dir: spec.dir.clone(),
            base,
            branch: spec.branch.clone(),
            shared_store: Some(store),
        })
    }

    fn changeset(&self, dir: &Path, base: &str) -> Result<ChangeSet> {
        let base = self.commit_of(dir, base)?;
        let (tree, tmp, _) = self.worktree_tree(dir)?;
        let cs = self.diff(dir, &base, &tree, None);
        let _ = std::fs::remove_file(&tmp);
        cs
    }

    fn changeset_between(&self, dir: &Path, base: &str, head: &str) -> Result<ChangeSet> {
        let base = self.commit_of(dir, base)?;
        let head = self.commit_of(dir, head)?;
        self.diff(dir, &base, &head, Some(head.clone()))
    }

    fn commit_all(&self, dir: &Path, message: &str, author: &str) -> Result<String> {
        let (name, email) = identity(author);
        let (tree, tmp, skip) = self.worktree_tree(dir)?;
        let result = (|| -> Result<String> {
            let parent = self.commit_of(dir, "HEAD").ok();
            if let Some(p) = &parent {
                let ptree = self
                    .git(Some(dir))?
                    .args(["rev-parse", &format!("{p}^{{tree}}")])
                    .text()?;
                if ptree == tree {
                    return Ok(p.clone());
                }
            }
            let mut g = self
                .git(Some(dir))?
                .env("GIT_AUTHOR_NAME", &name)
                .env("GIT_AUTHOR_EMAIL", &email)
                .env("GIT_COMMITTER_NAME", &name)
                .env("GIT_COMMITTER_EMAIL", &email)
                .args(["commit-tree", &tree]);
            if let Some(p) = &parent {
                g = g.args(["-p", p]);
            }
            let commit = g
                .args(["-F", "-"])
                .stdin(message.as_bytes().to_vec())
                .text()?;
            let head_ref = self
                .git(Some(dir))?
                .args(["symbolic-ref", "-q", "HEAD"])
                .text()
                .unwrap_or_else(|_| "HEAD".into());
            let old = parent.unwrap_or_else(|| self.zero_id(dir).unwrap_or_default());
            self.git(Some(dir))?
                .args(["update-ref", "-m", "han: commit", &head_ref, &commit, &old])
                .run()?;
            // The session's index now matches the commit (sparse flags kept: the temporary index started
            // as a copy of it).
            let gd = self.git_dir(dir)?;
            std::fs::rename(&tmp, gd.join("index"))?;
            if !skip.is_empty() {
                let mut list = Vec::new();
                for p in &skip {
                    list.extend_from_slice(p.as_bytes());
                    list.push(0);
                }
                self.git(Some(dir))?
                    .args(["update-index", "-z", "--skip-worktree", "--stdin"])
                    .stdin(list)
                    .run()?;
            }
            Ok(commit)
        })();
        let _ = std::fs::remove_file(&tmp);
        result
    }

    fn remote_ref(
        &self,
        dir: &Path,
        remote: &str,
        refname: &str,
        auth: Option<&RemoteAuth>,
    ) -> Result<Option<String>> {
        let out = self
            .git_auth(Some(dir), remote, auth)?
            .args(["ls-remote", "--end-of-options", remote, refname])
            .text()?;
        Ok(out.lines().find_map(|l| {
            let (sha, r) = l.split_once('\t')?;
            (r == refname && is_hex_id(sha)).then(|| sha.to_owned())
        }))
    }

    fn land(&self, dir: &Path, spec: &LandSpec, auth: Option<&RemoteAuth>) -> Result<LandOutcome> {
        if !spec.refname.starts_with("refs/") || spec.refname.contains(':') {
            return Err(VcsError::Invalid(format!(
                "not a full ref name: {}",
                spec.refname
            )));
        }
        if !is_hex_id(&spec.new) || spec.expected.as_deref().is_some_and(|e| !is_hex_id(e)) {
            return Err(VcsError::Invalid("land takes full commit ids".into()));
        }
        let lease = format!(
            "--force-with-lease={}:{}",
            spec.refname,
            spec.expected.as_deref().unwrap_or("")
        );
        let out = self
            .git_auth(Some(dir), &spec.remote, auth)?
            .args([
                "push",
                "--porcelain",
                "--no-verify",
                &lease,
                "--end-of-options",
            ])
            .arg(&spec.remote)
            .arg(format!("{}:{}", spec.new, spec.refname))
            .run_codes(&[1]);
        let flag = match &out {
            Ok(o) if o.status.success() => Some(
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .find(|l| l.contains(&spec.refname) && l.contains('\t'))
                    .and_then(|l| l.chars().next()),
            ),
            _ => None,
        };
        match flag {
            Some(Some('=')) => return Ok(LandOutcome::AlreadyThere),
            Some(_) => return Ok(LandOutcome::Landed),
            None => {}
        }
        // Rejected or failed: look at where the ref is now. Nothing was forced.
        let why = match &out {
            Ok(o) => format!(
                "push refused: {} {}",
                String::from_utf8_lossy(&o.stdout).trim(),
                String::from_utf8_lossy(&o.stderr).trim()
            ),
            Err(e) => e.to_string(),
        };
        let actual = self.remote_ref(dir, &spec.remote, &spec.refname, auth)?;
        if actual.as_deref() == Some(spec.new.as_str()) {
            Ok(LandOutcome::AlreadyThere)
        } else if actual == spec.expected {
            // The ref is where we expected and still did not move: a real failure, not a race.
            let secrets = self.git_auth(None, &spec.remote, auth)?.secrets;
            Err(VcsError::Tool {
                tool: "git".into(),
                message: scrub(&why, &secrets),
            })
        } else {
            Ok(LandOutcome::Conflict { actual })
        }
    }

    fn bundle(&self, dir: &Path, base: &str, head: &str) -> Result<Vec<u8>> {
        let base = self.commit_of(dir, base)?;
        let head = self.commit_of(dir, head)?;
        if base == head {
            return Err(VcsError::Invalid("nothing to bundle: base is head".into()));
        }
        let file = self.tmp_dir().join(unique("bundle") + ".bundle");
        // The bundle's one ref exists in the repository only while the bundle is written.
        self.git(Some(dir))?
            .args(["update-ref", "--no-deref", BUNDLE_REF, &head])
            .run()?;
        let made = self
            .git(Some(dir))?
            .args(["bundle", "create", "--quiet"])
            .arg(&file)
            .args([BUNDLE_REF, &format!("^{base}")])
            .run();
        let _ = self
            .git(Some(dir))
            .and_then(|g| g.args(["update-ref", "-d", BUNDLE_REF]).run());
        let bytes = made.and_then(|_| Ok(std::fs::read(&file)?));
        let _ = std::fs::remove_file(&file);
        let bytes = bytes?;
        check_bundle(&bytes, &base, &head)?;
        Ok(bytes)
    }

    fn import_bundle(
        &self,
        remote: &str,
        auth: Option<&RemoteAuth>,
        bundle: &[u8],
        base: &str,
        head: &str,
    ) -> Result<PathBuf> {
        if !is_hex_id(base) || !is_hex_id(head) {
            return Err(VcsError::Invalid(
                "a bundle is imported by full commit ids".into(),
            ));
        }
        // What git is handed is checked first: one ref, `head`, needing nothing but `base`.
        check_bundle(bundle, base, head)?;
        self.ensure_root()?;
        let repo = self.landing_for(remote);
        let _local = self.fetch_lock.lock().unwrap_or_else(|p| p.into_inner());
        let _lock = self.store_lock(&repo)?;
        if !repo.join("HEAD").exists() {
            remove_tree(&repo)?;
            self.git(None)?
                .args(["init", "--bare", "--quiet"])
                .arg(format!("--template={}", slash(&self.template_dir())))
                .arg(&repo)
                .run()?;
            for (k, v) in [
                ("gc.auto", "0"),
                ("gc.pruneExpire", "never"),
                ("core.logAllRefUpdates", "false"),
                ("fetch.fsckObjects", "true"),
            ] {
                self.git(Some(&repo))?.args(["config", k, v]).run()?;
            }
        }
        let file = self.tmp_dir().join(unique("import") + ".bundle");
        std::fs::write(&file, bundle)?;
        let result = (|| -> Result<PathBuf> {
            if self.commit_of(&repo, base).is_err() {
                // The bundle needs `base`, which the remote the push goes to holds: by id first, then its
                // branches.
                let by_id = self
                    .git_auth(Some(&repo), remote, auth)?
                    .args([
                        "fetch",
                        "--quiet",
                        "--no-tags",
                        "--no-write-fetch-head",
                        "--end-of-options",
                        remote,
                        &format!("{base}:refs/han/base/{base}"),
                    ])
                    .run();
                if by_id.is_err() || self.commit_of(&repo, base).is_err() {
                    self.git_auth(Some(&repo), remote, auth)?
                        .args([
                            "fetch",
                            "--quiet",
                            "--no-tags",
                            "--no-write-fetch-head",
                            "--end-of-options",
                            remote,
                            "+refs/heads/*:refs/han/remote/*",
                        ])
                        .run()?;
                }
                if self.commit_of(&repo, base).is_err() {
                    return Err(VcsError::NotFound(format!(
                        "the remote does not hold the base {base}"
                    )));
                }
            }
            let imported = format!("refs/han/import/{head}");
            // A bundle is read through the file transport, whatever protocols the remotes may use.
            self.git(Some(&repo))?
                .args([
                    "-c",
                    "protocol.file.allow=always",
                    "fetch",
                    "--quiet",
                    "--no-tags",
                    "--no-write-fetch-head",
                ])
                .arg(&file)
                .arg(format!("+{BUNDLE_REF}:{imported}"))
                .run()?;
            let got = self.commit_of(&repo, &imported)?;
            if got != head {
                let _ = self
                    .git(Some(&repo))
                    .and_then(|g| g.args(["update-ref", "-d", &imported]).run());
                return Err(VcsError::Invalid(format!(
                    "the bundle's head is {got}, not {head}"
                )));
            }
            Ok(repo.clone())
        })();
        let _ = std::fs::remove_file(&file);
        result
    }

    fn merge_simulate(&self, dir: &Path, ours: &str, theirs: &str) -> Result<MergeSim> {
        Ok(MergeSim::from(&self.merge_record(dir, ours, theirs)?))
    }

    fn merge_record(&self, dir: &Path, ours: &str, theirs: &str) -> Result<ConflictRecord> {
        let ours = self.commit_of(dir, ours)?;
        let theirs = self.commit_of(dir, theirs)?;
        let s = self.scratch(dir)?;
        Ok(self.three_way(dir, &s, None, &ours, &theirs)?.1)
    }

    fn delta(&self, dir: &Path, base: &str, head: &str) -> Result<Delta> {
        self.delta_between(dir, base, head)
    }

    fn path_disjoint(&self, dir: &Path, delta: &Delta, new_base: &str) -> Result<PathDisjoint> {
        self.path_disjoint_of(dir, delta, new_base)
    }

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
        if committer_time < 0 {
            return Err(VcsError::Invalid("committer time before the epoch".into()));
        }
        let base = self.commit_of(dir, base)?;
        let head = self.commit_of(dir, head)?;
        let onto = self.commit_of(dir, onto)?;
        let delta = self.delta_between(dir, &base, &head)?;
        let s = self.scratch(dir)?;
        if self.path_disjoint_of(dir, &delta, &onto)?.ok {
            let tree = self.apply_delta(dir, &s, &onto, &delta)?;
            let commit = self.commit_with(&s, &tree, &onto, message, author, committer_time)?;
            let got = self.delta_between(dir, &onto, &commit)?;
            if got == delta {
                return Ok(RebaseOutcome::Rebased(Rebased {
                    commit,
                    delta: got,
                    path_disjoint: true,
                }));
            }
            // Not the approved result after all (a similarity quirk): treat it as a merge.
        }
        let (tree, record) = self.three_way(dir, &s, Some(&base), &onto, &head)?;
        if !record.conflicts.is_empty() {
            return Ok(RebaseOutcome::Conflict(record));
        }
        let commit = self.commit_with(&s, &tree, &onto, message, author, committer_time)?;
        let delta = self.delta_between(dir, &onto, &commit)?;
        Ok(RebaseOutcome::Rebased(Rebased {
            commit,
            delta,
            path_disjoint: false,
        }))
    }

    fn remove_workspace(&self, ws: &Workspace) -> Result<()> {
        let dir = std::fs::canonicalize(&ws.dir).unwrap_or_else(|_| ws.dir.clone());
        let root =
            std::fs::canonicalize(&self.store_root).unwrap_or_else(|_| self.store_root.clone());
        if dir.starts_with(&root) || root.starts_with(&dir) {
            return Err(VcsError::Invalid(
                "a workspace is never inside, or around, the shared stores".into(),
            ));
        }
        self.stamps
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&ws.dir);
        remove_tree(&ws.dir)?;
        Ok(())
    }
}

/// The cone-mode sparse-checkout file for a set of directories.
fn cone_patterns(dirs: &[String]) -> String {
    let mut out = String::from("/*\n!/*/\n");
    let mut parents: Vec<String> = Vec::new();
    let mut leaves: Vec<String> = Vec::new();
    for d in dirs {
        let d = d.trim_matches('/');
        let segs: Vec<&str> = d.split('/').filter(|s| !s.is_empty()).collect();
        for i in 1..segs.len() {
            let p = segs[..i].join("/");
            if !parents.contains(&p) {
                parents.push(p);
            }
        }
        leaves.push(segs.join("/"));
    }
    parents.sort();
    for p in &parents {
        out.push_str(&format!("/{p}/\n!/{p}/*/\n"));
    }
    leaves.sort();
    leaves.dedup();
    for l in leaves {
        out.push_str(&format!("/{l}/\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_removes_secrets_and_userinfo() {
        let s = scrub(
            "fatal: https://x-access-token:TOPSECRET@github.com/a/b.git said TOPSECRET",
            &["TOPSECRET".into()],
        );
        assert!(!s.contains("TOPSECRET"), "{s}");
        assert!(s.contains("https://<redacted>@github.com/a/b.git"), "{s}");
    }

    #[test]
    fn hunks_and_quotes_parse() {
        let patch = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1,2 @@\n-a\n+b\n+c\n@@ -5,0 +7 @@\n+d\n\
                     diff --git a/y b/y\ndeleted file mode 100644\n--- a/y\n+++ /dev/null\n@@ -1 +0,0 @@\n-y\n\
                     diff --git \"a/t\\tab\" \"b/t\\tab\"\n--- \"a/t\\tab\"\n+++ \"b/t\\tab\"\n@@ -0,0 +1 @@\n+z\n";
        let h = parse_hunks(patch);
        assert_eq!(h["x"], vec![(1, 2), (7, 1)]);
        assert!(!h.contains_key("y"));
        assert_eq!(h["t\tab"], vec![(1, 1)]);
    }

    #[test]
    fn cone_file_lists_parents() {
        assert_eq!(
            cone_patterns(&["a/b".into(), "c".into()]),
            "/*\n!/*/\n/a/\n!/a/*/\n/a/b/\n/c/\n"
        );
    }
}
