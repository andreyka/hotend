# Sandbox notes

Component: `crates/hotend-sandbox` (library `hotend_sandbox`, launcher binary `hotend-sandbox`).
Contract: `docs/PROTOCOL.md` section 7. This note records how each guarantee is provided, what was
measured on which kernel, what is different inside the sandbox, and what is not covered.

## 1. Which mechanism gives which guarantee (Linux)

The launcher forks, the child confines itself and then `execve`s the program. Landlock and seccomp
are mandatory, and so is a kernel-enforced cap on the number of processes (section 1.1): if any of
them cannot be applied the launcher prints the reason and exits 125 without starting the program.
Namespaces are a second layer, used when the kernel allows them and skipped otherwise.
`hotend-sandbox probe` prints the same mapping for the machine it runs on.

| Guarantee | Primary mechanism (always) | Second layer (when available) |
| --- | --- | --- |
| 1 no network | seccomp: `socket`, `connect`, `bind`, `listen`, `accept*`, `sendmsg`, `recvmsg`, `sendmmsg`, `recvmmsg`, `getsockopt`, `setsockopt`, `shutdown`, `getsockname`, `getpeername`, `socketpair`, `sendto`, `recvfrom` return EPERM; `io_uring_*` returns ENOSYS; every inherited descriptor except 0, 1, 2 is closed before exec | empty network namespace (not even loopback is up); Landlock TCP bind/connect denial (ABI 4+) and abstract unix socket scoping (ABI 6+) |
| 2 filesystem | Landlock, ABI 3 or newer required: write/create/remove/rename/truncate only below `--job`; read only `--job`, each `--ro` path, and the system list in section 3. seccomp neutralises what Landlock does not cover: chmod/chown/utimes return success without doing anything, xattr calls return ENOTSUP. The launcher holds the job folder and every `--ro` path open (`O_PATH`) from before the fork until the tree is gone: on a filesystem without stable inodes (9p, which WSL2 uses for Windows drives) that is what keeps the Landlock rules attached to those folders | mount namespace with a private `/proc` that shows only the sandbox |
| 3 no privilege gain or escape | `no_new_privs`; the effective, permitted, inheritable and ambient capability sets are emptied, always. The bounding set is emptied when the launcher is root or the namespace layer is on; an unprivileged process outside a user namespace may not change its bounding set, and with `no_new_privs` and empty permitted sets nothing can be regained from it (a root launcher that cannot empty it is refused). seccomp denies ptrace, `process_vm_*`, `kcmp`, `pidfd_getfd`, the whole mount family including the new mount API, `setns`, `unshare` and `clone` with any namespace flag, `clone3` (ENOSYS, glibc falls back to `clone`), bpf, `perf_event_open`, userfaultfd, keyrings, kexec, module loading, reboot, swap, time setting, `open_by_handle_at`, `personality`, `TIOCSTI`, `prlimit64` on other processes, `inotify_init*` (section 1.2). Signals stay inside the sandbox through Landlock signal scoping (ABI 6+) or the PID namespace; where a machine has neither, a seccomp signal guard takes their place (below) | Landlock ptrace restriction (always, part of any Landlock domain); user + PID namespaces: no other process is visible or signalable |
| 4 resource caps | section 1.1 | section 1.1 |
| 4 whole-tree kill | The child calls `setsid()`; seccomp then denies `setsid` and `setpgid`, so no descendant can leave the process group. The supervisor kills with `kill(-pgid, SIGKILL)` on wall-clock expiry, on a limit, on SIGTERM/SIGINT/SIGHUP to the launcher, when the launcher's parent dies, and after the program itself exits (so nothing is left behind). A watchdog process covers the launcher's own death, also from SIGKILL (below) | PID namespace: when its first process dies the kernel kills everything in it; `cgroup.kill` where the job has a cgroup v2 cgroup |
| 5 clean environment | The library clears the launcher's environment and passes only allowlisted names; the launcher builds the child's environment from scratch: `PATH=/usr/local/bin:/usr/bin:/bin`, `HOME=<job>/.sandbox/home`, `TMPDIR=TMP=TEMP=<job>/.sandbox/tmp`, `LANG=C.UTF-8`, plus the `--env` names. Names that look like credentials (containing KEY, TOKEN, SECRET, PASSWORD, PASSWD, CREDENTIAL, ACCESS_CODE) and the reserved names are refused with exit 125 | none |

The seccomp filter is a deny-list (the engine's native stack, OCCT + numpy + threads, uses a wide
and version-dependent set of system calls; an allow-list would break on the next wheel update). It
is four `seccompiler` filters, one per action (EPERM, ENOSYS, "succeed without doing anything",
ENOTSUP), plus a four-instruction hand-written guard that answers any x32 system call number with
ENOSYS. The guard is needed because `seccompiler` checks the architecture word but not the x32 bit,
and the WSL2 kernel is built with `CONFIG_X86_X32_ABI=y` (section 2), where x32 numbers would
otherwise bypass a deny-list. A foreign architecture (32-bit `int 0x80`) kills the process.

**The seccomp signal guard.** Without the namespace layer and with Landlock older than ABI 6
(kernels 6.2 to 6.11), nothing else would stop the program from signalling the launcher, the
watchdog and every other process of its user. On such a machine a fifth filter is installed: `kill`
is allowed only towards the program's own process group or the program itself, the thread-directed
calls (`tgkill`, `tkill`, `rt_sigqueueinfo`, `rt_tgsigqueueinfo`) only towards the program, and
`pidfd_send_signal` and the ways of redirecting I/O signals (`F_SETOWN`, `F_SETOWN_EX`,
`FIOSETOWN`, `SIOCSPGRP`) not at all. A filter sees numbers, not processes, so the price is that a
subprocess of the program cannot raise a signal in itself (its `abort` still ends it) and one
process of the tree cannot signal a single other one. The probe lists this under `limitations`
when it applies. It does not apply on a kernel with Landlock ABI 6 or newer.

**The watchdog.** The parent-death signal reaches only the first process of the tree, and a
launcher killed with SIGKILL runs no cleanup. So the launcher forks a watchdog beside every job: a
process in a session of its own that holds nothing but the read end of a pipe whose only write end
the launcher holds, and ignores SIGTERM, SIGINT and SIGHUP. When the pipe closes without the
launcher's "done" byte, the launcher is gone, and the watchdog kills the job's process group
(which no member can leave), kills and removes the job's cgroup where there is one, and exits. So
no process of a job outlives its launcher, with or without namespaces. The job cannot signal the
watchdog: Landlock (ABI 6), the PID namespace or the signal guard keep its signals in.

Tests that prove the layering, not only the result: the integration tests run every scenario twice,
once with namespaces and once with `--no-namespaces`, so Landlock + seccomp alone are shown to hold.
As a mutation check during development, seccomp was disabled in the child (4 isolation tests
failed) and then Landlock (3 failed); both were restored. For the process cap, each of the three
mechanisms was loosened by ten tasks in turn; each time the tests that run a fork bomb failed.
The later additions were checked the same way, one at a time, and each made the test written
for it fail: `inotify_init1` allowed again; the pending-signal limit doubled; the supervisor no
longer following the user's other programs; no process-limit guard; a refused `fork` counted
without its control; the old ceiling for a user close to `ulimit -u`; the job's first process
counted twice; every tick with eight new process ids treated as a burst. With root sent into the
user-namespace mechanism on purpose, the launcher exited 125 at the step `process limit` and
started nothing.

### 1.1 What enforces each resource limit

| Limit | Enforced by | How exactly |
| --- | --- | --- |
| memory of one process (`--memory-mb`) | kernel: `RLIMIT_AS` | exact; the allocation fails |
| memory of the whole tree (`--memory-mb`) | supervisor: resident memory of every process of the group, added up from `/proc/<pid>/stat` | polled every 100 ms, every 10 ms above half the limit; the tree can overshoot by what it touches between two polls. Not kernel-enforced |
| CPU time of one process (`--cpu-seconds`) | kernel: `RLIMIT_CPU`, soft = hard, so SIGKILL | exact, per process |
| wall clock (`--wall-seconds`) | supervisor: a deadline on the wait for the first process; then the tree is killed, exit 124 | exact to a few milliseconds while the launcher runs; the watchdog covers a dead launcher; nothing covers a kernel that stops scheduling (section 5) |
| number of processes (`--max-processes`) | kernel, by one of three mechanisms (next table); the supervisor's exact count kills a tree that reaches the limit, exit 123 | next table |
| size of one file (`--max-file-mb`) | kernel: `RLIMIT_FSIZE` | exact |
| everything in the job folder (`--max-disk-mb`, 100 000 entries, 64 levels) | supervisor: walks the job folder from outside the sandbox, never following a link; a file counts with the larger of its length and the space it occupies | polled every 100 ms, or four times as long as the last walk took; the job can overshoot by what it writes between two walks. Not kernel-enforced |
| open files (`--max-open-files`) | kernel: `RLIMIT_NOFILE` | exact, per process |
| core dumps | kernel: `RLIMIT_CORE` = 0 | none are written |
| queued signals and POSIX timers (no flag: 1024 per job) | kernel: `RLIMIT_SIGPENDING` | exact inside the job's user namespace; without one the count is the user's (section 1.2) |
| inotify instances, POSIX message queues (no flag: none) | seccomp: `inotify_init`, `inotify_init1` return EPERM, the `mq_*` calls ENOSYS; `RLIMIT_MSGQUEUE` = 0 on top | none can be created (section 1.2) |

The process cap is the one limit whose mechanism depends on who runs the launcher. It is never
left to polling alone: where the kernel offers no mechanism the launcher starts nothing.

| Who runs the launcher | Mechanism | What the kernel guarantees for `--max-processes N` | `probe`: `process_cap` |
| --- | --- | --- | --- |
| an ordinary user, namespace layer on | `RLIMIT_NPROC` = N inside the job's own user namespace | exact: the namespace never holds more than N tasks. Its init process (a copy of the launcher that only reaps) is one of them, so the program's tree gets N − 1. The user's own limit stays in force above the namespace: a user whose `ulimit -u` leaves less room than N gets less | `rlimit-in-user-namespace`, `kernel-exact` |
| anyone whose launcher can create a pids cgroup inside its own cgroup (the layouts are below), when the row above does not apply. Root is always in this row or the last one | `pids.max` = N of a cgroup made for the job | exact: the cgroup never holds more than N tasks (with the namespace layer, the namespace's init is one of them) | `cgroup-pids`, `kernel-exact` |
| an ordinary user, no namespace layer, no such cgroup | `RLIMIT_NPROC` = the tasks the user runs outside the job + N + 1024, taken when the job starts and again every second | approximate: the job cannot push the user's task count above that number, nor into the part of the user's own limit that is kept free (the last eighth of `ulimit -u`). The job itself can therefore pass N by at most 1024 plus whatever the user's other programs ended since the count was last taken, and only until the supervisor's count catches it | `rlimit-on-user-total`, `kernel-approximate` |
| root, no such cgroup | none | the launcher exits 125 and starts nothing | `none`, `refused` |

Why three mechanisms (the kernel's rule is in `copy_process`, `kernel/fork.c`, read in the 6.18
source and tried on the test machine):

- `RLIMIT_NPROC` compares the number of tasks of the caller's real user id, in the caller's user
  namespace and those below it, with the caller's own soft limit. The count belongs to the user,
  the limit to the process. In a fresh user namespace the count starts at one, so the limit is a
  per-job cap. Outside one, the count is everything the numeric user id runs, in every PID
  namespace and every container that does not have a user namespace of its own. The count is
  kept per user namespace since Linux 5.14 (when it moved into the kernel's `ucounts`); on an
  older kernel a limit set inside the namespace would be compared with the user's total. The
  launcher needs Landlock ABI 3, which is Linux 6.2, so it never runs a job on such a kernel.
- It is not applied when the real user is root of the initial user namespace, whatever
  capabilities the process still holds (`real_cred->user == INIT_USER`), nor to a process with
  `CAP_SYS_RESOURCE` or `CAP_SYS_ADMIN` there. Tried: root with every capability dropped
  forks freely under `RLIMIT_NPROC` = 0, also inside a user namespace that maps 0 to 0; uid 65534
  is refused. So root needs something else, and that is the pids cgroup.
- The launcher does not guess from the user id. It tries a fork under a limit of 0 in a
  throwaway child (after dropping capabilities), which also gives the right answer for "root"
  inside a container whose user namespace maps it to an ordinary id. `fork` fails with EAGAIN
  for other reasons too (a full pids cgroup around the launcher, the machine's thread limit),
  so the refusal counts only together with its control: the same child must be able to fork
  under the limit it started with. A trial that cannot tell is repeated once and then treated
  as "not enforced" (`process_cap.rlimit_detail` says why); the 125 message then names the
  failed fork as the cause, not root. In the user-namespace mechanism the cap is tried once
  more where it counts: inside the new namespace, before the program is executed, a fork under
  a limit of 0 must be refused, or the launcher exits 125.
- Dropping from root to an unprivileged user id was considered and rejected. The job folder and
  the `--ro` paths belong to root with whatever modes the caller chose, so the launcher would have
  to change the ownership of the caller's data, and every file a job writes would belong to the
  other user. And there is no user id that is the launcher's to use: `nobody` is shared with
  other services, so its task count is not the job's either. Root therefore gets a cgroup or no
  job.

**The cgroup.** The job's cgroup is always a child of the cgroup the launcher itself runs in, so
every limit an administrator put on the launcher still applies to the job, and whatever kills the
launcher's cgroup kills the job. The launcher creates it before the fork, moves the child in while
the child waits at a pipe, and only then lets the child go on, so no process of the job ever runs
outside it. Nothing in the sandbox can raise the cap or leave: the cgroup files are outside the
Landlock rules (tested, also for a job that runs as the owner of those files). The launcher
removes the cgroup when the tree is gone; if the launcher was killed, its watchdog does; a cgroup
left by a launcher that died together with its watchdog is removed by the next launcher that
finds it, once it is 30 seconds old and its launcher's process id is gone.

A new cgroup is tried before it is trusted, for every job and in the probe: the launcher sets
its `pids.max` to 1, moves a throwaway process into it and has that process try a `fork`, which
the kernel must refuse. A cgroup that fails (the launcher may not move a process there, or the
fork goes through) is not used. Three layouts give a child cgroup with the pids controller:

- A cgroup v1 `pids` hierarchy the launcher may write to: v1 lets one
  cgroup hold both processes and child cgroups.
- cgroup v2 with the launcher in the root cgroup, which is where every process is on a machine
  without a service manager (WSL2 without systemd). The root cgroup may hold processes and pass
  controllers on to its children at once. Where `pids` is not yet in its
  `cgroup.subtree_control`, the launcher writes `+pids` there and leaves it: by itself that
  limits nothing, and it is what a service manager sets on every machine it runs.
- cgroup v2 with the launcher in any other cgroup that it may write to: the job's cgroup is
  made a **threaded** cgroup. The kernel's rule that a cgroup which holds processes cannot pass
  controllers on to child cgroups covers the domain controllers (memory, io) only. `pids` is a
  threaded controller (`kernel/cgroup/pids.c`: `.threaded = true`), and those may be enabled in
  a cgroup that holds processes (`cgroup_vet_subtree_control_enable` in
  `kernel/cgroup/cgroup.c`, and "Threads" in the kernel's `cgroup-v2.rst`). The launcher makes
  the child, writes `threaded` to its `cgroup.type`, writes `+pids` to the
  `cgroup.subtree_control` of its own cgroup, and sets the child's `pids.max`. What is different
  in this layout:
  - The launcher's own cgroup is a "threaded domain" for as long as a job cgroup exists in it.
    Memory and I/O of the job are accounted in the launcher's cgroup as before, the job's
    processes are listed in the launcher's `cgroup.procs`, and a kill of that cgroup reaches
    them. While it lasts, no ordinary (domain) child cgroup can be put to use in the
    launcher's cgroup by anyone else.
  - The kernel offers no `cgroup.kill` in a threaded cgroup, and its `cgroup.procs` cannot be
    read. The tree is killed through its process group, as in every mode.
  - When the job is gone the launcher puts its cgroup back: once no job cgroup of any launcher
    and no other child cgroup is left there, it writes `-pids`, and the cgroup is an ordinary
    domain again. Taking the controller away while another launcher's job still relied on it
    would leave that job without its cap, so the launchers of one cgroup keep a lock on it
    (`flock` on the cgroup directory): shared for as long as a job cgroup exists, exclusive
    and without waiting to take the controller away. A launcher that does not get the
    exclusive lock leaves the job to the last one out.
  - It does not work where the launcher's cgroup has child cgroups with processes in them, or
    passes memory or io on to its children; and not where the launcher may not write to its
    own cgroup. The launcher then says so in `process_cap.cgroup_detail` and falls back to
    the next row of the table.

  Who this is for: root in a login session or a service (systemd puts it in a scope or service
  cgroup that root may write to), root in a container whose cgroup mount is writable, and an
  ordinary user whose launcher runs in a cgroup that belongs to that user, which is what the
  units below `user@UID.service` are (applications started from a desktop session). A login
  over SSH or on a console sits in `session-N.scope`, which belongs to root: an ordinary user
  gets nothing from it and keeps the cap on the user's total.

**Neither cgroup v2 layout has run on a hierarchy that carries the pids controller.** The test
machine has the controller on cgroup v1. What was tried on its cgroup v2 hierarchy (kernel 6.18,
by hand and through the launcher): a threaded child in a cgroup that holds processes, the parent turning into a
threaded domain and back when the child is removed, moving a process into the threaded child
through `cgroup.procs`, `cgroup.kill` and reading `cgroup.procs` failing there with EOPNOTSUPP,
`flock` on a cgroup directory, and the launcher's failure path (no pids controller to pass on:
the reason is reported, nothing is left behind, the parent's type is unchanged). What rests on
the kernel source and documentation alone: that `+pids` is accepted in a cgroup with processes,
that `pids.max` of the threaded child then caps the job, and that the scopes of a systemd user
session have `pids` in their `cgroup.controllers` (systemd turns task accounting on by
default). The launcher does not depend on that reading being right: a cgroup that does not
refuse the trial fork is not used. The first run on a cgroup v2 machine should be the crate's
tests as root with `HOTEND_TEST_REQUIRE_SANDBOX=1` (section 2).

Not built: a transient systemd scope per job (`systemd-run --user --scope -p TasksMax=N`, or the
same over D-Bus), which would give an ordinary user an exact cap in a login session whose scope
belongs to root.

**The cap on the user's total.** There is no file that shows the kernel's count for a user, and
adding up `/proc` gives a different number wherever some of the user's processes are invisible
(another PID namespace) or run under other ids inside a user namespace the user owns. So the child
measures the count with the check itself: with capabilities dropped, it lowers its own soft limit
and tries a `fork`; the smallest limit at which the fork still works is the count plus the new
process. A bisection finds it in about twenty forks (a whole launch of `/bin/true` without
namespaces takes about 9 ms on the test machine, against 7 ms with them). The count includes the
job's first process, the launcher and its watchdog. Then the child sets two limits:

- the soft limit, which is the cap: the count without the job's own first process + N + 1024.
  A job can therefore hold N + 1024 tasks when nothing else changes (1088 for a limit of 64,
  measured with the launcher stopped);
- the hard limit, which is the ceiling the cap can never pass: seven eighths of the user's own
  limit (the soft `ulimit -u`, because that is what the user's other programs run under). A
  user who already runs inside the last eighth is not turned away: the ceiling is then what
  the user runs plus half of what is free, so the other half stays for the user's other
  programs (3604 tasks under `ulimit -u 4096`: the job may take 246 of the 492 free places).
  Only a user with no room for a single `fork` gets no job (exit 125).

Other programs of the user keep their own, much higher limit, so they fork normally while a job
sits at its cap. The job cannot raise the hard limit (no process can), and it cannot move the
soft one either: a seccomp filter, installed only in this mode, refuses `setrlimit` and
`prlimit64` for `RLIMIT_NPROC` with EPERM. The soft limit is the supervisor's to move:

- Once a second the supervisor counts, in `/proc`, the tasks of the user that are not in the
  job's process group and sets the soft limit of every process of the job (`prlimit`) to that
  count + N + 1024, never above the hard limit. It goes up when the user starts other programs
  and down again when they end. Without this a job lost `fork` and thread creation for good
  once the user's other programs had grown by 1024 tasks (opening a browser during an agent
  run is enough); with it the test starts 1200 threads beside a job, and the job still starts
  threads and processes afterwards.
- `/proc` is used for this count, not the kernel's own check as at the start, because the job
  cannot change what `/proc` shows outside its process group, while it could time forks of its
  own against a measurement that counts them and so talk its cap up. What `/proc` does not
  show (tasks of the same numeric user id in another PID namespace, which the kernel counts
  all the same) is taken from the difference at the start and assumed to stay as it was.
- When the job has fewer than N tasks and a fork under its cap would still be refused (tried
  in a throwaway process, because only the kernel knows its count), the launcher prints one
  line: `hotend-sandbox: the job cannot start another process or thread at the moment ...`.
  That happens when the user is at the ceiling, or when tasks the launcher cannot see have
  grown. The job's `fork` fails with EAGAIN then, and the core can report why.

What makes this approximate:

- The 1024 are an allowance for what the user's other programs start between two counts. The
  price is that a fork bomb can reach N + 1024 tasks before it is killed.
- If the user's other programs end tasks, the job can use their places until the next count,
  a second later, or for as long as the launcher is stopped.
- Jobs of the same user that run at the same time share the count: a bomb in one takes the
  headroom of the others until its supervisor kills it.
- The count is the kernel's for the numeric user id. A job that runs as uid 1000 in a container
  without a user namespace shares it with uid 1000 in every other such container and on the
  host. Growth there is not seen in `/proc`, so the cap does not follow it; the line on stderr
  is all the launcher can do.
- A user who is within N + 1024 of the ceiling gets a smaller cap, with a line on stderr.

**The supervisor.** Whatever the kernel mechanism, the launcher counts the tasks of the job
exactly (the cgroup's `pids.current`, or the threads of every process of the process group from
`/proc`) and kills a tree that has N of them: exit 123 and `process limit of N reached (M tasks)`
on stderr. It wakes every 10 ms (every 50 ms once nothing has forked for two seconds) and reads
one small file: with a cgroup the task count itself, otherwise the last process id the kernel
handed out (`/proc/loadavg`). That number is the machine's, not the job's: a build or a shell
loop elsewhere moves it hundreds of times a second. So a tick counts as a burst, and makes the
supervisor walk `/proc` at once, only when it brought at least eight new process ids and more
than three times the running average of the ticks before it; steady activity stops triggering
after three or four ticks, a fork bomb on top of it still stands out. The early walk happens no
more often than every 10 ms or nine times as long as a walk takes. The regular walk, which also
adds up the memory, stays at every 100 ms, and every 10 ms above half of a limit. Measured
(debug build, idle job, launcher CPU after 7 s): 120 to 140 ms on a quiet machine, 130 to 140 ms
beside an unrelated shell loop that starts 690 processes a second; with every tick of eight new
ids treated as a burst, as it was at first, the same loop cost 500 to 510 ms.

### 1.2 What the kernel counts for the whole user account

The task count is not the only allowance the kernel keeps per user id instead of per process. A
job that takes all of such an allowance leaves the sandbox intact but stops something for every
other program of its user, for as long as it runs. What was looked at, and what is done:

| Allowance (default) | What one job could do | Now |
| --- | --- | --- |
| inotify instances (`fs.inotify.max_user_instances`, 128) | Measured before the change: one process took all 128, and `inotify_init1` in another program of the same user failed with EMFILE while the job ran, with and without namespaces. File watchers, IDEs and sync tools of the user stop working | seccomp refuses `inotify_init` and `inotify_init1` (EPERM). A job had nothing to watch anyway: `inotify_add_watch` was already refused, because Landlock does not mediate it |
| queued signals and POSIX timers (`RLIMIT_SIGPENDING`; the default scales with memory) | Measured before the change: one process blocked a real-time signal and queued as many as the limit allows to itself; `sigqueue` in another program of the same user then failed with EAGAIN, with and without namespaces. Real-time signals and `timer_create` fail for the whole account | `RLIMIT_SIGPENDING` = 1024 for the job (seven eighths of the launcher's own limit where that is less). Inside the job's user namespace the kernel counts the job alone: exact. Without one (root in a cgroup, an ordinary user without the namespace layer) the count is the user's: the job cannot push it past 1024, and it gets EAGAIN from `sigqueue`, real-time signals and `timer_create` while the user's other programs hold more than that. Ordinary signals are delivered at the limit all the same. Measured after the change, in all four modes: 1024 queued, the other program unaffected |
| POSIX message queue bytes (`RLIMIT_MSGQUEUE`) | nothing: `mq_open` was already refused | `RLIMIT_MSGQUEUE` = 0 as a second lock |
| epoll watches (`fs.epoll.max_user_watches`; it scales with memory) | **Not capped.** The kernel counts them per user id whatever the user namespace (`fs/eventpoll.c`). One process can register about a quarter of the square of its descriptor limit, so a job with the engine's limits (512 processes of 1024 descriptors) can pass the allowance; `epoll_ctl` of the user's other programs then fails with ENOSPC until the job ends. Read from the source, not tried: it needs several hundred megabytes of kernel memory on a shared machine | open. `epoll` cannot be refused (asyncio needs it), and there is no per-process limit for it |
| pipe buffer memory (`fs.pipe-user-pages-soft`, 16384 pages) | **Not capped.** Measured: four processes of a job that hold about 510 pipes each put their user over the allowance, and every new pipe of that user, in any program, then gets 8 KiB instead of 64 KiB while the job runs. It slows programs down and fails nothing; there is no hard limit by default, and root is exempt | open |

`hotend-sandbox probe` lists the two open rows under `limitations`.

## 2. What was measured where

### The test virtual machine

Linux 6.18 in a virtual machine (not WSL2), Landlock ABI 7, seccomp filter available,
user/PID/network/IPC/UTS/mount namespaces usable, running as root, no Yama LSM,
`CONFIG_X86_X32_ABI` not set, no systemd. Cgroups are the hybrid layout: the pids controller is on
a cgroup v1 hierarchy that root may write to, and the cgroup v2 hierarchy carries no pids
controller. The first process of the machine collects orphaned processes only every second or two,
so the killed processes of a job without a PID namespace linger that long as zombies; they hold no
resources but count for their user until then.

All tests pass there in every mode the machine offers. Because the tests run as root, the ones
about the process cap run each scenario four times: as root (cgroup) and as an unprivileged user id
that nothing else uses (one per test), each with and without namespaces. One more test re-runs the
whole confinement as uid 65534.

The test binaries were also run as an ordinary user (through `setpriv`). Everything that runs
there passes, but less runs: a test that needs a second user id, or a cgroup to hand over, prints
`SKIPPED` with the reason and passes. Those are the bomb against the cap on the user's total with
the launcher stopped, the three tests of the cap that follows the user's other programs, the small
`ulimit -u`, the delegated cgroup, the full cgroup around the launcher, the cgroup escape and the
refusal of root. The bomb without namespaces does run there, against the tests' own user. What the
kernel bounds in that case is the user's total, and the other tests of the same binary start and
end tasks of that user all the time, so the test asserts what holds then: the job stays below its
hard limit (the ceiling of section 1.1), and that ceiling is below the user's own limit. With
eighteen test threads and the launcher slowed down, six such runs passed with peaks up to 1188
tasks. Setting `HOTEND_TEST_REQUIRE_SANDBOX=1` turns the one skip that would hide everything ("the
sandbox is not usable on this machine") into a failure; the skips for lack of root stay skips.

Fork bomb (`while True: os.fork()`, on failure sleep 10 ms and retry) with `--max-processes 64`,
debug build, three runs of the test suite at default parallelism. "Peak" is the largest number of
live tasks in the job's process group, sampled from `/proc` as fast as a thread can, and the
count the supervisor reported when it killed. "Gone after" runs from the moment the bomb was
released until the launcher had returned and no process of the job was left; it includes the time
the bomb needs to grow, and the other tests (three more bombs among them) run at the same time. A
sibling process of the same user, outside the sandbox, forked every 2 ms throughout.

| Mode | Mechanism | Peak live tasks | Gone after | Sibling forks refused (worked) |
| --- | --- | --- | --- | --- |
| root, namespaces | cgroup v1 `pids.max` | 64 | 72 to 87 ms | 0 (22 to 23) |
| root, no namespaces | cgroup v1 `pids.max` | 64 | 26 to 68 ms | 0 (7 to 18) |
| uid 65533, namespaces | `RLIMIT_NPROC` in the user namespace | 64 | 97 to 182 ms | 0 (19 to 35) |
| uid 65533, no namespaces | `RLIMIT_NPROC` on the user's total | 349, 397 and 1013 (kernel bound 1088) | 112 to 370 ms | 0 (24 to 65) |

In the last row the supervisor's count is what stops the job, and it is only as fast as the
machine lets it be: in the run that reached 1013 tasks the launcher shared the CPUs with several
hundred forking processes and needed 276 ms. Three earlier runs of the same test gave peaks of
241, 384 and 518. The number that holds in every run is the kernel's: 1088 tasks of the job, and
the test allows one more for a child of the sibling that was counted and ended a moment later.

The same bomb with the launcher stopped (SIGSTOP) for 700 ms, so that nothing counts, nothing
kills and nothing moves the cap. This is the kernel's cap alone:

| Mode | Peak live tasks while unsupervised | Killed after the launcher resumed | Sibling forks refused (worked) |
| --- | --- | --- | --- |
| root, namespaces | 64 | 11 to 14 ms | 0 (123 to 164) |
| root, no namespaces | 64 | 13 to 21 ms | 0 (176 to 188) |
| uid 65532, namespaces | 64 | 1 to 40 ms | 0 (145 to 179) |
| uid 65532, no namespaces, `ulimit -u 320` | 277 = 280 (seven eighths of 320) less the launcher, its watchdog and the sibling | 43 to 61 ms | 0 (162 to 199) |
| uid 65521, no namespaces, default `ulimit -u`, stopped for 1 s, run by hand | 1088 = 64 + 1024; the job's limits were 1090 (soft: launcher, watchdog, 64, 1024) and seven eighths of `ulimit -u` (hard) | 129 ms | not run |
| uid 65530, no namespaces, in a cgroup of its own | 64 | 8 to 25 ms | 0 (151 to 164) |

On a machine with more CPUs the first table's fourth row will show the kernel bound (1088) more
often: many CPUs fork faster than a 10 ms tick can catch.

What a job can do to the allowances of its user's account (section 1.2), as uid 65525 by hand
before the change and as uid 65528 in the test `a_job_cannot_use_up_the_allowances_of_its_users_account`
after it, with and without namespaces:

| Attempt inside the job | Before | Now |
| --- | --- | --- |
| open inotify instances until refused | 128 held; `inotify_init` of a program of the same user outside the job failed with EMFILE | none: EPERM; the program outside gets its instance |
| queue real-time signals to a blocked process until refused | as many queued as the user's limit allows; `sigqueue` of a program outside failed with EAGAIN | 1024 queued, then EAGAIN in the job; the program outside queues its signal |

### A WSL2 machine (ordinary user)

- With namespaces, `RLIMIT_NPROC` capped a fork bomb in the kernel at exactly `max_processes`
  (64 tasks in 0.1 s).
- With `--no-namespaces`, an earlier version had no cap in the kernel, and a fork bomb used up the
  user's own `ulimit -u` for a few seconds. The cap on the user's total (section 1.1) replaced
  that; it has not been measured on a WSL2 machine.
- With `--no-namespaces` as an ordinary user, the launcher first tries a cgroup. A shell is
  expected to sit in a cgroup that belongs to root (the root cgroup without systemd, a session
  scope with it), so the cap on the user's total applies: the kernel holds the bomb test's job at
  64 + 1024 tasks, and the cap follows the user's other programs. Should the launcher find a
  cgroup v2 it can write to, it takes the threaded-cgroup route, which has not run anywhere
  (below).
- As root (`wsl -u root`, or a container) the launcher needs a cgroup. Current WSL2 is reported to
  mount cgroup v2 only, so root gets the cgroup v2 layouts of section 1.1 and nothing else. They
  prove themselves before every job, and if the proof fails the launcher exits 125 with the
  reason; they have not run on a hierarchy that carries the pids controller, so which of the two
  happens there is not known.

### WSL2 kernels (read from Microsoft's published kernel configs, not run)

Source: `arch/x86/configs/config-wsl` in the public `microsoft/WSL2-Linux-Kernel` repository, tags
of the 5.15, 6.6 and 6.18 lines (the arm64 configs of 6.6 and 6.18 agree on these options).

| WSL2 kernel | Landlock | Result for Hotend |
| --- | --- | --- |
| 5.15.x (WSL installs not updated since 2024) | ABI 1: no rename/link control across directories, no truncate control | launcher exits 125 with a message telling the user to run `wsl --update` |
| 6.6.x (the long-standing default after `wsl --update`) | ABI 3: full filesystem control including truncate; no TCP rules, no signal or abstract-socket scoping | usable; network is still fully closed by seccomp + network namespace; signal isolation comes from the PID namespace |
| 6.18.x (current kernel line in November 2026) | ABI 7 | usable, same feature level as the test machine |

All three have `landlock` in `CONFIG_LSM`, `CONFIG_SECCOMP_FILTER`, user/PID/net/IPC namespaces,
`CONFIG_X86_X32_ABI=y` and IA32 emulation enabled. WSL2 does not ship AppArmor restrictions on
unprivileged user namespaces, so the namespace layer is expected to be available there; a run on a
6.18 WSL2 kernel confirmed it.

Not verified: the ABI 3 path was reasoned from the `landlock` crate's compatibility handling
(filesystem rights are hard requirements, newer features are best-effort) and from the kernel
configs; it has not been executed on a 6.6 kernel. The cgroup layout of WSL2 was not looked at
on a WSL2 machine either: that current WSL2 mounts only cgroup v2 at `/sys/fs/cgroup` is a
report, not a measurement. All three configs set `CONFIG_CGROUP_PIDS=y`, so the controller is
there for the cgroup v2 layouts of section 1.1.

### Landlock unavailable

There is no fallback that provides guarantee 2 without Landlock. Below ABI 3, or with Landlock
disabled at boot, the launcher fails closed (exit 125, reason on stderr and in `probe`). A
mount-namespace root (bind mounts + `pivot_root`) was considered and left out: it only works where
unprivileged user namespaces work, which is not the case on every desktop distribution, and it
would be a second filesystem policy to keep in step with the first.

### Native Ubuntu 24.04 and similar

Ubuntu 23.10+ restricts unprivileged user namespaces through AppArmor. The launcher probes by
actually creating the namespaces in a throwaway child, so it will run without that layer there.
With a 6.8 kernel (Landlock ABI 4) and no namespaces, guarantees 1, 2 and 5 hold unchanged; signals
are kept in by the seccomp signal guard (section 1), with the limitation described there; the
capability bounding set is not emptied; and the process cap is the exact one where the launcher's
own cgroup belongs to the user (an application started from the desktop session runs in one;
the launcher then makes a threaded cgroup per job in it, section 1.1) and the one on the user's
total otherwise, which includes a launcher started from a login shell. Not
verified on such a machine: what was run on the test machine with `--no-namespaces` as an unprivileged user, on
a kernel with Landlock ABI 7, is the cap on the user's total and a delegated cgroup v1; the
threaded cgroup v2 route has not run (below), and the signal guard itself has only its unit test.

### What has not run anywhere

- Both cgroup v2 layouts on a hierarchy that carries the pids controller: the launcher in the
  root cgroup (`+pids` written to the root's `cgroup.subtree_control`), and threaded cgroups
  inside the launcher's own cgroup. The test machine has the pids controller on cgroup v1, and its
  cgroup v2 hierarchy does not carry it. What did run, by hand on that hierarchy: making
  a threaded child of a populated cgroup, moving a process into it, the parent turning "domain
  threaded", `cgroup.kill` refusing in a threaded cgroup, `flock` on a cgroup directory, and the
  launcher's failure path, which ends in exit 125 with the list of controllers and leaves no
  cgroup behind. What did not: `pids.max` appearing and counting in such a child, the proof
  passing, and taking `-pids` back from the parent after the last job. Because each job's cgroup
  is proven with a fork that must fail before the job starts, a kernel that behaves differently
  from what the source says gives exit 125 (for an ordinary user: the cap on the user's total),
  not a job without a cap; that is the claim, and it
  rests on the proof having run on cgroup v1 only. A run on a machine with a cgroup v2 kernel
  would settle it.
- The unit tests for finding the cgroup mounts run on text copied from `/proc/self/mountinfo`
  layouts, not on such machines.
- A run as root on a machine with no writable cgroup, other than the test that hides the cgroup
  mounts from the launcher in a mount namespace of its own (the probe then reports
  `usable: false`, every profile exits 125 and the program is not started), and one run by hand
  with only the test machine's cgroup v2 mount visible (exit 125: `cgroup v2 has no pids controller
  to pass on`).
- The epoll-watch allowance of section 1.2 was read from the kernel source and not exhausted.

## 3. What is different inside the sandbox

Things the engine, agent and slicer authors need to know.

- Working directory is `--job`. `HOME` and `TMPDIR` are `<job>/.sandbox/home` and
  `<job>/.sandbox/tmp`, created by the launcher. The job folder must be an existing directory
  given as an absolute path; the launcher refuses a `.sandbox` entry in it that is a symlink or
  not a directory.
- Readable system paths: `/usr`, `/lib`, `/lib64`, `/lib32`, `/bin`, `/sbin`, `/etc/ld.so.cache`,
  `/etc/ld.so.conf`, `/etc/ld.so.conf.d`, `/etc/alternatives`, `/etc/localtime`, `/etc/mime.types`,
  `/etc/os-release`, `/etc/ssl/openssl.cnf`, `/sys/devices/system/cpu`, `/proc/cpuinfo`,
  `/proc/meminfo`, `/proc/stat`, `/proc/loadavg`, `/proc/sys/vm/overcommit_memory`, the program's own
  `/proc/<pid>`, `/dev/zero`, `/dev/urandom`, `/dev/random` (read) and `/dev/null` (read, write).
  Nothing else under `/etc`, `/proc`, `/sys`, `/dev`, `/tmp`, `/var`, `/home`, `/root`, `/mnt`.
- No `/etc/passwd`, no `/etc/resolv.conf`, no `/etc/hosts`: user lookups and name resolution fail.
  OCCT's STEP writer calls `getpwuid`; it fails harmlessly and the file is written.
- Inside the job folder a program can create, write, rename, remove and truncate regular files and
  directories. It cannot create symlinks, FIFOs, sockets or device nodes there, and it cannot
  start files from the job folder directly (`execve` is refused; scripts are run by passing them
  to an interpreter that lives on a `--ro` path, and the dynamic loader on such a path can load
  native code written there in the same way, still inside the sandbox).
- `chmod`, `chown` and `utime` report success and do nothing; `setxattr` and friends return
  ENOTSUP. This was chosen over an error because `shutil.copy2`, `tarfile` and `zipfile` call them
  routinely and treat ENOTSUP/no-op as normal.
- `/dev/shm` is not available. POSIX named semaphores, and therefore Python `multiprocessing`
  locks and queues, do not work; `joblib` prints "will operate in serial mode". Threads work.
  `subprocess` and `fork` work.
- `clone3` and `io_uring` return ENOSYS, SysV IPC and POSIX message queues return ENOSYS.
- No inotify: `inotify_init` and `inotify_init1` fail with EPERM, because the kernel counts
  inotify instances for the whole user account (section 1.2). A program that watches files has
  to poll. Python's `watchdog` and `watchfiles`, GLib's file monitors and Qt's
  `QFileSystemWatcher` use inotify; whether each of them falls back to polling was not tried.
  The build123d engine workload and the agent stack of this crate's workload tests run without
  it; no slicer has been run by these tests.
- At most 1024 signals can be queued at a time (`RLIMIT_SIGPENDING`; the kernel counts the job
  alone with the namespace layer and the whole user without it, section 1.2). `sigqueue` beyond
  that fails with EAGAIN, and so does `timer_create`, because each POSIX timer holds one place
  (1024 timers were created in a job, the next was refused). Plain `kill` with a standard
  signal is not affected.
- `engine` and `slicer`: no socket call of any kind works. `agent`: one exception, see section 4.
- The process count limit counts tasks, so threads count too. The engine default is 512; a
  build123d import with OpenBLAS/OpenMP on a many-core machine uses a few dozen. A program that
  reaches the limit is killed (exit 123); it does not get to see a failing `fork`, except where
  the cap is the one on the user's total (section 1.1). There a `fork` or a new thread can fail
  with EAGAIN below the limit in three cases: within a second of the user's other programs
  growing by more than 1024 tasks, until the launcher has moved the cap; when the user is at
  the ceiling (seven eighths of `ulimit -u`); and when tasks the launcher cannot see have grown
  (the same user id in another container). For the last two the launcher prints
  `hotend-sandbox: the job cannot start another process or thread at the moment ...` on
  stderr, once per job. In that mode the job also cannot change its own `RLIMIT_NPROC`, not
  even lower it: `setrlimit` and `prlimit` for that limit return EPERM.
- Memory: `--memory-mb` is applied twice, as an address-space limit per process (allocation fails
  with `MemoryError`/`ENOMEM`) and as a resident-memory limit for the whole tree (supervisor kill,
  exit 123). Address space is larger than resident memory: the build123d workload peaks at about
  1.13 GB of address space for a trivial box, so limits below roughly 1.5 GB will not start it.
- Exit codes: the program's own; 128+N if it died from signal N; 124 wall clock; 125 sandbox not
  applied (this includes a machine where nothing in the kernel can cap the process count); 126
  program not executable; 127 program not found; 123 with a line on stderr when the supervisor
  killed the tree for the process, memory or disk limit (`EXIT_LIMIT` in `src/error.rs`); 137
  when the kernel killed the program with SIGKILL (128+9).

### What the caller must pass

- `--ro` for the Python virtual environment and for the interpreter's real install prefix
  (`sys.base_prefix`; a venv's `bin/python` is a symlink out of the venv). When the base prefix is
  under `/usr` it is already readable.
- In this repository the `python/` packages are installed editable (a `.pth` file pointing at
  `<checkout>/python`), so `python -I -m hotend_engine.worker` also needs `--ro` for the
  `python/` directory itself. Granting only `python/hotend_engine` is not enough: the import
  system has to list the directory named in the `.pth` file. A packaged, non-editable install does
  not need this.
- Every environment variable the child should see must be on the allowlist and set on the returned
  `Command` (or in the parent). API keys cannot be passed at all, by design: the agent process
  talks to the model through the core over stdio.
- The launcher binary must exist: build it with `cargo build -p hotend-sandbox` before running
  another crate's tests, or point `HOTEND_SANDBOX_BIN` at it (debug builds only; a release
  build ignores it). The library looks next to the
  current executable (and one level up from a `deps` directory, which is where cargo puts test
  binaries).
- For tokio: `tokio::process::Command::from(hotend_sandbox::command(&policy, program, args)?)`.
  The crate does not depend on tokio.
- `HOTEND_UNSAFE_NO_SANDBOX=1` runs the program unconfined, in debug builds only, and prints a
  warning on stderr for every launch. Release builds ignore it and say so.

## 4. The agent profile's socketpair

The contract used to say that no profile gets a socket of any kind. That cannot be met for the
agent: Python's `asyncio` creates a `socketpair(AF_UNIX, SOCK_STREAM)` for its self-pipe when an
event loop is created, and the agent stack (pydantic-ai) is asyncio-based. With `socketpair`
denied, `asyncio.new_event_loop()` raises.

The `agent` profile therefore allows exactly `socketpair(AF_UNIX, SOCK_STREAM[|NONBLOCK|CLOEXEC])`
and `sendto`/`recvfrom` with a null address (what `send`/`recv` compile to). Such a pair is
anonymous: it has no name, cannot be connected to from outside, and no end of it can reach a
process outside the sandbox because descriptor passing needs `sendmsg`, which stays denied, as do
`socket`, `connect`, `bind`, `listen` and `accept`. `engine` and `slicer` get no exception. The
tests `agent_gets_an_anonymous_socketpair_and_nothing_else` and
`agent_stack_runs_under_the_agent_profile` cover both sides.

## 5. Known limitations

- Metadata is visible. Landlock does not mediate `stat`, `readlink` or `access`, so a confined
  program can learn that a path outside its sandbox exists, its size and its timestamps. It
  cannot open, list or read it.
- A kernel that stops scheduling defeats every time limit. The wall clock, the tree-wide limits
  and the kill of the tree are all things a process does: the launcher has to run to notice, and
  SIGKILL takes effect only when the kernel lets the target reach a point where signals are
  handled. A worker stuck in the kernel (a soft lockup) keeps SIGKILL pending; the launcher's limits
  can do nothing about it, and nothing in user space can. What still holds in that state: the kernel-enforced caps
  (process count, memory per process, file size) and the confinement itself. What does not: "the
  job ends after `--wall-seconds`" and "the whole tree is gone when the launcher returns". The
  launcher gives up waiting for a job's cgroup to empty after 2 seconds and says so on stderr; it
  does not otherwise detect a stuck kernel. The core should treat a launcher that outlives its
  wall-clock limit by a wide margin as a machine problem (on WSL2: `wsl --shutdown`), not retry.
- The process cap of an ordinary user without the namespace layer and without a cgroup of the
  user's own is a bound on the user's total, not an exact count (section 1.1): a fork bomb can
  reach `max_processes` + 1024 tasks (more if the user's other programs ended tasks within the
  last second) before the supervisor kills it; a job can lose the ability to fork for up to a
  second when the user's other programs grow by more than 1024 tasks at once, and for longer
  when the user is at seven eighths of `ulimit -u` or when the growth is in tasks the launcher
  cannot see (the same numeric user id in another container), which it reports on stderr and
  cannot repair; and jobs of the same user share the count. Following the user's other programs
  costs the launcher a walk of `/proc` and one throwaway process a second in this mode. An
  exact cap there needs the transient systemd scope described in section 1.1, or a seccomp
  user-notification gate on `clone` that the launcher answers; neither is built.
- Root needs a cgroup with the pids controller that the launcher can create inside its own
  cgroup, on cgroup v1 or v2. No job starts (exit 125, by design) where the cgroup mount is
  read-only or absent, which is what a container run as root without extra options has, and
  where the launcher's cgroup v2 cannot pass the pids controller on (it is not in
  `cgroup.controllers`, or the cgroup has child cgroups with processes or passes memory or io
  on to its children). "Root" here is uid 0 of the initial user namespace; root of a rootless
  container is an ordinary user to the kernel and gets the rows for one.
- The cgroup v2 layouts have not run on a machine whose cgroup v2 carries the pids controller
  (section 2), which is every machine that mounts only cgroup v2. Until they have, the claim
  for root there is "an exact cap or exit 125", not "an exact cap".
- With the threaded cgroup v2 layout, the launcher's own cgroup is a threaded domain while a
  job runs, and the launcher turns the pids controller on for the children of that cgroup and
  off again after the last job. Another program that manages child cgroups in the same cgroup
  at the same time (a container runtime started from the same scope, say) can find it unable
  to use a new ordinary child cgroup meanwhile. When a launcher is killed, its watchdog does
  the clean-up. When both are killed, an empty `hotend-sandbox-*` cgroup and the `+pids`
  setting stay behind until a later launcher runs a job in that cgroup: it removes abandoned
  job cgroups that are older than 30 seconds and, after its own job, the setting. In the
  root-cgroup layout `+pids` stays on the root cgroup for good; it is what systemd sets there
  on every machine it runs.
- Two allowances that the kernel keeps for the whole user account are not capped per job
  (section 1.2): epoll watches and pipe buffer memory. A job that takes them makes `epoll_ctl`
  fail with ENOSPC, or new pipes small and slow, for the other programs of its user while it
  runs. Closing them needs seccomp argument checks the deny-list cannot express (a count), or
  a user namespace per job with its own allowance, which the kernel does not offer for these
  two.
- No inotify inside the sandbox (section 3). A slicer or library that needs file change
  notifications and has no polling fallback will not work.
- Without the namespace layer, the processes a killed job leaves behind wait as zombies until the
  machine's first process collects them. They run nothing and hold no memory, but they count for
  their user (and, in a cgroup, for the cgroup, which is why the launcher removes the cgroup and
  never reuses it).
- Signals without namespaces on old Landlock. Where the namespace layer is unavailable and
  Landlock is older than ABI 6 (kernel before 6.12), signals are confined by the seccomp signal
  guard (section 1), which cannot tell processes apart: a subprocess of the program cannot raise a
  signal in itself, and one process of the tree cannot signal a single other one. `probe` reports
  this in `limitations`. WSL2 has namespaces, so this affects native distributions that restrict
  user namespaces and ship kernels 6.2 to 6.11.
- The capability bounding set is not emptied for an ordinary user without the namespace layer
  (section 1). `probe` reports it in `limitations`.
- Scheduling of other processes. `setpriority` and `sched_setaffinity` on other same-uid
  processes are not filtered (only relevant without a PID namespace).
- Tree memory and the size of the job folder are polled by the supervisor, not enforced by the
  kernel (section 1.1): a job can overshoot either by what it does between two checks. A memory
  cgroup would make the first exact; only the pids controller is used so far.
- A deny-list misses system calls added to the kernel later. The list covers the x86-64 and
  arm64 system call numbers up to 469 (`file_setattr`); new mount-, namespace- or socket-like
  calls need to be added when the supported kernel range moves. Landlock and the capability drop still apply to such calls.
- If the launcher and its watchdog are killed in the same instant, nobody is left to kill the
  tree. With the PID namespace the tree still dies (the namespace's first process gets the
  parent-death signal). Without it the program itself dies the same way, but processes it started
  keep running, still confined and still capped, until they exit or reach a CPU limit. A kill of
  the launcher's whole cgroup is not such a case where the job has a cgroup, because the job's
  cgroup is inside the launcher's. The core should stop a job with SIGTERM to the launcher.
- Exit codes are not authenticated: a program can exit with 123 or 124 by itself. The stderr line
  from the launcher can be imitated as well. There is no separate status channel.
- Files created in the job folder keep the mode the program asked for at creation (including
  setuid bits on its own files). They cannot be started with `execve` from inside the sandbox (an interpreter or the dynamic
  loader can still load them there, confined); the core must
  never execute job files outside it.
- The test machine has no Yama; nothing relies on it.

## 6. macOS

`src/seatbelt.rs` generates a Seatbelt (SBPL) profile: deny by default, no network, read of system
paths and `--ro` paths, write only in the job folder, paths passed as `-D` parameters rather than
spliced into the profile text. `src/macos.rs` runs the program through `/usr/bin/sandbox-exec` with
the same environment handling, rlimits and a wall-clock process-group kill.

**Untested on real hardware.** The code is type-checked for `aarch64-apple-darwin`
(`cargo clippy --target aarch64-apple-darwin`) and the profile generator has unit tests, but no
profile has ever been loaded by a macOS kernel. For that reason the backend refuses to run
(`Unavailable`, exit 125) unless `HOTEND_SANDBOX_MACOS_UNTESTED=1` is set. Known gaps even on
paper: no process-count or tree-memory cap (`RLIMIT_AS` is believed not to be reliably enforced
on macOS; not checked), nothing
prevents a child from calling `setsid` and escaping the process-group kill, and `sandbox-exec` is
deprecated by Apple (it still ships). Before enabling it someone has to run the acceptance tests on
a Mac and fix what they find.

## 7. Windows: planned backend

Today `Backend::run` returns `SandboxError::NotSupported { platform: "windows", .. }` and `probe`
reports `usable: false`. The launcher exits 125. Nothing pretends to confine anything.

Plan, in order of preference:

1. **AppContainer + job object.** Create an AppContainer profile per job with no capabilities
   (so no `internetClient`, hence no network through the Windows Filtering Platform), grant the
   AppContainer SID modify access on the job folder and read access on the Python runtime and each
   `--ro` path through ACLs, start the process with
   `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` inside a job object with
   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, `JOB_OBJECT_LIMIT_ACTIVE_PROCESS`,
   `JOB_OBJECT_LIMIT_JOB_MEMORY`, `JOB_OBJECT_LIMIT_JOB_TIME` and UI restrictions. The job object
   gives the whole-tree kill and all resource caps natively, which is better than the Linux
   watchdog. Open questions: ACL changes on the runtime directories are persistent state that must
   be cleaned up; CPython and OCCT have to be checked for startup under an AppContainer token
   (registry and `%TEMP%` access).
2. **Restricted token + job object** as the fallback if AppContainer breaks the Python runtime:
   `CreateRestrictedToken` with deny-only SIDs and a low integrity level, same job object. This is
   weaker: low integrity stops writes outside low-integrity locations but not reads of the user's
   profile, and it does not block network, so it would need a per-process WFP filter or it does not
   meet guarantee 1. It is not acceptable on its own.

Since the engine currently targets WSL2 on Windows, the Linux backend is what runs there; a native
Windows backend only matters once the engine or the slicers run as Windows processes.

## 8. Microsoft MXC SDK (`mxc-sdk`)

The MXC SDK (MIT, Rust) offers one interface over several per-system sandbox backends, among them
a Windows process container. It is not used on Linux, where this crate provides the guarantees of
section 1; it may be looked at again for the Windows backend of section 7.
