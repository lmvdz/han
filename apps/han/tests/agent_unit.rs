//! The `han-agent@` unit must not cover `/proc`. `ProtectKernelTunables=yes` makes systemd bind read-only
//! mounts over parts of it, and the kernel then refuses bubblewrap a fresh proc mount in its user namespace
//! ("bwrap: Can't mount proc on /newroot/proc: Operation not permitted"): the host admitted the sandbox and
//! every launch failed. bwrap mounts its own `/proc`, so the unit has no reason to protect it.

const UNIT: &str = include_str!("../../../integrations/runner/han-agent@.service");

#[test]
fn the_agent_unit_does_not_cover_proc() {
    for line in UNIT.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
        for banned in ["ProtectKernelTunables", "ProtectProc", "ProcSubset"] {
            assert!(!line.starts_with(banned), "han-agent@.service sets {line}");
        }
    }
}
