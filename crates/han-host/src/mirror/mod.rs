//! Worktree mirroring (PLAN §6.5 Mirroring; `han.timeline/1` §12, TL-81…TL-88).
//!
//! A [`Mirror`] records every change of a session's worktree, whatever made it, as signed batches of a
//! worktree history:
//!
//! * on open it creates the history (genesis: main, the tree map, the root's `tree`), records main at the
//!   base commit (the base tree's files, then `git.commit`), and forks the session timeline `session` from
//!   that version (TL-88);
//! * a `notify` watcher collects changed paths; after a quiet period they become one or more batches on the
//!   session timeline (TL-85): creates, minimal line-then-character edits, deletes, renames (a delete and a
//!   create of identical content in one quiet period), mode and line-ending changes;
//! * each batch carries the current step (the ACP tool call id the host sets with [`Mirror::set_step`]);
//! * every envelope goes to the host's sink as a [`MirrorBatch`], which the host publishes as a
//!   `han.timeline/1` stream item. Batches are kept small enough for a 64 KiB stream item: large texts are
//!   created empty and filled by several edit batches.
//!
//! Never mirrored: anything under `.git`, paths on the exclusion list, and untracked paths git ignores. Text
//! content is masked before it is recorded (SR-52). Binary files are recorded as a BlobRef only; their bytes
//! are not uploaded yet.

pub mod diff;
pub mod glob;
pub mod store;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use han_timeline::{
    At, Envelope, History, HistoryId, HistoryKind, KeySigner, ObjectId, ObjectState, Op, OpId,
    ReplicaId, Signer, Value,
};
use sha2::{Digest, Sha256};

use crate::mask::Masker;
use store::FileStore;

/// The text bytes one batch carries at most (well under the 64 KiB stream item after compression and
/// base64).
pub const CHUNK: usize = 16 * 1024;
/// Ops one batch carries at most.
const MAX_OPS: usize = 256;
/// Larger files are binary (TL-84).
const MAX_TEXT: usize = 1024 * 1024;
/// The session timeline's name.
pub const SESSION_TIMELINE: &str = "session";

/// Whether an untracked path is ignored by git: `(path, is_dir)`.
pub type IgnoreFn = Arc<dyn Fn(&str, bool) -> bool + Send + Sync>;

/// Where each batch goes (the host publishes it as a `han.timeline/1` stream item).
pub type BatchSink = Box<dyn Fn(MirrorBatch) + Send + Sync>;

/// One file of the base tree, as the version-control backend reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaseFile {
    /// The path, relative, `/`-separated.
    pub path: String,
    /// git's mode: `100644`, `100755` or `120000` (a symlink, whose bytes are the target).
    pub mode: String,
    /// The bytes.
    pub bytes: Vec<u8>,
}

/// How a mirror is set up.
pub struct MirrorConfig {
    /// The session's worktree.
    pub worktree: PathBuf,
    /// Where the history is stored (a directory of its own).
    pub store_dir: PathBuf,
    /// The space.
    pub space: String,
    /// The session (`session.open` entry id).
    pub session: String,
    /// The agent's device key, which signs the batches.
    pub signer: KeySigner,
    /// The base commit (full hex).
    pub base_commit: String,
    /// The base commit's files (ignored when the store already holds the history).
    pub base_files: Vec<BaseFile>,
    /// Masks secrets in text content, and says which paths are excluded.
    pub masker: Arc<Masker>,
    /// Whether an untracked path is ignored; `None` reads the worktree's root `.gitignore` crudely.
    pub ignore: Option<IgnoreFn>,
    /// The quiet period that ends a batch of watched changes.
    pub quiet: Duration,
}

/// One batch as the host publishes it: the body of a `han.timeline/1` stream item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MirrorBatch {
    /// The history id (64 hex digits).
    pub history: String,
    /// The envelope, base64url without padding.
    pub envelope_b64: String,
    /// The signing actor.
    pub actor: String,
    /// The signing key id.
    pub key_id: String,
    /// The signer's public key, base64url (checked by readers against the session's actor, see han-node).
    pub public_key: String,
    /// `main` or `session` (empty when unknown, for a republished envelope).
    pub timeline: String,
    /// The step (tool call id) the batch is attributed to.
    pub step: Option<String>,
    /// The paths the batch changed.
    pub files: Vec<String>,
}

impl MirrorBatch {
    /// The stream item body.
    pub fn body(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        m.insert("history".into(), self.history.clone().into());
        m.insert("envelope".into(), self.envelope_b64.clone().into());
        m.insert("actor".into(), self.actor.clone().into());
        m.insert("key".into(), self.key_id.clone().into());
        m.insert("public_key".into(), self.public_key.clone().into());
        m.insert("timeline".into(), self.timeline.clone().into());
        if let Some(s) = &self.step {
            m.insert("step".into(), s.clone().into());
        }
        m.insert(
            "files".into(),
            serde_json::Value::Array(self.files.iter().cloned().map(Into::into).collect()),
        );
        serde_json::Value::Object(m)
    }

    /// Reads a stream item body back.
    pub fn from_body(v: &serde_json::Value) -> Option<MirrorBatch> {
        let s = |k: &str| {
            v.get(k)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        Some(MirrorBatch {
            history: s("history")?,
            envelope_b64: s("envelope")?,
            actor: s("actor")?,
            key_id: s("key")?,
            public_key: s("public_key")?,
            timeline: s("timeline").unwrap_or_default(),
            step: s("step"),
            files: v
                .get("files")
                .and_then(serde_json::Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    /// The decoded envelope bytes.
    pub fn envelope(&self) -> Option<Vec<u8>> {
        URL_SAFE_NO_PAD.decode(&self.envelope_b64).ok()
    }
}

/// Something the mirror could not record as asked, reported instead (TL-85).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MirrorReport {
    /// A file could not be read; it is not recorded as deleted.
    Unreadable {
        /// The path.
        path: String,
        /// Why.
        error: String,
    },
    /// A binary file: recorded as a BlobRef, its bytes not uploaded (no blob transport yet).
    BlobNotUploaded {
        /// The path.
        path: String,
        /// The size.
        size: u64,
    },
    /// A text file that is binary only because it mixes line endings (TL-84 SHOULD warn).
    MixedLineEndings {
        /// The path.
        path: String,
    },
    /// A path that is not a valid tree key (TL-82).
    InvalidPath {
        /// The path.
        path: String,
    },
}

/// Why mirroring failed.
#[derive(Debug, thiserror::Error)]
pub enum MirrorError {
    /// The timeline engine refused something.
    #[error("timeline: {0}")]
    Timeline(#[from] han_timeline::Error),
    /// Storage.
    #[error("store: {0}")]
    Store(String),
    /// I/O.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The watcher.
    #[error("watch: {0}")]
    Watch(String),
}

/// A file's content as recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    /// A text file: LF-only text (masked) and its line ending.
    Text {
        /// The text, without CRs when `crlf`.
        text: String,
        /// Whether the file uses CRLF.
        crlf: bool,
    },
    /// A binary file: its SHA-256 and size.
    Blob {
        /// Lowercase hex SHA-256.
        hash: String,
        /// Bytes.
        size: u64,
    },
    /// A symbolic link's target.
    Link(String),
}

/// A file as the mirror sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileView {
    /// git's mode.
    pub mode: String,
    /// The content.
    pub content: Content,
}

#[derive(Clone, Debug)]
struct Shadow {
    obj: OpId,
    file: FileView,
}

/// Classifies bytes per TL-84 and masks text.
fn classify(bytes: &[u8], masker: &Masker) -> (Content, bool) {
    let blob = |mixed| {
        (
            Content::Blob {
                hash: hex(&Sha256::digest(bytes)),
                size: bytes.len() as u64,
            },
            mixed,
        )
    };
    if bytes.len() > MAX_TEXT || bytes.contains(&0) {
        return blob(false);
    }
    let Ok(s) = std::str::from_utf8(bytes) else {
        return blob(false);
    };
    let cr = s.matches('\r').count();
    let crlf = s.matches("\r\n").count();
    let text = if cr == 0 {
        (s.to_owned(), false)
    } else if cr == crlf && crlf == s.matches('\n').count() {
        (s.replace("\r\n", "\n"), true)
    } else {
        return blob(true);
    };
    (
        Content::Text {
            text: masker.mask(&text.0).into_owned(),
            crlf: text.1,
        },
        false,
    )
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn parse_history_id(s: &str) -> Option<HistoryId> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(han_timeline::BatchId(out))
}

/// Whether `p` may be a tree key (TL-82).
pub fn is_tree_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 4096
        && !p.contains('\0')
        && p.split('/').all(|seg| {
            !seg.is_empty() && seg != "." && seg != ".." && !seg.eq_ignore_ascii_case(".git")
        })
}

fn obj_value(id: &OpId) -> Value {
    Value::object_ref(id)
        .as_map()
        .and_then(|m| m.get("obj").cloned())
        .unwrap_or(Value::Null)
}

fn entry(obj: &OpId, f: &FileView) -> Value {
    let mut m = BTreeMap::new();
    m.insert("obj".to_owned(), obj_value(obj));
    m.insert("mode".to_owned(), Value::text(f.mode.clone()));
    if let Content::Text { crlf, .. } = &f.content {
        m.insert(
            "eol".to_owned(),
            Value::text(if *crlf { "crlf" } else { "lf" }),
        );
    }
    Value::Map(m)
}

fn blob_ref(hash: &str, size: u64) -> Value {
    let mut m = BTreeMap::new();
    m.insert("hash".to_owned(), Value::text(hash));
    m.insert(
        "size".to_owned(),
        Value::Int(i64::try_from(size).unwrap_or(i64::MAX)),
    );
    m.insert("type".to_owned(), Value::text("application/octet-stream"));
    Value::Map(m)
}

/// Splits a string into pieces of at most `max` bytes on character boundaries.
fn chunks(s: &str, max: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = s;
    while rest.len() > max {
        let mut cut = max;
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        out.push(&rest[..cut]);
        rest = &rest[cut..];
    }
    if !rest.is_empty() {
        out.push(rest);
    }
    out
}

/// A batch being put together; op ids of creates in it are predicted from its start counter.
struct Builder {
    start: u64,
    replica: Arc<ReplicaId>,
    ops: Vec<Op>,
    bytes: usize,
    files: Vec<String>,
}

impl Builder {
    fn next_id(&self) -> OpId {
        OpId::new(self.start + self.ops.len() as u64, self.replica.clone())
    }
}

/// Work that needs the head after the current batch: filling a large text in chunks.
enum Later {
    Append { obj: OpId, text: String },
    Edits { obj: OpId, edits: Vec<diff::Edit> },
}

/// A change to record: path, what was recorded, what is on disk, and the path it was renamed from.
type Planned = (String, Option<Shadow>, Option<FileView>, Option<String>);

struct Inner {
    worktree: PathBuf,
    masker: Arc<Masker>,
    ignore: IgnoreFn,
    history: History<FileStore>,
    history_hex: String,
    signer: KeySigner,
    public_key: String,
    main: OpId,
    session: OpId,
    tree: ObjectId,
    shadow: BTreeMap<String, Shadow>,
    next_counter: u64,
    step: Option<String>,
    sink: BatchSink,
    reports: Vec<MirrorReport>,
    batches: u64,
}

/// A mirror of one session's worktree.
pub struct Mirror {
    inner: Arc<Mutex<Inner>>,
    pending: Arc<Mutex<BTreeSet<String>>>,
    quiet: Duration,
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
}

/// The root `.gitignore`, read again whenever it changes.
fn crude_gitignore(worktree: &Path) -> IgnoreFn {
    let file = worktree.join(".gitignore");
    let cache: Mutex<(Option<std::time::SystemTime>, Vec<String>)> = Mutex::new((None, Vec::new()));
    Arc::new(move |p: &str, _dir: bool| {
        let mtime = std::fs::metadata(&file).and_then(|m| m.modified()).ok();
        let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
        if c.0 != mtime {
            c.1 = std::fs::read_to_string(&file)
                .map(|t| glob::gitignore_patterns(&t))
                .unwrap_or_default();
            c.0 = mtime;
        }
        c.1.iter().any(|g| glob::glob_match(g, p))
    })
}

impl Mirror {
    /// Opens the mirror: creates the history (main at the base, the session timeline) in an empty store, or
    /// reopens the one the store holds. Emits every batch it writes to `sink`.
    pub fn open(cfg: MirrorConfig, sink: BatchSink) -> Result<Mirror, MirrorError> {
        let store = FileStore::open(&cfg.store_dir)?;
        let ignore = cfg
            .ignore
            .clone()
            .unwrap_or_else(|| crude_gitignore(&cfg.worktree));
        let public_key = URL_SAFE_NO_PAD.encode(cfg.signer.public_key());
        let replica = Arc::new(ReplicaId::new(cfg.signer.actor(), cfg.signer.key_id(), 0));
        let inner = match store
            .read_meta("history")
            .and_then(|h| parse_history_id(&h))
        {
            Some(id) => {
                let history = History::open(store, &cfg.space, id)?;
                let main = history.main().ok_or(han_timeline::Error::UnknownTimeline)?;
                let session = history.timeline_by_name(SESSION_TIMELINE)?;
                let state = history.state(&At::Timeline(main.clone()))?;
                let tree = state
                    .map_value(&ObjectId::Root, "tree")
                    .and_then(Value::obj_field)
                    .map(ObjectId::Op)
                    .ok_or_else(|| MirrorError::Store("the history has no tree".into()))?;
                let mut next = 1;
                for env in history.envelopes() {
                    if let Ok(e) = Envelope::decode(env)
                        && let Ok((b, _)) = e.batch()
                    {
                        next = next.max(b.start + b.ops.len() as u64);
                    }
                }
                let mut inner = Inner {
                    worktree: cfg.worktree.clone(),
                    masker: cfg.masker.clone(),
                    ignore,
                    history_hex: id.to_hex(),
                    history,
                    signer: cfg.signer,
                    public_key,
                    main,
                    session,
                    tree,
                    shadow: BTreeMap::new(),
                    next_counter: next,
                    step: None,
                    sink,
                    reports: Vec::new(),
                    batches: 0,
                };
                inner.shadow = inner.read_shadow()?;
                inner
            }
            None => {
                // Genesis: main (op 0), the tree map (op 1), the root's `tree` (op 2).
                let start = 1 + han_timeline::Store::high_water(
                    &store,
                    cfg.signer.actor(),
                    cfg.signer.key_id(),
                )
                .map_err(MirrorError::Store)?;
                let tree_id = OpId::new(start + 1, replica.clone());
                let (history, genesis) = History::create(
                    store,
                    &cfg.space,
                    &cfg.signer,
                    0,
                    HistoryKind::Worktree,
                    None,
                    vec![
                        Op::MapCreate { tree: true },
                        Op::MapSet {
                            target: ObjectId::Root,
                            key: "tree".into(),
                            value: Value::object_ref(&tree_id),
                        },
                    ],
                )?;
                let id = history.id();
                history
                    .store()
                    .write_meta("history", &id.to_hex())
                    .map_err(MirrorError::Store)?;
                let main = history.main().ok_or(han_timeline::Error::UnknownTimeline)?;
                let next = genesis.ops.last().map_or(start, |o| o.counter + 1);
                let mut inner = Inner {
                    worktree: cfg.worktree.clone(),
                    masker: cfg.masker.clone(),
                    ignore,
                    history_hex: id.to_hex(),
                    history,
                    signer: cfg.signer,
                    public_key,
                    main: main.clone(),
                    session: main.clone(),
                    tree: ObjectId::Op(tree_id),
                    shadow: BTreeMap::new(),
                    next_counter: next,
                    step: None,
                    sink,
                    reports: Vec::new(),
                    batches: 0,
                };
                inner.emit(&genesis.envelope, "main", None, Vec::new());
                // Main at the base commit: the base tree's files, then `git.commit` (TL-88).
                let mut changes = Vec::new();
                for f in &cfg.base_files {
                    if !is_tree_path(&f.path) || inner.masker.is_excluded(&f.path) {
                        continue;
                    }
                    let content = if f.mode == "120000" {
                        Content::Link(String::from_utf8_lossy(&f.bytes).into_owned())
                    } else {
                        classify(&f.bytes, &inner.masker).0
                    };
                    let mode = match f.mode.as_str() {
                        "100755" | "120000" => f.mode.clone(),
                        _ => "100644".into(),
                    };
                    changes.push((f.path.clone(), None, Some(FileView { mode, content })));
                }
                let main_tl = inner.main.clone();
                inner.apply(&main_tl, changes, "main")?;
                let w = inner.history.write(
                    &inner.signer,
                    0,
                    &main_tl,
                    vec![Op::MapSet {
                        target: ObjectId::Root,
                        key: "git.commit".into(),
                        value: Value::text(cfg.base_commit.clone()),
                    }],
                    None,
                )?;
                inner.after_write(&w, "main", None, Vec::new());
                let fork =
                    inner
                        .history
                        .fork(&inner.signer, 0, &main_tl, None, SESSION_TIMELINE)?;
                inner.session = fork.ops[0].clone();
                inner.after_write(&fork, "session", None, Vec::new());
                inner
            }
        };
        Ok(Mirror {
            inner: Arc::new(Mutex::new(inner)),
            pending: Arc::new(Mutex::new(BTreeSet::new())),
            quiet: cfg.quiet,
            watcher: Mutex::new(None),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Starts the file watcher: changes are recorded after each quiet period.
    pub fn watch(&self) -> Result<(), MirrorError> {
        use notify::Watcher as _;
        let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let mut watcher = notify::recommended_watcher(move |ev| {
            let _ = tx.send(ev);
        })
        .map_err(|e| MirrorError::Watch(e.to_string()))?;
        let root = self.lock().worktree.clone();
        watcher
            .watch(&root, notify::RecursiveMode::Recursive)
            .map_err(|e| MirrorError::Watch(e.to_string()))?;
        let inner = Arc::clone(&self.inner);
        let pending = Arc::clone(&self.pending);
        let quiet = self.quiet;
        std::thread::Builder::new()
            .name("han-mirror".into())
            .spawn(move || {
                let add = |ev: notify::Result<notify::Event>| {
                    let mut p = pending.lock().unwrap_or_else(|e| e.into_inner());
                    match ev {
                        Ok(ev) => {
                            for path in ev.paths {
                                match relative(&root, &path) {
                                    Some(r) if r.is_empty() => {
                                        p.insert(String::new());
                                    }
                                    Some(r) => {
                                        p.insert(r);
                                    }
                                    None => {
                                        p.insert(String::new()); // unknown: rescan all
                                    }
                                }
                            }
                        }
                        Err(_) => {
                            p.insert(String::new());
                        }
                    }
                };
                while let Ok(ev) = rx.recv() {
                    add(ev);
                    loop {
                        match rx.recv_timeout(quiet) {
                            Ok(ev) => add(ev),
                            Err(mpsc::RecvTimeoutError::Timeout) => break,
                            Err(mpsc::RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    let paths =
                        std::mem::take(&mut *pending.lock().unwrap_or_else(|e| e.into_inner()));
                    let mut g = inner.lock().unwrap_or_else(|e| e.into_inner());
                    if let Err(e) = g.process(paths) {
                        tracing::warn!("mirror: {e}");
                    }
                }
            })?;
        *self.watcher.lock().unwrap_or_else(|p| p.into_inner()) = Some(watcher);
        Ok(())
    }

    /// Stops the watcher.
    pub fn unwatch(&self) {
        self.watcher
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
    }

    /// Attributes the batches that follow to a step (the ACP tool call id), or to none.
    pub fn set_step(&self, step: Option<String>) {
        self.lock().step = step.map(|s| {
            let mut s = s;
            while s.len() > 256 {
                s.pop();
            }
            s
        });
    }

    /// Records the paths the watcher has collected but not recorded yet, now.
    pub fn flush(&self) -> Result<(), MirrorError> {
        let paths = std::mem::take(&mut *self.pending.lock().unwrap_or_else(|e| e.into_inner()));
        if paths.is_empty() {
            return Ok(());
        }
        self.lock().process(paths)
    }

    /// Compares the whole worktree with what is recorded and records the difference. Returns how many
    /// batches it wrote.
    pub fn rescan(&self) -> Result<u64, MirrorError> {
        let mut g = self.lock();
        let before = g.batches;
        g.process(BTreeSet::from([String::new()]))?;
        Ok(g.batches - before)
    }

    /// The history id (64 hex digits).
    pub fn history_id(&self) -> String {
        self.lock().history_hex.clone()
    }

    /// Every envelope of the history, in order, for republishing to readers that joined late.
    pub fn envelopes(&self) -> Vec<MirrorBatch> {
        let g = self.lock();
        g.history
            .envelopes()
            .map(|e| {
                let step = Envelope::decode(e)
                    .ok()
                    .and_then(|x| x.batch().ok())
                    .and_then(|(b, _)| b.step);
                g.batch_of(e, "", step, Vec::new())
            })
            .collect()
    }

    /// The files at the session timeline's head, as recorded.
    pub fn files(&self) -> Result<BTreeMap<String, FileView>, MirrorError> {
        let g = self.lock();
        Ok(g.read_shadow()?
            .into_iter()
            .map(|(k, v)| (k, v.file))
            .collect())
    }

    /// What the mirror reported since the last call.
    pub fn take_reports(&self) -> Vec<MirrorReport> {
        std::mem::take(&mut self.lock().reports)
    }
}

impl Drop for Mirror {
    fn drop(&mut self) {
        self.unwatch();
    }
}

fn relative(root: &Path, path: &Path) -> Option<String> {
    let r = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = r
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

impl Inner {
    fn batch_of(
        &self,
        envelope: &[u8],
        timeline: &str,
        step: Option<String>,
        files: Vec<String>,
    ) -> MirrorBatch {
        MirrorBatch {
            history: self.history_hex.clone(),
            envelope_b64: URL_SAFE_NO_PAD.encode(envelope),
            actor: self.signer.actor().to_owned(),
            key_id: self.signer.key_id().to_owned(),
            public_key: self.public_key.clone(),
            timeline: timeline.to_owned(),
            step,
            files,
        }
    }

    fn emit(&mut self, envelope: &[u8], timeline: &str, step: Option<String>, files: Vec<String>) {
        let b = self.batch_of(envelope, timeline, step, files);
        self.batches += 1;
        (self.sink)(b);
    }

    fn after_write(
        &mut self,
        w: &han_timeline::Written,
        timeline: &str,
        step: Option<String>,
        files: Vec<String>,
    ) {
        if let Some(last) = w.ops.last() {
            self.next_counter = last.counter + 1;
        }
        self.emit(&w.envelope.clone(), timeline, step, files);
    }

    fn builder(&self) -> Builder {
        Builder {
            start: self.next_counter,
            replica: Arc::new(ReplicaId::new(self.signer.actor(), self.signer.key_id(), 0)),
            ops: Vec::new(),
            bytes: 0,
            files: Vec::new(),
        }
    }

    fn flush_builder(&mut self, tl: &OpId, b: &mut Builder, name: &str) -> Result<(), MirrorError> {
        if b.ops.is_empty() {
            return Ok(());
        }
        let ops = std::mem::take(&mut b.ops);
        let files = std::mem::take(&mut b.files);
        let step = self.step.clone();
        let w = self.history.write(&self.signer, 0, tl, ops, step.clone())?;
        debug_assert_eq!(w.ops.first().map(|o| o.counter), Some(b.start));
        self.after_write(&w, name, step, files);
        *b = self.builder();
        Ok(())
    }

    fn write_one(&mut self, tl: &OpId, op: Op, path: &str, name: &str) -> Result<(), MirrorError> {
        let step = self.step.clone();
        let w = self
            .history
            .write(&self.signer, 0, tl, vec![op], step.clone())?;
        self.after_write(&w, name, step, vec![path.to_owned()]);
        Ok(())
    }

    fn run_later(
        &mut self,
        tl: &OpId,
        later: Vec<(String, Later)>,
        name: &str,
    ) -> Result<(), MirrorError> {
        for (path, l) in later {
            match l {
                Later::Append { obj, text } => {
                    let object = ObjectId::Op(obj);
                    let mut pos = 0;
                    for piece in chunks(&text, CHUNK) {
                        if let Some(op) = self.history.plan_edit(
                            &At::Timeline(tl.clone()),
                            &object,
                            pos,
                            0,
                            piece,
                        )? {
                            self.write_one(tl, op, &path, name)?;
                        }
                        pos += piece.chars().count();
                    }
                }
                Later::Edits { obj, edits } => {
                    let object = ObjectId::Op(obj);
                    // Last first, so earlier positions stay valid.
                    for e in edits.iter().rev() {
                        let pieces = chunks(&e.insert, CHUNK);
                        let mut first = true;
                        let mut pos = e.pos;
                        if pieces.is_empty() {
                            if let Some(op) = self.history.plan_edit(
                                &At::Timeline(tl.clone()),
                                &object,
                                e.pos,
                                e.delete,
                                "",
                            )? {
                                self.write_one(tl, op, &path, name)?;
                            }
                            continue;
                        }
                        for piece in pieces {
                            let delete = if first { e.delete } else { 0 };
                            if let Some(op) = self.history.plan_edit(
                                &At::Timeline(tl.clone()),
                                &object,
                                pos,
                                delete,
                                piece,
                            )? {
                                self.write_one(tl, op, &path, name)?;
                            }
                            first = false;
                            pos += piece.chars().count();
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Records a list of `(path, old, new)` changes on timeline `tl`.
    fn apply(
        &mut self,
        tl: &OpId,
        changes: Vec<(String, Option<Shadow>, Option<FileView>)>,
        name: &str,
    ) -> Result<(), MirrorError> {
        let tree = self.tree.clone();
        let mut b = self.builder();
        let mut later: Vec<(String, Later)> = Vec::new();
        // Renames: a delete and a create of identical content and kind.
        let mut deleted: Vec<(String, Shadow)> = Vec::new();
        let mut rest = Vec::new();
        for (p, old, new) in changes {
            match (old, new) {
                (Some(o), None) => deleted.push((p, o)),
                other => rest.push((p, other.0, other.1)),
            }
        }
        let mut renamed_from: BTreeSet<String> = BTreeSet::new();
        let mut planned: Vec<Planned> = Vec::new();
        for (p, old, new) in rest {
            if old.is_none()
                && let Some(n) = &new
                && let Some((from, _)) = deleted
                    .iter()
                    .find(|(dp, d)| !renamed_from.contains(dp) && d.file.content == n.content)
            {
                let from = from.clone();
                renamed_from.insert(from.clone());
                planned.push((p, None, new, Some(from)));
                continue;
            }
            planned.push((p, old, new, None));
        }
        for (p, o) in deleted {
            if !renamed_from.contains(&p) {
                planned.push((p, Some(o), None, None));
            }
        }
        planned.sort_by(|a, b| a.0.cmp(&b.0));

        for (path, old, new, from) in planned {
            if b.ops.len() >= MAX_OPS || b.bytes >= CHUNK {
                self.flush_builder(tl, &mut b, name)?;
            }
            match (old, new) {
                (_, None) => {
                    b.ops.push(Op::MapDelete {
                        target: tree.clone(),
                        key: path.clone(),
                    });
                    self.shadow.remove(&path);
                }
                (None, Some(n)) if from.is_some() => {
                    let from = from.unwrap_or_default();
                    let Some(o) = self.shadow.remove(&from) else {
                        continue;
                    };
                    b.ops.push(Op::MapDelete {
                        target: tree.clone(),
                        key: from.clone(),
                    });
                    b.ops.push(Op::MapSet {
                        target: tree.clone(),
                        key: path.clone(),
                        value: entry(&o.obj, &n),
                    });
                    b.files.push(from);
                    self.shadow.insert(
                        path.clone(),
                        Shadow {
                            obj: o.obj,
                            file: n,
                        },
                    );
                }
                (old, Some(n)) => {
                    let same_kind = old.as_ref().is_some_and(|o| {
                        std::mem::discriminant(&o.file.content)
                            == std::mem::discriminant(&n.content)
                    });
                    if let (true, Some(o)) = (same_kind, old) {
                        let obj = o.obj.clone();
                        match (&o.file.content, &n.content) {
                            (Content::Text { text: a, .. }, Content::Text { text: c, .. })
                                if a != c =>
                            {
                                let edits = diff::char_edits(a, c);
                                let ins: usize = edits.iter().map(|e| e.insert.len()).sum();
                                if ins + b.bytes > CHUNK * 2 || edits.len() > MAX_OPS {
                                    later.push((
                                        path.clone(),
                                        Later::Edits {
                                            obj: obj.clone(),
                                            edits,
                                        },
                                    ));
                                } else {
                                    let object = ObjectId::Op(obj.clone());
                                    for e in &edits {
                                        if let Some(op) = self.history.plan_edit(
                                            &At::Timeline(tl.clone()),
                                            &object,
                                            e.pos,
                                            e.delete,
                                            &e.insert,
                                        )? {
                                            b.ops.push(op);
                                        }
                                    }
                                    b.bytes += ins;
                                }
                            }
                            (Content::Blob { hash: a, .. }, Content::Blob { hash: c, size })
                                if a != c =>
                            {
                                b.ops.push(Op::RegisterSet {
                                    target: ObjectId::Op(obj.clone()),
                                    value: blob_ref(c, *size),
                                });
                                self.reports.push(MirrorReport::BlobNotUploaded {
                                    path: path.clone(),
                                    size: *size,
                                });
                            }
                            (Content::Link(a), Content::Link(c)) if a != c => {
                                b.ops.push(Op::RegisterSet {
                                    target: ObjectId::Op(obj.clone()),
                                    value: Value::text(c.clone()),
                                });
                            }
                            _ => {}
                        }
                        if entry(&obj, &o.file) != entry(&obj, &n) {
                            b.ops.push(Op::MapSet {
                                target: tree.clone(),
                                key: path.clone(),
                                value: entry(&obj, &n),
                            });
                        }
                        self.shadow.insert(path.clone(), Shadow { obj, file: n });
                    } else {
                        // A new file, or a change of kind: a new content object under the path.
                        let obj = b.next_id();
                        match &n.content {
                            Content::Text { text, .. } if text.len() <= CHUNK => {
                                b.bytes += text.len();
                                b.ops.push(Op::TextCreate {
                                    text: text.clone(),
                                    transcript: false,
                                });
                            }
                            Content::Text { text, .. } => {
                                b.ops.push(Op::TextCreate {
                                    text: String::new(),
                                    transcript: false,
                                });
                                later.push((
                                    path.clone(),
                                    Later::Append {
                                        obj: obj.clone(),
                                        text: text.clone(),
                                    },
                                ));
                            }
                            Content::Blob { hash, size } => {
                                b.ops.push(Op::RegisterCreate {
                                    value: blob_ref(hash, *size),
                                });
                                self.reports.push(MirrorReport::BlobNotUploaded {
                                    path: path.clone(),
                                    size: *size,
                                });
                            }
                            Content::Link(t) => {
                                b.ops.push(Op::RegisterCreate {
                                    value: Value::text(t.clone()),
                                });
                            }
                        }
                        b.ops.push(Op::MapSet {
                            target: tree.clone(),
                            key: path.clone(),
                            value: entry(&obj, &n),
                        });
                        self.shadow.insert(path.clone(), Shadow { obj, file: n });
                    }
                }
            }
            b.files.push(path);
        }
        self.flush_builder(tl, &mut b, name)?;
        self.run_later(tl, later, name)
    }

    /// Reads the recorded files at the session head.
    fn read_shadow(&self) -> Result<BTreeMap<String, Shadow>, MirrorError> {
        let state = self.history.state(&At::Timeline(self.session.clone()))?;
        let mut out = BTreeMap::new();
        let Some(ObjectState::Map { entries, .. }) = state.object(&self.tree) else {
            return Ok(out);
        };
        for (path, values) in entries {
            let Some((_, v)) = values.first() else {
                continue;
            };
            let Some(obj) = v.obj_field() else { continue };
            let m = v.as_map();
            let mode = m
                .and_then(|m| m.get("mode"))
                .and_then(Value::as_text)
                .unwrap_or("100644")
                .to_owned();
            let crlf = m.and_then(|m| m.get("eol")).and_then(Value::as_text) == Some("crlf");
            let id = ObjectId::Op(obj.clone());
            let content = match state.object(&id) {
                Some(ObjectState::Text { text, .. }) => Content::Text { text, crlf },
                Some(ObjectState::Register(Value::Text(t))) => Content::Link(t),
                Some(ObjectState::Register(Value::Map(r))) => Content::Blob {
                    hash: r
                        .get("hash")
                        .and_then(Value::as_text)
                        .unwrap_or("")
                        .to_owned(),
                    size: match r.get("size") {
                        Some(Value::Int(n)) => u64::try_from(*n).unwrap_or(0),
                        _ => 0,
                    },
                },
                _ => continue,
            };
            out.insert(
                path,
                Shadow {
                    obj,
                    file: FileView { mode, content },
                },
            );
        }
        Ok(out)
    }

    /// Reads a path from disk: `Ok(None)` when it is gone or a directory.
    fn read_disk(&mut self, path: &str) -> Result<Option<FileView>, String> {
        let full = self.worktree.join(path);
        let meta = match std::fs::symlink_metadata(&full) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&full).map_err(|e| e.to_string())?;
            return Ok(Some(FileView {
                mode: "120000".into(),
                content: Content::Link(target.to_string_lossy().replace('\\', "/")),
            }));
        }
        if !meta.is_file() {
            return Ok(None);
        }
        let bytes = std::fs::read(&full).map_err(|e| e.to_string())?;
        let (content, mixed) = classify(&bytes, &self.masker);
        if mixed {
            self.reports.push(MirrorReport::MixedLineEndings {
                path: path.to_owned(),
            });
        }
        Ok(Some(FileView {
            mode: mode_of(&meta),
            content,
        }))
    }

    /// Every file under `dir` (relative; empty for the root) that may be mirrored.
    fn walk(&self, dir: &str, out: &mut BTreeSet<String>) {
        let full = if dir.is_empty() {
            self.worktree.clone()
        } else {
            self.worktree.join(dir)
        };
        let Ok(rd) = std::fs::read_dir(&full) else {
            return;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let rel = if dir.is_empty() {
                name.clone()
            } else {
                format!("{dir}/{name}")
            };
            if name.eq_ignore_ascii_case(".git") {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if self.masker.is_excluded(&rel) || (self.ignore)(&rel, true) {
                    continue;
                }
                self.walk(&rel, out);
            } else {
                out.insert(rel);
            }
        }
    }

    /// Records the changes of these paths (an empty path means the whole worktree).
    fn process(&mut self, dirty: BTreeSet<String>) -> Result<(), MirrorError> {
        let mut paths: BTreeSet<String> = BTreeSet::new();
        for d in &dirty {
            if d.is_empty() {
                self.walk("", &mut paths);
                paths.extend(self.shadow.keys().cloned());
                continue;
            }
            if d.split('/').any(|s| s.eq_ignore_ascii_case(".git")) {
                continue;
            }
            if self.worktree.join(d).is_dir() {
                self.walk(d, &mut paths);
            }
            let prefix = format!("{d}/");
            paths.extend(
                self.shadow
                    .keys()
                    .filter(|k| k.starts_with(&prefix))
                    .cloned(),
            );
            paths.insert(d.clone());
        }
        let mut changes = Vec::new();
        for p in paths {
            if !is_tree_path(&p) {
                if !p.split('/').any(|s| s.eq_ignore_ascii_case(".git")) && !p.is_empty() {
                    self.reports.push(MirrorReport::InvalidPath { path: p });
                }
                continue;
            }
            if self.masker.is_excluded(&p) {
                continue;
            }
            let old = self.shadow.get(&p).cloned();
            if old.is_none() && (self.ignore)(&p, false) {
                continue;
            }
            match self.read_disk(&p).map(|n| keep_exec_bit(old.as_ref(), n)) {
                Err(error) => self
                    .reports
                    .push(MirrorReport::Unreadable { path: p, error }),
                Ok(new) => {
                    let same = match (&old, &new) {
                        (Some(o), Some(n)) => o.file == *n,
                        (None, None) => true,
                        _ => false,
                    };
                    if !same {
                        changes.push((p, old, new));
                    }
                }
            }
        }
        if changes.is_empty() {
            return Ok(());
        }
        let tl = self.session.clone();
        self.apply(&tl, changes, SESSION_TIMELINE)
    }
}

/// Where the file system has no executable bit, a recorded `100755` stays as it was.
fn keep_exec_bit(old: Option<&Shadow>, new: Option<FileView>) -> Option<FileView> {
    let mut new = new?;
    if cfg!(not(unix))
        && let Some(o) = old
        && o.file.mode == "100755"
        && new.mode == "100644"
    {
        new.mode = o.file.mode.clone();
    }
    Some(new)
}

#[cfg(unix)]
fn mode_of(meta: &std::fs::Metadata) -> String {
    use std::os::unix::fs::PermissionsExt as _;
    if meta.permissions().mode() & 0o111 != 0 {
        "100755".into()
    } else {
        "100644".into()
    }
}

#[cfg(not(unix))]
fn mode_of(_meta: &std::fs::Metadata) -> String {
    "100644".into()
}

/// Materialises a recorded file's bytes (TL-84: CRLF restored); `None` for a blob, whose bytes are not held.
pub fn materialise(f: &FileView) -> Option<Vec<u8>> {
    match &f.content {
        Content::Text { text, crlf: true } => Some(text.replace('\n', "\r\n").into_bytes()),
        Content::Text { text, crlf: false } => Some(text.clone().into_bytes()),
        Content::Link(t) => Some(t.clone().into_bytes()),
        Content::Blob { .. } => None,
    }
}
