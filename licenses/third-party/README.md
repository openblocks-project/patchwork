# Third-party notices of the Windows Player runtime

Every exported Windows app ships these files in `licenses/third-party/`
(P6 Stage 2.4). They cover what is inside `patchwork-player.exe` and the
DLLs bundled beside it. Reviewed 2026-11-19 (fonts and models: P6 Stage 2.5).

| Component | How it is included | Licence | File |
|---|---|---|---|
| 537 Rust crates (exactly `cargo tree -e normal --target x86_64-pc-windows-msvc`) | statically linked | MIT, Apache-2.0, ISC, MPL-2.0, BSD-2/3-Clause, Unicode-3.0, Zlib, BSL-1.0, CC0-1.0, CDLA-Permissive-2.0, OFL-1.1, Ubuntu-font-1.0 (choices resolved by cargo-about) | `rust-crates.txt` |
| ONNX Runtime 1.24.2 (pyke `ort` distribution of Microsoft's source) | statically linked (`onnxruntime.lib`) | MIT, plus its own third-party notices | `onnxruntime-LICENSE.txt`, `onnxruntime-ThirdPartyNotices.txt` |
| DirectML 1.15.4 | `DirectML.dll`, pinned by SHA-256 | Microsoft Software License Terms (DirectML): distribution in applications allowed | `DirectML-LICENSE.txt` |
| Microsoft Visual C++ runtime | `vcruntime140*.dll`, `msvcp140*.dll` from an installed Visual Studio's `VC\Redist` | Visual Studio Distributable Code (requires a licensed Visual Studio) | written per export (`VCRuntime-NOTICE.txt`) |
| libopus (Xiph) | compiled from source by `audiopus_sys` 0.2.2, statically linked | BSD-3-Clause | `libopus-COPYING.txt` |
| HIDAPI C library | compiled by `hidapi` 2.6.5 | used under its BSD-style option | `hidapi-LICENSE.txt`, `hidapi-LICENSE-bsd.txt` |
| ring 0.17.14 C/assembly (BoringSSL-derived) | compiled, statically linked | ISC, Apache-2.0 (per file) | `ring-LICENSE*.txt` |
| Phosphor icon font | embedded (`assets/fonts/Phosphor.ttf`) | MIT | `Phosphor-LICENSE.txt` |
| Outfit font | embedded (`assets/fonts/Outfit-Regular.ttf`) | OFL-1.1 | `Outfit-OFL.txt` |
| egui default fonts (Ubuntu, Noto Emoji, emoji-icon-font) | embedded by `epaint_default_fonts` | OFL-1.1, Ubuntu-font-1.0 | in `rust-crates.txt` |

Sources: crate texts from the crates' own packages (cargo-about); ONNX
Runtime texts from Microsoft's NuGet package
`Microsoft.ML.OnnxRuntime.Managed` 1.24.2 (source commit
`058787ceead760166e3c50a0a4cba8a833a6f53f`); DirectML terms from
https://www.nuget.org/packages/Microsoft.AI.DirectML/1.15.4/License; libopus,
HIDAPI and ring texts from the crate sources that are compiled; Phosphor from
https://github.com/phosphor-icons/homepage/blob/master/LICENSE.

## Fonts (P6 Stage 2.5)

- **Outfit Regular** (`assets/fonts/Outfit-Regular.ttf`, 75,132 bytes,
  SHA-256 `3b64ac4f6ab6a8eebddd4b0bc03c811c43602e11e176382ab0ee6be615ab861b`):
  the primary UI font and the Text-to-Image font. Unmodified copy of
  `fonts/ttf/Outfit-Regular.ttf` from https://github.com/Outfitio/Outfit-Fonts
  (commit `902773808eb372f70fb34e8946dd1ffe604efc79`), version 1.100,
  © 2021 The Outfit Project Authors, SIL Open Font License 1.1
  (`Outfit-OFL.txt`). OFL-1.1 allows bundling and redistribution with
  software.
- **Satoshi Regular** was removed. Its file (SHA-256
  `b94308c3dc042ffbca813300ce851412100b6526ee03594c3137a9bbfab60952`) is the
  official `Satoshi_Complete/Fonts/WEB/fonts/Satoshi-Regular.ttf` from
  Fontshare, under the ITF Free Font License 2.0 (17 Aug 2026). §02 of that
  licence forbids making the font available to others through an
  application, and embedding it in a design tool as a font third parties use
  to generate their own content; both describe Patchwork and its exports.
  Using it would need ITF's written consent.

## Not cleared: exports are refused until resolved

The six MediaPipe models (`assets/presets/ml/MediaPipe*.onnx`) are embedded
in every Patchwork binary, so in every export. None could be cleared, so the
exporter refuses to produce an app (`runtime::UNRESOLVED_COMPONENTS`):

| File | SHA-256 | Format | Source | Status |
|---|---|---|---|---|
| MediaPipeHandDetector.onnx | `2af615f1e6df6ac0ef01a85d93713ea21c9bc59a87524caa3d5f16b8e74e60f3` | ONNX (PyTorch 2.2.1 export) | qualcomm/MediaPipe-Hand-Detection @ 7c266a43 (2025-05-19) | Qualcomm AI Hub Proprietary License: blocked |
| MediaPipeFaceLandmarkDetector.onnx | `3482ce52873546a94a05c2fb9d38e3499adba87a7c55745061cb8a375ee838df` | ONNX (PyTorch 2.2.1 export) | qualcomm/MediaPipe-Face-Detection @ d19ecb46 (2025-05-19) | Qualcomm AI Hub Proprietary License: blocked |
| MediaPipePoseDetector.onnx | `8288a955cb0b379e520fd3bc76b6c19b670e9bac39ed3c26898fc7f513cbe6d8` | ONNX (PyTorch 2.2.1 export) | qualcomm/MediaPipe-Pose-Estimation @ 752a604a (2024-11-06) | Qualcomm AI Hub Proprietary License: blocked |
| MediaPipeFaceDetector.onnx | `4d4700749360befd0d9fb56fedd2e8f682649de826dfa34dbeccbb169dcb7dbe` | ONNX (tf2onnx 1.16.1) | not found | unknown: blocked |
| MediaPipeHandLandmark.onnx | `ce9cd3e871d8b11819ee6a695c44fd8ac2dee7361b5d94c55d01803e99221744` | ONNX (tf2onnx 1.16.1) | not found | unknown: blocked |
| MediaPipePoseLandmark.onnx | `91366321af68df7ce3b7f786c17e668e07327fe0f3cf41141ffae9767c007600` | ONNX (tf2onnx 1.16.1) | not found | unknown: blocked |

Evidence: the Hugging Face API's LFS SHA-256 of every file in every revision
of qualcomm/MediaPipe-{Hand-Detection,Face-Detection,Pose-Estimation}
(327, 409 and 346 revisions) and opencv/{palm_detection,handpose_estimation,
person_detection,pose_estimation}_mediapipe. The Qualcomm model cards say
"The license for the compiled assets for on-device deployment" is the
Qualcomm AI Hub Proprietary License (PDF SHA-256
`4409f93b0e82531303b3e10f52f1fdfb56467a25f05b7441c6bbd8bb8a64b42c`): it is
revocable without cause, forbids reverse engineering and some uses, and
requires indemnification, which are further restrictions incompatible with
conveying them as part of a GPL-3.0 program and would bind every export
author. The original implementation's Apache-2.0 licence
(zmurez/MediaPipePyTorch) does not cover these exported assets.

Replacement options (each needs the ML pipeline adapted to its inputs and
outputs, then its notice added here):
- Google's official MediaPipe models (Apache-2.0 model cards, TFLite),
  converted to ONNX by Patchwork itself, keeping Google's notices.
- OpenCV Model Zoo's MediaPipe conversions (Apache-2.0):
  palm_detection_mediapipe, handpose_estimation_mediapipe,
  person_detection_mediapipe, pose_estimation_mediapipe; and YuNet (MIT) for
  face detection.
- A written licence from Qualcomm permitting GPL-compatible redistribution.

To clear one: record its source and licence here, add its notice file, and
remove it from `UNRESOLVED_COMPONENTS` (and `UNRESOLVED_MODEL_SHA256`) in
`src/app/export/runtime.rs`.

### Internal test builds (P6 Stage 2.6)

All six models are kept, unchanged, in the editor and in every Player
runtime; hand, face and pose detection work as before. Until they are
cleared:

- **Distributable** exports stay refused, listing each model.
- **Internal test builds** (Export dialog → Build → "Internal test build —
  NOT FOR DISTRIBUTION") are exported for development and testing only.
  Each one is labelled `INTERNAL TEST BUILD — NOT FOR DISTRIBUTION`:
  - in its folder name: `<app> (INTERNAL TEST - NOT FOR DISTRIBUTION)`;
  - at its root, `INTERNAL-TEST-BUILD-NOT-FOR-DISTRIBUTION.txt`, and the same
    text in `licenses/NOT-FOR-DISTRIBUTION.txt`. Both list each model with
    its SHA-256 and what is outstanding;
  - at the top of `licenses/THIRD-PARTY-NOTICES.txt`;
  - in the Player's window title and as an on-screen badge. These come from
    the runtime itself (it embeds the models), so editing or deleting files
    in the export does not remove them.

An internal test build is never treated as cleared. Making one changes
nothing about the licence state, and the exporter checks that a
distributable build contains nothing uncleared and no internal marker.

Outstanding before any distribution, for each model: establish its source
and a redistribution licence compatible with GPL-3.0 (or get one in writing
from the rights holder), or replace it. Then add its notice here and export
again as a distributable build.

## Regenerating

`rust-crates.txt` must match `Cargo.lock`. After any dependency change run
`powershell -File licenses\third-party\regenerate.ps1` (needs
`cargo install cargo-about --locked --features cli`). `about.toml` lists the
accepted licences; generation fails on any other.
