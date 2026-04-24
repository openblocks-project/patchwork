# Syphon.framework (pre-built)

This is a pre-built Metal-capable `Syphon.framework` binary, committed here
so PatchWork can build without requiring a full Xcode installation on every
developer machine (Command Line Tools alone don't ship the Metal shader
toolchain needed to compile Syphon from source).

## Provenance

Extracted from **OBS.app**'s `Contents/Frameworks/` on 2026-04-23. OBS's
build is a universal `arm64` + `x86_64` binary with the full Metal surface
(`SyphonMetalServer`, `SyphonMetalClient`) and all their dependencies.

Verified via `nm -g Syphon.framework/Syphon | grep SyphonMetal` —
`_OBJC_CLASS_$_SyphonMetalServer` and `_OBJC_CLASS_$_SyphonMetalClient`
are both exported.

## Why not build from source?

See `/docs/video_io_spec.md` §7.1 and the phased plan at
`~/.claude/plans/no-i-want-to-golden-breeze.md` Phase 2. The committed
source at `vendor/Syphon-Framework/` (git submodule) requires
`xcodebuild` + the Metal toolchain (`metal`, `metallib`), which are
bundled only with full Xcode, not Command Line Tools. Vendoring a
pre-built binary skips the 10 GB Xcode requirement for contributors
who just want to build PatchWork.

When we graduate to a CI-built framework, the source submodule stays
as the authoritative reference — this prebuilt copy can be regenerated
from it on a CI macOS runner.

## License

Syphon Framework is BSD-3-Clause, © 2010 bangnoise (Tom Butterworth)
& vade (Anton Marini). The full license text is at
`vendor/Syphon-Framework/License.txt`. Binary redistribution is
permitted subject to reproducing the copyright notice — we do that via
this README + the license file in the source submodule.

## Do NOT edit

This directory is treated as an opaque vendor drop. If Syphon needs
updating, re-extract from a newer OBS / TouchDesigner build OR compile
the submodule on a Xcode-equipped machine and replace the contents.
