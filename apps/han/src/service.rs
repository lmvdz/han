//! `han node install-service` and `han node status`: the node as a per-user background service that starts at
//! logon, with no admin rights.
//!
//! * Windows: a scheduled task `HAN Node` for the current user, triggered at logon (`Register-ScheduledTask`
//!   works for your own tasks without elevation), restarted if it fails.
//! * Linux: a systemd user unit `han-node.service` in `~/.config/systemd/user`. For it to run without a login
//!   session, an admin runs `loginctl enable-linger <user>` once.

use han_proc::NoWindow as _;
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

/// The PowerShell that registers the task for the current user and optionally starts it.
pub fn windows_script(spec: &Spec, start: bool) -> String {
    let arguments = std::iter::once("node".to_owned())
        .chain(spec.args.iter().cloned())
        .map(|a| win_quote(&a))
        .collect::<Vec<_>>()
        .join(" ");
    let ps = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let mut script = format!(
        "$ErrorActionPreference = 'Stop'\n\
         $a = New-ScheduledTaskAction -Execute {exe} -Argument {arguments}\n\
         $t = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME\n\
         $s = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries \
         -StartWhenAvailable -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 5 \
         -RestartInterval (New-TimeSpan -Minutes 1)\n\
         Register-ScheduledTask -TaskName {name} -Action $a -Trigger $t -Settings $s -Force | Out-Null\n",
        exe = ps(&spec.exe.to_string_lossy()),
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
        .no_window()
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
) -> Result<String, String> {
    if cfg!(windows) {
        let script = windows_script(spec, start);
        if dry_run {
            return Ok(script);
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
        Ok(format!(
            "scheduled task \"{TASK_NAME}\" will start the node at logon{}",
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
        let s = windows_script(&spec(), true);
        assert!(
            s.contains("New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME"),
            "{s}"
        );
        assert!(s.contains("Register-ScheduledTask -TaskName 'HAN Node'"));
        // no principal with elevated rights, no SYSTEM
        assert!(!s.contains("-RunLevel") && !s.contains("SYSTEM"));
        assert!(s.contains(r#"-Execute 'C:\Program Files\han\han.exe'"#));
        assert!(
            s.contains(
                r#"-Argument 'node --actor @lars:han.example --data "C:\Users\Lars\han data"'"#
            ),
            "{s}"
        );
        assert!(s.contains("Start-ScheduledTask"));
        assert!(!windows_script(&spec(), false).contains("Start-ScheduledTask"));
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
