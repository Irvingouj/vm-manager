use std::fs::File;
use std::path::PathBuf;

const RUNTIME_SNAPSHOT_PATH: &str = env!("RUNTIME_SNAPSHOT_PATH");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/runtime/bindings.rs");
    println!("cargo:rerun-if-changed=src/snapshot.rs");

    let path = PathBuf::from(RUNTIME_SNAPSHOT_PATH);

    // Create the file if it doesn't exist. The runtime runs without a
    // snapshot when the file is missing, so a path whose directory does not
    // exist on the build machine (a deploy path such as /app/..., or /tmp/...
    // on Windows) is not a build failure.
    if !path.exists() {
        println!("Creating empty snapshot file: {:?}", path);
        if let Err(e) = File::create(&path) {
            println!(
                "cargo:warning=cannot create {}: {e}; the runtime starts without a snapshot unless the file exists at run time or the host calls platform::set_snapshot",
                path.display()
            );
        }
    }
}
