# Build the Windows release Editor and Player without personal build paths.
#
#   powershell -ExecutionPolicy Bypass -File scripts\build-release-windows.ps1 [extra cargo args]
#
# A plain `cargo build --release` embeds the absolute paths of the sources
# it compiled: dependency sources under CARGO_HOME (by default
# C:\Users\<account>\.cargo\registry\src\...) and, when the rust-src
# component is installed, the standard library under the rustup toolchain
# (C:\Users\<account>\.rustup\...). They end up in panic and error
# locations, so a shared executable would carry the build machine's
# account name.
#
# This script passes rustc's stable --remap-path-prefix so those paths are
# recorded under fixed, machine-independent names instead:
#   CARGO_HOME                      -> /cargo   (/cargo/registry/src/<index>/<crate>-<version>/src/...)
#   <sysroot>\lib\rustlib\src\rust  -> /rustc/<rustc commit hash>  (rustc's own name for these)
#   this repository                 -> /patchwork
# Crash and error messages keep the crate, version, file and line. Nothing
# machine-specific is committed: the real paths are worked out here, at
# build time. Program behaviour is unchanged.
#
# The flags go in CARGO_ENCODED_RUSTFLAGS (so paths containing spaces work)
# after any RUSTFLAGS you already set. Changing flags makes cargo rebuild
# the dependencies once. Afterwards, both executables are checked and the
# build fails if the account name or the profile path is still in either.

$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$cargoHome = [IO.Path]::GetFullPath($cargoHome).TrimEnd('\')
$sysroot = (& rustc --print sysroot).Trim()
if ($LASTEXITCODE -ne 0 -or -not $sysroot) { throw 'rustc --print sysroot failed' }
$rustSrc = Join-Path $sysroot 'lib\rustlib\src\rust'
$hash = ((& rustc -vV) | Where-Object { $_ -like 'commit-hash: *' }) -replace '^commit-hash: ', ''
if (-not $hash) { throw 'rustc -vV did not report a commit hash' }

if ($env:CARGO_ENCODED_RUSTFLAGS) {
  throw 'CARGO_ENCODED_RUSTFLAGS is already set; unset it (or put the flags in RUSTFLAGS) and run again.'
}
$flags = @()
if ($env:RUSTFLAGS) { $flags += ($env:RUSTFLAGS -split '\s+' | Where-Object { $_ }) }
# The last matching mapping wins, so the more specific ones come later.
$flags += "--remap-path-prefix=$repo=/patchwork"
$flags += "--remap-path-prefix=$cargoHome=/cargo"
$flags += "--remap-path-prefix=$rustSrc=/rustc/$hash"

$env:CARGO_ENCODED_RUSTFLAGS = $flags -join [char]0x1f
try {
  Push-Location $repo
  & cargo build --release --locked --bin patchwork --bin patchwork-player @args
  if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }
} finally {
  Pop-Location
  Remove-Item Env:CARGO_ENCODED_RUSTFLAGS -ErrorAction SilentlyContinue
}

# Check: no account name or profile path left in either executable.
$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo 'target' }
$needles = @($env:USERPROFILE, $env:USERNAME) | Where-Object { $_ } | Select-Object -Unique
$latin1 = [Text.Encoding]::GetEncoding(28591)
$failed = $false
foreach ($exe in 'patchwork.exe', 'patchwork-player.exe') {
  $path = Join-Path $targetDir "release\$exe"
  $bytes = [IO.File]::ReadAllBytes($path)
  $ascii = $latin1.GetString($bytes)
  $wide = [Text.Encoding]::Unicode.GetString($bytes)
  $wideOdd = [Text.Encoding]::Unicode.GetString($bytes, 1, $bytes.Length - 1)
  $found = $false
  foreach ($needle in $needles) {
    foreach ($text in $ascii, $wide, $wideOdd) {
      if ($text.IndexOf($needle, [StringComparison]::OrdinalIgnoreCase) -ge 0) { $found = $true }
    }
  }
  if ($found) {
    Write-Host "FAIL: $exe still contains a build-machine path or account name."
    $failed = $true
  } else {
    Write-Host "OK: $exe contains no build-machine account name or profile path."
  }
}
if ($failed) { exit 1 }
