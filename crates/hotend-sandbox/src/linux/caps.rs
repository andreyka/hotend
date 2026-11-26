//! Dropping every Linux capability, so that neither root nor a process that owns a user
//! namespace keeps any privilege across `execve`.

use std::io;

use crate::unix::cvt;

const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;

#[repr(C)]
struct CapHeader {
    version: u32,
    pid: libc::c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CapData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

/// Empties the bounding, ambient, inheritable, permitted and effective sets.
///
/// With an empty bounding set even uid 0 gains nothing when it executes a program. A process
/// that may not change its bounding set (no `CAP_SETPCAP`) is accepted only if it is not
/// root: without `no_new_privs`-defeating file capabilities it cannot regain anything.
pub(crate) fn drop_all() -> io::Result<()> {
    let mut bounding_left = false;
    for capability in 0..64 {
        // SAFETY: prctl with integer arguments only.
        match unsafe { libc::prctl(libc::PR_CAPBSET_READ, capability, 0, 0, 0) } {
            1 => {
                // SAFETY: as above.
                if unsafe { libc::prctl(libc::PR_CAPBSET_DROP, capability, 0, 0, 0) } != 0 {
                    bounding_left = true;
                }
            }
            0 => {}
            // EINVAL: past the last capability this kernel knows.
            _ => break,
        }
    }
    // SAFETY: getuid cannot fail.
    if bounding_left && unsafe { libc::getuid() } == 0 {
        return Err(io::Error::other(
            "running as root without CAP_SETPCAP: the capability bounding set cannot be emptied",
        ));
    }
    // Not available before Linux 4.3; such kernels have no Landlock either, so the error
    // would be reported there. Ambient capabilities need inheritable ones, cleared below.
    // SAFETY: prctl with integer arguments only.
    unsafe {
        libc::prctl(
            libc::PR_CAP_AMBIENT,
            libc::PR_CAP_AMBIENT_CLEAR_ALL,
            0,
            0,
            0,
        )
    };

    let header = CapHeader {
        version: LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    let empty = [CapData::default(); 2];
    // SAFETY: header and data match the kernel's version 3 layout (two 32-bit data blocks).
    cvt(unsafe { libc::syscall(libc::SYS_capset, &header, empty.as_ptr()) } as libc::c_int)?;

    let mut current = [CapData::default(); 2];
    // SAFETY: as above; the kernel fills `current`.
    cvt(unsafe { libc::syscall(libc::SYS_capget, &header, current.as_mut_ptr()) } as libc::c_int)?;
    if current
        .iter()
        .any(|d| d.effective | d.permitted | d.inheritable != 0)
    {
        return Err(io::Error::other("capabilities are still held after capset"));
    }
    Ok(())
}
