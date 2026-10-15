//! `han executor add`: sets up an executor (the `han exec` role): its key where `han exec` looks for it, its
//! configuration and environment file, and (where this device may) its profile, the operator's confirmation and
//! its membership of a space. It needs no root: the unit and the OS user are `han runner setup`'s, and the unit
//! stays disabled until an admin enables it.
//!
//! Safe to repeat: each step looks at what is there and the output says what changed.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use han_exec::{ExecConfig, GithubApp, RepoTarget};
use han_node::keys::{FileKeyStore, KeyStore};
use han_node::rpc::{RpcClient, default_endpoint};
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
fn mode_600(path: &Path) -> bool {
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
fn install_key(dest: &Path, pem: &[u8]) -> Res<bool> {
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
    } = args.cmd;
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
                    name: rname.to_owned(),
                    remote: remote.to_owned(),
                    github: slug.clone(),
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
    Ok(())
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
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false);
    if root {
        let ok = std::process::Command::new("chown")
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

    #[test]
    fn github_slug_maps_only_github_names() {
        assert_eq!(github_slug("github:o/r").unwrap().as_deref(), Some("o/r"));
        assert_eq!(github_slug("/srv/repo").unwrap(), None);
        assert!(github_slug("github:o").is_err());
        assert!(github_slug("github:o/r/x").is_err());
        assert!(github_slug("github:/r").is_err());
    }
}
