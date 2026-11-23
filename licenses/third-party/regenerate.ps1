# Regenerates rust-crates.txt, the licence notices of the Rust crates linked
# into the Windows Player runtime (P6 Stage 2.4).
#
# Requires cargo-about 0.9 (`cargo install cargo-about --locked --features cli`).
# Run from anywhere:  powershell -File licenses\third-party\regenerate.ps1
#
# The first lines record the SHA-256 of Cargo.lock (line endings normalised
# to LF). The test `rust_crate_notices_match_cargo_lock` fails when
# Cargo.lock changes without regenerating this file.
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$root = Resolve-Path (Join-Path $here '..\..')
$out = Join-Path $here 'rust-crates.txt'
$tmp = [System.IO.Path]::GetTempFileName()
Push-Location $root
try {
    cargo about generate --locked --fail `
        -c licenses/third-party/about.toml `
        licenses/third-party/about.hbs -o $tmp
    if ($LASTEXITCODE -ne 0) { throw "cargo about failed ($LASTEXITCODE)" }
    $lock = [System.IO.File]::ReadAllText((Join-Path $root 'Cargo.lock')) -replace "`r`n", "`n"
    $sha = [System.Security.Cryptography.SHA256]::Create()
    $hash = -join ($sha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($lock)) |
        ForEach-Object { $_.ToString('x2') })
    $body = [System.IO.File]::ReadAllText($tmp) -replace "`r`n", "`n"
    $text = "Cargo.lock SHA-256 (LF): $hash`n$body"
    [System.IO.File]::WriteAllText($out, $text, (New-Object System.Text.UTF8Encoding $false))
    Write-Host "wrote $out for Cargo.lock $hash"
}
finally {
    Pop-Location
    Remove-Item $tmp -ErrorAction SilentlyContinue
}
