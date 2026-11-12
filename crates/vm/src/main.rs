//! The platform program (ADR 0015). Scaffolding only: it builds against the
//! execution core and answers `--version`; serving comes with the walking
//! skeleton.

use std::process::ExitCode;

const USAGE: &str = "usage: vm --version";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match (args.next().as_deref(), args.next()) {
        (Some("--version" | "-V"), None) => {
            println!("vm {}", env!("CARGO_PKG_VERSION"));
            println!(
                "execution core: openworkers-core v0.18.1, openworkers-runtime-v8 v0.18.7 (patched, vendor/VENDORED.md)"
            );
            let snapshot = match openworkers_runtime_v8::platform::get_snapshot() {
                Some(_) => "present",
                None => "none",
            };
            println!("V8 startup snapshot: {snapshot}");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
