//! Embed an identity for every file that can change the shipped implementation.
//!
//! A Git commit alone is insufficient for local builds because a wheel can be
//! built from a dirty checkout, and an sdist normally has no `.git` directory.
//! The digest below therefore covers the manifests, locked dependencies, build
//! configuration, Rust sources, and Python package sources that determine the
//! installed package. The same files are present in a source distribution, so
//! rebuilding that distribution retains the identity without trusting an
//! externally supplied environment variable.

use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

const ROOT_FILES: &[&str] = &[
    ".cargo/config.toml",
    "Cargo.lock",
    "Cargo.toml",
    "README.md",
    "pyproject.toml",
    "rust-toolchain.toml",
];

fn main() {
    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR")
            .expect("Cargo always defines CARGO_MANIFEST_DIR for build scripts"),
    );
    let workspace = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("the Python crate must remain two directories below the workspace root");

    let mut files = ROOT_FILES
        .iter()
        .map(|relative| workspace.join(relative))
        .collect::<Vec<_>>();
    collect_source_files(&workspace.join("crates"), &mut files);
    collect_source_files(&workspace.join("python"), &mut files);
    files.sort_by(|left, right| {
        relative_name(workspace, left).cmp(&relative_name(workspace, right))
    });
    files.dedup();

    let mut digest = Sha256::new();
    for path in files {
        let relative = relative_name(workspace, &path);
        let contents = fs::read(&path).unwrap_or_else(|error| {
            panic!("failed to read source identity file {relative}: {error}")
        });
        let contents = canonical_identity_contents(&relative, contents);
        println!("cargo:rerun-if-changed={}", path.display());

        // Include both the path and the byte length so concatenating two files
        // cannot produce the identity of one differently partitioned file.
        digest.update(relative.as_bytes());
        digest.update([0]);
        digest.update((contents.len() as u64).to_be_bytes());
        digest.update(contents);
    }

    let source_tree_sha256 = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    println!("cargo:rustc-env=CSTRUCT_RS2_SOURCE_TREE_SHA256={source_tree_sha256}");
}

/// Normalize Cargo's semantics-preserving source-archive manifest rewrite.
///
/// `cargo package` copies an inherited workspace README into each member and
/// rewrites `readme.workspace = true` to `readme = "README.md"`. Both forms
/// identify the same bytes and metadata, so hashing their literal spelling
/// would make a source rebuild look unrelated to its checkout build.
fn canonical_identity_contents(relative: &str, contents: Vec<u8>) -> Vec<u8> {
    if relative.starts_with("crates/") && relative.ends_with("/Cargo.toml") {
        String::from_utf8(contents)
            .expect("Cargo manifests covered by source identity must be UTF-8")
            .replace("readme.workspace = true", "readme = \"README.md\"")
            .into_bytes()
    } else {
        contents
    }
}

/// Recursively collect files Cargo or maturin use to build the shipped package.
fn collect_source_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()))
        .map(|entry| {
            entry
                .expect("source directory entry must remain readable")
                .path()
        })
        .collect::<Vec<_>>();
    entries.sort();

    for path in entries {
        if path.is_dir() {
            if path.file_name() != Some(OsStr::new("__pycache__")) {
                collect_source_files(&path, files);
            }
        } else if is_source_identity_file(&path) {
            files.push(path);
        }
    }
}

/// Return whether a recursively discovered file affects the distributed code.
fn is_source_identity_file(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(OsStr::to_str),
        Some("Cargo.toml" | "build.rs")
    ) || matches!(path.extension().and_then(OsStr::to_str), Some("rs" | "py"))
}

/// Produce the stable, slash-separated path included in the digest.
fn relative_name(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .expect("identity files must remain below the workspace root")
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}
