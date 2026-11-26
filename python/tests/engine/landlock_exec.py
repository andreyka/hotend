"""Test helper: run a command with file writes confined to one folder, using Linux Landlock.

    python landlock_exec.py <writable-dir> -- <program> <args>...

This is a stand-in for the `engine` profile of hotend-sandbox, so the engine's tests can prove
"writes only inside the job folder, no network" without depending on the Rust crate. Reading
and executing stay allowed everywhere; every write, create, remove or rename outside
<writable-dir> fails with EACCES, and TCP bind/connect is denied where the kernel supports it.
Exit code 125 means Landlock could not be applied (the program was not started).
"""

from __future__ import annotations

import ctypes
import os
import sys

SYS_CREATE_RULESET, SYS_ADD_RULE, SYS_RESTRICT_SELF = 444, 445, 446
PR_SET_NO_NEW_PRIVS = 38
RULE_PATH_BENEATH = 1
CREATE_RULESET_VERSION = 1

FS_EXECUTE, FS_WRITE_FILE, FS_READ_FILE, FS_READ_DIR = 1 << 0, 1 << 1, 1 << 2, 1 << 3
FS_REMOVE_DIR, FS_REMOVE_FILE, FS_MAKE_CHAR, FS_MAKE_DIR = 1 << 4, 1 << 5, 1 << 6, 1 << 7
FS_MAKE_REG, FS_MAKE_SOCK, FS_MAKE_FIFO, FS_MAKE_BLOCK = 1 << 8, 1 << 9, 1 << 10, 1 << 11
FS_MAKE_SYM, FS_REFER, FS_TRUNCATE = 1 << 12, 1 << 13, 1 << 14
NET_BIND_TCP, NET_CONNECT_TCP = 1 << 0, 1 << 1

WRITES_V1 = (FS_WRITE_FILE | FS_REMOVE_DIR | FS_REMOVE_FILE | FS_MAKE_CHAR | FS_MAKE_DIR
             | FS_MAKE_REG | FS_MAKE_SOCK | FS_MAKE_FIFO | FS_MAKE_BLOCK | FS_MAKE_SYM)


class RulesetAttr(ctypes.Structure):
    _fields_ = (("handled_access_fs", ctypes.c_uint64), ("handled_access_net", ctypes.c_uint64))


class PathBeneathAttr(ctypes.Structure):
    _pack_ = 1
    _fields_ = (("allowed_access", ctypes.c_uint64), ("parent_fd", ctypes.c_int32))


def landlock_abi() -> int:
    """The kernel's Landlock ABI version, or 0 when Landlock is not available."""
    if not sys.platform.startswith("linux"):
        return 0
    libc = ctypes.CDLL(None, use_errno=True)
    version = libc.syscall(SYS_CREATE_RULESET, None, 0, CREATE_RULESET_VERSION)
    return max(version, 0)


def confine_writes_to(directory: str) -> None:
    abi = landlock_abi()
    if abi < 1:
        raise OSError("Landlock is not available on this kernel")
    libc = ctypes.CDLL(None, use_errno=True)
    writes = WRITES_V1
    if abi >= 2:
        writes |= FS_REFER
    if abi >= 3:
        writes |= FS_TRUNCATE
    attr = RulesetAttr(writes, (NET_BIND_TCP | NET_CONNECT_TCP) if abi >= 4 else 0)
    size = ctypes.sizeof(attr) if abi >= 4 else ctypes.sizeof(ctypes.c_uint64)
    ruleset = libc.syscall(SYS_CREATE_RULESET, ctypes.byref(attr), size, 0)
    if ruleset < 0:
        raise OSError(ctypes.get_errno(), "landlock_create_ruleset failed")
    for path in (directory, "/dev/null"):
        fd = os.open(path, os.O_PATH | os.O_CLOEXEC)
        file_only = FS_WRITE_FILE | (FS_TRUNCATE if abi >= 3 else 0)
        rule = PathBeneathAttr(writes if os.path.isdir(path) else file_only, fd)
        if libc.syscall(SYS_ADD_RULE, ruleset, RULE_PATH_BENEATH, ctypes.byref(rule), 0) < 0:
            raise OSError(ctypes.get_errno(), f"landlock_add_rule failed for {path}")
        os.close(fd)
    if libc.prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), "prctl(PR_SET_NO_NEW_PRIVS) failed")
    if libc.syscall(SYS_RESTRICT_SELF, ruleset, 0) < 0:
        raise OSError(ctypes.get_errno(), "landlock_restrict_self failed")
    os.close(ruleset)


def main(argv: list[str]) -> int:
    if len(argv) < 3 or argv[1] != "--":
        print("usage: landlock_exec.py <writable-dir> -- <program> <args>...", file=sys.stderr)
        return 125
    try:
        confine_writes_to(argv[0])
    except OSError as exc:
        print(f"landlock_exec: {exc}", file=sys.stderr)
        return 125
    os.execv(argv[2], argv[2:])
    return 125


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
