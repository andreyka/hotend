//! `hotend-sandbox`: starts one program inside the sandbox and reports its exit code.

fn main() {
    std::process::exit(hotend_sandbox::cli::main(std::env::args_os()));
}
