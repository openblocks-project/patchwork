//! Build script.
//!
//! All targets: records the source commit the binary is built from
//! ([`source_provenance`], P6 Stage 2.3).
//!
//! macOS:
//! Links `vendor/Syphon.framework.prebuilt/Syphon.framework` into the
//! final binary. A pre-built framework is committed to the tree (see
//! `vendor/Syphon.framework.prebuilt/README.md` for provenance) so
//! PatchWork builds with only Command Line Tools installed — no full
//! Xcode required.
//!
//! On non-macOS targets the Syphon part is skipped so cross-compilation
//! from non-mac hosts still works for the non-Syphon parts of the tree.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    source_provenance();

    if !cfg!(target_os = "macos") {
        return;
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let framework_dir = manifest_dir
        .join("vendor")
        .join("Syphon.framework.prebuilt");
    let framework_bin = framework_dir
        .join("Syphon.framework")
        .join("Versions/A/Syphon");

    // Rebuild-trigger: if someone re-vendors the binary, relink.
    println!("cargo:rerun-if-changed={}", framework_bin.display());

    if !framework_bin.exists() {
        panic!(
            "vendored Syphon.framework missing — expected at {}\n\n\
             The framework should be committed to vendor/Syphon.framework.prebuilt/. \
             See that directory's README.md for how to regenerate.",
            framework_bin.display()
        );
    }

    // Tell rustc where to find the framework at link time.
    println!(
        "cargo:rustc-link-search=framework={}",
        framework_dir.display()
    );
    println!("cargo:rustc-link-lib=framework=Syphon");

    // Runtime rpath: tried in order at launch; first hit wins.
    //   1. Absolute dev path — so `cargo run` finds the vendored
    //      framework without installing anything.
    //   2. Standard Apple convention — `.app/Contents/Frameworks/`.
    //   3. `cargo-packager`'s convention — it puts `resources`
    //      entries at `.app/Contents/Resources/…` instead of
    //      `Contents/Frameworks/`. Packaging copies the vendored
    //      framework to `Contents/Resources/Frameworks/Syphon.framework`,
    //      so this rpath resolves in the packaged `.app`.
    println!(
        "cargo:rustc-link-arg=-Wl,-rpath,{}",
        framework_dir.display()
    );
    println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path/../Frameworks");
    println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path/../Resources/Frameworks");
}

/// P6 Stage 2.3: record which source this binary is built from, so an
/// exported app can name its corresponding source exactly.
///
/// - `PATCHWORK_SOURCE_COMMIT`: the Git commit (40 hex digits), or
///   `unknown` outside a Git checkout or without Git.
/// - `PATCHWORK_SOURCE_MODIFIED`: `0` when the build inputs (`src`,
///   `assets`, `build.rs`, `Cargo.toml`, `Cargo.lock`, `LICENSE`) match that
///   commit exactly, `1` when they have uncommitted or untracked changes,
///   `unknown` when it cannot be told.
///
/// Only the local `git` is run; nothing is fetched.
fn source_provenance() {
    const INPUTS: &[&str] = &[
        "src",
        "assets",
        "build.rs",
        "Cargo.toml",
        "Cargo.lock",
        "LICENSE",
    ];
    let git = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git").args(args).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let commit = git(&["rev-parse", "--verify", "HEAD"])
        .map(|s| s.trim().to_string())
        .filter(|s| s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()));
    let modified = commit.as_ref().and_then(|_| {
        let mut args = vec!["status", "--porcelain", "--untracked-files=all", "--"];
        args.extend_from_slice(INPUTS);
        git(&args).map(|s| if s.trim().is_empty() { "0" } else { "1" })
    });
    println!(
        "cargo:rustc-env=PATCHWORK_SOURCE_COMMIT={}",
        commit.as_deref().unwrap_or("unknown")
    );
    println!(
        "cargo:rustc-env=PATCHWORK_SOURCE_MODIFIED={}",
        modified.unwrap_or("unknown")
    );
    // Rerun when the commit or the build inputs change.
    if let Some(dir) = git(&["rev-parse", "--git-dir"]) {
        let dir = PathBuf::from(dir.trim());
        for file in ["HEAD", "index", "packed-refs"] {
            println!("cargo:rerun-if-changed={}", dir.join(file).display());
        }
        if let Some(head) = git(&["symbolic-ref", "-q", "HEAD"]) {
            println!("cargo:rerun-if-changed={}", dir.join(head.trim()).display());
        }
    }
    for input in INPUTS {
        println!("cargo:rerun-if-changed={input}");
    }
}
