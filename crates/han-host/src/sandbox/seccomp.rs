//! The seccomp filter bubblewrap installs on the sandboxed tree (`--seccomp <fd>`). The network namespace
//! does not cover every socket family: AF_VSOCK reaches the hypervisor host on a VM (WSL2, a cloud runner),
//! AF_PACKET gives a raw view of the namespace's interfaces, and the rest are kernel attack surface a
//! session never needs. The filter allows only AF_UNIX (the proxy socket), AF_INET and AF_INET6 (loopback to
//! the shim) and AF_NETLINK (resolvers and runtimes enumerate interfaces; scoped to the sandbox's own network
//! namespace), for `socket` and `socketpair`; every other family fails with EAFNOSUPPORT. A call through any
//! other ABI than the native one (the 32-bit entry on x86_64, the x32 range) kills the process, so the
//! family check cannot be sidestepped through `socketcall`.
//!
//! The program is assembled by hand (classic BPF, a dozen instructions) so no dependency is needed.

/// One classic BPF instruction (`struct sock_filter`).
#[derive(Clone, Copy)]
struct Insn {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

const LD_W_ABS: u16 = 0x20;
const JEQ_K: u16 = 0x15;
const JGE_K: u16 = 0x35;
const RET_K: u16 = 0x06;

const RET_ALLOW: u32 = 0x7fff_0000;
const RET_KILL_PROCESS: u32 = 0x8000_0000;
const RET_ERRNO: u32 = 0x0005_0000;
const EAFNOSUPPORT: u32 = 97;

// struct seccomp_data { int nr; __u32 arch; __u64 ip; __u64 args[6]; } (little endian).
const OFF_NR: u32 = 0;
const OFF_ARCH: u32 = 4;
const OFF_ARG0_LO: u32 = 16;

/// The first syscall number of the x32 ABI on x86_64.
const X32_BIT: u32 = 0x4000_0000;

const AF_UNIX: u32 = 1;
const AF_INET: u32 = 2;
const AF_INET6: u32 = 10;
const AF_NETLINK: u32 = 16;

/// `(AUDIT_ARCH, SYS_socket, SYS_socketpair)` of the native ABI; `None` where no filter is built.
#[cfg(target_arch = "x86_64")]
const NATIVE: Option<(u32, u32, u32)> = Some((0xc000_003e, 41, 53));
#[cfg(target_arch = "aarch64")]
const NATIVE: Option<(u32, u32, u32)> = Some((0xc000_00b7, 198, 199));
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
const NATIVE: Option<(u32, u32, u32)> = None;

const fn stmt(code: u16, k: u32) -> Insn {
    Insn {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

const fn jump(code: u16, k: u32, jt: u8, jf: u8) -> Insn {
    Insn { code, jt, jf, k }
}

/// The filter program, or `None` when this architecture has no filter here.
fn program() -> Option<Vec<Insn>> {
    let (arch, sys_socket, sys_socketpair) = NATIVE?;
    // Indices: 0 arch, 1 jeq, 2 nr, 3 x32, 4 socket, 5 socketpair, 6 allow, 7 kill, 8 load family, 9..=12
    // the allowed families, 13 allow, 14 deny.
    Some(vec![
        stmt(LD_W_ABS, OFF_ARCH),
        jump(JEQ_K, arch, 0, 5),
        stmt(LD_W_ABS, OFF_NR),
        jump(JGE_K, X32_BIT, 3, 0),
        jump(JEQ_K, sys_socket, 3, 0),
        jump(JEQ_K, sys_socketpair, 2, 0),
        stmt(RET_K, RET_ALLOW),
        stmt(RET_K, RET_KILL_PROCESS),
        stmt(LD_W_ABS, OFF_ARG0_LO),
        jump(JEQ_K, AF_UNIX, 3, 0),
        jump(JEQ_K, AF_INET, 2, 0),
        jump(JEQ_K, AF_INET6, 1, 0),
        jump(JEQ_K, AF_NETLINK, 0, 1),
        stmt(RET_K, RET_ALLOW),
        stmt(RET_K, RET_ERRNO | EAFNOSUPPORT),
    ])
}

/// The filter in the byte layout `bwrap --seccomp` reads (an array of `struct sock_filter`), or `None`.
pub(super) fn filter_bytes() -> Option<Vec<u8>> {
    let prog = program()?;
    let mut out = Vec::with_capacity(prog.len() * 8);
    for i in prog {
        out.extend_from_slice(&i.code.to_ne_bytes());
        out.push(i.jt);
        out.push(i.jf);
        out.extend_from_slice(&i.k.to_ne_bytes());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny interpreter for the instructions used, to check the jump targets.
    fn run(prog: &[Insn], arch: u32, nr: u32, arg0: u32) -> u32 {
        let (mut a, mut pc) = (0u32, 0usize);
        loop {
            let i = prog[pc];
            pc += 1;
            match i.code {
                LD_W_ABS => {
                    a = match i.k {
                        OFF_ARCH => arch,
                        OFF_NR => nr,
                        _ => arg0,
                    }
                }
                RET_K => return i.k,
                JEQ_K => pc += usize::from(if a == i.k { i.jt } else { i.jf }),
                JGE_K => pc += usize::from(if a >= i.k { i.jt } else { i.jf }),
                c => panic!("opcode {c}"),
            }
        }
    }

    #[test]
    fn only_the_needed_families_pass() {
        let Some(prog) = program() else { return };
        let (arch, socket, pair) = NATIVE.unwrap();
        for call in [socket, pair] {
            for af in [AF_UNIX, AF_INET, AF_INET6, AF_NETLINK] {
                assert_eq!(run(&prog, arch, call, af), RET_ALLOW, "af {af}");
            }
            for af in [0, 17, 21, 29, 30, 31, 38, 40, 41, 44] {
                assert_eq!(
                    run(&prog, arch, call, af),
                    RET_ERRNO | EAFNOSUPPORT,
                    "af {af}"
                );
            }
        }
        assert_eq!(run(&prog, arch, 0, 40), RET_ALLOW, "read is untouched");
        assert_eq!(run(&prog, arch, socket | X32_BIT, 1), RET_KILL_PROCESS);
        assert_eq!(run(&prog, 0x4000_0003, socket, 1), RET_KILL_PROCESS);
        assert_eq!(filter_bytes().unwrap().len(), prog.len() * 8);
    }
}
