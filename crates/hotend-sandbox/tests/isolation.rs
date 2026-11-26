//! Denial tests: network, filesystem, environment and privilege.
//!
//! Every test runs with the namespace layer (when the kernel offers it) and again with
//! Landlock and seccomp alone, because the guarantees must hold without the optional layer.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::ErrorKind;
use std::net::TcpListener;
use std::os::unix::fs::{PermissionsExt, symlink};

use common::*;
use hotend_sandbox::{Policy, Profile, SandboxError};

#[test]
fn no_network_of_any_kind() {
    if !sandbox_available("no_network_of_any_kind") {
        return;
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();

    for namespaces in namespace_modes() {
        let job = tempfile::tempdir().unwrap();
        let script = format!(
            r#"
attempt("tcp_connect", lambda: socket.create_connection(("127.0.0.1", {port}), timeout=2))
attempt("tcp_socket", lambda: socket.socket(socket.AF_INET, socket.SOCK_STREAM))
attempt("udp_socket", lambda: socket.socket(socket.AF_INET, socket.SOCK_DGRAM))
attempt("ipv6_socket", lambda: socket.socket(socket.AF_INET6, socket.SOCK_STREAM))
attempt("unix_socket", lambda: socket.socket(socket.AF_UNIX, socket.SOCK_STREAM))
attempt("unix_socketpair", lambda: socket.socketpair())
attempt("netlink_socket", lambda: socket.socket(socket.AF_NETLINK, socket.SOCK_RAW, 0))
attempt("packet_socket", lambda: socket.socket(socket.AF_PACKET, socket.SOCK_RAW, 0))
attempt("dns_lookup", lambda: socket.getaddrinfo("example.com", 80))
attempt("localhost_lookup", lambda: socket.getaddrinfo("localhost", 80))
# The same calls without Python's wrappers: socket(2) is 41 on x86-64 and 198 on arm64.
nr_socket = 41 if os.uname().machine == "x86_64" else 198
attempt("raw_socket_syscall", lambda: syscall(nr_socket, socket.AF_INET, socket.SOCK_STREAM, 0))
if os.uname().machine == "x86_64":
    # The x32 entry point of socket(2).
    attempt("x32_socket_syscall", lambda: syscall(0x40000000 + 41, socket.AF_INET, socket.SOCK_STREAM, 0))
    # io_uring_setup(2), which could issue socket calls behind the filter.
    attempt("io_uring", lambda: syscall(425, 4, ctypes.create_string_buffer(120)))
else:
    print("x32_socket_syscall=DENIED:not applicable on this architecture")
    attempt("io_uring", lambda: syscall(425, 4, ctypes.create_string_buffer(120)))
"#
        );
        let run = run_python(&policy(Profile::Engine, job.path(), namespaces), &script);
        run.assert_denied(&[
            "tcp_connect",
            "tcp_socket",
            "udp_socket",
            "ipv6_socket",
            "unix_socket",
            "unix_socketpair",
            "netlink_socket",
            "packet_socket",
            "dns_lookup",
            "localhost_lookup",
            "raw_socket_syscall",
            "x32_socket_syscall",
            "io_uring",
        ]);
        let accepted = listener.accept();
        assert!(
            matches!(&accepted, Err(e) if e.kind() == ErrorKind::WouldBlock),
            "the listener saw a connection (namespaces={namespaces}): {accepted:?}"
        );
    }
}

#[test]
fn agent_gets_an_anonymous_socketpair_and_nothing_else() {
    if !sandbox_available("agent_gets_an_anonymous_socketpair_and_nothing_else") {
        return;
    }
    let outside = tempfile::tempdir().unwrap();
    let socket_path = outside.path().join("listener.sock");
    let _listener = std::os::unix::net::UnixDatagram::bind(&socket_path).unwrap();
    for namespaces in namespace_modes() {
        let job = tempfile::tempdir().unwrap();
        let script = format!(
            r#"
a, b = socket.socketpair()
a.send(b"ping")
assert b.recv(4) == b"ping"
print("stream_pair=ALLOWED")
attempt("datagram_pair", lambda: socket.socketpair(socket.AF_UNIX, socket.SOCK_DGRAM))
attempt("inet_pair", lambda: socket.socketpair(socket.AF_INET, socket.SOCK_STREAM))
attempt("unix_socket", lambda: socket.socket(socket.AF_UNIX, socket.SOCK_STREAM))
attempt("tcp_socket", lambda: socket.socket(socket.AF_INET, socket.SOCK_STREAM))
attempt("send_to_address", lambda: a.sendto(b"x", {path:?}))
attempt("connect_pair", lambda: a.connect({path:?}))
attempt("pass_descriptor", lambda: socket.send_fds(a, [b"x"], [0]))
attempt("receive_descriptor", lambda: socket.recv_fds(b, 1, 1))
"#,
            path = socket_path.to_str().unwrap()
        );
        let run = run_python(&policy(Profile::Agent, job.path(), namespaces), &script);
        run.assert_allowed(&["stream_pair"]);
        run.assert_denied(&[
            "datagram_pair",
            "inet_pair",
            "unix_socket",
            "tcp_socket",
            "send_to_address",
            "connect_pair",
            "pass_descriptor",
            "receive_descriptor",
        ]);
    }
}

#[test]
fn filesystem_is_limited_to_the_job_folder() {
    if !sandbox_available("filesystem_is_limited_to_the_job_folder") {
        return;
    }
    for namespaces in namespace_modes() {
        // A stand-in for the user's home, outside every allowed path.
        let home = tempfile::tempdir().unwrap();
        let secret = home.path().join(".ssh").join("id_ed25519");
        std::fs::create_dir(secret.parent().unwrap()).unwrap();
        std::fs::write(&secret, "PRIVATE KEY").unwrap();
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
        let secret_mtime = std::fs::metadata(&secret).unwrap().modified().unwrap();

        // Shared locations a broken sandbox would let the child write to. The names are
        // unique to this run and the files are removed below whatever happens.
        let tag = format!("hotend-sandbox-test-{}-{namespaces}", std::process::id());
        let shared = [
            std::path::PathBuf::from("/tmp").join(&tag),
            std::path::PathBuf::from("/dev/shm").join(&tag),
            python_read_only_paths()[0].join(&tag),
        ];

        let job = tempfile::tempdir().unwrap();
        // Symlinks out of the job folder, as a previous or cooperating process might leave.
        symlink(home.path(), job.path().join("link_to_home")).unwrap();
        symlink(&secret, job.path().join("link_to_secret")).unwrap();

        let script = format!(
            r#"
home = {home:?}
secret = {secret:?}
job = os.getcwd()

# Writing outside the job folder.
attempt("write_outside", lambda: open(home + "/planted.txt", "w"))
attempt("write_via_symlink_dir", lambda: open("link_to_home/planted.txt", "w"))
attempt("write_via_symlink_file", lambda: open("link_to_secret", "w"))
attempt("write_via_proc_root", lambda: open("/proc/self/root" + home + "/planted.txt", "w"))
attempt("write_via_proc_cwd", lambda: open("/proc/self/cwd/../" + os.path.basename(home) + "/planted.txt", "w"))
attempt("write_via_dotdot", lambda: open(job + "/../" + os.path.basename(home) + "/planted.txt", "w"))
attempt("append_secret", lambda: open(secret, "a"))
attempt("truncate_outside", lambda: os.truncate(secret, 0))
attempt("truncate_via_open", lambda: os.open(secret, os.O_RDONLY | os.O_TRUNC))
attempt("mkdir_outside", lambda: os.mkdir(home + "/newdir"))
attempt("unlink_outside", lambda: os.unlink(secret))
attempt("rename_outside_in", lambda: os.rename(secret, "stolen"))
attempt("rename_inside_out", lambda: (open("mine.txt", "w").close(), os.rename("mine.txt", home + "/mine.txt")))
attempt("write_tmp", lambda: open({shared_tmp:?}, "w"))
attempt("write_dev_shm", lambda: open({shared_shm:?}, "w"))
attempt("write_runtime", lambda: open({shared_runtime:?}, "w"))

# Reading outside the allowed paths.
attempt("read_secret", lambda: open(secret).read())
attempt("read_via_symlink_file", lambda: open("link_to_secret").read())
attempt("read_via_symlink_dir", lambda: open("link_to_home/.ssh/id_ed25519").read())
attempt("read_via_proc_root", lambda: open("/proc/self/root" + secret).read())
attempt("hardlink_secret", lambda: os.link(secret, "hardlinked"))
attempt("list_home", lambda: os.listdir(home))
attempt("list_root", lambda: os.listdir("/"))
attempt("read_passwd", lambda: open("/etc/passwd").read())
attempt("read_hosts", lambda: open("/etc/hosts").read())
attempt("list_proc", lambda: os.listdir("/proc"))
attempt("read_parent_environ", lambda: open(f"/proc/{{os.getppid()}}/environ", "rb").read())
attempt("read_init_environ", lambda: open("/proc/1/environ", "rb").read())

# Things that would mislead whoever reads the job's outputs, or run new code.
attempt("create_symlink", lambda: os.symlink(secret, "result.json"))
def run_dropped_binary():
    import shutil
    shutil.copyfile("/bin/true", "dropped")
    os.chmod("dropped", 0o755)
    os.execv(job + "/dropped", ["dropped"])
attempt("exec_from_job", run_dropped_binary)

# Metadata of files outside: the calls are neutralised, so they "succeed" and change nothing.
os.chmod(secret, 0o644)
os.utime(secret, (1, 1))
attempt("setxattr_outside", lambda: os.setxattr(secret, "user.hotend", b"1"))

# Inside the job folder everything a worker needs works.
os.makedirs("out/sub")
with open("out/result.txt", "w") as f:
    f.write("ok")
os.rename("out/result.txt", "out/sub/result.json")
assert open("out/sub/result.json").read() == "ok"
with open("out/sub/result.json", "r+") as f:
    f.truncate(1)
os.unlink("out/sub/result.json")
import tempfile
with tempfile.NamedTemporaryFile() as f:
    assert f.name.startswith(job + "/.sandbox/tmp/"), f.name
assert os.environ["HOME"] == job + "/.sandbox/home"
os.makedirs(os.path.expanduser("~/.cache/tool"))
with open("/dev/null", "w") as f:
    f.write("x")
assert len(open("/dev/urandom", "rb").read(8)) == 8
assert "VmRSS" in open("/proc/self/status").read()
print("inside=ALLOWED")
"#,
            home = home.path().to_str().unwrap(),
            secret = secret.to_str().unwrap(),
            shared_tmp = shared[0].to_str().unwrap(),
            shared_shm = shared[1].to_str().unwrap(),
            shared_runtime = shared[2].to_str().unwrap(),
        );
        let run = run_python(&policy(Profile::Engine, job.path(), namespaces), &script);
        let planted: Vec<_> = shared
            .iter()
            .filter(|path| path.exists() && std::fs::remove_file(path).is_ok())
            .collect();
        assert!(
            planted.is_empty(),
            "files were planted outside the job: {planted:?}\n{run}"
        );
        run.assert_denied(&[
            "write_outside",
            "write_via_symlink_dir",
            "write_via_symlink_file",
            "write_via_proc_root",
            "write_via_proc_cwd",
            "write_via_dotdot",
            "append_secret",
            "truncate_outside",
            "truncate_via_open",
            "mkdir_outside",
            "unlink_outside",
            "rename_outside_in",
            "rename_inside_out",
            "write_tmp",
            "write_dev_shm",
            "write_runtime",
            "read_secret",
            "read_via_symlink_file",
            "read_via_symlink_dir",
            "read_via_proc_root",
            "hardlink_secret",
            "list_home",
            "list_root",
            "read_passwd",
            "read_hosts",
            "list_proc",
            "read_parent_environ",
            "read_init_environ",
            "create_symlink",
            "exec_from_job",
            "setxattr_outside",
        ]);
        run.assert_allowed(&["inside"]);

        // Seen from outside: nothing was planted, the secret is untouched.
        let entries: Vec<_> = std::fs::read_dir(home.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, [std::ffi::OsString::from(".ssh")], "{run}");
        assert_eq!(std::fs::read_to_string(&secret).unwrap(), "PRIVATE KEY");
        let meta = std::fs::metadata(&secret).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600, "mode changed");
        assert_eq!(meta.modified().unwrap(), secret_mtime, "mtime changed");
        assert!(job.path().join("mine.txt").exists());
        assert!(!job.path().join("stolen").exists());
        assert!(job.path().join("out/sub").is_dir());
    }
}

#[test]
fn environment_is_an_allowlist() {
    if !sandbox_available("environment_is_an_allowlist") {
        return;
    }
    for namespaces in namespace_modes() {
        let job = tempfile::tempdir().unwrap();
        let script = r#"
import json
print(json.dumps(dict(os.environ)))
print(open("/proc/self/environ", "rb").read().decode().replace("\0", "\n"))
"#;
        let policy =
            policy(Profile::Engine, job.path(), namespaces).allow_env("HOTEND_TEST_VISIBLE");
        let mut command = python_command(&policy, script);
        // What a careless parent might hold. The values are not real credentials.
        command
            .env("ANTHROPIC_API_KEY", "not-a-real-key-anthropic")
            .env("OPENAI_API_KEY", "not-a-real-key-openai")
            .env("HOTEND_TEST_HIDDEN", "hidden-value")
            .env("HOTEND_TEST_VISIBLE", "visible-value");
        let run = run(command);
        assert_eq!(run.code, Some(0), "{run}");
        for leaked in [
            "not-a-real-key",
            "ANTHROPIC",
            "OPENAI",
            "hidden-value",
            "HOTEND_TEST_HIDDEN",
        ] {
            assert!(
                !run.stdout.contains(leaked),
                "`{leaked}` reached the child: {run}"
            );
        }
        assert!(run.stdout.contains("visible-value"), "{run}");
        let env: serde_json::Value =
            serde_json::from_str(run.stdout.lines().next().unwrap()).unwrap();
        let mut names: Vec<_> = env
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        names.sort_unstable();
        // LC_CTYPE is added by Python itself when it coerces the C locale.
        names.retain(|name| *name != "LC_CTYPE");
        assert_eq!(
            names,
            [
                "HOME",
                "HOTEND_TEST_VISIBLE",
                "LANG",
                "PATH",
                "TEMP",
                "TMP",
                "TMPDIR"
            ],
            "{run}"
        );
        let job_path = job.path().canonicalize().unwrap();
        for name in ["HOME", "TMPDIR"] {
            let value = std::path::Path::new(env[name].as_str().unwrap());
            assert!(
                value.starts_with(&job_path),
                "{name}={value:?} is outside the job folder"
            );
        }
    }
}

#[test]
fn credentials_cannot_be_allowlisted() {
    let job = tempfile::tempdir().unwrap();
    let policy = Policy::new(Profile::Agent, job.path()).allow_env("ANTHROPIC_API_KEY");
    let error = hotend_sandbox::command(&policy, "/bin/true", [""; 0]).unwrap_err();
    assert!(matches!(error, SandboxError::InvalidPolicy(_)), "{error}");

    // The launcher refuses too, whoever builds its command line.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_hotend-sandbox"))
        .args(["run", "--profile", "agent", "--job"])
        .arg(job.path())
        .args(["--env", "ANTHROPIC_API_KEY", "--", "/bin/true"])
        .env("ANTHROPIC_API_KEY", "not-a-real-key")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("not-a-real-key"));
}

#[test]
fn no_privilege_and_no_way_out() {
    if !sandbox_available("no_privilege_and_no_way_out") {
        return;
    }
    let test_pid = std::process::id();
    for namespaces in namespace_modes() {
        let job = tempfile::tempdir().unwrap();
        let bounding_set_emptied = if namespaces || report().root {
            "True"
        } else {
            "False"
        };
        let script = format!(
            r#"
status = dict(line.split(":\t", 1) for line in open("/proc/self/status").read().splitlines() if ":\t" in line)
assert status["NoNewPrivs"] == "1", status["NoNewPrivs"]
assert status["Seccomp"] == "2", status["Seccomp"]
for name in ("CapInh", "CapPrm", "CapEff", "CapAmb"):
    assert int(status[name], 16) == 0, (name, status[name])
# The bounding set can only be emptied with CAP_SETPCAP: by root, or inside the user
# namespace. An ordinary user without the namespace layer keeps it, and can gain nothing
# from it: no_new_privs is set and the permitted sets above are empty.
if {bounding_set_emptied}:
    assert int(status["CapBnd"], 16) == 0, ("CapBnd", status["CapBnd"])
print("status=ALLOWED")

x86 = os.uname().machine == "x86_64"
nr = (lambda x, a: x if x86 else a)
PTRACE_ATTACH, PTRACE_TRACEME = 16, 0
attempt("ptrace_attach", lambda: syscall(nr(101, 117), PTRACE_ATTACH, {test_pid}, 0, 0))
attempt("ptrace_traceme", lambda: syscall(nr(101, 117), PTRACE_TRACEME, 0, 0, 0))
attempt("process_vm_readv", lambda: syscall(nr(310, 270), {test_pid}, 0, 0, 0, 0, 0))
attempt("mount", lambda: syscall(nr(165, 40), b"none", b"/mnt", b"tmpfs", 0, 0))
attempt("chroot", lambda: os.chroot("."))
attempt("unshare_user", lambda: os.unshare(os.CLONE_NEWUSER))
attempt("unshare_net", lambda: os.unshare(os.CLONE_NEWNET))
attempt("unshare_mount", lambda: os.unshare(os.CLONE_NEWNS))
attempt("clone_newuser", lambda: syscall(nr(56, 220), os.CLONE_NEWUSER | signal.SIGCHLD, 0, 0, 0, 0))
attempt("clone3", lambda: syscall(435, ctypes.create_string_buffer(88), 88))
attempt("setns", lambda: syscall(nr(308, 268), 0, 0))
attempt("bpf", lambda: syscall(nr(321, 280), 0, ctypes.create_string_buffer(120), 120))
attempt("keyctl", lambda: syscall(nr(250, 219), 0, 0, 0, 0, 0))
attempt("add_key", lambda: syscall(nr(248, 217), b"user", b"k", b"v", 1, -3))
attempt("kexec_load", lambda: syscall(nr(246, 104), 0, 0, 0, 0))
attempt("init_module", lambda: syscall(nr(175, 105), 0, 0, b""))
attempt("perf_event_open", lambda: syscall(nr(298, 241), ctypes.create_string_buffer(128), 0, -1, -1, 0))
attempt("userfaultfd", lambda: syscall(nr(323, 282), 0))
attempt("shmget", lambda: syscall(nr(29, 194), 0, 4096, 0o1600))
attempt("setsid", lambda: os.setsid())
attempt("setpgid", lambda: os.setpgid(0, 0))
attempt("setuid", lambda: os.setuid(12345))
attempt("lower_other_limits", lambda: syscall(nr(302, 261), {test_pid}, 7, 0, ctypes.create_string_buffer(16)))
attempt("signal_outside", lambda: os.kill({test_pid}, 0))
attempt("inotify_init1", lambda: syscall(nr(294, 26), 0))
attempt("inotify_init", lambda: syscall(nr(253, 26), 0))
attempt("inotify_watch", lambda: syscall(nr(254, 27), 3, b"/", 0x100))
attempt("mq_open", lambda: syscall(nr(240, 180), b"/q", os.O_CREAT | os.O_RDWR, 0o600, 0))

# Threads and child processes still work: clone3 is refused in a way glibc falls back from.
import threading, subprocess
t = threading.Thread(target=lambda: None); t.start(); t.join()
assert subprocess.run(["/bin/true"]).returncode == 0
print("threads_and_children=ALLOWED")
"#
        );
        let run = run_python(&policy(Profile::Engine, job.path(), namespaces), &script);
        run.assert_allowed(&["status", "threads_and_children"]);
        let mut denied = vec![
            "ptrace_attach",
            "ptrace_traceme",
            "process_vm_readv",
            "mount",
            "chroot",
            "unshare_user",
            "unshare_net",
            "unshare_mount",
            "clone_newuser",
            "clone3",
            "setns",
            "bpf",
            "keyctl",
            "add_key",
            "kexec_load",
            "init_module",
            "perf_event_open",
            "userfaultfd",
            "shmget",
            "setsid",
            "setpgid",
            "setuid",
            "lower_other_limits",
            "inotify_init1",
            "inotify_init",
            "inotify_watch",
            "mq_open",
        ];
        // By the PID namespace, Landlock ABI 6 or, with neither, the seccomp signal guard.
        denied.push("signal_outside");
        run.assert_denied(&denied);
    }
}

#[test]
fn probe_reports_the_mechanisms() {
    let report = hotend_sandbox::probe().unwrap();
    assert_eq!(report.platform, "linux");
    assert_eq!(report.usable, report.reason.is_none());
    if report.usable {
        assert!(report.seccomp);
        assert!(report.landlock_abi.unwrap() >= 3);
        for guarantee in [
            "network",
            "filesystem",
            "privileges",
            "resources",
            "environment",
        ] {
            assert!(
                !report.guarantees[guarantee].is_empty(),
                "no mechanism listed for {guarantee}"
            );
        }
        // A usable sandbox has a kernel mechanism for the process cap in both modes, and
        // says which one and what it bounds.
        let cap = &report.process_cap;
        for mode in [&cap.default, &cap.without_namespaces] {
            assert!(
                matches!(
                    mode.enforcement.as_str(),
                    "kernel-exact" | "kernel-approximate"
                ),
                "{mode:?}"
            );
            assert!(!mode.bound.is_empty(), "{mode:?}");
        }
        assert!(cap.rlimit_enforced || cap.cgroup.is_some(), "{cap:?}");
        assert!(
            report.guarantees["resources"]
                .iter()
                .any(|mechanism| mechanism.starts_with("process count: ")),
            "{:?}",
            report.guarantees["resources"]
        );
    }
    // The launcher prints the same report.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_hotend-sandbox"))
        .arg("probe")
        .output()
        .unwrap();
    assert!(output.status.success());
    let printed: hotend_sandbox::ProbeReport = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(printed, report);
}
