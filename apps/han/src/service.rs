//! `han node install-service` and `han node status`: the node as a per-user background service that starts at
//! logon, with no admin rights.
//!
//! * Windows: a scheduled task `HAN Node` for the current user, triggered at logon (`Register-ScheduledTask`
//!   works for your own tasks without elevation), restarted if it fails.
//! * Linux: a systemd user unit `han-node.service` in `~/.config/systemd/user`. For it to run without a login
//!   session, an admin runs `loginctl enable-linger <user>` once.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The scheduled task's name.
pub const TASK_NAME: &str = "HAN Node";

/// The systemd user unit's name.
pub const UNIT_NAME: &str = "han-node.service";

/// What the service runs: `<exe> node <args>`.
pub struct Spec {
    /// The `han` executable.
    pub exe: PathBuf,
    /// The arguments of `han node`.
    pub args: Vec<String>,
}

/// A command-line argument as the Windows command line takes it.
fn win_quote(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '"']) {
        return a.to_owned();
    }
    format!("\"{}\"", a.replace('"', "\\\""))
}

/// A word of a systemd `ExecStart=` line.
fn unit_quote(a: &str) -> String {
    let a = a.replace('%', "%%").replace('$', "$$");
    if !a.is_empty() && !a.contains([' ', '\t', '"', '\\', '\'']) {
        return a;
    }
    format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The console host the Windows task action goes through.
pub const CONHOST: &str = r"%SystemRoot%\System32\conhost.exe";

/// The task action `(Execute, Argument)`. Task Scheduler starts a console program with a console it sometimes
/// never finishes setting up, so the program stalls before `main`; `conhost.exe --headless` gives it a console
/// of its own without a window, and the stall does not happen. `headless: false` is the plain action.
pub fn windows_action(spec: &Spec, headless: bool) -> (String, String) {
    let args = std::iter::once("node".to_owned())
        .chain(spec.args.iter().cloned())
        .map(|a| win_quote(&a))
        .collect::<Vec<_>>()
        .join(" ");
    let exe = spec.exe.to_string_lossy();
    if headless {
        (CONHOST.to_owned(), format!("--headless \"{exe}\" {args}"))
    } else {
        (exe.into_owned(), args)
    }
}

/// Whether a task action runs the program through `conhost.exe --headless`.
pub fn action_is_wrapped(execute: &str, arguments: &str) -> bool {
    let execute = execute.trim().trim_matches('"').to_ascii_lowercase();
    (execute == "conhost.exe" || execute.ends_with(r"\conhost.exe"))
        && arguments.trim_start().starts_with("--headless ")
}

/// Whether this machine has a `conhost.exe` that takes `--headless`: it runs a program that exits 0.
fn conhost_headless_works() -> bool {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let sys = Path::new(&root).join("System32");
    let conhost = sys.join("conhost.exe");
    conhost.is_file()
        && Command::new(conhost)
            .args(["--headless"])
            .arg(sys.join("cmd.exe"))
            .args(["/c", "exit", "0"])
            .output()
            .is_ok_and(|o| o.status.success())
}

/// The PowerShell that registers the task for the current user and optionally starts it. It prints `changed`
/// when it registered the task and `unchanged` when the task already had this action.
pub fn windows_script(spec: &Spec, start: bool, headless: bool) -> String {
    let (execute, arguments) = windows_action(spec, headless);
    let ps = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let mut script = format!(
        "$ErrorActionPreference = 'Stop'\n\
         $exe = {execute}\n\
         $arg = {arguments}\n\
         $old = Get-ScheduledTask -TaskName {name} -ErrorAction SilentlyContinue\n\
         if ($old -and @($old.Actions).Count -eq 1 -and $old.Actions[0].Execute -eq $exe \
         -and $old.Actions[0].Arguments -eq $arg) {{ Write-Output 'unchanged' }} else {{\n\
         $a = New-ScheduledTaskAction -Execute $exe -Argument $arg\n\
         $t = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME\n\
         $s = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries \
         -StartWhenAvailable -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 5 \
         -RestartInterval (New-TimeSpan -Minutes 1)\n\
         Register-ScheduledTask -TaskName {name} -Action $a -Trigger $t -Settings $s -Force | Out-Null\n\
         Write-Output 'changed' }}\n",
        execute = ps(&execute),
        arguments = ps(&arguments),
        name = ps(TASK_NAME),
    );
    if start {
        script.push_str(&format!(
            "Start-ScheduledTask -TaskName {}\n",
            ps(TASK_NAME)
        ));
    }
    script
}

/// The action of the `HAN Node` task as `(Execute, Arguments)`, when this machine has the task.
pub fn windows_task_action() -> Option<(String, String)> {
    if !cfg!(windows) {
        return None;
    }
    let script = format!(
        "$a = (Get-ScheduledTask -TaskName '{TASK_NAME}' -ErrorAction Stop).Actions[0]\n\
         $a.Execute\n$a.Arguments"
    );
    let out = run(
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command", &script],
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut lines = text.lines();
    let execute = lines.next()?.trim().to_owned();
    Some((execute, lines.next().unwrap_or("").trim().to_owned()))
}

/// The text of the systemd user unit.
pub fn unit_text(spec: &Spec, description: &str) -> String {
    let exec = std::iter::once(spec.exe.to_string_lossy().into_owned())
        .chain(std::iter::once("node".to_owned()))
        .chain(spec.args.iter().cloned())
        .map(|a| unit_quote(&a))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "[Unit]\nDescription=HAN node ({description})\nAfter=network-online.target\n\n\
         [Service]\nExecStart={exec}\nRestart=on-failure\nRestartSec=5\n\n\
         [Install]\nWantedBy=default.target\n"
    )
}

/// Where the user unit goes.
pub fn unit_path() -> Result<PathBuf, String> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(x) if !x.is_empty() => PathBuf::from(x),
        _ => PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?).join(".config"),
    };
    Ok(base.join("systemd").join("user").join(UNIT_NAME))
}

fn run(program: &str, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{program}: {e}"))
}

/// Installs the service for the current user. With `dry_run` it only returns what it would do.
pub fn install(
    spec: &Spec,
    description: &str,
    start: bool,
    dry_run: bool,
    plain: bool,
) -> Result<String, String> {
    if cfg!(windows) {
        let headless = !plain && conhost_headless_works();
        let note = if plain || headless {
            ""
        } else {
            "note: conhost.exe is missing or does not take --headless; the task runs han directly and may stall at startup
"
        };
        let script = windows_script(spec, start, headless);
        if dry_run {
            return Ok(format!("{note}{script}"));
        }
        let out = run(
            "powershell.exe",
            &["-NoProfile", "-NonInteractive", "-Command", &script],
        )?;
        if !out.status.success() {
            return Err(format!(
                "registering the scheduled task failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let verdict = if String::from_utf8_lossy(&out.stdout).contains("unchanged") {
            "unchanged"
        } else {
            "changed"
        };
        Ok(format!(
            "{note}{verdict}: scheduled task \"{TASK_NAME}\" will start the node at logon{}",
            if start { " (started now)" } else { "" }
        ))
    } else if cfg!(target_os = "linux") {
        let text = unit_text(spec, description);
        if dry_run {
            return Ok(text);
        }
        let path = unit_path()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut steps: Vec<Vec<&str>> = vec![vec!["--user", "daemon-reload"]];
        steps.push(if start {
            vec!["--user", "enable", "--now", UNIT_NAME]
        } else {
            vec!["--user", "enable", UNIT_NAME]
        });
        for step in steps {
            let out = run("systemctl", &step)?;
            if !out.status.success() {
                return Err(format!(
                    "systemctl {}: {}",
                    step.join(" "),
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
        }
        Ok(format!(
            "{} installed; it starts with your session (for a headless runner an admin runs `loginctl enable-linger $USER` once)",
            path.display()
        ))
    } else {
        Err("install-service is for Windows and Linux".into())
    }
}

/// Removes the service.
pub fn uninstall() -> Result<String, String> {
    if cfg!(windows) {
        let script = format!(
            "Unregister-ScheduledTask -TaskName '{TASK_NAME}' -Confirm:$false -ErrorAction Stop"
        );
        let out = run(
            "powershell.exe",
            &["-NoProfile", "-NonInteractive", "-Command", &script],
        )?;
        if out.status.success() {
            Ok(format!("scheduled task \"{TASK_NAME}\" removed"))
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
        }
    } else {
        let _ = run("systemctl", &["--user", "disable", "--now", UNIT_NAME]);
        let path = unit_path()?;
        let _ = std::fs::remove_file(&path);
        let _ = run("systemctl", &["--user", "daemon-reload"]);
        Ok(format!("{} removed", path.display()))
    }
}

/// How the background service stands, if there is one: a line for `han node status`.
pub fn service_state() -> String {
    if cfg!(windows) {
        let script = format!("(Get-ScheduledTask -TaskName '{TASK_NAME}' -ErrorAction Stop).State");
        match run(
            "powershell.exe",
            &["-NoProfile", "-NonInteractive", "-Command", &script],
        ) {
            Ok(o) if o.status.success() => format!(
                "scheduled task \"{TASK_NAME}\": {}",
                String::from_utf8_lossy(&o.stdout).trim()
            ),
            _ => format!("no scheduled task \"{TASK_NAME}\" (han node install-service)"),
        }
    } else {
        match run("systemctl", &["--user", "is-active", UNIT_NAME]) {
            Ok(o) => {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_owned();
                if s.is_empty() {
                    format!("no user unit {UNIT_NAME} (han node install-service)")
                } else {
                    format!("user unit {UNIT_NAME}: {s}")
                }
            }
            Err(_) => "systemd is not available".into(),
        }
    }
}

/// The `han` executable path to put in the service.
pub fn current_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| e.to_string())
}

/// Makes `p` absolute, so a service started from another directory finds the same files.
pub fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> Spec {
        Spec {
            exe: PathBuf::from(r"C:\Program Files\han\han.exe"),
            args: vec![
                "--actor".into(),
                "@lars:han.example".into(),
                "--data".into(),
                r"C:\Users\Lars\han data".into(),
            ],
        }
    }

    #[test]
    fn the_windows_task_runs_at_logon_for_the_current_user_without_admin() {
        let s = windows_script(&spec(), true, true);
        assert!(
            s.contains("New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME"),
            "{s}"
        );
        assert!(s.contains("Register-ScheduledTask -TaskName 'HAN Node'"));
        // no principal with elevated rights, no SYSTEM
        assert!(!s.contains("-RunLevel") && !s.contains("SYSTEM"));
        assert!(s.contains("Start-ScheduledTask"));
        assert!(!windows_script(&spec(), false, true).contains("Start-ScheduledTask"));
        // running it again reports whether the task changed
        assert!(s.contains("'unchanged'") && s.contains("'changed'"), "{s}");
    }

    #[test]
    fn the_windows_action_goes_through_a_headless_conhost_and_quotes_its_paths() {
        let (execute, arguments) = windows_action(&spec(), true);
        assert_eq!(execute, r"%SystemRoot%\System32\conhost.exe");
        assert_eq!(
            arguments,
            r#"--headless "C:\Program Files\han\han.exe" node --actor @lars:han.example --data "C:\Users\Lars\han data""#
        );
        assert!(action_is_wrapped(&execute, &arguments));
        let s = windows_script(&spec(), true, true);
        assert!(
            s.contains(r"$exe = '%SystemRoot%\System32\conhost.exe'"),
            "{s}"
        );
        // a path without spaces is quoted too
        let plain = Spec {
            exe: PathBuf::from(r"C:\han\han.exe"),
            args: vec![],
        };
        assert_eq!(
            windows_action(&plain, true).1,
            r#"--headless "C:\han\han.exe" node"#
        );
    }

    #[test]
    fn the_plain_action_is_the_old_one_and_is_not_wrapped() {
        let (execute, arguments) = windows_action(&spec(), false);
        assert_eq!(execute, r"C:\Program Files\han\han.exe");
        assert_eq!(
            arguments,
            r#"node --actor @lars:han.example --data "C:\Users\Lars\han data""#
        );
        assert!(!action_is_wrapped(&execute, &arguments));
        assert!(!action_is_wrapped("conhost.exe", "node --actor x"));
        assert!(action_is_wrapped(
            r"C:\Windows\System32\CONHOST.EXE",
            "--headless \"x\" node"
        ));
    }

    #[test]
    fn the_user_unit_restarts_the_node_and_quotes_its_paths() {
        let u = unit_text(
            &Spec {
                exe: PathBuf::from("/usr/local/bin/han"),
                args: vec![
                    "--data".into(),
                    "/home/l/han data".into(),
                    "--actor".into(),
                    "@l:h".into(),
                ],
            },
            "@l:h",
        );
        assert!(
            u.contains(
                r#"ExecStart=/usr/local/bin/han node --data "/home/l/han data" --actor @l:h"#
            ),
            "{u}"
        );
        assert!(u.contains("Restart=on-failure"));
        assert!(u.contains("WantedBy=default.target"));
        assert!(!u.contains("User="), "a user unit runs as its user");
        assert_eq!(unit_quote("50%"), "50%%");
    }
}
