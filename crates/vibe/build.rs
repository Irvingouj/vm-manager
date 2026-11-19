//! What `vibe new` carries into a new project that is made from the
//! repository at build time (blackbox/CONTRACT.md, "What a new project
//! holds"), so that it cannot drift from what the gate tested and the build
//! needs neither `npm` nor `node`:
//!
//! - the SDK's package (`sdk/`) as the tarball `npm pack` would make of it,
//!   the same bytes every time: every file of the folder but `node_modules`
//!   and names beginning with `.`, in the order of their paths, under
//!   `package/`, each with mode `0644`, owner and group `0`, and the fixed
//!   time npm itself gives every entry; gzip'd with no name and no time;
//! - the versions a new project's `package.json` names: the SDK's own (from
//!   `sdk/package.json`) and, from the root's `package-lock.json`, those of
//!   `drizzle-orm`, `drizzle-kit`, `typescript` and `esbuild`.
//!
//! Cargo runs this again when anything under `sdk/` or the lock file
//! changes, so a change to the SDK reaches the next build of the binary.
//! A missing file or version fails the build with a message that says
//! which.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use flate2::{Compression, GzBuilder};
use serde_json::Value;

/// The time npm gives every entry of a tarball it packs:
/// 1985-10-26T08:15:00Z.
const NPM_ENTRY_TIME: u64 = 499_162_500;

/// The packages whose locked version a new project's `package.json` names,
/// with the name of the constant each becomes.
const LOCKED: [(&str, &str); 4] = [
    ("drizzle-orm", "DRIZZLE_ORM"),
    ("drizzle-kit", "DRIZZLE_KIT"),
    ("typescript", "TYPESCRIPT"),
    ("esbuild", "ESBUILD"),
];

fn main() -> Result<(), String> {
    let crate_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").ok_or("CARGO_MANIFEST_DIR is not set")?,
    );
    let out = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("OUT_DIR is not set")?);
    let root = crate_dir
        .parent()
        .and_then(Path::parent)
        .ok_or("crates/vibe is not two folders below the repository's root")?;
    let sdk = root.join("sdk");
    let lock = root.join("package-lock.json");
    println!("cargo::rerun-if-changed={}", sdk.display());
    println!("cargo::rerun-if-changed={}", lock.display());
    println!("cargo::rerun-if-changed=build.rs");

    let package = read_json(&sdk.join("package.json"))?;
    let sdk_version = package
        .get("version")
        .and_then(Value::as_str)
        .ok_or("sdk/package.json has no version")?;
    if package.get("name").and_then(Value::as_str) != Some("@vm/sdk") {
        return Err("sdk/package.json does not name the package @vm/sdk".into());
    }

    let lock = read_json(&lock)?;
    let mut versions = format!(
        "/// The SDK's version, as `sdk/package.json` gives it.\npub const SDK: &str = {};\n",
        quoted(sdk_version)
    );
    for (name, constant) in LOCKED {
        let version = lock
            .get("packages")
            .and_then(|p| p.get(format!("node_modules/{name}")))
            .and_then(|p| p.get("version"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                format!("package-lock.json has no version of node_modules/{name}: add {name} to the npm workspace and run npm install")
            })?;
        versions.push_str(&format!(
            "/// The version of `{name}` in the root's `package-lock.json`.\npub const {constant}: &str = {};\n",
            quoted(version)
        ));
    }
    fs::write(out.join("versions.rs"), versions)
        .map_err(|e| format!("cannot write versions.rs: {e}"))?;

    let mut files = Vec::new();
    walk(&sdk, &sdk, &mut files)?;
    files.sort();
    let tarball = tarball(&sdk, &files)?;
    fs::write(out.join("vm-sdk.tgz"), tarball)
        .map_err(|e| format!("cannot write the SDK's tarball: {e}"))?;
    Ok(())
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text =
        fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{} is not JSON: {e}", path.display()))
}

/// `text` as a Rust string literal.
fn quoted(text: &str) -> String {
    format!("{text:?}")
}

/// Every file under `dir`, by path relative to `root` with `/` between
/// names; not `node_modules`, not a name beginning with `.`, not a link.
fn walk(root: &Path, dir: &Path, files: &mut Vec<String>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| format!("a name in {} is not UTF-8", dir.display()))?;
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let path = entry.path();
        let kind = entry
            .file_type()
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if kind.is_dir() {
            walk(root, &path, files)?;
        } else if kind.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            let parts: Vec<&str> = relative
                .components()
                .filter_map(|c| c.as_os_str().to_str())
                .collect();
            files.push(parts.join("/"));
        }
    }
    Ok(())
}

fn tarball(sdk: &Path, files: &[String]) -> Result<Vec<u8>, String> {
    let gzip = GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), Compression::default());
    let mut archive = tar::Builder::new(gzip);
    archive.mode(tar::HeaderMode::Deterministic);
    for relative in files {
        let bytes =
            fs::read(sdk.join(relative)).map_err(|e| format!("cannot read sdk/{relative}: {e}"))?;
        let mut header = tar::Header::new_ustar();
        header
            .set_path(format!("package/{relative}"))
            .map_err(|e| format!("sdk/{relative} cannot be named in a tarball: {e}"))?;
        header.set_size(
            u64::try_from(bytes.len()).map_err(|_| format!("sdk/{relative} is too large"))?,
        );
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(NPM_ENTRY_TIME);
        header.set_cksum();
        archive
            .append(&header, bytes.as_slice())
            .map_err(|e| format!("cannot add sdk/{relative} to the tarball: {e}"))?;
    }
    let mut gzip = archive
        .into_inner()
        .map_err(|e| format!("cannot finish the tarball: {e}"))?;
    gzip.flush()
        .map_err(|e| format!("cannot finish the tarball: {e}"))?;
    gzip.finish()
        .map_err(|e| format!("cannot finish the tarball: {e}"))
}
