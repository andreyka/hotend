"""Hotend CAD engine: the sandboxed worker that runs a model script.

One process handles one request (`python -I -m hotend_engine.worker <job-dir>`), as described
in docs/PROTOCOL.md sections 2 and 3. The package guarantees that it writes only inside the job
folder and that importing it is cheap: build123d and the other heavy libraries are imported by
the modules that need them, never here.

Address space is bounded here, before any library starts a thread. The sandbox caps it (4 GiB
for the engine), and what the libraries reserve grows with the number of CPUs: OpenBLAS and
OpenMP start a thread per CPU, and glibc gives each new thread its own 64 MiB malloc arena
(up to eight per CPU) while OpenCascade runs a thread per CPU too. On a 32-CPU machine a build
reached 4.5 GiB and a four-view render 5 GiB, and the worker died in the CAD kernel (SIGSEGV)
or with a MemoryError. So the numeric libraries run on one thread and glibc keeps two arenas;
each job then stays under 2 GiB whatever the CPU count.
"""

import os
import sys

for _name in ("OPENBLAS_NUM_THREADS", "OMP_NUM_THREADS", "MKL_NUM_THREADS"):
    os.environ[_name] = "1"
del _name

_MALLOC_ARENAS = 2


def _limit_malloc_arenas() -> None:
    # MALLOC_ARENA_MAX is read only when a process starts; mallopt sets the same limit now.
    if not sys.platform.startswith("linux"):
        return
    import ctypes

    m_arena_max = -8  # from glibc's malloc.h
    try:
        ctypes.CDLL(None).mallopt(m_arena_max, _MALLOC_ARENAS)
    except (OSError, AttributeError):
        pass  # not glibc (musl has no arenas to limit)


_limit_malloc_arenas()

PROTOCOL_VERSION = 0

__all__ = ["PROTOCOL_VERSION"]
