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
//! On non-macOS targets the Syphon part is skipped. The target is read from
//! `CARGO_CFG_TARGET_OS` (a build script's own `cfg!` describes the machine
//! running the build, not the target being built).
//!
//! Syphon is linked at launch, not weakly: a binary whose runtime search
//! paths do not lead to `Syphon.framework` does not start. The Player
//! runtime (`patchwork-player`) gets only bundle-relative search paths
//! ([`macos_link_directives`]).

use std::path::{Path, PathBuf};

/// The exported apps' runtime (`[[bin]] patchwork-player`).
const PLAYER_BIN: &str = "patchwork-player";
/// Set to `1` by `scripts/build-release-mac.sh`: no binary gets the
/// development machine's absolute framework path.
const RELOCATABLE_ONLY_ENV: &str = "PATCHWORK_RELOCATABLE_RPATHS";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    source_provenance();

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
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

    println!("cargo:rerun-if-env-changed={RELOCATABLE_ONLY_ENV}");
    let relocatable_only = std::env::var(RELOCATABLE_ONLY_ENV).as_deref() == Ok("1");
    for directive in macos_link_directives(
        &framework_dir,
        &development_bins(&manifest_dir),
        relocatable_only,
    ) {
        println!("{directive}");
    }
}

/// The macOS link directives for Syphon.
///
/// Runtime search paths (LC_RPATH), tried in order at launch:
///   1. Development only: the vendored framework's absolute path, so
///      `cargo run` and `cargo test` find it without installing anything.
///      Given to the editor, the spike binaries and their unit tests (cargo
///      applies `rustc-link-arg-bin` to a binary's test build too), never to
///      the Player: an exported app must not carry this computer's paths or
///      search outside its bundle. With `relocatable_only` no binary gets
///      it. (A bare Player in `target/` therefore runs with
///      `DYLD_FRAMEWORK_PATH=vendor/Syphon.framework.prebuilt`.)
///   2. `.app/Contents/Frameworks/` (Apple's convention).
///   3. `.app/Contents/Resources/Frameworks/` (`cargo-packager` puts
///      `resources` entries there).
fn macos_link_directives(
    framework_dir: &Path,
    development_bins: &[String],
    relocatable_only: bool,
) -> Vec<String> {
    let mut out = vec![
        // Where to find the framework at link time (not recorded in the binary).
        format!(
            "cargo:rustc-link-search=framework={}",
            framework_dir.display()
        ),
        "cargo:rustc-link-lib=framework=Syphon".to_string(),
    ];
    if !relocatable_only {
        for bin in development_bins {
            out.push(format!(
                "cargo:rustc-link-arg-bin={bin}=-Wl,-rpath,{}",
                framework_dir.display()
            ));
        }
    }
    out.push("cargo:rustc-link-arg=-Wl,-rpath,@loader_path/../Frameworks".to_string());
    out.push("cargo:rustc-link-arg=-Wl,-rpath,@loader_path/../Resources/Frameworks".to_string());
    out
}

/// Every binary target except the Player: `patchwork` and the binaries
/// Cargo discovers in `src/bin/` (only names of existing targets may be
/// used with `rustc-link-arg-bin`).
fn development_bins(manifest_dir: &Path) -> Vec<String> {
    let mut bins = vec!["patchwork".to_string()];
    if let Ok(entries) = std::fs::read_dir(manifest_dir.join("src").join("bin")) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = if path.is_dir() {
                // src/bin/<name>/main.rs
                path.join("main.rs")
                    .is_file()
                    .then(|| {
                        path.file_name()
                            .and_then(|s| s.to_str())
                            .map(str::to_string)
                    })
                    .flatten()
            } else if path.extension().is_some_and(|e| e == "rs") {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_string)
            } else {
                None
            };
            bins.extend(name);
        }
    }
    bins.retain(|b| b != PLAYER_BIN);
    bins.sort();
    bins.dedup();
    bins
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
