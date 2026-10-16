//! `han review`: the tier-1 machine reviewer as a service (PLAN §12 "Machine review"; `crates/han-review`).
//!
//! * `han review run --dir D --keys K --home H` runs it, like `han exec`: its own node as the reviewer's actor
//!   (executor-class connection, so it cannot send `EffectApprove`), watching the open `vcs.land` proposals of
//!   the configured spaces. It starts only when `D/review.json` exists and says `enabled: true`.
//! * `han review set` writes `review.json` (and, with `--home`/`--key-file`, `review.env`, which the unit
//!   reads), so nobody edits the files by hand. Only what is given changes. Prints `changed:` or `unchanged:`.
//!   The model API key goes in the environment file only, mode 600, and is never printed or put in
//!   `review.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Subcommand};
use han_node::NodeApi as _;
use han_node::api::{Command, ConnectionClass, NodeError};
use han_registry::HomeRef;
use han_review::config::{self, AuthorSpec, KeyHeader, Route, ServiceConfig};
use han_review::service::Service;
use han_review::{Independence, OpenAiCompat};
use han_vcs::GitVcs;

use crate::rolenode;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// The file with the environment of the unit: `HAN_HOME`, `HAN_RELAY` and the model API key.
pub const ENV_FILE: &str = "review.env";

/// Options of `han review`.
#[derive(Args)]
pub struct ReviewArgs {
    #[command(subcommand)]
    cmd: ReviewCmd,
}

#[derive(Subcommand)]
enum ReviewCmd {
    /// Runs the reviewer service. Does nothing (and exits cleanly) unless `review.json` says `enabled: true`.
    Run(Box<RunArgs>),
    /// Writes `review.json` (and `review.env`): only what is given changes. Needs no running node.
    Set(Box<SetArgs>),
}

/// Options of `han review run`.
#[derive(Args)]
struct RunArgs {
    /// The reviewer's directory: `review.json`, `review.env`, `state/` (ledger and kept records), `node/`,
    /// `objects/`.
    #[arg(long, env = "HAN_REVIEW_DIR", default_value = "han-review")]
    dir: PathBuf,
    /// Where the key store is (`<keys>/keys` for file keys; else the keychain). For an OS-user agent made by
    /// `han agent add --os-user`, its state directory.
    #[arg(long)]
    keys: Option<PathBuf>,
    /// The id of the reviewer's key in its profile.
    #[arg(long, default_value = "k1")]
    key_id: String,
    /// The name of the key in the key store (default: the actor's, `agent-<name>-<server>`).
    #[arg(long)]
    key_name: Option<String>,
    /// The space's home: a home ticket (`han1home.…`) or a ServerName.
    #[arg(long, env = "HAN_HOME")]
    home: HomeRef,
    /// Relays: `n0`, `none`, or URLs.
    #[arg(long, env = "HAN_RELAY", default_value = "n0")]
    relay: String,
}

/// Options of `han review set`.
#[derive(Args, Default)]
struct SetArgs {
    /// The reviewer's directory (holds `review.json`).
    #[arg(long, env = "HAN_REVIEW_DIR", default_value = "han-review")]
    dir: PathBuf,
    /// The reviewer actor, `@name:server`. Required the first time.
    #[arg(long)]
    actor: Option<String>,
    /// The reviewer's operators, comma separated, nearest first (`""` for none).
    #[arg(long)]
    operators: Option<String>,
    /// The model, for example `agy/gemini-flash`. It must not be of the author's vendor. Required the first
    /// time.
    #[arg(long)]
    model: Option<String>,
    /// The output ceiling asked of the model. Required the first time; for the agy Gemini Flash route about
    /// 16384 (a wrong value gives truncation or spurious compaction).
    #[arg(long)]
    max_tokens: Option<u32>,
    /// The model route's base URL (`https://…`, or `http://` to a loopback host). Required the first time.
    #[arg(long)]
    base_url: Option<String>,
    /// The path of the chat endpoint (default `/v1/chat/completions`).
    #[arg(long)]
    path: Option<String>,
    /// The name of the environment variable that holds the API key. Required the first time.
    #[arg(long)]
    key_env: Option<String>,
    /// How the key is sent: `bearer` (default) or `x-api-key`.
    #[arg(long)]
    key_header: Option<String>,
    /// The backend's name in the record (default `omniroute`).
    #[arg(long)]
    route_name: Option<String>,
    /// Seconds one model call may take (default 120).
    #[arg(long)]
    timeout_secs: Option<u64>,
    /// The spaces to watch, comma separated; replaces the list.
    #[arg(long)]
    spaces: Option<String>,
    /// A repository: `<name>=<remote>` (the name is what plans call it, `github:owner/repo`; the remote is
    /// what the reviewer fetches a bundle's base from, readable without credentials). Repeatable.
    #[arg(long)]
    repo: Vec<String>,
    /// `strict` (default) or `dogfood_relaxed` (one person operates every agent: different actor and vendor
    /// only; records say so).
    #[arg(long)]
    independence: Option<String>,
    /// An author: `<actor>=<harness>:<model>[:<operator>,<operator>]`. Repeatable. The node does not say which
    /// harness and model ran an actor, so this does.
    #[arg(long)]
    author: Vec<String>,
    /// The author of any actor not given with `--author`: `<harness>:<model>[:<operators>]`; `""` removes it.
    #[arg(long)]
    default_author: Option<String>,
    /// Turn the service on or off (`--enabled`, `--enabled=false`). Default off.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    enabled: Option<bool>,
    /// Whether an `approve` becomes an approval (`--approve`, `--approve=false`). Off: it records and messages
    /// only.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    approve: Option<bool>,
    /// An `approve` below this confidence (0 to 100) is recorded, not acted on (default 80).
    #[arg(long)]
    min_confidence: Option<u8>,
    /// A diff over this many bytes is not sent to the model (default 200000).
    #[arg(long)]
    max_diff_bytes: Option<usize>,
    /// Seconds between two reads of the open proposals (default 15).
    #[arg(long)]
    poll_secs: Option<u64>,
    /// The home, written to `review.env` for the unit: a home ticket or a ServerName.
    #[arg(long)]
    home: Option<HomeRef>,
    /// The relays, written to `review.env`: `n0`, `none` or URLs.
    #[arg(long)]
    relay: Option<String>,
    /// A file holding the model API key (readable only by you). It is copied into `review.env` under the
    /// variable named by `--key-env`, mode 600, and never printed.
    #[arg(long)]
    key_file: Option<PathBuf>,
}

/// What `han review set` changes: what is `Some` is set, the rest stays.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Edit {
    pub actor: Option<String>,
    pub operators: Option<Vec<String>>,
    pub model: Option<String>,
    pub max_tokens: Option<u32>,
    pub base_url: Option<String>,
    pub path: Option<String>,
    pub key_env: Option<String>,
    pub key_header: Option<KeyHeader>,
    pub route_name: Option<String>,
    pub timeout_secs: Option<u64>,
    pub spaces: Option<Vec<String>>,
    pub repos: Vec<(String, String)>,
    pub independence: Option<Independence>,
    pub authors: Vec<(String, AuthorSpec)>,
    /// `Some(None)` removes the default author.
    pub default_author: Option<Option<AuthorSpec>>,
    pub enabled: Option<bool>,
    pub approve: Option<bool>,
    pub min_confidence: Option<u8>,
    pub max_diff_bytes: Option<usize>,
    pub poll_secs: Option<u64>,
}

impl Edit {
    fn is_empty(&self) -> bool {
        *self == Edit::default()
    }
}

fn split_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `<harness>:<model>[:<operators>]`.
fn parse_author(s: &str) -> Res<AuthorSpec> {
    let mut parts = s.splitn(3, ':');
    let (Some(h), Some(m)) = (parts.next(), parts.next()) else {
        return Err(format!("{s:?} is not <harness>:<model>[:<operator>,<operator>]").into());
    };
    if h.trim().is_empty() || m.trim().is_empty() {
        return Err(format!("{s:?} is not <harness>:<model>[:<operator>,<operator>]").into());
    }
    Ok(AuthorSpec {
        harness: h.trim().to_owned(),
        model: m.trim().to_owned(),
        operators: parts.next().map(split_list).unwrap_or_default(),
    })
}

/// `<actor>=<harness>:<model>[:<operators>]`. The actor has a `:` of its own, so the split is at the `=`.
fn parse_actor_author(s: &str) -> Res<(String, AuthorSpec)> {
    let (actor, spec) = s
        .split_once('=')
        .ok_or_else(|| format!("--author {s:?} is not <actor>=<harness>:<model>[:<operators>]"))?;
    if !actor.starts_with('@') || !actor.contains(':') {
        return Err(format!("--author {actor:?} is not an actor (`@name:server`)").into());
    }
    Ok((actor.to_owned(), parse_author(spec)?))
}

fn parse_independence(s: &str) -> Res<Independence> {
    match s.replace('-', "_").as_str() {
        "strict" => Ok(Independence::Strict),
        "dogfood_relaxed" => Ok(Independence::DogfoodRelaxed),
        _ => Err(format!("--independence {s:?} is `strict` or `dogfood_relaxed`").into()),
    }
}

fn parse_key_header(s: &str) -> Res<KeyHeader> {
    match s {
        "bearer" => Ok(KeyHeader::Bearer),
        "x-api-key" => Ok(KeyHeader::XApiKey),
        _ => Err(format!("--key-header {s:?} is `bearer` or `x-api-key`").into()),
    }
}

impl SetArgs {
    fn edit(&self) -> Res<Edit> {
        let mut repos = Vec::new();
        for r in &self.repo {
            let (name, remote) = r
                .split_once('=')
                .filter(|(n, m)| !n.is_empty() && !m.is_empty())
                .ok_or_else(|| format!("--repo {r:?} is not <name>=<remote>"))?;
            repos.push((name.to_owned(), remote.to_owned()));
        }
        Ok(Edit {
            actor: self.actor.clone(),
            operators: self.operators.as_deref().map(split_list),
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            base_url: self.base_url.clone(),
            path: self.path.clone(),
            key_env: self.key_env.clone(),
            key_header: self
                .key_header
                .as_deref()
                .map(parse_key_header)
                .transpose()?,
            route_name: self.route_name.clone(),
            timeout_secs: self.timeout_secs,
            spaces: self.spaces.as_deref().map(split_list),
            repos,
            independence: self
                .independence
                .as_deref()
                .map(parse_independence)
                .transpose()?,
            authors: self
                .author
                .iter()
                .map(|a| parse_actor_author(a))
                .collect::<Res<_>>()?,
            default_author: match self.default_author.as_deref() {
                None => None,
                Some("") => Some(None),
                Some(s) => Some(Some(parse_author(s)?)),
            },
            enabled: self.enabled,
            approve: self.approve,
            min_confidence: self.min_confidence,
            max_diff_bytes: self.max_diff_bytes,
            poll_secs: self.poll_secs,
        })
    }
}

/// Applies `edit` to `cfg` (or starts a configuration when there is none). Refuses a result the service could
/// not use before anything is written. Says whether the configuration changed.
pub fn apply(cfg: Option<ServiceConfig>, edit: &Edit) -> Res<(ServiceConfig, bool)> {
    let before = cfg.clone();
    let mut c = match cfg {
        Some(c) => c,
        None => {
            let need = |v: &Option<String>, flag: &str| -> Res<String> {
                v.clone().ok_or_else(|| {
                    format!("there is no review.json yet: give {flag} (and --actor, --model, --max-tokens, --base-url, --key-env) to make it").into()
                })
            };
            ServiceConfig::new(
                &need(&edit.actor, "--actor")?,
                &need(&edit.model, "--model")?,
                edit.max_tokens
                    .ok_or("there is no review.json yet: give --max-tokens (about 16384 for the agy Gemini Flash route)")?,
                Route {
                    name: edit.route_name.clone().unwrap_or_else(|| "omniroute".into()),
                    base: need(&edit.base_url, "--base-url")?,
                    path: edit
                        .path
                        .clone()
                        .unwrap_or_else(|| "/v1/chat/completions".into()),
                    key_env: need(&edit.key_env, "--key-env")?,
                    key_header: KeyHeader::Bearer,
                    timeout_secs: 120,
                },
            )
        }
    };
    if let Some(v) = &edit.actor {
        c.actor.clone_from(v);
    }
    if let Some(v) = &edit.operators {
        c.operators.clone_from(v);
    }
    if let Some(v) = &edit.model {
        c.model.clone_from(v);
    }
    if let Some(v) = edit.max_tokens {
        c.max_tokens = v;
    }
    if let Some(v) = &edit.base_url {
        c.route.base.clone_from(v);
    }
    if let Some(v) = &edit.path {
        c.route.path.clone_from(v);
    }
    if let Some(v) = &edit.key_env {
        c.route.key_env.clone_from(v);
    }
    if let Some(v) = edit.key_header {
        c.route.key_header = v;
    }
    if let Some(v) = &edit.route_name {
        c.route.name.clone_from(v);
    }
    if let Some(v) = edit.timeout_secs {
        c.route.timeout_secs = v;
    }
    if let Some(v) = &edit.spaces {
        c.spaces.clone_from(v);
    }
    for (n, r) in &edit.repos {
        c.repos.insert(n.clone(), r.clone());
    }
    if let Some(v) = edit.independence {
        c.independence = v;
    }
    for (a, s) in &edit.authors {
        c.authors.insert(a.clone(), s.clone());
    }
    if let Some(v) = &edit.default_author {
        c.default_author.clone_from(v);
    }
    if let Some(v) = edit.enabled {
        c.enabled = v;
    }
    if let Some(v) = edit.approve {
        c.approve = v;
    }
    if let Some(v) = edit.min_confidence {
        c.min_confidence = v;
    }
    if let Some(v) = edit.max_diff_bytes {
        c.max_diff_bytes = v;
    }
    if let Some(v) = edit.poll_secs {
        c.poll_secs = v;
    }
    c.validate()?;
    if c.enabled && c.spaces.is_empty() {
        return Err("--enabled needs at least one space (--spaces)".into());
    }
    if c.enabled && c.repos.is_empty() {
        return Err("--enabled needs at least one repository (--repo <name>=<remote>)".into());
    }
    let changed = before.as_ref() != Some(&c);
    Ok((c, changed))
}

/// Reads `<dir>/review.json`.
pub fn load(dir: &Path) -> Result<Option<ServiceConfig>, String> {
    let path = dir.join(config::FILE);
    match std::fs::read(&path) {
        Ok(b) => serde_json::from_slice(&b)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// `KEY=value` lines of an environment file as a map; other lines (comments) are dropped.
fn parse_env(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| !k.trim().is_empty() && !k.trim_start().starts_with('#'))
        .map(|(k, v)| (k.trim().to_owned(), v.to_owned()))
        .collect()
}

/// Reads the key from `path`: refused when others can read it (Unix) or it holds no single line.
fn read_key(path: &Path) -> Res<String> {
    let meta =
        std::fs::metadata(path).map_err(|e| format!("--key-file {}: {e}", path.display()))?;
    if !meta.is_file() || meta.len() > 4096 {
        return Err(format!("--key-file {} is not a small file", path.display()).into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(format!(
                "--key-file {} is readable by group or others (mode {mode:03o}): `chmod 600 {}` and run again",
                path.display(),
                path.display()
            )
            .into());
        }
    }
    let text = std::fs::read_to_string(path)
        .map_err(|_| format!("--key-file {} is not text", path.display()))?;
    let key = text.trim();
    if key.is_empty() || key.contains(['\n', '\r']) {
        return Err(format!(
            "--key-file {} must hold the key on one line",
            path.display()
        )
        .into());
    }
    Ok(key.to_owned())
}

/// `han review set`.
fn set(a: &SetArgs) -> Res<String> {
    let edit = a.edit()?;
    let env_touched = a.home.is_some() || a.relay.is_some() || a.key_file.is_some();
    if edit.is_empty() && !env_touched {
        return Err("nothing to set: give --actor, --model, --spaces, --repo, --enabled or another option (see `han review set --help`)".into());
    }
    let existing = load(&a.dir)?;
    let (cfg, changed) = if edit.is_empty() {
        (
            existing.clone().ok_or_else(|| {
                format!(
                    "{} does not exist: make it first with `han review set --dir {} --actor … --model … --max-tokens … --base-url … --key-env …`",
                    a.dir.join(config::FILE).display(),
                    a.dir.display()
                )
            })?,
            false,
        )
    } else {
        apply(existing, &edit)?
    };
    let mut lines = Vec::new();
    std::fs::create_dir_all(&a.dir)?;
    let path = a.dir.join(config::FILE);
    if changed {
        std::fs::write(&path, serde_json::to_string_pretty(&cfg)? + "\n")?;
    }
    lines.push(format!(
        "{}: {} in {}",
        if changed { "changed" } else { "unchanged" },
        cfg.actor,
        path.display()
    ));
    if env_touched {
        let env_path = a.dir.join(ENV_FILE);
        let mut env = std::fs::read_to_string(&env_path)
            .map(|t| parse_env(&t))
            .unwrap_or_default();
        let before = env.clone();
        if let Some(h) = &a.home {
            env.insert("HAN_HOME".into(), h.to_string());
        }
        if let Some(r) = &a.relay {
            env.insert("HAN_RELAY".into(), r.clone());
        }
        if let Some(k) = &a.key_file {
            env.insert(cfg.route.key_env.clone(), read_key(k)?);
        }
        let text: String = env.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
        let wrote = env != before || !crate::executor::mode_600(&env_path);
        if wrote {
            crate::executor::install_key(&env_path, text.as_bytes())?;
        }
        lines.push(format!(
            "{}: {} (mode 600; the model key {})",
            if env != before {
                "changed"
            } else {
                "unchanged"
            },
            env_path.display(),
            if env.contains_key(&cfg.route.key_env) {
                format!("is set as {}", cfg.route.key_env)
            } else {
                format!("is not set yet: give --key-file, for {}", cfg.route.key_env)
            }
        ));
    }
    lines.push(format!(
        "  {}, approve {}, independence {:?}, model {} (max_tokens {}), route {} {}",
        if cfg.enabled { "enabled" } else { "disabled" },
        if cfg.approve {
            "on"
        } else {
            "off (records and messages only)"
        },
        cfg.independence,
        cfg.model,
        cfg.max_tokens,
        cfg.route.name,
        cfg.route.base,
    ));
    lines.push(format!(
        "  spaces [{}], repos [{}]",
        cfg.spaces.join(", "),
        cfg.repos.keys().cloned().collect::<Vec<_>>().join(", ")
    ));
    Ok(lines.join("\n"))
}

fn now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    han_seq::time::format_time(secs)
}

async fn run_service(a: RunArgs) -> Res<()> {
    let Some(cfg) = load(&a.dir)? else {
        return Err(format!(
            "{} does not exist: the reviewer is off until it is set up (`han review set --dir {} …`)",
            a.dir.join(config::FILE).display(),
            a.dir.display()
        )
        .into());
    };
    if !cfg.enabled {
        println!(
            "review is disabled ({} says enabled: false); nothing started",
            a.dir.join(config::FILE).display()
        );
        return Ok(());
    }
    cfg.validate()?;
    // the key is read from the environment variable the config names; it is never printed
    let backend = OpenAiCompat::new(
        &cfg.route.name,
        &cfg.route.base,
        &cfg.route.path,
        cfg.route.key_header.into(),
        &cfg.route.key_env,
        Duration::from_secs(cfg.route.timeout_secs),
    )
    .map_err(|e| e.to_string())?;
    let keys = a.keys.clone().unwrap_or_else(|| a.dir.clone());
    let key_name = a
        .key_name
        .clone()
        .unwrap_or_else(|| rolenode::key_name(&cfg.actor));
    let first = cfg
        .spaces
        .first()
        .ok_or("review.json names no space (`han review set --spaces …`)")?
        .clone();
    let role = rolenode::start(
        &cfg.actor,
        &a.key_id,
        &key_name,
        &keys,
        &a.dir.join("node"),
        &a.relay,
        &a.home,
        &first,
        ConnectionClass::Executor,
    )
    .await?;
    for space in cfg.spaces.iter().skip(1) {
        match role
            .node
            .call(Command::SpaceFollow {
                home: Some(a.home.clone()),
                space: space.clone(),
            })
            .await
        {
            Ok(_) | Err(NodeError::Conflict { .. }) => {}
            Err(e) => return Err(format!("following {space}: {e}").into()),
        }
    }
    let actor = cfg.actor.clone();
    let service = Service::new(
        cfg,
        role.api.clone(),
        Arc::new(GitVcs::new(a.dir.join("objects"))),
        Arc::new(backend),
        &a.dir.join("state"),
        Arc::new(now),
    )?;
    println!(
        "review: running as {actor} (reviewer version {})",
        service.version()
    );
    let (stop, rx) = tokio::sync::watch::channel(false);
    tokio::select! {
        () = service.run(rx) => {}
        _ = tokio::signal::ctrl_c() => { let _ = stop.send(true); }
    }
    role.node.shutdown();
    role.transport.shutdown().await;
    Ok(())
}

pub async fn run(args: ReviewArgs) -> Res<()> {
    match args.cmd {
        ReviewCmd::Set(a) => {
            println!("{}", set(&a)?);
            Ok(())
        }
        ReviewCmd::Run(a) => run_service(*a).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first() -> Edit {
        Edit {
            actor: Some("@reviewer:han.example".into()),
            model: Some("agy/gemini-flash".into()),
            max_tokens: Some(16384),
            base_url: Some("http://127.0.0.1:20128".into()),
            key_env: Some("HAN_REVIEW_KEY".into()),
            ..Edit::default()
        }
    }

    #[test]
    fn the_first_set_needs_the_route_and_leaves_the_service_off() {
        let (c, changed) = apply(None, &first()).unwrap();
        assert!(changed);
        assert!(!c.enabled && !c.approve);
        assert_eq!(c.max_tokens, 16384);
        assert_eq!(c.route.key_env, "HAN_REVIEW_KEY");
        for missing in ["actor", "model", "base_url", "key_env"] {
            let mut e = first();
            match missing {
                "actor" => e.actor = None,
                "model" => e.model = None,
                "base_url" => e.base_url = None,
                _ => e.key_env = None,
            }
            assert!(apply(None, &e).is_err(), "{missing}");
        }
        let mut e = first();
        e.max_tokens = None;
        assert!(
            apply(None, &e)
                .unwrap_err()
                .to_string()
                .contains("max-tokens")
        );
    }

    #[test]
    fn only_what_is_given_changes_and_a_repeat_is_unchanged() {
        let (c, _) = apply(None, &first()).unwrap();
        let edit = Edit {
            spaces: Some(vec!["s1:han.example".into()]),
            repos: vec![("github:o/r".into(), "https://github.com/o/r.git".into())],
            independence: Some(Independence::DogfoodRelaxed),
            default_author: Some(Some(
                parse_author("claude-code:claude-opus-4-1:@lars:x").unwrap(),
            )),
            enabled: Some(true),
            ..Edit::default()
        };
        let (c2, changed) = apply(Some(c.clone()), &edit).unwrap();
        assert!(changed && c2.enabled);
        assert_eq!(c2.model, c.model);
        assert_eq!(c2.default_author.as_ref().unwrap().operators, ["@lars:x"]);
        let (c3, changed) = apply(Some(c2.clone()), &edit).unwrap();
        assert!(!changed);
        assert_eq!(c3, c2);
        // removing the default author
        let (c4, _) = apply(
            Some(c2),
            &Edit {
                default_author: Some(None),
                ..Edit::default()
            },
        )
        .unwrap();
        assert!(c4.default_author.is_none());
    }

    #[test]
    fn a_configuration_the_service_could_not_use_is_refused_before_anything_is_written() {
        let (c, _) = apply(None, &first()).unwrap();
        // enabled with nothing to watch
        let e = Edit {
            enabled: Some(true),
            ..Edit::default()
        };
        assert!(apply(Some(c.clone()), &e).is_err());
        // a model whose vendor is unknown, a zero ceiling, a key written into the variable name
        for bad in [
            Edit {
                model: Some("mystery-model".into()),
                ..Edit::default()
            },
            Edit {
                max_tokens: Some(0),
                ..Edit::default()
            },
            Edit {
                key_env: Some("sk-live-abcdef".into()),
                ..Edit::default()
            },
            Edit {
                base_url: Some("http://example.com".into()),
                ..Edit::default()
            },
        ] {
            assert!(apply(Some(c.clone()), &bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn authors_and_flags_parse() {
        let (a, s) =
            parse_actor_author("@coder:han.example=pi:claude-opus-4-1:@lars:han.example").unwrap();
        assert_eq!(a, "@coder:han.example");
        assert_eq!(
            (s.harness.as_str(), s.model.as_str()),
            ("pi", "claude-opus-4-1")
        );
        assert_eq!(s.operators, ["@lars:han.example"]);
        assert!(parse_actor_author("coder=pi:x").is_err());
        assert!(parse_author("pi").is_err());
        assert_eq!(
            parse_independence("dogfood-relaxed").unwrap(),
            Independence::DogfoodRelaxed
        );
        assert!(parse_independence("lax").is_err());
    }

    #[test]
    fn set_writes_the_file_and_the_key_only_to_the_env_file_with_mode_600() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("review");
        let keyfile = tmp.path().join("key.txt");
        std::fs::write(&keyfile, "sk-test-secret-value\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&keyfile, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let args = SetArgs {
            dir: dir.clone(),
            actor: Some("@reviewer:han.example".into()),
            model: Some("agy/gemini-flash".into()),
            max_tokens: Some(16384),
            base_url: Some("http://127.0.0.1:20128".into()),
            key_env: Some("HAN_REVIEW_KEY".into()),
            home: Some("han.example".parse().unwrap()),
            key_file: Some(keyfile.clone()),
            ..SetArgs::default()
        };
        let out = set(&args).unwrap();
        assert!(!out.contains("sk-test-secret-value"), "{out}");
        assert!(out.contains("changed:"));
        let json = std::fs::read_to_string(dir.join("review.json")).unwrap();
        assert!(!json.contains("sk-test-secret-value"));
        let env = std::fs::read_to_string(dir.join(ENV_FILE)).unwrap();
        assert!(env.contains("HAN_REVIEW_KEY=sk-test-secret-value"));
        assert!(env.contains("HAN_HOME=han.example"));
        assert!(crate::executor::mode_600(&dir.join(ENV_FILE)));
        // a repeat changes nothing
        let again = set(&args).unwrap();
        assert!(again.lines().all(|l| !l.starts_with("changed:")), "{again}");
        assert!(!again.contains("sk-test-secret-value"));
        // the loaded file is the one written
        assert_eq!(load(&dir).unwrap().unwrap().actor, "@reviewer:han.example");
    }
}
