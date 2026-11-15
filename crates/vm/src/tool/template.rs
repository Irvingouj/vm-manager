//! What a new project holds (blackbox/CONTRACT.md, "What a new project
//! holds"): the files of `crates/vm/template/`, kept there as files, with
//! `{{name}}` and the versions filled in; and the SDK's tarball, made by
//! `crates/vm/build.rs` from `sdk/`. The versions come from the root's
//! `package-lock.json` and `sdk/package.json` at build time (`versions`).

use std::borrow::Cow;

/// The versions `build.rs` read.
mod versions {
    include!(concat!(env!("OUT_DIR"), "/versions.rs"));
}

/// The SDK's package, as `npm pack` would make it of `sdk/`.
const SDK_TARBALL: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/vm-sdk.tgz"));

/// The template's text files: the path in a new project, and the text,
/// with `{{...}}` to fill in.
const TEXT_FILES: [(&str, &str); 10] = [
    (".gitignore", include_str!("../../template/gitignore")),
    ("AGENTS.md", include_str!("../../template/AGENTS.md")),
    ("CLAUDE.md", include_str!("../../template/CLAUDE.md")),
    ("manifest.ts", include_str!("../../template/manifest.ts")),
    (
        "migrations/.gitkeep",
        include_str!("../../template/migrations/.gitkeep"),
    ),
    ("package.json", include_str!("../../template/package.json")),
    (
        "src/db/schema.ts",
        include_str!("../../template/src/db/schema.ts"),
    ),
    ("src/index.ts", include_str!("../../template/src/index.ts")),
    (
        "tsconfig.json",
        include_str!("../../template/tsconfig.json"),
    ),
    ("vm.dev.json", include_str!("../../template/vm.dev.json")),
];

/// The path of the SDK's tarball in a project.
pub fn tarball_path() -> String {
    format!(".vm/packages/vm-sdk-{}.tgz", versions::SDK)
}

/// One file of a new project.
pub struct File {
    /// Relative to the project, with `/` between names.
    pub path: String,
    /// Borrowed from the program when there is nothing to fill in.
    pub bytes: Cow<'static, [u8]>,
}

/// Every file of a new project named `name` (an Application's name, so
/// plain in JSON and in a TypeScript string), in the order of their paths.
pub fn files(name: &str) -> Vec<File> {
    let mut files: Vec<File> = TEXT_FILES
        .iter()
        .map(|(path, text)| File {
            path: (*path).to_owned(),
            bytes: if text.contains("{{") {
                Cow::Owned(filled(text, name).into_bytes())
            } else {
                Cow::Borrowed(text.as_bytes())
            },
        })
        .collect();
    files.push(File {
        path: tarball_path(),
        bytes: Cow::Borrowed(SDK_TARBALL),
    });
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// `text` with `{{name}}` and the versions filled in.
fn filled(text: &str, name: &str) -> String {
    [
        ("{{name}}", name),
        ("{{sdk}}", versions::SDK),
        ("{{drizzle-orm}}", versions::DRIZZLE_ORM),
        ("{{drizzle-kit}}", versions::DRIZZLE_KIT),
        ("{{typescript}}", versions::TYPESCRIPT),
        ("{{esbuild}}", versions::ESBUILD),
    ]
    .iter()
    .fold(text.to_owned(), |text, (mark, value)| {
        text.replace(mark, value)
    })
}
