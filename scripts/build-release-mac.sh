#!/usr/bin/env bash
#
# Build the macOS release Editor and Player without personal build paths.
#
#   scripts/build-release-mac.sh [--allow-modified] [extra cargo build args]
#
# A plain `cargo build --release` records absolute source paths in the
# binaries: dependency sources under CARGO_HOME (/Users/<account>/.cargo/...),
# the standard library's rust-src copy under the rustup toolchain, and the
# __FILE__ strings of C/C++ dependencies compiled by build scripts (libopus,
# hidapi, ring). It also records the vendored Syphon.framework's absolute
# folder as a runtime search path (build.rs; development only).
#
# This script builds both binaries in one cargo invocation (so they share one
# source revision) with:
#   - rustc --remap-path-prefix (stable):
#       CARGO_HOME                         -> /cargo
#       <sysroot>/lib/rustlib/src/rust     -> /rustc/<rustc commit hash>
#       this repository                    -> /patchwork
#       the target folder (if outside it)  -> /target
#   - clang -ffile-prefix-map for C/C++ (through CFLAGS/CXXFLAGS, which the
#     `cc` and `cmake` crates pass on), for CARGO_HOME and the repository;
#   - PATCHWORK_RELOCATABLE_RPATHS=1 (build.rs): no absolute framework
#     search path in any binary, only @loader_path/../Frameworks and
#     @loader_path/../Resources/Frameworks;
#   - LIBOPUS_NO_PKG=1: libopus is built from the vendored source rather than
#     taken from a Homebrew/pkg-config installation, so the result does not
#     depend on what the build Mac has installed.
# Panic and error locations keep the crate, version, file and line. Nothing
# machine-specific is committed: the real paths are worked out here.
#
# Afterwards both executables are checked, and the script fails if:
#   - either contains the home folder, the repository path or any /Users/ path;
#   - either links a library outside /System/Library, /usr/lib and
#     @rpath/Syphon.framework (e.g. Homebrew dylibs);
#   - either has an absolute runtime search path (LC_RPATH);
#   - either does not record exactly this commit, unmodified (unless
#     --allow-modified), or the two disagree.
#
# These binaries are not runnable from target/ as they are (Syphon is found
# only inside an .app bundle): run them with
#   DYLD_FRAMEWORK_PATH="$PWD/vendor/Syphon.framework.prebuilt" target/release/patchwork
# No signing, bundling or notarization happens here (see release-mac.sh).
#
# Changing these flags makes cargo rebuild the dependencies once. libopus's
# build script (audiopus_sys) does not watch CFLAGS: if it was built before
# without this script, run `cargo clean --release -p audiopus_sys` first (the
# path check below catches it).

set -euo pipefail

die() {
    echo "error: $*" >&2
    exit 1
}

[[ "$(uname -s)" == "Darwin" ]] || die "this script builds on macOS (uname: $(uname -s))."

ALLOW_MODIFIED=0
CARGO_ARGS=()
for arg in "$@"; do
    case "$arg" in
        --allow-modified) ALLOW_MODIFIED=1 ;;
        -h|--help)
            sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) CARGO_ARGS+=("$arg") ;;
    esac
done

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$REPO"

[[ -z "${CARGO_ENCODED_RUSTFLAGS:-}" ]] ||
    die "CARGO_ENCODED_RUSTFLAGS is already set; unset it (or use RUSTFLAGS) and run again."

CARGO_HOME_DIR="${CARGO_HOME:-$HOME/.cargo}"
[[ -d "$CARGO_HOME_DIR" ]] || die "CARGO_HOME ($CARGO_HOME_DIR) not found."
CARGO_HOME_PHYS="$(cd "$CARGO_HOME_DIR" && pwd -P)"
SYSROOT="$(rustc --print sysroot)"
RUST_SRC="$SYSROOT/lib/rustlib/src/rust"
RUSTC_HASH="$(rustc -vV | sed -n 's/^commit-hash: //p')"
RUSTC_HOST="$(rustc -vV | sed -n 's/^host: //p')"
[[ -n "$RUSTC_HASH" ]] || die "rustc -vV did not report a commit hash."
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target}"
mkdir -p "$TARGET_DIR"
TARGET_DIR="$(cd "$TARGET_DIR" && pwd -P)"
HEAD="$(git rev-parse --verify HEAD)"

# The inputs build.rs compares with the commit (see source_provenance).
if [[ "$ALLOW_MODIFIED" == 0 ]] &&
    [[ -n "$(git status --porcelain --untracked-files=all -- src assets build.rs Cargo.toml Cargo.lock LICENSE)" ]]; then
    die "build inputs differ from commit $HEAD (git status); commit them, or pass --allow-modified for a local build."
fi

# `cc` splits CFLAGS at whitespace, so the mapped prefixes must not contain any.
for p in "$CARGO_HOME_DIR" "$CARGO_HOME_PHYS" "$REPO"; do
    [[ "$p" != *[[:space:]]* ]] || die "path contains whitespace, which CFLAGS cannot carry: $p"
done

# rustc: the last matching mapping wins, so the more specific ones come later.
FLAGS=()
if [[ -n "${RUSTFLAGS:-}" ]]; then
    read -r -a EXTRA <<<"$RUSTFLAGS"
    FLAGS+=("${EXTRA[@]}")
fi
case "$TARGET_DIR" in
    "$REPO"/*) ;;
    *) FLAGS+=("--remap-path-prefix=$TARGET_DIR=/target") ;;
esac
FLAGS+=("--remap-path-prefix=$REPO=/patchwork")
FLAGS+=("--remap-path-prefix=$CARGO_HOME_DIR=/cargo")
[[ "$CARGO_HOME_PHYS" == "$CARGO_HOME_DIR" ]] || FLAGS+=("--remap-path-prefix=$CARGO_HOME_PHYS=/cargo")
FLAGS+=("--remap-path-prefix=$RUST_SRC=/rustc/$RUSTC_HASH")

C_MAP="-ffile-prefix-map=$REPO=/patchwork -ffile-prefix-map=$CARGO_HOME_DIR=/cargo"
[[ "$CARGO_HOME_PHYS" == "$CARGO_HOME_DIR" ]] || C_MAP="$C_MAP -ffile-prefix-map=$CARGO_HOME_PHYS=/cargo"

echo "Building commit $HEAD for $RUSTC_HOST"
(
    IFS=$'\x1f'
    export CARGO_ENCODED_RUSTFLAGS="${FLAGS[*]}"
    unset IFS
    export CFLAGS="${CFLAGS:+$CFLAGS }$C_MAP"
    export CXXFLAGS="${CXXFLAGS:+$CXXFLAGS }$C_MAP"
    export PATCHWORK_RELOCATABLE_RPATHS=1
    export LIBOPUS_NO_PKG=1
    cargo build --release --locked --bin patchwork --bin patchwork-player ${CARGO_ARGS[@]+"${CARGO_ARGS[@]}"}
)

# ── Checks ──────────────────────────────────────────────────────────────────
FAILED=0
fail() {
    echo "FAIL: $*"
    FAILED=1
}
FIRST_COMMIT=""
for pair in editor:patchwork player:patchwork-player; do
    flavor="${pair%%:*}"
    exe="$TARGET_DIR/release/${pair#*:}"
    [[ -f "$exe" ]] || { fail "$exe was not built"; continue; }
    name="$(basename "$exe")"
    echo ""
    echo "== $name"
    echo "   architectures: $(lipo -archs "$exe")"
    echo "   minimum macOS: $(otool -l "$exe" | awk '/LC_BUILD_VERSION/{f=1} f && /minos/{print $2; exit}')"
    echo "   sha256: $(shasum -a 256 "$exe" | cut -d' ' -f1)"

    # Personal and machine paths.
    for needle in "$HOME" "$REPO" "/Users/"; do
        n="$(LC_ALL=C grep -a -c -F -- "$needle" "$exe" || true)"
        if [[ "$n" != 0 ]]; then
            fail "$name contains '$needle' ($n lines), e.g.:"
            LC_ALL=C grep -a -o -F -- "$needle" "$exe" >/dev/null &&
                LC_ALL=C grep -a -o -E -- "/Users/[[:print:]]{0,80}" "$exe" | sort -u | head -5 | sed 's/^/       /'
        fi
    done

    # Linked libraries (lines ending in ':' are file or architecture headers).
    LIBS="$(otool -L "$exe" | awk '$0 !~ /:$/ {print $1}')"
    while IFS= read -r lib; do
        [[ -n "$lib" ]] || continue
        case "$lib" in
            /System/Library/* | /usr/lib/* | @rpath/Syphon.framework/*) ;;
            *) fail "$name links $lib (only system libraries and Syphon.framework may be linked)" ;;
        esac
    done <<<"$LIBS"
    echo "   linked libraries:"
    printf '%s\n' "$LIBS" | sed 's/^/     /'

    # Runtime search paths.
    echo "   runtime search paths:"
    while IFS= read -r rpath; do
        echo "     $rpath"
        case "$rpath" in
            @loader_path/* | @executable_path/*) ;;
            *) fail "$name has a non-relocatable runtime search path: $rpath" ;;
        esac
    done < <(otool -l "$exe" | awk '/cmd LC_RPATH/{f=1; next} f && /^ *path /{print $2; f=0}')

    # Source provenance (one record per architecture slice).
    records="$(LC_ALL=C grep -a -o 'patchwork-build-info:v1;[a-z0-9=;]*' "$exe" | sort -u)"
    echo "   build record: $records"
    if [[ "$(printf '%s\n' "$records" | grep -c .)" != 1 ]]; then
        fail "$name does not record exactly one build"
    else
        [[ "$records" == *"flavor=$flavor;"* ]] || fail "$name is not the $flavor build"
        [[ "$records" == *"commit=$HEAD;"* ]] || fail "$name was not built from $HEAD"
        if [[ "$ALLOW_MODIFIED" == 0 ]]; then
            [[ "$records" == *"modified=0;"* ]] || fail "$name was built from modified source"
        fi
        commit="$(printf '%s' "$records" | sed -n 's/.*commit=\([^;]*\);.*/\1/p')"
        if [[ -z "$FIRST_COMMIT" ]]; then
            FIRST_COMMIT="$commit"
        elif [[ "$commit" != "$FIRST_COMMIT" ]]; then
            fail "the Editor and the Player record different commits"
        fi
    fi
done

echo ""
if [[ "$FAILED" != 0 ]]; then
    echo "If C/C++ dependencies were built earlier without this script, run"
    echo "  cargo clean --release -p audiopus_sys"
    echo "and build again (a fresh target folder is always clean)."
    exit 1
fi
echo "OK: both binaries are free of personal paths, link only system libraries and"
echo "Syphon.framework, use relocatable search paths and record commit $HEAD."
