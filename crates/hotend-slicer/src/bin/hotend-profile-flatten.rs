//! The profile reader: flattens Bambu Studio system profiles into a job folder.
//! `hotend_slicer::flatten` has the logic; `hotend_slicer::system` starts this program
//! inside the sandbox.

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    std::process::exit(hotend_slicer::flatten::helper_main(&args));
}
