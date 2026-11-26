"""The worker's address space must not grow with the number of CPUs.

The engine sandbox caps address space at 4 GiB. On a 32-CPU WSL machine OpenBLAS and OpenMP
started a thread per CPU and glibc gave every new thread a 64 MiB malloc arena, so a build
reached 4.5 GiB and a four-view render 5 GiB: the worker crashed in the CAD kernel (SIGSEGV)
or failed with a MemoryError. Importing hotend_engine must prevent both.
"""

from __future__ import annotations

import json
import subprocess
import sys

import pytest

CLEAN_ENV = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8"}

# Each probe runs the way the core starts the worker: a fresh process in isolated mode, an
# environment without the thread or malloc variables, the engine package imported first.
THREAD_POOLS = """
import json
import hotend_engine.worker  # noqa: F401
import numpy, scipy.linalg, sklearn  # noqa: E401, F401
from threadpoolctl import threadpool_info
print(json.dumps(threadpool_info()))
"""

# 32 threads alive at once, each allocating with malloc, as OpenCascade's pool does. Without
# an arena limit each gets its own arena (64 MiB of address space); stacks are kept small so
# that the arenas are what the measurement sees.
MALLOC_ARENAS = """
import ctypes, json, threading
import hotend_engine  # noqa: F401

def vm_kib():
    for line in open("/proc/self/status"):
        if line.startswith("VmSize:"):
            return int(line.split()[1])

libc = ctypes.CDLL(None)
libc.malloc.restype = ctypes.c_void_p
threading.stack_size(256 * 1024)
barrier = threading.Barrier(33)
def work():
    libc.malloc(4096)
    barrier.wait()
    barrier.wait()
before = vm_kib()
threads = [threading.Thread(target=work) for _ in range(32)]
for t in threads:
    t.start()
barrier.wait()
grown = vm_kib() - before
barrier.wait()
for t in threads:
    t.join()
print(json.dumps({"grown_mib": grown // 1024}))
"""


def probe(code: str) -> dict | list:
    done = subprocess.run(
        [sys.executable, "-I", "-c", code], capture_output=True, text=True, timeout=120,
        env=CLEAN_ENV, check=False,
    )
    assert done.returncode == 0, done.stderr
    return json.loads(done.stdout.strip().splitlines()[-1])


def test_every_numeric_thread_pool_has_one_thread():
    pools = probe(THREAD_POOLS)
    assert pools, "no BLAS or OpenMP library was loaded, so the test proves nothing"
    wide = [(p["internal_api"], p["filepath"], p["num_threads"]) for p in pools
            if p["num_threads"] != 1]
    assert not wide, f"thread pools wider than one thread: {wide}"


@pytest.mark.skipif(not sys.platform.startswith("linux"), reason="glibc malloc arenas")
def test_many_threads_share_a_few_malloc_arenas():
    grown = probe(MALLOC_ARENAS)["grown_mib"]
    # Two arenas and 32 small stacks take well under 200 MiB. Unlimited, glibc allows eight
    # arenas per CPU, so even a one-CPU machine reaches 512 MiB.
    assert grown < 384, f"32 threads grew the address space by {grown} MiB"
