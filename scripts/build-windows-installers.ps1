# Build "Patchwork Setup.exe": the Patchwork editor installer (internal test
# builds while embedded components are not cleared for redistribution).
#
#   powershell -ExecutionPolicy Bypass -File scripts\build-windows-installers.ps1 `
#     -Ffmpeg C:\tools\ffmpeg\bin\ffmpeg.exe `
#     -FfmpegLicences C:\tools\ffmpeg\LICENSE,C:\tools\ffmpeg\README.txt `
#     -Out D:\installers
#
# 1. Builds the editor and the Player with build-release-windows.ps1 (no
#    personal paths) unless -SkipBuild.
# 2. Copies the release programs (and DirectML.dll) to <Out>\release so the
#    package is made from exactly those files.
# 3. Packages the editor (export::editor_package) and builds its installer
#    with NSIS 3 (export::installer), via the manual test
#    manual_build_editor_installer. Without makensis the installer source is
#    written instead; NSIS is never installed by this script.
#
# Apps exported from Patchwork get their installers from File > Export as App
# > "Create Windows installer (.exe)".
param(
  [Parameter(Mandatory = $true)][string]$Ffmpeg,
  [Parameter(Mandatory = $true)][string[]]$FfmpegLicences,
  [Parameter(Mandatory = $true)][string]$Out,
  [switch]$SkipBuild
)
$ErrorActionPreference = 'Stop'
# With -File, a list arrives as one "a,b" string: split it (',' or ';').
$FfmpegLicences = @($FfmpegLicences | ForEach-Object { $_ -split '[,;]' } | Where-Object { $_ })
$repo = Split-Path -Parent $PSScriptRoot
if (-not $SkipBuild) {
  & (Join-Path $PSScriptRoot 'build-release-windows.ps1')
  if ($LASTEXITCODE -ne 0) { throw "build-release-windows.ps1 failed (exit $LASTEXITCODE)" }
}
New-Item -ItemType Directory -Force $Out | Out-Null
$release = Join-Path $Out 'release'
if (Test-Path $release) { throw "$release already exists; choose a new -Out folder." }
New-Item -ItemType Directory $release | Out-Null
foreach ($f in 'patchwork.exe', 'patchwork-player.exe', 'DirectML.dll') {
  Copy-Item (Join-Path $repo "target\release\$f") $release
}
$env:PATCHWORK_EDITOR_EXE = Join-Path $release 'patchwork.exe'
$env:PATCHWORK_EXPORT_FFMPEG = $Ffmpeg
$env:PATCHWORK_EXPORT_FFMPEG_LICENCES = ($FfmpegLicences -join ';')
$env:PATCHWORK_INSTALLER_OUT = $Out
Push-Location $repo
try {
  & cargo test --release --locked --bin patchwork -- --ignored manual_build_editor_installer --nocapture
  if ($LASTEXITCODE -ne 0) { throw "building the editor installer failed (exit $LASTEXITCODE)" }
} finally {
  Pop-Location
}
