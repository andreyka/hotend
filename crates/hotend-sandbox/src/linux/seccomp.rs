//! The seccomp filters: a deny-list of system calls a confined child never needs.
//!
//! A deny-list was chosen over an allow-list on purpose. The workloads (CPython, numpy,
//! OpenCascade, vendor slicers) and glibc pick up new system calls with every release, and an
//! allow-list that kills them on the next update would push people to the unsafe override.
//! The price is that a system call added to the kernel later is allowed until it is listed
//! here; the capability drop and Landlock still apply to it.
//!
//! What the filters enforce:
//! - no socket can be created or used (network guarantee, with no other layer needed);
//! - nothing that needs privilege or leads out of the sandbox: ptrace, mounts, namespaces,
//!   BPF, keyrings, kexec, modules, perf, userfaultfd, io_uring, System V IPC;
//! - nothing that is counted against a quota of the whole user account and that a job has
//!   no use for: inotify instances (a few hundred per user), POSIX message queues;
//! - the process tree stays in the launcher's process group (`setsid`/`setpgid` are denied),
//!   which is what makes the wall-clock kill reach every descendant;
//! - file metadata outside the job folder cannot be changed. Landlock does not cover
//!   chmod, chown, utime or extended attributes, so these calls are neutralised everywhere:
//!   mode, owner and timestamp changes report success without doing anything, extended
//!   attributes report "not supported".
//!
//! Several filters are installed because one seccompiler filter has a single action. The
//! kernel applies the most restrictive result.
//!
//! One more filter, the signal guard ([`signal_guard`]), is installed only where neither the
//! PID namespace nor Landlock's signal scoping (ABI 6) exists: there it is the only thing
//! between the child and every other process of the user, the launcher included. Another,
//! the process-limit guard ([`nproc_guard`]), is installed where the process cap is the
//! job's soft `RLIMIT_NPROC`: there the limit is the supervisor's to move, not the job's.

use std::collections::BTreeMap;

use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen as Len, SeccompCmpOp as Op, SeccompCondition,
    SeccompFilter, SeccompRule, TargetArch, sock_filter,
};

type Rules = BTreeMap<i64, Vec<SeccompRule>>;

// System calls newer than some libc crate releases. Numbers from 424 on are the same on every
// architecture.
const SYS_OPEN_TREE: i64 = 428;
const SYS_MOVE_MOUNT: i64 = 429;
const SYS_FSOPEN: i64 = 430;
const SYS_FSCONFIG: i64 = 431;
const SYS_FSMOUNT: i64 = 432;
const SYS_FSPICK: i64 = 433;
const SYS_PIDFD_SEND_SIGNAL: i64 = 424;
const SYS_CLONE3: i64 = 435;
const SYS_PIDFD_GETFD: i64 = 438;
const SYS_PROCESS_MADVISE: i64 = 440;
const SYS_MOUNT_SETATTR: i64 = 442;
const SYS_QUOTACTL_FD: i64 = 443;
const SYS_FCHMODAT2: i64 = 452;
const SYS_LSM_SET_SELF_ATTR: i64 = 460;
const SYS_SETXATTRAT: i64 = 463;
const SYS_GETXATTRAT: i64 = 464;
const SYS_LISTXATTRAT: i64 = 465;
const SYS_REMOVEXATTRAT: i64 = 466;
const SYS_OPEN_TREE_ATTR: i64 = 467;
const SYS_FILE_SETATTR: i64 = 469;

// `setrlimit` is not in every libc's list for arm64; the kernel has it there under the
// number of the generic table.
#[cfg(target_arch = "x86_64")]
const SYS_SETRLIMIT: i64 = 160;
#[cfg(not(target_arch = "x86_64"))]
const SYS_SETRLIMIT: i64 = 164;

const NAMESPACE_FLAGS: [libc::c_int; 8] = [
    libc::CLONE_NEWNS,
    libc::CLONE_NEWCGROUP,
    libc::CLONE_NEWUTS,
    libc::CLONE_NEWIPC,
    libc::CLONE_NEWUSER,
    libc::CLONE_NEWPID,
    libc::CLONE_NEWNET,
    0x80, // CLONE_NEWTIME
];

/// The compiled filters, ready to install in the child.
pub(crate) struct Filters(Vec<BpfProgram>);

impl Filters {
    /// Installs every filter on the calling thread. `no_new_privs` must already be set.
    pub fn apply(&self) -> Result<(), String> {
        for program in &self.0 {
            seccompiler::apply_filter(program).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

/// The signal guard for a program whose process id is `program` (also the id of the
/// process group the whole tree stays in).
///
/// A seccomp filter sees numbers, not processes, so it cannot ask "is the target inside the
/// sandbox". What it can allow without letting a signal out:
/// - `kill` aimed at the caller's own process group (`0` or `-program`) or at the program;
/// - the thread-directed calls (`tgkill`, `tkill`, `rt_sigqueueinfo`, `rt_tgsigqueueinfo`)
///   when the target process is the program, which is what `raise`, `abort` and
///   `pthread_kill` use in the program itself.
///
/// Everything else that delivers a signal is refused with `EPERM`: other targets,
/// `pidfd_send_signal`, and making another process the owner of a descriptor's I/O signals
/// (`F_SETOWN`, `F_SETOWN_EX`, `FIOSETOWN`, `SIOCSPGRP`). The price: a subprocess of the
/// program cannot raise a signal in itself (its `abort` still ends it, through the fallback
/// in libc) and one process of the tree cannot signal a single other one.
pub(crate) fn signal_guard(program: libc::pid_t) -> Result<BpfProgram, String> {
    // Not in every release of the libc crate; the value is the same on all architectures.
    const F_SETOWN_EX: u64 = 15;
    const FIOSETOWN: u64 = 0x8901;
    const SIOCSPGRP: u64 = 0x8902;
    let arch = TargetArch::try_from(std::env::consts::ARCH)
        .map_err(|_| format!("no seccomp support for {}", std::env::consts::ARCH))?;
    // `pid_t` arguments are 32 bits wide; the kernel ignores the upper half.
    let own = u64::from(program as u32);
    let own_group = u64::from(program.wrapping_neg() as u32);
    let mut rules = Rules::new();
    rules.insert(
        libc::SYS_kill,
        vec![rule(vec![
            cond(0, Len::Dword, Op::Ne, 0)?,
            cond(0, Len::Dword, Op::Ne, own)?,
            cond(0, Len::Dword, Op::Ne, own_group)?,
        ])?],
    );
    for syscall in [
        libc::SYS_tgkill,
        libc::SYS_tkill,
        libc::SYS_rt_sigqueueinfo,
        libc::SYS_rt_tgsigqueueinfo,
    ] {
        rules.insert(
            syscall,
            vec![rule(vec![cond(0, Len::Dword, Op::Ne, own)?])?],
        );
    }
    always(&mut rules, &[SYS_PIDFD_SEND_SIGNAL]);
    rules.insert(
        libc::SYS_fcntl,
        vec![
            rule(vec![cond(1, Len::Dword, Op::Eq, libc::F_SETOWN as u64)?])?,
            rule(vec![cond(1, Len::Dword, Op::Eq, F_SETOWN_EX)?])?,
        ],
    );
    rules.insert(
        libc::SYS_ioctl,
        vec![
            rule(vec![cond(1, Len::Dword, Op::Eq, FIOSETOWN)?])?,
            rule(vec![cond(1, Len::Dword, Op::Eq, SIOCSPGRP)?])?,
        ],
    );
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        arch,
    )
    .map_err(|e| e.to_string())?;
    BpfProgram::try_from(filter).map_err(|e| e.to_string())
}

/// The process-limit guard: the calling process and its descendants can no longer set
/// their own `RLIMIT_NPROC` (`EPERM`). Reading it, and every other limit, still works.
///
/// It is installed where the process cap is that limit, set on the total of the user
/// (`nproc::cap_user_total`): the soft limit is the cap, the hard limit is the ceiling up
/// to which the supervisor moves the cap along with the user's other programs. Without
/// the guard the job could raise its soft limit to the ceiling itself.
pub(crate) fn nproc_guard() -> Result<BpfProgram, String> {
    let arch = TargetArch::try_from(std::env::consts::ARCH)
        .map_err(|_| format!("no seccomp support for {}", std::env::consts::ARCH))?;
    #[allow(clippy::unnecessary_cast)]
    let nproc = libc::RLIMIT_NPROC as u64;
    let mut rules = Rules::new();
    // prlimit64(pid, resource, new, old): a null `new` only reads.
    rules.insert(
        libc::SYS_prlimit64,
        vec![rule(vec![
            cond(1, Len::Dword, Op::Eq, nproc)?,
            cond(2, Len::Qword, Op::Ne, 0)?,
        ])?],
    );
    rules.insert(
        SYS_SETRLIMIT,
        vec![rule(vec![cond(0, Len::Dword, Op::Eq, nproc)?])?],
    );
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        arch,
    )
    .map_err(|e| e.to_string())?;
    BpfProgram::try_from(filter).map_err(|e| e.to_string())
}

/// Installs one compiled filter on the calling thread.
pub(crate) fn apply_program(program: &BpfProgram) -> Result<(), String> {
    seccompiler::apply_filter(program).map_err(|e| e.to_string())
}

/// Compiles the filters. `allow_socketpair` opens exactly one hole, for the agent's event
/// loop (see `Profile::allows_socketpair`): anonymous `AF_UNIX` stream pairs and plain
/// `send`/`recv` on them. `sendmsg`/`recvmsg` stay denied, so no descriptor can be passed.
pub(crate) fn build(allow_socketpair: bool) -> Result<Filters, String> {
    let arch = TargetArch::try_from(std::env::consts::ARCH)
        .map_err(|_| format!("no seccomp support for {}", std::env::consts::ARCH))?;
    let groups = [
        (denied(allow_socketpair)?, libc::EPERM),
        (not_implemented(), libc::ENOSYS),
        (silently_ignored(), 0),
        (xattrs(), libc::ENOTSUP),
    ];
    let mut programs = Vec::new();
    for (rules, errno) in groups {
        let filter = SeccompFilter::new(
            rules,
            SeccompAction::Allow,
            SeccompAction::Errno(errno as u32),
            arch,
        )
        .map_err(|e| e.to_string())?;
        programs.push(BpfProgram::try_from(filter).map_err(|e| e.to_string())?);
    }
    if cfg!(target_arch = "x86_64") {
        programs.push(x32_guard());
    }
    Ok(Filters(programs))
}

/// Refused with `EPERM`.
fn denied(allow_socketpair: bool) -> Result<Rules, String> {
    let mut rules = Rules::new();
    always(
        &mut rules,
        &[
            // Network. Stdio pipes need none of these.
            libc::SYS_socket,
            libc::SYS_connect,
            libc::SYS_bind,
            libc::SYS_listen,
            libc::SYS_accept,
            libc::SYS_accept4,
            libc::SYS_sendmsg,
            libc::SYS_recvmsg,
            libc::SYS_sendmmsg,
            libc::SYS_recvmmsg,
            libc::SYS_shutdown,
            libc::SYS_getsockname,
            libc::SYS_getpeername,
            libc::SYS_setsockopt,
            libc::SYS_getsockopt,
            // Other processes.
            libc::SYS_ptrace,
            libc::SYS_process_vm_readv,
            libc::SYS_process_vm_writev,
            libc::SYS_kcmp,
            SYS_PIDFD_GETFD,
            SYS_PROCESS_MADVISE,
            // Mounts and filesystem roots.
            libc::SYS_mount,
            libc::SYS_umount2,
            libc::SYS_pivot_root,
            libc::SYS_chroot,
            SYS_OPEN_TREE,
            SYS_OPEN_TREE_ATTR,
            SYS_MOVE_MOUNT,
            SYS_FSOPEN,
            SYS_FSCONFIG,
            SYS_FSMOUNT,
            SYS_FSPICK,
            SYS_MOUNT_SETATTR,
            libc::SYS_name_to_handle_at,
            libc::SYS_open_by_handle_at,
            libc::SYS_setns,
            // Kernel facilities with a history of privilege escalation.
            libc::SYS_bpf,
            libc::SYS_perf_event_open,
            libc::SYS_userfaultfd,
            libc::SYS_keyctl,
            libc::SYS_add_key,
            libc::SYS_request_key,
            libc::SYS_kexec_load,
            libc::SYS_kexec_file_load,
            libc::SYS_init_module,
            libc::SYS_finit_module,
            libc::SYS_delete_module,
            SYS_LSM_SET_SELF_ATTR,
            // System administration.
            libc::SYS_reboot,
            libc::SYS_swapon,
            libc::SYS_swapoff,
            libc::SYS_acct,
            libc::SYS_settimeofday,
            libc::SYS_clock_settime,
            libc::SYS_clock_adjtime,
            libc::SYS_adjtimex,
            libc::SYS_syslog,
            libc::SYS_quotactl,
            SYS_QUOTACTL_FD,
            libc::SYS_vhangup,
            libc::SYS_sethostname,
            libc::SYS_setdomainname,
            libc::SYS_fanotify_init,
            // The kernel allows each user a small number of inotify instances (128 by
            // default), counted for the account and not for the process: one job could
            // hold them all and stop every file watcher of its user. A job has nothing
            // to watch anyway, because Landlock does not mediate `inotify_add_watch`
            // and so that call is refused as well.
            libc::SYS_inotify_init1,
            libc::SYS_inotify_add_watch,
            // Landlock does not mediate this path-based call either.
            SYS_FILE_SETATTR,
            // Keep every descendant in the process group the launcher kills.
            libc::SYS_setsid,
            libc::SYS_setpgid,
        ],
    );
    #[cfg(target_arch = "x86_64")]
    always(
        &mut rules,
        &[libc::SYS_iopl, libc::SYS_ioperm, libc::SYS_inotify_init],
    );

    if allow_socketpair {
        const STREAM: u64 = libc::SOCK_STREAM as u64;
        const NONBLOCK: u64 = libc::SOCK_NONBLOCK as u64;
        const CLOEXEC: u64 = libc::SOCK_CLOEXEC as u64;
        rules.insert(
            libc::SYS_socketpair,
            vec![
                rule(vec![cond(0, Len::Dword, Op::Ne, libc::AF_UNIX as u64)?])?,
                rule(vec![
                    cond(1, Len::Dword, Op::Ne, STREAM)?,
                    cond(1, Len::Dword, Op::Ne, STREAM | NONBLOCK)?,
                    cond(1, Len::Dword, Op::Ne, STREAM | CLOEXEC)?,
                    cond(1, Len::Dword, Op::Ne, STREAM | NONBLOCK | CLOEXEC)?,
                ])?,
            ],
        );
        // `send` is `sendto` without an address; an address could name a socket file.
        rules.insert(
            libc::SYS_sendto,
            vec![rule(vec![cond(4, Len::Qword, Op::Ne, 0)?])?],
        );
    } else {
        always(
            &mut rules,
            &[libc::SYS_socketpair, libc::SYS_sendto, libc::SYS_recvfrom],
        );
    }

    // New namespaces would hand the child a full capability set inside them.
    let mut namespace_rules = Vec::new();
    for flag in NAMESPACE_FLAGS {
        let flag = flag as u64;
        namespace_rules.push(rule(vec![cond(0, Len::Dword, Op::MaskedEq(flag), flag)?])?);
    }
    rules.insert(libc::SYS_unshare, namespace_rules.clone());
    // CLONE_NEWTIME shares its bit with the exit signal of clone(2) and is not valid there.
    namespace_rules.pop();
    rules.insert(libc::SYS_clone, namespace_rules);

    // Typing into the terminal the launcher was started from. The request constants are
    // `c_ulong` with glibc and `c_int` with musl.
    #[allow(clippy::unnecessary_cast)]
    rules.insert(
        libc::SYS_ioctl,
        vec![
            rule(vec![cond(1, Len::Dword, Op::Eq, libc::TIOCSTI as u64)?])?,
            rule(vec![cond(1, Len::Dword, Op::Eq, libc::TIOCLINUX as u64)?])?,
        ],
    );
    // Only the plain Linux personality, or a query.
    rules.insert(
        libc::SYS_personality,
        vec![rule(vec![
            cond(0, Len::Dword, Op::Ne, 0)?,
            cond(0, Len::Dword, Op::Ne, 0xffff_ffff)?,
        ])?],
    );
    // Resource limits of other processes of the same user.
    rules.insert(
        libc::SYS_prlimit64,
        vec![rule(vec![cond(0, Len::Dword, Op::Ne, 0)?])?],
    );
    Ok(rules)
}

/// Refused with `ENOSYS`, which callers treat as "old kernel" and work around.
fn not_implemented() -> Rules {
    let mut rules = Rules::new();
    always(
        &mut rules,
        &[
            // Its flags live in a struct the filter cannot read. glibc falls back to clone(2),
            // where namespace flags are checked.
            SYS_CLONE3,
            // io_uring performs I/O, including socket calls, without passing the filter.
            libc::SYS_io_uring_setup,
            libc::SYS_io_uring_enter,
            libc::SYS_io_uring_register,
            // System V IPC and POSIX message queues are global unless an IPC namespace is in
            // place; denied always so behaviour does not depend on the optional layer.
            libc::SYS_shmget,
            libc::SYS_shmat,
            libc::SYS_shmctl,
            libc::SYS_shmdt,
            libc::SYS_semget,
            libc::SYS_semop,
            libc::SYS_semtimedop,
            libc::SYS_semctl,
            libc::SYS_msgget,
            libc::SYS_msgsnd,
            libc::SYS_msgrcv,
            libc::SYS_msgctl,
            libc::SYS_mq_open,
            libc::SYS_mq_unlink,
            libc::SYS_mq_timedsend,
            libc::SYS_mq_timedreceive,
            libc::SYS_mq_notify,
            libc::SYS_mq_getsetattr,
        ],
    );
    rules
}

/// Skipped while reporting success: mode, owner and timestamp changes.
fn silently_ignored() -> Rules {
    let mut rules = Rules::new();
    always(
        &mut rules,
        &[
            libc::SYS_fchmod,
            libc::SYS_fchmodat,
            SYS_FCHMODAT2,
            libc::SYS_fchown,
            libc::SYS_fchownat,
            libc::SYS_utimensat,
        ],
    );
    #[cfg(target_arch = "x86_64")]
    always(
        &mut rules,
        &[
            libc::SYS_chmod,
            libc::SYS_chown,
            libc::SYS_lchown,
            libc::SYS_utime,
            libc::SYS_utimes,
            libc::SYS_futimesat,
        ],
    );
    rules
}

/// Refused with `ENOTSUP`, like a filesystem without extended attributes.
fn xattrs() -> Rules {
    let mut rules = Rules::new();
    always(
        &mut rules,
        &[
            libc::SYS_setxattr,
            libc::SYS_lsetxattr,
            libc::SYS_fsetxattr,
            libc::SYS_removexattr,
            libc::SYS_lremovexattr,
            libc::SYS_fremovexattr,
            libc::SYS_getxattr,
            libc::SYS_lgetxattr,
            libc::SYS_fgetxattr,
            libc::SYS_listxattr,
            libc::SYS_llistxattr,
            libc::SYS_flistxattr,
            SYS_SETXATTRAT,
            SYS_GETXATTRAT,
            SYS_LISTXATTRAT,
            SYS_REMOVEXATTRAT,
        ],
    );
    rules
}

/// On x86-64 the x32 ABI reaches the same kernel functions through numbers with bit 30 set.
/// Those numbers match no entry of a deny-list, so the whole range is refused.
fn x32_guard() -> BpfProgram {
    const X32_SYSCALL_BIT: u32 = 0x4000_0000;
    // BPF_LD | BPF_W | BPF_ABS, BPF_JMP | BPF_JGE | BPF_K, BPF_RET | BPF_K
    const LOAD_WORD: u16 = 0x20;
    const JUMP_GE: u16 = 0x35;
    const RETURN: u16 = 0x06;
    let instruction = |code, jt, jf, k| sock_filter { code, jt, jf, k };
    vec![
        // Offset 0 of seccomp_data is the system call number.
        instruction(LOAD_WORD, 0, 0, 0),
        instruction(JUMP_GE, 0, 1, X32_SYSCALL_BIT),
        instruction(RETURN, 0, 0, libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32),
        instruction(RETURN, 0, 0, libc::SECCOMP_RET_ALLOW),
    ]
}

fn always(rules: &mut Rules, syscalls: &[i64]) {
    for syscall in syscalls {
        rules.insert(*syscall, Vec::new());
    }
}

fn rule(conditions: Vec<SeccompCondition>) -> Result<SeccompRule, String> {
    SeccompRule::new(conditions).map_err(|e| e.to_string())
}

fn cond(index: u8, len: Len, op: Op, value: u64) -> Result<SeccompCondition, String> {
    SeccompCondition::new(index, len, op, value).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn filters_compile_in_both_modes() {
        for allow_socketpair in [false, true] {
            let filters = build(allow_socketpair).unwrap();
            assert!(filters.0.len() >= 4);
            assert!(filters.0.iter().all(|program| !program.is_empty()));
        }
    }

    /// Runs `attempts` in a forked child that has installed the signal guard for itself and
    /// returns the bit mask of the attempts the kernel refused with `EPERM`.
    fn refused_under_signal_guard(attempts: &[&dyn Fn() -> libc::c_long]) -> i32 {
        // SAFETY: the forked child only makes system calls and exits; it never returns into
        // the test harness. The parent waits for exactly that child.
        unsafe {
            let pid = libc::fork();
            assert!(pid >= 0);
            if pid == 0 {
                // The filter is compiled after the fork because it names the child's pid.
                let Ok(guard) = signal_guard(libc::getpid()) else {
                    libc::_exit(101)
                };
                if apply_program(&guard).is_err() {
                    libc::_exit(102);
                }
                let mut mask = 0;
                for (bit, attempt) in attempts.iter().enumerate() {
                    *libc::__errno_location() = 0;
                    if attempt() < 0 && *libc::__errno_location() == libc::EPERM {
                        mask |= 1 << bit;
                    }
                }
                libc::_exit(mask);
            }
            let mut status = 0;
            assert_eq!(libc::waitpid(pid, &mut status, 0), pid);
            assert!(libc::WIFEXITED(status), "status {status}");
            libc::WEXITSTATUS(status)
        }
    }

    #[test]
    fn the_signal_guard_keeps_signals_inside_the_process_group() {
        // SAFETY: getpid and getppid cannot fail.
        let (test, parent) = unsafe { (libc::getpid(), libc::getppid()) };
        // SAFETY (all closures): raw system calls with integer arguments; signal 0 only
        // probes, and SIGCONT to the caller itself does nothing.
        let refused = refused_under_signal_guard(&[
            // Bits 0..=5: signals that would leave the sandbox.
            &|| unsafe { libc::syscall(libc::SYS_kill, test, 0) },
            &|| unsafe { libc::syscall(libc::SYS_kill, parent, 0) },
            &|| unsafe { libc::syscall(libc::SYS_kill, -1, 0) },
            &|| unsafe { libc::syscall(libc::SYS_tgkill, test, test, 0) },
            &|| unsafe { libc::syscall(libc::SYS_tkill, test, 0) },
            &|| unsafe { libc::syscall(SYS_PIDFD_SEND_SIGNAL, -1, 0, 0, 0) },
            // Bit 6: handing a descriptor's I/O signals to another process.
            &|| unsafe { libc::syscall(libc::SYS_fcntl, 0, libc::F_SETOWN, test) },
        ]);
        assert_eq!(refused, 0b0111_1111, "refused mask {refused:#b}");

        // What a program does to itself must keep working.
        let refused = refused_under_signal_guard(&[
            &|| unsafe { libc::syscall(libc::SYS_kill, libc::getpid(), libc::SIGCONT) },
            &|| unsafe {
                libc::syscall(
                    libc::SYS_tgkill,
                    libc::getpid(),
                    libc::getpid(),
                    libc::SIGCONT,
                )
            },
        ]);
        assert_eq!(refused, 0, "refused mask {refused:#b}");
    }

    /// Under the process-limit guard a process can read its limits and set every one but
    /// `RLIMIT_NPROC`, by either system call.
    #[test]
    fn the_process_limit_guard_refuses_only_changes_to_the_process_limit() {
        // SAFETY: the forked child only makes system calls and exits; the parent waits
        // for exactly that child.
        let code = unsafe {
            let pid = libc::fork();
            assert!(pid >= 0);
            if pid == 0 {
                let Ok(guard) = nproc_guard() else {
                    libc::_exit(101)
                };
                // Unprivileged processes need this before they may install a filter.
                libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
                if apply_program(&guard).is_err() {
                    libc::_exit(102);
                }
                let mut limit = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                let refused =
                    |result: libc::c_long| result < 0 && *libc::__errno_location() == libc::EPERM;
                let mut mask = 0;
                // Bit 0: reading works.
                if libc::getrlimit(libc::RLIMIT_NPROC, &mut limit) == 0 {
                    mask |= 1;
                }
                // Bits 1 and 2: setting it to what it already is is refused both ways.
                if refused(libc::syscall(
                    libc::SYS_prlimit64,
                    0,
                    libc::RLIMIT_NPROC,
                    &limit,
                    0usize,
                )) {
                    mask |= 2;
                }
                if refused(libc::syscall(SYS_SETRLIMIT, libc::RLIMIT_NPROC, &limit)) {
                    mask |= 4;
                }
                // Bit 3: another limit can still be set.
                if libc::getrlimit(libc::RLIMIT_CORE, &mut limit) == 0
                    && libc::setrlimit(libc::RLIMIT_CORE, &limit) == 0
                {
                    mask |= 8;
                }
                libc::_exit(mask);
            }
            let mut status = 0;
            assert_eq!(libc::waitpid(pid, &mut status, 0), pid);
            assert!(libc::WIFEXITED(status), "status {status}");
            libc::WEXITSTATUS(status)
        };
        assert_eq!(code, 0b1111, "mask {code:#b}");
    }

    /// Inotify instances are counted per user account, so a job gets none.
    #[test]
    fn no_inotify_instance_can_be_created() {
        let rules = denied(false).unwrap();
        assert_eq!(rules.get(&libc::SYS_inotify_init1).map(Vec::len), Some(0));
        assert_eq!(
            rules.get(&libc::SYS_inotify_add_watch).map(Vec::len),
            Some(0)
        );
        #[cfg(target_arch = "x86_64")]
        assert_eq!(rules.get(&libc::SYS_inotify_init).map(Vec::len), Some(0));
    }

    #[test]
    fn strict_mode_denies_every_socket_call() {
        let rules = denied(false).unwrap();
        for syscall in [
            libc::SYS_socket,
            libc::SYS_socketpair,
            libc::SYS_connect,
            libc::SYS_sendto,
            libc::SYS_recvfrom,
            libc::SYS_sendmsg,
            libc::SYS_recvmsg,
        ] {
            assert_eq!(rules.get(&syscall).map(Vec::len), Some(0), "{syscall}");
        }
    }
}
