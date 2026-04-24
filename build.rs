//! Build script — macOS only.
//!
//! Links `vendor/Syphon.framework.prebuilt/Syphon.framework` into the
//! final binary. A pre-built framework is committed to the tree (see
//! `vendor/Syphon.framework.prebuilt/README.md` for provenance) so
//! PatchWork builds with only Command Line Tools installed — no full
//! Xcode required.
//!
//! On non-macOS targets this script is a no-op so cross-compilation
//! from non-mac hosts still works for the non-Syphon parts of the tree.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

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
