//! `han executor add --github-app …`, run as the real binary: the `github` block, the key's mode and its
//! secrecy, idempotence, the refusals, and what `han doctor` says about it. No node runs.

use han_proc::NoWindow as _;
use std::path::Path;
use std::process::{Command, Output};

const HAN: &str = env!("CARGO_BIN_EXE_han");
const PEM: &str = "-----BEGIN PRIVATE KEY-----\nTESTONLYSECRETBODY\n-----END PRIVATE KEY-----\n";
const BODY: &str = "TESTONLYSECRETBODY";

fn han(args: &[&str]) -> Output {
    Command::new(HAN)
        .no_window()
        .args(args)
        .env_remove("HAN_DATA")
        .env_remove("HAN_GITHUB_READ_APP")
        .output()
        .expect("han runs")
}

fn all(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn pem(dir: &Path, name: &str, text: &str, mode: u32) -> String {
    let p = dir.join(name);
    std::fs::write(&p, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    let _ = mode;
    p.to_string_lossy().into_owned()
}

fn add(exec: &str, data: &str, extra: &[&str]) -> Output {
    let mut a = vec![
        "executor",
        "add",
        "exec",
        "--operator",
        "@lars:han.example",
        "--dir",
        exec,
        "--data",
        data,
        "--repo",
        "github:o/r=https://github.com/o/r.git",
    ];
    a.extend(extra);
    han(&a)
}

#[test]
fn the_flags_write_the_block_the_key_and_the_repo_mapping_and_a_repeat_changes_nothing() {
    let t = tempfile::tempdir().unwrap();
    let exec = t.path().join("exec").to_string_lossy().into_owned();
    let data = t.path().join("data").to_string_lossy().into_owned();
    let key = pem(t.path(), "src.pem", PEM, 0o600);
    let flags = [
        "--github-app",
        "4242",
        "--github-installation",
        "77",
        "--github-key",
        key.as_str(),
    ];
    let first = add(&exec, &data, &flags);
    let out = all(&first);
    assert!(first.status.success(), "{out}");
    assert!(out.contains("changed:   github app key"), "{out}");
    assert!(!out.contains(BODY), "the key must never be printed");

    let cfg: serde_json::Value =
        serde_json::from_slice(&std::fs::read(Path::new(&exec).join("exec.json")).unwrap())
            .unwrap();
    assert_eq!(cfg["github"]["app_id"], 4242);
    assert_eq!(cfg["github"]["installation_id"], 77);
    assert_eq!(cfg["repos"][0]["github"], "o/r");
    let dest = Path::new(&exec).join("github-app.pem");
    assert!(
        cfg["github"]["private_key"]
            .as_str()
            .unwrap()
            .ends_with("github-app.pem")
    );
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), PEM);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    let again = add(&exec, &data, &flags);
    let out = all(&again);
    assert!(again.status.success(), "{out}");
    assert!(out.contains("unchanged: github app key"), "{out}");
    assert!(!out.contains("changed:   "), "{out}");
    assert!(!out.contains(BODY));
}

#[test]
fn a_world_readable_key_a_non_pem_file_and_the_read_app_are_refused_and_nothing_is_written() {
    let t = tempfile::tempdir().unwrap();
    let exec = t.path().join("exec").to_string_lossy().into_owned();
    let data = t.path().join("data").to_string_lossy().into_owned();
    let junk = pem(
        t.path(),
        "junk.pem",
        "not a key\nTESTONLYSECRETBODY\n",
        0o600,
    );
    let good = pem(t.path(), "good.pem", PEM, 0o600);
    let with = |key: &str, app: &str, extra: &[&str]| {
        let mut f = vec![
            "--github-app",
            app,
            "--github-installation",
            "1",
            "--github-key",
            key,
        ];
        f.extend(extra);
        add(&exec, &data, &f)
    };

    #[cfg(unix)]
    {
        let loose = pem(t.path(), "loose.pem", PEM, 0o644);
        let o = with(&loose, "4242", &[]);
        assert!(!o.status.success());
        assert!(
            all(&o).contains("readable by group or others"),
            "{}",
            all(&o)
        );
    }
    let o = with(&junk, "4242", &[]);
    assert!(!o.status.success());
    assert!(!all(&o).contains(BODY));
    let o = with(&good, "5309257", &["--read-app", "5309257"]);
    assert!(!o.status.success());
    assert!(all(&o).contains("read App"), "{}", all(&o));
    assert!(!Path::new(&exec).exists(), "a refused call writes nothing");

    // a partial set of flags is refused too
    let o = add(&exec, &data, &["--github-app", "4242"]);
    assert!(!o.status.success());
}

#[test]
fn doctor_names_the_flags_until_the_executor_has_its_app() {
    let t = tempfile::tempdir().unwrap();
    let exec = t.path().join("exec").to_string_lossy().into_owned();
    let data = t.path().join("data").to_string_lossy().into_owned();
    let o = add(&exec, &data, &[]);
    assert!(o.status.success(), "{}", all(&o));
    let doctor = || {
        han(&[
            "doctor",
            "--json",
            "--data",
            &data,
            "--exec-dir",
            &exec,
            "--endpoint",
            "tcp://127.0.0.1:1",
        ])
    };
    let r = all(&doctor());
    assert!(r.contains("executor.github.missing"), "{r}");
    assert!(
        r.contains("--github-app") && r.contains("--github-key"),
        "{r}"
    );

    let key = pem(t.path(), "src.pem", PEM, 0o600);
    let o = add(
        &exec,
        &data,
        &[
            "--github-app",
            "4242",
            "--github-installation",
            "1",
            "--github-key",
            &key,
        ],
    );
    assert!(o.status.success(), "{}", all(&o));
    let r = all(&doctor());
    assert!(!r.contains("executor.github."), "{r}");

    let dest = Path::new(&exec).join("github-app.pem");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(all(&doctor()).contains("executor.github.key-mode"));
    }
    std::fs::remove_file(&dest).unwrap();
    assert!(all(&doctor()).contains("executor.github.key-missing"));
}

#[test]
fn doctor_exec_only_prints_who_ran_ok_lines_and_what_the_node_user_must_cover() {
    let t = tempfile::tempdir().unwrap();
    let exec = t.path().join("exec").to_string_lossy().into_owned();
    let data = t.path().join("data").to_string_lossy().into_owned();
    let key = pem(t.path(), "app.pem", PEM, 0o600);
    let o = add(
        &exec,
        &data,
        &[
            "--github-app",
            "4242",
            "--github-installation",
            "7",
            "--github-key",
            &key,
        ],
    );
    assert!(o.status.success(), "{}", all(&o));
    let o = Command::new(HAN)
        .no_window()
        .args([
            "doctor",
            "--exec-only",
            "--data",
            &data,
            "--exec-dir",
            &exec,
        ])
        .env("USER", "han-exec")
        .env_remove("HAN_DATA")
        .output()
        .expect("han runs");
    let out = all(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        out.starts_with("checked as han-exec (exec-only run)"),
        "{out}"
    );
    assert!(out.contains("ok executor.github\n"), "{out}");
    assert!(
        out.contains(
            "skipped node: not part of --exec-only; run as han, or `sudo -u han han doctor`"
        ),
        "{out}"
    );
    assert!(!out.contains(BODY), "the key is never printed");
    // the App check asks GitHub, so it says how to be asked
    assert!(
        out.contains("skipped land.github.permissions") && out.contains("--check-github-app"),
        "{out}"
    );
}
