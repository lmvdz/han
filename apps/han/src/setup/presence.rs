//! Prepare the operating-system action used by the normal Linux presence backend.

use super::Res;
#[cfg(any(target_os = "linux", test))]
use std::path::Path;

pub(crate) const POLICY_PATH: &str = "/usr/share/polkit-1/actions/dev.han.approve.policy";
#[cfg(any(target_os = "linux", test))]
const POLICY: &str = include_str!("../../../../crates/han-presence/polkit/dev.han.approve.policy");

#[cfg(any(target_os = "linux", test))]
fn install_at(path: &Path) -> Res<bool> {
    use std::io::Write as _;
    match std::fs::read(path) {
        Ok(bytes) if bytes == POLICY.as_bytes() => return Ok(false),
        Ok(_) => return Err("an existing HAN presence policy differs; inspect it instead of overwriting administrator policy".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    // Publish a complete file without replacing a policy that appeared during installation.
    let parent = path.parent().ok_or("policy has no parent directory")?;
    let temp = parent.join(format!(
        ".han-presence-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let result = (|| -> Res<bool> {
        file.write_all(POLICY.as_bytes())?;
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(std::fs::Permissions::from_mode(0o644))?;
        }
        file.sync_all()?;
        std::fs::hard_link(&temp, path)?;
        Ok(true)
    })();
    drop(file);
    let _ = std::fs::remove_file(temp);
    result
}

#[cfg(target_os = "linux")]
fn is_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|o| o.status.success() && o.stdout == b"0\n")
}

pub(super) fn install() -> Res<()> {
    #[cfg(target_os = "linux")]
    {
        if !is_root() {
            return Err("installing the OS action requires administrator authentication; run sudo han setup install-presence".into());
        }
        if !Path::new("/usr/share/polkit-1/actions").is_dir() {
            return Err("polkit is not installed; install your distribution's polkit package and authentication agent, then resume setup".into());
        }
        let changed = install_at(Path::new(POLICY_PATH))?;
        println!(
            "{}: installed HAN's auth_self presence action (confirmation remains required)",
            if changed { "changed" } else { "unchanged" }
        );
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("the polkit presence action is only used on Linux".into())
    }
}

pub(super) fn prepare() -> Res<()> {
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        if !Command::new("pkcheck")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            return Err("Linux confirmation needs polkit: install your distribution's polkit package and authentication agent, then resume the same setup".into());
        }
        let registered = || {
            Command::new("pkaction")
                .args(["--action-id", "dev.han.approve"])
                .output()
                .is_ok_and(|o| {
                    o.status.success()
                        && String::from_utf8_lossy(&o.stdout)
                            .lines()
                            .any(|l| l.trim() == "dev.han.approve")
                })
        };
        if registered() {
            return Ok(());
        }
        println!(
            "Preparing HAN's Linux confirmation action. Administrator authentication installs the action; each governing operation still uses normal polkit confirmation."
        );
        if is_root() {
            install()?;
        } else {
            let status = Command::new("pkexec").arg(std::env::current_exe()?).args(["setup", "install-presence"]).status()
                .map_err(|e| format!("cannot request administrator authentication: {e}; install the action with sudo han setup install-presence, then resume setup"))?;
            if !status.success() {
                return Err("presence-action installation was not confirmed; resume on a Linux desktop with a polkit authentication agent, or ask its administrator to run sudo han setup install-presence".into());
            }
        }
        for _ in 0..20 {
            if registered() {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if !registered() {
            return Err("the HAN presence action is not registered with polkit; check that polkitd and a desktop authentication agent are running, then resume setup (presence has not been disabled)".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    #[test]
    fn installation_does_not_follow_a_dangling_policy_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dev.han.approve.policy");
        let target = dir.path().join("administrator-target");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(install_at(&path).is_err());
        assert!(!target.exists());
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "temporary publication is cleaned up"
        );
    }

    #[test]
    fn install_is_idempotent_and_preserves_an_administrators_changed_policy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dev.han.approve.policy");
        assert!(install_at(&path).unwrap());
        assert!(!install_at(&path).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), POLICY);
        assert!(POLICY.contains("<allow_active>auth_self</allow_active>"));
        assert!(POLICY.contains("<allow_any>no</allow_any>"));
        std::fs::write(&path, "administrator policy").unwrap();
        assert!(install_at(&path).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "administrator policy"
        );
    }
}
