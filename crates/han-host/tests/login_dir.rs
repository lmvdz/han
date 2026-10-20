//! The per-agent login directory (blocker 1 of the first dogfood): a driver with `login_dir_env` gets its
//! login directory in its environment and read-write in its sandbox, and nothing else of the user's home.
//! Backends this machine lacks are skipped with a message; one that is available is always tested.

use std::path::{Path, PathBuf};
use std::time::Duration;

use han_host::driver::{CLAUDE_LOGIN_FILE, bind_login, builtin, login_binding};
use han_host::sandbox::{self, Limits, Preference, SandboxSpec};
use han_node::api::{Fidelity, SandboxBackend};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

mod production_shim;

const CRED: &str = "han-fake-login-not-a-secret";

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn available(b: SandboxBackend) -> bool {
    match sandbox::probe(&production_shim::shim())
        .into_iter()
        .find(|x| x.backend == b)
    {
        Some(x) if x.available => true,
        Some(x) => {
            eprintln!("SKIP: {b:?} unavailable here: {}", x.detail);
            false
        }
        None => {
            eprintln!("SKIP: {b:?} does not exist on this OS");
            false
        }
    }
}

async fn probe_with_login(backend: Preference) {
    let root = tempfile::Builder::new()
        .prefix("han-login-")
        .tempdir()
        .unwrap();
    let root = root.path().to_path_buf();
    let (wt, temp) = (root.join("wt"), root.join("temp"));
    // `home` stands for the user's home: it holds their own login, which must stay out of sight.
    let (mine, theirs) = (root.join("agent/claude-config"), root.join("home/.claude"));
    for d in [&wt, &temp, &mine, &theirs] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(mine.join(CLAUDE_LOGIN_FILE), CRED).unwrap();
    std::fs::write(theirs.join(CLAUDE_LOGIN_FILE), CRED).unwrap();

    let driver = builtin("claude").unwrap();
    let binding = login_binding(&driver, Some(&mine)).unwrap();
    let bound = binding.clone().unwrap().1;
    let mut env = Vec::new();
    let mut rw = vec![wt.clone(), temp.clone()];
    bind_login(binding, &mut env, &mut rw);
    let written = bound.join("refreshed.json");
    let spec_json = json!({
        "envvar": ["CLAUDE_CONFIG_DIR"],
        "read": [s(&bound.join(CLAUDE_LOGIN_FILE)), s(&theirs.join(CLAUDE_LOGIN_FILE))],
        "write": [s(&written), s(&theirs.join("planted.json"))],
        "child": true,
    });
    env.push(("HAN_PROBE_SPEC".into(), spec_json.to_string()));
    let program = PathBuf::from(env!("CARGO_BIN_EXE_han-sandbox-probe"));
    let ro = if cfg!(windows) {
        vec![program.parent().unwrap().to_path_buf()]
    } else {
        vec![program.clone()]
    };
    let spec = SandboxSpec {
        session: "login".into(),
        backend,
        program,
        args: vec![],
        cwd: wt,
        env,
        rw,
        ro,
        temp,
        proxy: None,
        volatile_dirs: Vec::new(),
        allow_advisory: false,
        limits: Limits {
            wall_time: Some(Duration::from_secs(120)),
            ..Limits::default()
        },
        shim: production_shim::shim(),
        inside_port: None,
    };
    let mut sb = sandbox::launch(spec)
        .await
        .unwrap_or_else(|e| panic!("launch: {e}"));
    assert_eq!(sb.info.fidelity, Fidelity::Enforced);
    let mut out = sb.stdout.take().unwrap();
    drop(sb.stdin.take());
    let mut text = String::new();
    tokio::time::timeout(Duration::from_secs(150), out.read_to_string(&mut text))
        .await
        .expect("the probe finished")
        .unwrap();
    let _ = sb.wait().await;
    let report: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let find = |op: String| -> &Value {
        report
            .iter()
            .find(|r| r["op"] == op)
            .unwrap_or_else(|| panic!("no {op} in the report: {text}"))
    };
    for prefix in ["", "child:"] {
        let var = find(format!("{prefix}envvar:CLAUDE_CONFIG_DIR"));
        assert_eq!(var["ok"], true, "{var}");
        assert_eq!(var["detail"], s(&bound), "the variable names the bound dir");
        let read_mine = find(format!(
            "{prefix}read:{}",
            s(&bound.join(CLAUDE_LOGIN_FILE))
        ));
        assert_eq!(
            read_mine["ok"], true,
            "the login is visible inside: {read_mine}"
        );
        let write_mine = find(format!("{prefix}write:{}", s(&written)));
        assert_eq!(
            write_mine["ok"], true,
            "the login dir is writable: {write_mine}"
        );
        // The user's own ~/.claude is not exposed, and cannot be written.
        let read_theirs = find(format!(
            "{prefix}read:{}",
            s(&theirs.join(CLAUDE_LOGIN_FILE))
        ));
        assert_eq!(read_theirs["ok"], false, "{read_theirs}");
        let write_theirs = find(format!("{prefix}write:{}", s(&theirs.join("planted.json"))));
        assert_eq!(write_theirs["ok"], false, "{write_theirs}");
    }
    assert!(written.exists(), "the write reached the bound directory");
    assert!(!theirs.join("planted.json").exists());
    assert!(!text.contains(CRED), "the probe never reports contents");
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mxc_sees_the_agents_login_dir_and_nothing_else() {
    if available(SandboxBackend::Mxc) {
        probe_with_login(Preference::Mxc).await;
    }
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bwrap_sees_the_agents_login_dir_and_nothing_else() {
    if available(SandboxBackend::Bwrap) {
        probe_with_login(Preference::Bwrap).await;
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn volatile_login_state_is_empty_on_each_start_but_credentials_refresh() {
    if !available(SandboxBackend::Bwrap) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let login = root.path().join("login");
    let skills = login.join("skills");
    std::fs::create_dir_all(&skills).unwrap();
    std::fs::write(skills.join("inherited"), "operator skill").unwrap();
    std::fs::write(login.join("auth.json"), "fake").unwrap();
    let spec = SandboxSpec {
        session: "volatile-login".into(), backend: Preference::Bwrap,
        program: "/bin/sh".into(),
        args: vec!["-ec".into(), "test ! -e \"$LOGIN/skills/inherited\"; test ! -e \"$LOGIN/skills/generated\"; test ! -e \"$LOGIN/plugins/generated\"; printf builtin > \"$LOGIN/skills/generated\"; printf plugin > \"$LOGIN/plugins/generated\"; printf refreshed > \"$LOGIN/auth.new\"; mv \"$LOGIN/auth.new\" \"$LOGIN/auth.json\"".into()],
        cwd: login.clone(), env: vec![("LOGIN".into(), s(&login))],
        rw: vec![login.clone()], ro: vec![], volatile_dirs: vec![skills.clone(), login.join("plugins")],
        temp: root.path().join("temp"), proxy: None, allow_advisory: false,
        limits: Limits { wall_time: Some(Duration::from_secs(20)), ..Limits::default() },
        shim: production_shim::shim(), inside_port: None,
    };
    for _ in 0..2 {
        let mut child = sandbox::launch(spec.clone()).await.unwrap();
        assert_eq!(child.wait().await, Some(0));
        assert_eq!(
            std::fs::read_to_string(login.join("auth.json")).unwrap(),
            "refreshed"
        );
        assert_eq!(
            std::fs::read_to_string(skills.join("inherited")).unwrap(),
            "operator skill"
        );
        assert!(!skills.join("generated").exists());
        assert!(!login.join("plugins/generated").exists());
    }
}
