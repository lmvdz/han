//! `han executor add`: sets up an executor (the `han exec` role): its key where `han exec` looks for it, its
//! configuration and environment file, and (where this device may) its profile, the operator's confirmation and
//! its membership of a space. It needs no root: the unit and the OS user are `han runner setup`'s, and the unit
//! stays disabled until an admin enables it.
//!
//! Safe to repeat: each step looks at what is there and the output says what changed.
//!
//! `han executor set` changes what the landing lane may do on one repository of `exec.json` (the refs it may
//! move, the checks it gates on, its deadlines), so nobody edits the file by hand.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use han_exec::{ExecConfig, GithubApp, RepoTarget};
use han_node::keys::{FileKeyStore, KeyStore};
use han_node::rpc::{RpcClient, default_endpoint};
use han_proc::NoWindow as _;
use han_registry::HomeRef;

use crate::rolenode::{key_dir, key_name};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// Where the executor of an OS user keeps everything (the unit of `han runner setup` reads here).
pub const RUNNER_DIR: &str = "/var/lib/han-exec";

/// Options of `han executor`.
#[derive(Args)]
pub struct ExecutorArgs {
    #[command(subcommand)]
    cmd: ExecutorCmd,
}

#[derive(Subcommand)]
enum ExecutorCmd {
    /// Sets an executor up. Prints what another party (the home's admin, the operator) has to run.
    Add {
        /// The executor's name, without `@` and server (`exec` makes `@exec:<server>`).
        name: String,
        /// Its operator (a person, `@name:server`). The executor's server defaults to the operator's.
        #[arg(long)]
        operator: String,
        /// The executor's server, if it is not the operator's.
        #[arg(long)]
        server: Option<String>,
        /// Linux: the OS user the executor runs as; its files default to `/var/lib/han-exec`.
        #[arg(long)]
        os_user: Option<String>,
        /// The executor's directory: `keys/`, `state/`, `exec.json`, `exec.env`.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// A repository the executor may push to: `<name>=<remote>` (the name is what proposals call it; for
        /// a local repository it is the repository's path). Repeatable.
        #[arg(long)]
        repo: Vec<String>,
        /// The space the executor serves; it is made a member of it.
        #[arg(long)]
        space: Option<String>,
        /// The data directory of the home, when this machine runs it.
        #[arg(long)]
        home_data: Option<PathBuf>,
        /// The data directory of the operator's running `han node`, if this device is the operator's.
        #[arg(long, env = "HAN_DATA", default_value = "han-data")]
        data: PathBuf,
        /// Relays for the executor: `n0`, `none` or URLs.
        #[arg(long, default_value = "n0")]
        relay: String,
        /// The executor's home: a home ticket (`han1home.…`) or a ServerName. Default: the executor's server.
        #[arg(long)]
        home: Option<HomeRef>,
        /// The write GitHub App's id (not the read App `han-github` uses). Needs `--github-installation` and
        /// `--github-key`; they write the `github` block of `exec.json`.
        #[arg(long, requires_all = ["github_installation", "github_key"])]
        github_app: Option<u64>,
        /// The write App's installation id.
        #[arg(long, requires_all = ["github_app", "github_key"])]
        github_installation: Option<u64>,
        /// The write App's private key (a PEM file readable only by you). It is copied to
        /// `<dir>/github-app.pem` with mode 600 and never printed.
        #[arg(long, requires_all = ["github_app", "github_installation"])]
        github_key: Option<PathBuf>,
        /// The id of the read App `han-github` uses; `--github-app` is refused when it is the same.
        #[arg(long, env = "HAN_GITHUB_READ_APP")]
        read_app: Option<u64>,
    },
    /// Sets how the landing lane treats one repository in the executor's `exec.json`; only what is given
    /// changes. Needs no running node. Prints `changed:` or `unchanged:`.
    Set {
        /// The repository's name as plans call it, `github:owner/repo` (the `<name>` of `add --repo`).
        repo: String,
        /// The executor's directory (holds `exec.json`). Default: `/var/lib/han-exec` where `exec.json` is
        /// there (Linux), else `han-exec`.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// The remote, to add the repository when `exec.json` does not have it yet.
        #[arg(long)]
        remote: Option<String>,
        /// The full refs `vcs.land` may move here, comma separated (`refs/heads/main`); `""` for none, which
        /// turns the landing lane off for the repository.
        #[arg(long)]
        landing_refs: Option<String>,
        /// The checks that must be green on the commit before it lands (`fast`), comma separated.
        #[arg(long)]
        required: Option<String>,
        /// The checks that must be green on the landed commit before it is `confirmed` (`full`), comma
        /// separated; `""` for none (then `landed` counts as `confirmed`).
        #[arg(long)]
        full: Option<String>,
        /// Seconds between two reads of the checks backend.
        #[arg(long)]
        poll_secs: Option<u64>,
        /// Seconds a required check may be missing before the change is reported stalled.
        #[arg(long)]
        appear_secs: Option<u64>,
        /// How many times a red required check is asked to run again before the change is rejected.
        #[arg(long)]
        rerun: Option<u32>,
        /// Whether the lane reverts the change that breaks the post-merge run (`true`/`false`; default off).
        #[arg(long)]
        auto_revert: Option<bool>,
        /// Deadline of `queued`, in seconds.
        #[arg(long)]
        queued_secs: Option<u64>,
        /// Deadline of `testing`, in seconds.
        #[arg(long)]
        testing_secs: Option<u64>,
        /// Deadline of `landing`, in seconds (default: the claim's launch window).
        #[arg(long)]
        landing_secs: Option<u64>,
        /// Deadline of `landed`, in seconds.
        #[arg(long)]
        landed_secs: Option<u64>,
        /// How long a stalled change in `testing` waits before it is rejected, in seconds.
        #[arg(long)]
        stall_secs: Option<u64>,
        /// The branch prefix the lane pushes the commits it tests to (default `han/lane/`).
        #[arg(long)]
        lane_branch_prefix: Option<String>,
    },
}

/// What `han executor set` changes on a repository: what is `Some` is set, the rest stays.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LandingEdit {
    pub remote: Option<String>,
    pub landing_refs: Option<Vec<String>>,
    pub required: Option<Vec<String>>,
    pub full: Option<Vec<String>>,
    pub poll_secs: Option<u64>,
    pub appear_secs: Option<u64>,
    pub rerun: Option<u32>,
    pub auto_revert: Option<bool>,
    pub queued_secs: Option<u64>,
    pub testing_secs: Option<u64>,
    pub landing_secs: Option<u64>,
    pub landed_secs: Option<u64>,
    pub stall_secs: Option<u64>,
    pub lane_branch_prefix: Option<String>,
}

/// `a,b` as names; the empty string is no names.
fn split_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A ref the lane may move: a full branch ref.
fn valid_landing_ref(r: &str) -> bool {
    r.strip_prefix("refs/heads/")
        .is_some_and(|b| !b.is_empty() && !b.ends_with('/') && !b.contains(".."))
}

/// Applies `edit` to the repository `name` of `cfg`. Refuses a value the lane could not use before changing
/// anything. Says whether `cfg` changed.
fn apply_landing(cfg: &mut ExecConfig, name: &str, edit: &LandingEdit) -> Res<bool> {
    for r in edit.landing_refs.iter().flatten() {
        if !valid_landing_ref(r) {
            return Err(format!(
                "--landing-refs {r:?} is not a full branch ref (for example refs/heads/main)"
            )
            .into());
        }
    }
    for (flag, list) in [("--required", &edit.required), ("--full", &edit.full)] {
        if list
            .iter()
            .flatten()
            .any(|c| c.contains(char::is_whitespace))
        {
            return Err(
                format!("{flag}: check names are comma separated and hold no spaces").into(),
            );
        }
    }
    if edit
        .lane_branch_prefix
        .as_deref()
        .is_some_and(str::is_empty)
    {
        return Err("--lane-branch-prefix is empty".into());
    }
    let before = cfg.repos.clone();
    let idx = match cfg.repos.iter().position(|r| r.name == name) {
        Some(i) => i,
        None => {
            let Some(remote) = &edit.remote else {
                return Err(format!(
                    "exec.json has no repository {name:?}: add it with `han executor add <name> --operator <operator> --repo {name}=<remote>`, or give --remote"
                )
                .into());
            };
            cfg.repos.push(RepoTarget {
                github: github_slug(name)?,
                ..RepoTarget::new(name, remote)
            });
            cfg.repos.len() - 1
        }
    };
    let r = &mut cfg.repos[idx];
    if let Some(remote) = &edit.remote {
        r.remote.clone_from(remote);
    }
    if let Some(v) = &edit.landing_refs {
        r.landing_refs.clone_from(v);
    }
    if let Some(v) = &edit.required {
        r.checks.required.clone_from(v);
    }
    if let Some(v) = &edit.full {
        r.checks.full.clone_from(v);
    }
    if let Some(v) = edit.poll_secs {
        r.checks.poll_secs = v;
    }
    if let Some(v) = edit.appear_secs {
        r.checks.appear_secs = v;
    }
    if let Some(v) = edit.rerun {
        r.checks.rerun = v;
    }
    if let Some(v) = edit.auto_revert {
        r.checks.auto_revert = v;
    }
    if let Some(v) = edit.queued_secs {
        r.deadlines.queued_secs = v;
    }
    if let Some(v) = edit.testing_secs {
        r.deadlines.testing_secs = v;
    }
    if let Some(v) = edit.landing_secs {
        r.deadlines.landing_secs = Some(v);
    }
    if let Some(v) = edit.landed_secs {
        r.deadlines.landed_secs = v;
    }
    if let Some(v) = edit.stall_secs {
        r.deadlines.stall_secs = v;
    }
    if let Some(v) = &edit.lane_branch_prefix {
        r.lane_branch_prefix.clone_from(v);
    }
    Ok(cfg.repos != before)
}

/// Where `han executor set` looks for `exec.json` when it is not told.
fn default_dir() -> PathBuf {
    let runner = PathBuf::from(RUNNER_DIR);
    if cfg!(target_os = "linux") && runner.join("exec.json").exists() {
        runner
    } else {
        PathBuf::from("han-exec")
    }
}

/// `han executor set`: one repository's landing settings in `<dir>/exec.json`.
fn set(dir: &Path, name: &str, edit: &LandingEdit) -> Res<String> {
    if *edit == LandingEdit::default() {
        return Err("nothing to set: give --landing-refs, --required, --full, a deadline or another option (see `han executor set --help`)".into());
    }
    let path = dir.join("exec.json");
    let mut cfg: ExecConfig = match std::fs::read(&path) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && edit.remote.is_some() => {
            serde_json::from_str(r#"{"repos": [], "branch_prefixes": ["han/"]}"#)?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "{} does not exist: set the executor up first with `han executor add <name> --operator <operator> --dir {} --repo {name}=<remote>`",
                path.display(),
                dir.display()
            )
            .into());
        }
        Err(e) => return Err(format!("{}: {e}", path.display()).into()),
    };
    let changed = apply_landing(&mut cfg, name, edit)?;
    if changed {
        write_text(&path, &serde_json::to_string_pretty(&cfg)?)?;
    }
    let r = cfg.repo(name).ok_or("the repository vanished")?;
    Ok(format!(
        "{}: {name} in {}\n  landing refs [{}], required [{}], full [{}]",
        if changed { "changed" } else { "unchanged" },
        path.display(),
        r.landing_refs.join(", "),
        r.checks.required.join(", "),
        r.checks.full.join(", "),
    ))
}

/// The key's name in the executor's directory.
pub const GITHUB_KEY_FILE: &str = "github-app.pem";

/// The first lines a PEM private key may start with.
const PEM_HEADERS: [&str; 3] = [
    "-----BEGIN PRIVATE KEY-----",
    "-----BEGIN RSA PRIVATE KEY-----",
    "-----BEGIN EC PRIVATE KEY-----",
];

/// Reads the App key at `path` for installing. Refuses a key that others can read (Unix) or whose first line is
/// not a PEM private key header. Errors say which check failed and never carry the file's contents.
fn read_pem(path: &Path) -> Res<Vec<u8>> {
    let meta =
        std::fs::metadata(path).map_err(|e| format!("--github-key {}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("--github-key {} is not a file", path.display()).into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(format!(
                "--github-key {} is readable by group or others (mode {mode:03o}): `chmod 600 {}` and run again",
                path.display(),
                path.display()
            )
            .into());
        }
    }
    if meta.len() > 16 * 1024 {
        return Err(format!("--github-key {} is too large for a PEM key", path.display()).into());
    }
    let bytes = std::fs::read(path)?;
    let first = bytes
        .split(|b| *b == b'\n')
        .next()
        .map(|l| String::from_utf8_lossy(l).trim().to_owned())
        .unwrap_or_default();
    if !PEM_HEADERS.contains(&first.as_str()) {
        return Err(format!(
            "--github-key {} does not start with a PEM private key header (`-----BEGIN PRIVATE KEY-----`)",
            path.display()
        )
        .into());
    }
    Ok(bytes)
}

/// Whether `path` has exactly mode 600 (always true where there are no modes).
pub(crate) fn mode_600(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o777 == 0o600)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        true
    }
}

/// Puts `pem` at `dest` with mode 600, created that way (never readable by others, not even briefly).
/// Returns whether anything changed.
pub(crate) fn install_key(dest: &Path, pem: &[u8]) -> Res<bool> {
    if std::fs::read(dest).ok().as_deref() == Some(pem) && mode_600(dest) {
        return Ok(false);
    }
    let tmp = dest.with_extension("pem.tmp");
    let _ = std::fs::remove_file(&tmp);
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(pem)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, dest)?;
    Ok(true)
}

/// `github:<owner>/<repo>` as `<owner>/<repo>`; `None` for any other repository name.
fn github_slug(name: &str) -> Res<Option<String>> {
    let Some(slug) = name.strip_prefix("github:") else {
        return Ok(None);
    };
    match slug.split_once('/') {
        Some((o, r)) if !o.is_empty() && !r.is_empty() && !r.contains('/') => {
            Ok(Some(slug.to_owned()))
        }
        _ => Err(format!("--repo {name}: a GitHub repository is github:<owner>/<repo>").into()),
    }
}

pub async fn run(args: ExecutorArgs) -> Res<()> {
    provision(args).await.map(|_| ())
}

/// Runs the existing provisioning steps and returns unresolved enrollment work to the launcher.
pub(crate) async fn provision(args: ExecutorArgs) -> Res<crate::enroll::Outcome> {
    let args = match args.cmd {
        ExecutorCmd::Set {
            repo,
            dir,
            remote,
            landing_refs,
            required,
            full,
            poll_secs,
            appear_secs,
            rerun,
            auto_revert,
            queued_secs,
            testing_secs,
            landing_secs,
            landed_secs,
            stall_secs,
            lane_branch_prefix,
        } => {
            let edit = LandingEdit {
                remote,
                landing_refs: landing_refs.as_deref().map(split_list),
                required: required.as_deref().map(split_list),
                full: full.as_deref().map(split_list),
                poll_secs,
                appear_secs,
                rerun,
                auto_revert,
                queued_secs,
                testing_secs,
                landing_secs,
                landed_secs,
                stall_secs,
                lane_branch_prefix,
            };
            let dir = dir.unwrap_or_else(default_dir);
            println!("{}", set(&dir, &repo, &edit)?);
            return Ok(crate::enroll::Outcome::default());
        }
        cmd => ExecutorArgs { cmd },
    };
    let ExecutorCmd::Add {
        name,
        operator,
        server,
        os_user,
        dir,
        repo,
        space,
        home_data,
        data,
        relay,
        home,
        github_app,
        github_installation,
        github_key,
        read_app,
    } = args.cmd
    else {
        return Err("expected executor add options".into());
    };
    if github_app.is_some() && github_app == read_app {
        return Err(format!(
            "--github-app {} is the read App (the one `han-github` uses). The executor needs the separate write App:              the read App must never hold write permissions, and its key must stay with `han-github` (docs/runner.md §10)",
            github_app.unwrap_or_default()
        )
        .into());
    }
    // everything that can be refused is, before anything is written
    let pem = github_key.as_deref().map(read_pem).transpose()?;
    let mut slugs = Vec::new();
    for spec in &repo {
        let (rname, _) = spec.split_once('=').ok_or("--repo is <name>=<remote>")?;
        slugs.push(github_slug(rname)?);
    }
    let server = match (&server, operator.rsplit_once(':')) {
        (Some(s), _) => s.clone(),
        (None, Some((_, s))) => s.to_owned(),
        (None, None) => return Err("--operator is @name:server".into()),
    };
    let actor = format!("@{name}:{server}");
    let dir = dir.unwrap_or_else(|| {
        if os_user.is_some() {
            PathBuf::from(RUNNER_DIR)
        } else {
            PathBuf::from("han-exec")
        }
    });
    println!(
        "executor {actor} (operator {operator}) in {}",
        dir.display()
    );

    // 1. the key, where `han exec --keys <dir>` looks for it
    let store = FileKeyStore::new(key_dir(&dir));
    let kname = key_name(&actor);
    let key_file = key_dir(&dir).join(format!("{kname}.key"));
    let existed = store.load(&kname)?.is_some();
    let seed = store.load_or_create(&kname)?;
    let public_key = han_core::SigningKey::from_seed(seed)
        .public_key()
        .to_base64url();
    if existed {
        println!("  unchanged: key {}", key_file.display());
    } else {
        println!("  changed:   made key {}", key_file.display());
    }
    println!("  key k1 {public_key}");
    std::fs::create_dir_all(dir.join("state"))?;

    // 2. exec.json
    let cfg_path = dir.join("exec.json");
    let mut cfg_changed = !cfg_path.exists();
    let mut cfg: ExecConfig = match std::fs::read(&cfg_path) {
        Ok(b) => serde_json::from_slice(&b)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            serde_json::from_str(r#"{"repos": [], "branch_prefixes": ["han/"]}"#)?
        }
        Err(e) => return Err(format!("{}: {e}", cfg_path.display()).into()),
    };
    for (spec, slug) in repo.iter().zip(&slugs) {
        let (rname, remote) = spec.split_once('=').ok_or("--repo is <name>=<remote>")?;
        match cfg.repos.iter_mut().find(|r| r.name == rname) {
            Some(r) => {
                if r.remote != remote {
                    r.remote = remote.to_owned();
                    cfg_changed = true;
                }
                if slug.is_some() && r.github != *slug {
                    r.github.clone_from(slug);
                    cfg_changed = true;
                }
            }
            None => {
                cfg.repos.push(RepoTarget {
                    github: slug.clone(),
                    ..RepoTarget::new(rname, remote)
                });
                cfg_changed = true;
            }
        }
    }
    if let (Some(pem), Some(app_id), Some(installation_id)) =
        (&pem, github_app, github_installation)
    {
        let dest = dir.join(GITHUB_KEY_FILE);
        std::fs::create_dir_all(&dir)?;
        if install_key(&dest, pem)? {
            println!("  changed:   github app key {}", dest.display());
        } else {
            println!("  unchanged: github app key {}", dest.display());
        }
        let private_key = std::path::absolute(&dest)?.to_string_lossy().into_owned();
        let api_base = cfg.github.as_ref().and_then(|g| g.api_base.clone());
        let block = GithubApp {
            app_id,
            installation_id,
            api_base,
            private_key,
        };
        if cfg.github.as_ref() != Some(&block) {
            cfg.github = Some(block);
            cfg_changed = true;
        }
    }
    if cfg_changed {
        write_text(&cfg_path, &serde_json::to_string_pretty(&cfg)?)?;
        println!("  changed:   {}", cfg_path.display());
    } else {
        println!("  unchanged: {}", cfg_path.display());
    }

    // 3. exec.env (what the unit reads)
    let env_path = dir.join("exec.env");
    let home_text = home.as_ref().map(ToString::to_string).unwrap_or_default();
    let env = format!(
        "HAN_SERVER={server}\nHAN_SPACE={}\nHAN_RELAY={relay}\nHAN_HOME={home_text}\n",
        space.clone().unwrap_or_default()
    );
    if std::fs::read_to_string(&env_path).ok().as_deref() == Some(env.as_str()) {
        println!("  unchanged: {}", env_path.display());
    } else if space.is_none() && home_text.is_empty() && env_path.exists() {
        println!(
            "  unchanged: {} (no --space or --home given)",
            env_path.display()
        );
    } else {
        write_text(&env_path, &env)?;
        println!("  changed:   {}", env_path.display());
    }

    // 4. ownership (root only; otherwise the files are the caller's and the note says so)
    if let Some(user) = &os_user {
        chown(&dir, user);
    }

    // 5. the unit, for an executor that runs as an OS user
    if os_user.is_some() {
        unit_state();
    }

    // 6. profile, confirmation, membership
    let client = RpcClient::connect(&default_endpoint(&data)).await.ok();
    if client.is_none() {
        println!(
            "  (no node running with --data {}: the steps that need the operator's node are listed below)",
            data.display()
        );
    }
    let outcome = crate::enroll::run(&crate::enroll::Enroll {
        actor: &actor,
        kind: "service",
        key_id: "k1",
        public_key: &public_key,
        operator: &operator,
        home: home.as_ref(),
        home_data: home_data.as_deref(),
        space: space.as_deref(),
        client: client.as_ref(),
    })
    .await?;
    println!(
        "by hand: han exec --actor {actor} --space {} --home <ticket> --keys {} --data {} --config {}",
        space.as_deref().unwrap_or("<space>"),
        dir.display(),
        dir.join("state").display(),
        cfg_path.display()
    );
    if outcome.todo.is_empty() {
        println!("ready: {actor}");
    } else {
        println!(
            "not ready: {} step(s) wait for somebody else; run this command again afterwards",
            outcome.todo.len()
        );
    }
    Ok(outcome)
}

fn write_text(path: &Path, text: &str) -> Res<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(path, text)?;
    Ok(())
}

/// Hands the executor's directory to its OS user, when that is allowed (root).
fn chown(dir: &Path, user: &str) {
    if !cfg!(unix) {
        return;
    }
    let root = std::process::Command::new("id")
        .no_window()
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false);
    if root {
        let ok = std::process::Command::new("chown")
            .no_window()
            .args(["-R", &format!("{user}:{user}")])
            .arg(dir)
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            println!("  changed:   {} is owned by {user}", dir.display());
        } else {
            println!("  to do:     could not chown {} to {user}", dir.display());
        }
    } else {
        println!(
            "  note:      {} stays owned by you; `sudo chown -R {user}: {}` hands it to {user}",
            dir.display(),
            dir.display()
        );
    }
}

/// Says where the unit stands. Never installs or enables it: that is `han runner setup` and an admin.
fn unit_state() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let unit = Path::new("/etc/systemd/system/han-exec.service");
    if !unit.exists() {
        println!(
            "  to do:     the unit is not installed: an admin runs `sudo han runner setup` (installs it, disabled)"
        );
        return;
    }
    let enabled = std::process::Command::new("systemctl")
        .no_window()
        .args(["is-enabled", "han-exec"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default();
    println!(
        "  unchanged: han-exec.service is installed ({}); an admin enables it with `sudo systemctl enable --now han-exec`",
        if enabled.is_empty() {
            "state unknown"
        } else {
            &enabled
        }
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEM: &str = "-----BEGIN PRIVATE KEY-----\nTESTONLYNOTAKEY\n-----END PRIVATE KEY-----\n";

    fn key_file(dir: &Path, name: &str, text: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        p
    }

    #[test]
    fn read_pem_takes_a_private_key_and_refuses_anything_else() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            read_pem(&key_file(d.path(), "ok.pem", PEM)).unwrap(),
            PEM.as_bytes()
        );
        let bad = key_file(
            d.path(),
            "bad.pem",
            "SECRETCONTENT\n-----BEGIN PRIVATE KEY-----\n",
        );
        let e = read_pem(&bad).unwrap_err().to_string();
        assert!(e.contains("PEM private key header"), "{e}");
        assert!(!e.contains("SECRETCONTENT"));
        let public = key_file(d.path(), "pub.pem", "-----BEGIN PUBLIC KEY-----\nx\n");
        assert!(read_pem(&public).is_err());
        assert!(read_pem(&d.path().join("none.pem")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn read_pem_refuses_a_key_others_can_read() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = key_file(d.path(), "k.pem", PEM);
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o640)).unwrap();
        let e = read_pem(&p).unwrap_err().to_string();
        assert!(e.contains("readable by group or others"), "{e}");
        assert!(!e.contains("TESTONLYNOTAKEY"));
    }

    #[test]
    fn install_key_is_mode_600_and_idempotent() {
        let d = tempfile::tempdir().unwrap();
        let dest = d.path().join(GITHUB_KEY_FILE);
        assert!(install_key(&dest, PEM.as_bytes()).unwrap());
        assert!(mode_600(&dest));
        assert_eq!(std::fs::read(&dest).unwrap(), PEM.as_bytes());
        assert!(!install_key(&dest, PEM.as_bytes()).unwrap());
        assert!(install_key(&dest, b"other").unwrap());
        assert!(!d.path().join("github-app.pem.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(
                install_key(&dest, b"other").unwrap(),
                "a loose mode is repaired"
            );
            assert!(mode_600(&dest));
        }
    }

    fn exec_dir(repo: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let cfg = ExecConfig {
            repos: vec![RepoTarget::new(repo, "https://github.com/o/r.git")],
            ..serde_json::from_str(r#"{"repos": []}"#).unwrap()
        };
        std::fs::write(
            d.path().join("exec.json"),
            serde_json::to_string(&cfg).unwrap(),
        )
        .unwrap();
        d
    }

    #[test]
    fn set_gives_a_repository_its_landing_config_and_repeats_as_unchanged() {
        let d = exec_dir("github:o/r");
        let edit = LandingEdit {
            landing_refs: Some(split_list("refs/heads/main")),
            required: Some(split_list("quick,lint")),
            full: Some(split_list("full")),
            landed_secs: Some(7200),
            stall_secs: Some(600),
            ..LandingEdit::default()
        };
        let first = set(d.path(), "github:o/r", &edit).unwrap();
        assert!(first.starts_with("changed: github:o/r"), "{first}");
        assert!(first.contains("refs/heads/main"), "{first}");
        let again = set(d.path(), "github:o/r", &edit).unwrap();
        assert!(again.starts_with("unchanged: "), "{again}");
        let cfg: ExecConfig =
            serde_json::from_slice(&std::fs::read(d.path().join("exec.json")).unwrap()).unwrap();
        let r = cfg.repo("github:o/r").unwrap();
        assert_eq!(r.landing_refs, ["refs/heads/main"]);
        assert_eq!(r.checks.required, ["quick", "lint"]);
        assert_eq!(r.checks.full, ["full"]);
        assert_eq!(
            (r.deadlines.landed_secs, r.deadlines.stall_secs),
            (7200, 600)
        );
        // what is not given stays; `""` empties a list
        let off = LandingEdit {
            landing_refs: Some(split_list("")),
            ..LandingEdit::default()
        };
        assert!(
            set(d.path(), "github:o/r", &off)
                .unwrap()
                .starts_with("changed: ")
        );
        let cfg: ExecConfig =
            serde_json::from_slice(&std::fs::read(d.path().join("exec.json")).unwrap()).unwrap();
        let r = cfg.repo("github:o/r").unwrap();
        assert!(r.landing_refs.is_empty());
        assert_eq!(r.checks.full, ["full"]);
    }

    #[test]
    fn set_refuses_what_the_lane_could_not_use_and_writes_nothing() {
        let d = exec_dir("github:o/r");
        let before = std::fs::read(d.path().join("exec.json")).unwrap();
        let bad = |edit: LandingEdit| set(d.path(), "github:o/r", &edit).unwrap_err().to_string();
        let e = bad(LandingEdit {
            landing_refs: Some(split_list("main")),
            ..LandingEdit::default()
        });
        assert!(e.contains("full branch ref"), "{e}");
        assert!(
            bad(LandingEdit {
                required: Some(vec!["a b".into()]),
                ..LandingEdit::default()
            })
            .contains("--required")
        );
        assert!(bad(LandingEdit::default()).contains("nothing to set"));
        // an unknown repository is named, with the command that adds it
        let e = set(
            d.path(),
            "github:o/other",
            &LandingEdit {
                rerun: Some(1),
                ..LandingEdit::default()
            },
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("han executor add"), "{e}");
        // no exec.json at all
        let empty = tempfile::tempdir().unwrap();
        let e = set(
            empty.path(),
            "github:o/r",
            &LandingEdit {
                rerun: Some(1),
                ..LandingEdit::default()
            },
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("does not exist"), "{e}");
        assert_eq!(std::fs::read(d.path().join("exec.json")).unwrap(), before);
    }

    #[test]
    fn set_adds_a_repository_given_its_remote() {
        let empty = tempfile::tempdir().unwrap();
        let edit = LandingEdit {
            remote: Some("https://github.com/o/r.git".into()),
            landing_refs: Some(split_list("refs/heads/main")),
            ..LandingEdit::default()
        };
        assert!(
            set(empty.path(), "github:o/r", &edit)
                .unwrap()
                .starts_with("changed: ")
        );
        let cfg: ExecConfig =
            serde_json::from_slice(&std::fs::read(empty.path().join("exec.json")).unwrap())
                .unwrap();
        let r = cfg.repo("github:o/r").unwrap();
        assert_eq!(r.github.as_deref(), Some("o/r"));
        assert_eq!(r.landing_refs, ["refs/heads/main"]);
        assert_eq!(cfg.branch_prefixes, ["han/"]);
    }

    #[test]
    fn github_slug_maps_only_github_names() {
        assert_eq!(github_slug("github:o/r").unwrap().as_deref(), Some("o/r"));
        assert_eq!(github_slug("/srv/repo").unwrap(), None);
        assert!(github_slug("github:o").is_err());
        assert!(github_slug("github:o/r/x").is_err());
        assert!(github_slug("github:/r").is_err());
    }
}
