//! A seccomp deny-list applied inside the sandbox (Linux only).
//!
//! `bwrap` isolates files, network and processes; this filter additionally removes the kernel
//! entry points a strategy never needs and that are the usual route out of a sandbox or into
//! the rest of the system: tracing other processes, mounting, loading kernel modules, `bpf`,
//! `perf_event_open`, `io_uring`, keyrings, namespace changes, changing the clock, and so on.
//! A denied call fails with `EPERM`; everything else is allowed.
//!
//! It is a deny-list on purpose: an allow-list would have to track what a JVM, CPython or a
//! Rust binary happens to call, and a mistake there breaks strategies. The cost is that a
//! syscall missing from the list is allowed. It reduces kernel attack surface; it is not a
//! complete policy. Syscall numbers come from the `libc` crate (per architecture), never from
//! hand-typed tables, and the BPF program is compiled by `seccompiler` (rust-vmm).

#![allow(clippy::unnecessary_cast)] // libc::SYS_* is c_long: i64 on some targets, i32 on others

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use seccompiler::{
    BackendError, BpfProgram, SeccompAction, SeccompFilter, SeccompRule, TargetArch,
};

macro_rules! denied {
    ($($name:ident),* $(,)?) => {
        &[$((stringify!($name), libc::$name as i64)),*]
    };
}

/// Syscalls refused inside the sandbox.
pub const DENIED: &[(&str, i64)] = denied![
    SYS_ptrace,
    SYS_process_vm_readv,
    SYS_process_vm_writev,
    SYS_mount,
    SYS_umount2,
    SYS_pivot_root,
    SYS_chroot,
    SYS_setns,
    SYS_unshare,
    SYS_swapon,
    SYS_swapoff,
    SYS_reboot,
    SYS_kexec_load,
    SYS_init_module,
    SYS_finit_module,
    SYS_delete_module,
    SYS_bpf,
    SYS_perf_event_open,
    SYS_userfaultfd,
    SYS_keyctl,
    SYS_add_key,
    SYS_request_key,
    SYS_open_by_handle_at,
    SYS_clock_settime,
    SYS_settimeofday,
    SYS_adjtimex,
    SYS_acct,
    SYS_quotactl,
    SYS_syslog,
    SYS_sethostname,
    SYS_setdomainname,
    SYS_io_uring_setup,
    SYS_io_uring_enter,
    SYS_io_uring_register,
];

/// The compiled BPF program: every denied syscall returns `EPERM`, all others are allowed.
pub fn program() -> Result<BpfProgram, String> {
    let rules: BTreeMap<i64, Vec<SeccompRule>> =
        DENIED.iter().map(|(_, nr)| (*nr, Vec::new())).collect();
    let arch = TargetArch::try_from(std::env::consts::ARCH).map_err(|e: BackendError| {
        format!(
            "seccomp: unsupported architecture {}: {e}",
            std::env::consts::ARCH
        )
    })?;
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        arch,
    )
    .map_err(|e| format!("seccomp: {e}"))?;
    filter
        .try_into()
        .map_err(|e: BackendError| format!("seccomp: {e}"))
}

/// The program as the raw `struct sock_filter` array `bwrap --seccomp FD` reads.
pub fn program_bytes() -> Result<Vec<u8>, String> {
    let prog = program()?;
    let mut out = Vec::with_capacity(prog.len() * 8);
    for f in prog {
        out.extend_from_slice(&f.code.to_ne_bytes());
        out.push(f.jt);
        out.push(f.jf);
        out.extend_from_slice(&f.k.to_ne_bytes());
    }
    Ok(out)
}

/// Write the program to a new private file (mode 0600, created exclusively, so a file planted
/// by someone else in a shared temp directory makes this fail instead of being used).
pub fn write_filter_file() -> Result<PathBuf, String> {
    static N: AtomicU64 = AtomicU64::new(0);
    let bytes = program_bytes()?;
    let path = std::env::temp_dir().join(format!(
        "protocol-seccomp-{}-{}.bpf",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| format!("cannot create seccomp filter file {}: {e}", path.display()))?;
    f.write_all(&bytes)
        .map_err(|e| format!("cannot write seccomp filter file: {e}"))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn deny_list_has_no_duplicates_and_leaves_the_basics_alone() {
        let names: BTreeSet<_> = DENIED.iter().map(|(n, _)| *n).collect();
        assert_eq!(names.len(), DENIED.len(), "duplicate names");
        let nrs: BTreeSet<_> = DENIED.iter().map(|(_, n)| *n).collect();
        assert_eq!(nrs.len(), DENIED.len(), "duplicate syscall numbers");
        for essential in [
            libc::SYS_read as i64,
            libc::SYS_write as i64,
            libc::SYS_execve as i64,
            libc::SYS_mmap as i64,
            libc::SYS_futex as i64,
            libc::SYS_exit_group as i64,
            libc::SYS_clone as i64,
        ] {
            assert!(!nrs.contains(&essential), "essential syscall denied");
        }
    }

    #[test]
    fn program_compiles_and_serializes_to_whole_instructions() {
        let bytes = program_bytes().unwrap();
        assert!(!bytes.is_empty());
        assert_eq!(bytes.len() % 8, 0, "sock_filter is 8 bytes");
    }

    #[test]
    fn filter_file_is_private_and_matches_the_program() {
        use std::os::unix::fs::PermissionsExt;
        let p = write_filter_file().unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(std::fs::read(&p).unwrap(), program_bytes().unwrap());
        let _ = std::fs::remove_file(&p);
    }
}
