//! `hotend-fake-slicer`: stands in for Bambu Studio or PrusaSlicer in tests and in the
//! offline demo. See `hotend_slicer::fake` for what it does and does not do.

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let code = hotend_slicer::fake::run(&args, &mut std::io::stdout(), &mut std::io::stderr());
    std::process::exit(code);
}
