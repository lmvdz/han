//! `han agent setup-harness <name-or-driver>`: managed installation of an ACP adapter (PLAN section 6.4).
//!
//! It installs the adapter version the driver pins (M4, SR-76) with npm into the han data directory, using a
//! Node 22 that is already there or a pinned Node build it downloads (and checks against an embedded
//! SHA-256), and records the paths in `<data>/harness/<name>.json`. `han agent start` then needs no flags.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use han_host::driver::{DriverConfig, builtin, fill};
use han_proc::NoWindow as _;
use han_registry::{AgentEdit, AgentsFile, HarnessInstall, Registry};
use serde_json::Value;
use sha2::{Digest, Sha256};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// Resolves an installed adapter's driver, a bundled driver, or a driver file. All CLI consumers use the
/// same definition as the host; adding an adapter needs no login or diagnostic branches.
pub fn driver(data: &Path, harness: &str) -> Res<DriverConfig> {
    let name = if harness == "claude-code" {
        "claude"
    } else {
        harness
    };
    if han_registry::agents::valid_harness_name(name)
        && let Some(install) = HarnessInstall::load(data, name)?
        && let Some(path) = install.driver
    {
        return Ok(DriverConfig::load(&path)?);
    }
    if let Some(driver) = builtin(name) {
        return Ok(driver);
    }
    Ok(DriverConfig::load(Path::new(harness))?)
}

/// The pinned Node build (an LTS 22 release) and the SHA-256 of each archive, from nodejs.org's
/// `SHASUMS256.txt`.
pub const NODE_VERSION: &str = "22.23.3";

/// `(os, arch, archive name suffix, sha256)`.
const NODE_ARCHIVES: &[(&str, &str, &str, &str)] = &[
    (
        "windows",
        "x86_64",
        "win-x64.zip",
        "2b0ff57b049cda1bbcea2240eec20467018713c1efe1f7360c2681859b90ed71",
    ),
    (
        "windows",
        "aarch64",
        "win-arm64.zip",
        "33dad22e4cef5ee8f9fbb1b0d037fdacd0e56d12a4580f0d63f68b894deab535",
    ),
    (
        "linux",
        "x86_64",
        "linux-x64.tar.xz",
        "df450af89261115ef9f9e3830c3eeb2cc9213b63c720b1af623cb5dcbe2e02de",
    ),
    (
        "linux",
        "aarch64",
        "linux-arm64.tar.xz",
        "a44aeb94849a299b22df10b9e622ec2f605c2183501bc40590705131de7c740f",
    ),
    (
        "macos",
        "aarch64",
        "darwin-arm64.tar.gz",
        "23b25245dcfb9af7262f8ff142e9e2e0af025368117329e7a7458a51e5922f53",
    ),
    (
        "macos",
        "x86_64",
        "darwin-x64.tar.gz",
        "8a677b0219178efd6eb0e475457c4afb452b521a92f6e67845a73bd85727f2a8",
    ),
];

/// The major version in `node --version` output (`v22.23.3`).
pub fn parse_node_major(out: &str) -> Option<u32> {
    out.trim()
        .strip_prefix('v')?
        .split('.')
        .next()?
        .parse()
        .ok()
}

fn node_major(node: &Path) -> Option<u32> {
    let out = Command::new(node)
        .no_window()
        .arg("--version")
        .output()
        .ok()?;
    parse_node_major(&String::from_utf8_lossy(&out.stdout))
}

/// The `integrity` npm recorded for `package` in a `package-lock.json`.
pub fn lock_integrity(lock: &str, package: &str) -> Option<String> {
    let v: Value = serde_json::from_str(lock).ok()?;
    v["packages"][format!("node_modules/{package}")]["integrity"]
        .as_str()
        .map(str::to_owned)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Checks `bytes` against a SHA-256 in hex.
pub fn sha256_matches(bytes: &[u8], expected: &str) -> bool {
    hex(&Sha256::digest(bytes)) == expected
}

fn node_in(dir: &Path) -> PathBuf {
    if cfg!(windows) {
        dir.join("node.exe")
    } else {
        dir.join("bin").join("node")
    }
}

/// A Node 22 for the adapter: the one asked for, one on the PATH, or the pinned build in the data directory
/// (downloaded if it is not there yet).
fn resolve_node(data: &Path, asked: Option<&Path>) -> Res<PathBuf> {
    if let Some(n) = asked {
        return match node_major(n) {
            Some(22) => Ok(n.to_path_buf()),
            other => Err(format!("{}: want Node 22, found {other:?}", n.display()).into()),
        };
    }
    if node_major(Path::new("node")) == Some(22) {
        let found = which("node").ok_or("node is on the PATH but cannot be located")?;
        return Ok(found);
    }
    let os = match std::env::consts::OS {
        "windows" | "linux" | "macos" => std::env::consts::OS,
        other => return Err(format!("no managed Node for {other}: pass --node").into()),
    };
    let (_, _, suffix, sha) = NODE_ARCHIVES
        .iter()
        .find(|(o, a, _, _)| *o == os && *a == std::env::consts::ARCH)
        .ok_or("no managed Node for this CPU: pass --node")?;
    let stem = format!(
        "node-v{NODE_VERSION}-{}",
        suffix.split('.').next().unwrap_or("")
    );
    let root = data.join("runtime");
    let dir = root.join(&stem);
    let node = node_in(&dir);
    if node_major(&node) == Some(22) {
        return Ok(node);
    }
    std::fs::create_dir_all(&root)?;
    let url = format!("https://nodejs.org/dist/v{NODE_VERSION}/node-v{NODE_VERSION}-{suffix}");
    let archive = root.join(format!("node-v{NODE_VERSION}-{suffix}"));
    println!("downloading {url}");
    let st = Command::new(if cfg!(windows) { "curl.exe" } else { "curl" })
        .no_window()
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "-o",
        ])
        .arg(&archive)
        .arg(&url)
        .status()?;
    if !st.success() {
        return Err(format!("downloading {url} failed").into());
    }
    if !sha256_matches(&std::fs::read(&archive)?, sha) {
        let _ = std::fs::remove_file(&archive);
        return Err(format!("{url} does not match the pinned SHA-256; removed").into());
    }
    // Windows' own bsdtar reads zip; the one on the PATH may be GNU tar.
    let tar = if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        PathBuf::from(root).join("System32").join("tar.exe")
    } else {
        PathBuf::from("tar")
    };
    let st = Command::new(tar)
        .no_window()
        .arg("-xf")
        .arg(&archive)
        .arg("-C")
        .arg(&root)
        .status()?;
    if !st.success() {
        return Err("unpacking Node failed".into());
    }
    let _ = std::fs::remove_file(&archive);
    match node_major(&node) {
        Some(22) => Ok(node),
        _ => Err(format!("{} is not Node 22 after unpacking", node.display()).into()),
    }
}

fn which(program: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        vec![".exe".into()]
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&paths)
        .flat_map(|d| exts.iter().map(move |e| d.join(format!("{program}{e}"))))
        .find(|p| p.is_file())
}

/// npm next to `node`, run with that node first on the PATH.
fn npm(node: &Path, dir: &Path, args: &[&str]) -> Res<()> {
    let bin = node.parent().ok_or("node has no directory")?;
    // Windows: `<node dir>/npm.cmd`; elsewhere `<node dir>/npm` (a managed Node keeps it beside `node`).
    let npm = if cfg!(windows) {
        bin.join("npm.cmd")
    } else {
        bin.join("npm")
    };
    let npm = if npm.exists() {
        npm
    } else {
        which(if cfg!(windows) { "npm.cmd" } else { "npm" })
            .ok_or("npm not found next to node or on the PATH")?
    };
    let mut path = std::ffi::OsString::from(bin);
    if let Some(old) = std::env::var_os("PATH") {
        path.push(if cfg!(windows) { ";" } else { ":" });
        path.push(old);
    }
    let st = Command::new(npm)
        .no_window()
        .args(args)
        .current_dir(dir)
        .env("PATH", path)
        .stdin(Stdio::null())
        .status()?;
    if st.success() {
        Ok(())
    } else {
        Err(format!("npm {} failed", args.first().unwrap_or(&"")).into())
    }
}

/// Installs `harness` and records it. `dir` is where the adapter goes (default `<data>/harness/<name>`).
pub fn setup(
    data: &Path,
    harness: &str,
    dir: Option<&Path>,
    node: Option<&Path>,
) -> Res<HarnessInstall> {
    let driver = driver(data, harness)?;
    let name = driver.name.as_str();
    if !han_registry::agents::valid_harness_name(name) {
        return Err("driver name must be a portable harness name, not a path".into());
    }
    if let Some(login) = driver.login() {
        login.validate()?;
    }
    let adapter = driver.adapter.clone();
    if adapter.package.is_empty() || adapter.version.is_empty() || adapter.entry.is_empty() {
        return Err(
            "managed setup needs the adapter's npm package, exact version and entry".into(),
        );
    }
    let install_dir = dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| data.join("harness").join(name));
    let install_dir = std::path::absolute(install_dir)?;
    std::fs::create_dir_all(&install_dir)?;
    // An explicit shared installation must also keep its managed runtime outside the private node
    // data directory, so an OS-user host can execute it without access to the node's keys.
    let runtime_root = if dir.is_some() {
        install_dir.as_path()
    } else {
        data
    };
    let node = resolve_node(runtime_root, node)?;
    // A package.json of our own keeps npm from walking up to some other project.
    let manifest = install_dir.join("package.json");
    if !manifest.exists() {
        std::fs::write(
            &manifest,
            format!("{{\"name\":\"han-harness-{name}\",\"private\":true}}\n"),
        )?;
    }
    println!(
        "installing {}@{} into {}",
        adapter.package,
        adapter.version,
        install_dir.display()
    );
    npm(
        &node,
        &install_dir,
        &[
            "install",
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
            "--save-exact",
            &format!("{}@{}", adapter.package, adapter.version),
        ],
    )?;
    let lock = std::fs::read_to_string(install_dir.join("package-lock.json"))?;
    let integrity = lock_integrity(&lock, &adapter.package)
        .ok_or("package-lock.json has no integrity for the adapter")?;
    if !adapter.integrity.is_empty() && adapter.integrity != integrity {
        return Err(format!(
            "{} {}: integrity {integrity} is not the pinned {}",
            adapter.package, adapter.version, adapter.integrity
        )
        .into());
    }
    let adapter_dir = install_dir.join("node_modules").join(&adapter.package);
    if !adapter_dir.join(&adapter.entry).is_file() {
        return Err(format!("{} has no {}", adapter_dir.display(), adapter.entry).into());
    }
    // Store a complete snapshot beside the adapter, where an OS-user host can read it. The source file
    // may be moved afterwards; installing by name reuses this definition instead of guessing a built-in.
    let driver_path = install_dir.join("driver.json");
    std::fs::write(&driver_path, serde_json::to_string_pretty(&driver)?)?;
    let record = HarnessInstall {
        harness: name.to_owned(),
        node,
        adapter_dir,
        package: adapter.package,
        version: adapter.version,
        integrity,
        driver: Some(driver_path),
    };
    record.save(data)?;
    Ok(record)
}

/// Runs the adapter's login command with only this agent's harness login directory selected.
pub fn login(
    data: &Path,
    agent: &str,
    check: bool,
    dir: Option<&Path>,
    program: Option<&Path>,
    harness: Option<&str>,
) -> Res<()> {
    let file = AgentsFile::load(data)?;
    let recorded = file.agent(agent).harness;
    let name = harness
        .or(recorded.as_deref())
        .ok_or("record a harness with `han agent set --harness`, or pass --harness")?;
    let driver = driver(data, name)?;
    let login = driver
        .login()
        .ok_or("this driver does not use a subscription login")?;
    login.validate()?;
    let root = match dir {
        Some(d) => d.to_path_buf(),
        None => file.login_dir(data, agent),
    };
    let dir = std::path::absolute(login.directory(&root))?;
    if check {
        // Only whether a login exists: nothing of it is read or shown.
        return if login.present(&root) {
            println!(
                "{agent}: cached {} login present ({})",
                driver.harness,
                dir.display()
            );
            Ok(())
        } else {
            Err(format!(
                "{agent}: no login in {} (run: han agent login {agent})",
                dir.display()
            )
            .into())
        };
    }
    std::fs::create_dir_all(&dir)?;
    println!(
        "logging {agent} in to {}; the login goes to {}. Follow the harness's sign-in instructions.",
        driver.harness,
        dir.display()
    );
    let install = HarnessInstall::load(data, &driver.name)?;
    let mut vars = std::collections::BTreeMap::new();
    vars.insert("login_dir", dir.to_string_lossy().into_owned());
    if let Some(install) = install {
        vars.insert("node", install.node.to_string_lossy().into_owned());
        vars.insert(
            "adapter_dir",
            install.adapter_dir.to_string_lossy().into_owned(),
        );
    }
    let argv: Vec<String> = login.command.iter().map(|part| fill(part, &vars)).collect();
    if argv
        .iter()
        .any(|part| part.contains("{node}") || part.contains("{adapter_dir}"))
    {
        return Err(format!(
            "set up {} before login: han agent setup-harness {name}",
            driver.name
        )
        .into());
    }
    let executable = program
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&argv[0]));
    // Resolve Windows npm shims generically too; the old Claude login path supported .cmd installers.
    let executable = if cfg!(windows) && executable.components().count() == 1 {
        std::env::var_os("PATH")
            .and_then(|paths| {
                std::env::split_paths(&paths)
                    .flat_map(|dir| {
                        ["", ".exe", ".cmd"]
                            .into_iter()
                            .map(|ext| dir.join(format!("{}{ext}", executable.display())))
                            .collect::<Vec<_>>()
                    })
                    .find(|path| path.is_file())
            })
            .unwrap_or(executable)
    } else {
        executable
    };
    let status = std::process::Command::new(&executable)
        .no_window()
        .args(&argv[1..])
        .env(
            driver
                .isolation
                .login_dir_env
                .as_ref()
                .expect("login has an env"),
            &dir,
        )
        .status()
        .map_err(|e| {
            format!(
                "cannot run {} ({e}); install the harness or pass --program",
                executable.display()
            )
        })?;
    if !status.success() {
        return Err(format!("{} login exited with {status}", driver.harness).into());
    }
    if login.present(&root) {
        println!("{agent}: cached login present");
        Ok(())
    } else {
        Err(format!("{agent}: no login was made in {}", dir.display()).into())
    }
}

/// Whether the harness `name` is set up in the node's data directory.
pub fn is_set_up(data: &Path, name: &str) -> Res<bool> {
    Ok(Registry::open(data).harness_set_up(name)?)
}

/// Applies `edit` to how `actor` is launched, in `<data>/agents.json`; only what the edit names changes. Says
/// whether the file changed.
pub fn record_settings(data: &Path, actor: &str, edit: &AgentEdit) -> Res<bool> {
    let mut file = AgentsFile::load(data)?;
    let changed = file.set(actor, edit)?;
    if changed {
        file.save(data)?;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_custom_drivers_control_login_checks_without_a_builtin() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path();
        let mut cfg = builtin("claude").unwrap();
        cfg.name = "test-adapter".into();
        cfg.harness = "test-adapter".into();
        cfg.isolation.login_dir_env = Some("TEST_ADAPTER_HOME".into());
        cfg.isolation.login = Some(han_host::driver::Login {
            subdir: "test-adapter".into(),
            file: "token.json".into(),
            command: vec!["test-login".into()],
        });
        let source = data.join("custom.json");
        std::fs::write(&source, serde_json::to_string(&cfg).unwrap()).unwrap();
        HarnessInstall {
            harness: cfg.name.clone(),
            node: "node".into(),
            adapter_dir: data.join("adapter"),
            package: "test-adapter".into(),
            version: "1.0.0".into(),
            integrity: String::new(),
            driver: Some(source),
        }
        .save(data)
        .unwrap();
        assert_eq!(driver(data, "test-adapter").unwrap(), cfg);
        let root = data.join("login-root");
        std::fs::create_dir_all(root.join("test-adapter")).unwrap();
        std::fs::write(root.join(".credentials.json"), "{}").unwrap();
        assert!(
            login(
                data,
                "@coder:test",
                true,
                Some(&root),
                None,
                Some("test-adapter")
            )
            .is_err()
        );
        std::fs::write(root.join("test-adapter/token.json"), "{}").unwrap();
        login(
            data,
            "@coder:test",
            true,
            Some(&root),
            None,
            Some("test-adapter"),
        )
        .unwrap();
        // A missing installed definition must not silently fall back to a different adapter.
        std::fs::remove_file(data.join("custom.json")).unwrap();
        assert!(driver(data, "test-adapter").is_err());
    }

    #[test]
    fn node_versions_and_lock_files_parse() {
        assert_eq!(parse_node_major("v22.23.3\n"), Some(22));
        assert_eq!(parse_node_major("v25.8.2"), Some(25));
        assert_eq!(parse_node_major("not node"), None);
        let lock = r#"{"packages":{"":{},"node_modules/@a/b":{"version":"1.0.0","integrity":"sha512-xyz"}}}"#;
        assert_eq!(lock_integrity(lock, "@a/b").as_deref(), Some("sha512-xyz"));
        assert_eq!(lock_integrity(lock, "c"), None);
    }

    #[test]
    fn the_pinned_node_archive_is_checked() {
        assert!(sha256_matches(
            b"abc",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        ));
        assert!(!sha256_matches(
            b"abd",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        ));
        // every platform entry is a SHA-256 in hex
        for (_, _, _, sha) in NODE_ARCHIVES {
            assert!(sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn a_node_that_is_not_22_is_refused() {
        let dir = std::env::temp_dir().join(format!("han-setup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let e = resolve_node(&dir, Some(Path::new("/definitely/not/node"))).unwrap_err();
        assert!(e.to_string().contains("want Node 22"), "{e}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
