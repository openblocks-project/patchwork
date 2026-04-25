# Video I/O — Architecture Spec

Status: **Draft**. Covers the unified video‑in / video‑out feature work. Companion to the phased plan at `~/.claude/plans/no-i-want-to-golden-breeze.md`.

---

## 1. Goals

- Receive video from any source macOS / Windows / Linux expose: multiple physical cameras, virtual cameras (OBS Virtual Camera, Continuity Camera), screen capture, files, **and** inter‑app bridges (Syphon, NDI, Spout).
- Send the graph's final visuals to displays, to other software (TouchDesigner, Resolume, OBS, …), and — eventually — to any app that consumes a system camera (Zoom, Meet).
- Keep the hot path GPU‑resident. CPU readback happens only when a sink requires it, and no more than once per frame per node.
- Graceful degradation: if an SDK/runtime isn't installed (NDI), that destination disables with a tooltip; nothing else breaks.

## 2. Terminology

| Term | Meaning |
|---|---|
| **Source** | A producer of `PortValue::Image` / `PortValue::GpuImage` (Camera, Screen, File, Syphon Client, NDI Receiver, Spout Receiver). |
| **Sink** | A consumer of `PortValue::Image` / `PortValue::GpuImage` that sends frames somewhere outside the graph (Window on display N, Syphon Server, NDI Sender, Spout Sender, Virtual Camera, File Recorder). |
| **Bridge** | The inter‑app protocol (Syphon, NDI, Spout). Bridges come in pairs: a *server/sender* (sink side) and a *client/receiver* (source side). |
| **GPU path** | Frame is a `wgpu::Texture` in `GpuTextureCache`, never read back to CPU. |
| **CPU path** | Frame is `Arc<ImageData>` with RGBA8 bytes. |
| **Readback** | Copy from GPU texture → CPU `Vec<u8>`. Stalls the GPU queue; avoided unless strictly required. |

## 3. User‑facing node model

Three trait‑based nodes. **One source or one destination per node. Type is picked via dropdown. Fan‑out happens by wiring one upstream into multiple `Video Out` nodes.** This mirrors PatchWork's existing node‑graph idioms (Audio In, Audio Out, OSC In, OSC Out — each one source, one sink).

### 3.1 `Video In` (new — replaces `Camera`)

- **Outputs**: `Frame` (Image).
- **State**: `source: VideoSource`, plus source‑specific config (device_id, file_path, screen_idx, syphon_server_name, ndi_source, spout_sender_name), `res_w`, `res_h`, `active: bool`, `status: String`.
- **UI**: Source dropdown at the top, then a source‑specific row (Camera → device dropdown + ↻ refresh; File → path picker; Syphon → server list dropdown; NDI → discovered‑sources dropdown; Spout → sender‑name dropdown).
- **Lifecycle**: Start / Stop button manages the per‑source decoder/client. On `on_removed` or source change, tear down cleanly (kill ffmpeg child, release SyphonClient, drop NDI recv instance).
- Legacy saved projects that contain `NodeType::Camera { device_index, res_w, res_h, active, status }` are migrated on load to `VideoInNode { source: VideoSource::Camera, device_id: device_index.to_string(), res_w, res_h, active, status, .. }`.

### 3.2 `Visual Output` (existing — unchanged in scope)

- **Inputs**: `Image`.
- **Purpose**: inline preview **only**. Stays as the node you drop to see what the graph is producing. No pop‑out window, no bridges — that work moves to `Video Out`.
- **Deprecation path**: keep existing behaviour. The "Pop Out" button and fullscreen toggle currently inside `VisualOutputNode` will be removed in Phase 1 after `Video Out` ships, to avoid two ways to do the same thing.

### 3.3 `Video Out` (new)

- **Inputs**: `Image`.
- **Outputs**: none (terminal sink).
- **State**: `sink: VideoSink`, plus sink‑specific config (display_id for Window sinks, bridge publish name, resolution override, fps cap), `enabled: bool`, `status: String`.
- **UI**: Destination dropdown at the top, then a sink‑specific row:
  - **Window** → display picker dropdown + fullscreen toggle.
  - **Syphon** (mac only) → publish‑name text field.
  - **NDI** → publish‑name text field + fps cap.
  - **Spout** (win only) → publish‑name text field.
  - **Virtual Camera** (mac only, v1) → explanatory tooltip + "Use OBS as the bridge" one‑time modal.
  - **File recorder** (future) → file path + codec picker.
- **Multi‑destination**: user adds a second `Video Out` node. Same upstream can feed any number of them. No compound‑toggle UI inside a single node.
- **Lifecycle**: on sink change or `on_removed`, tear down the previous sink (stop Syphon server, release NDI sender, close window).

## 4. Core types

```rust
// src/video_io/types.rs

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub enum VideoSource {
    #[default] Camera,
    Screen,
    File,
    Syphon,  // macOS only; variant still serialisable on other platforms
    Ndi,
    Spout,   // Windows only
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub enum VideoSink {
    #[default] Window,
    Syphon,
    Ndi,
    Spout,
    VirtualCamera,
    FileRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Display {
    pub id: String,       // platform‑stable id (e.g. CGDirectDisplayID as string on mac)
    pub name: String,     // "Built‑in Retina", "LG UltraFine 27"
    pub origin: (i32, i32),
    pub size: (u32, u32),
    pub is_primary: bool,
}
```

## 5. Trait additions

### 5.1 `NodeBehavior::on_removed`

```rust
/// Called when the node is removed from the graph OR when the project
/// is closed. Trait‑based sinks/sources use this to release external
/// resources (ffmpeg subprocesses, SyphonServer, NDI sender). Default
/// is a no‑op. Must be safe to call multiple times.
fn on_removed(&mut self) {}
```

Added to `src/node_trait.rs`. Invoked from `Graph::remove_node` and from `PatchworkApp::save_session` on exit, and in the project‑reset path at `src/app/io.rs:167` / `:812`.

### 5.2 `NodeBehavior::needs_cpu_image_input` (already exists)

We *use* it on `VideoOut` to flip readback gating per sink. Example:

```rust
fn needs_cpu_image_input(&self, port: usize) -> bool {
    if port != 0 { return false; }
    match self.sink {
        VideoSink::Window | VideoSink::Syphon | VideoSink::Spout => false,
        VideoSink::Ndi | VideoSink::VirtualCamera | VideoSink::FileRecord => true,
    }
}
```

## 6. Data flow

### 6.1 GPU‑only chain (zero readback)

```
Video In (Camera)         Video Out (Syphon)
┌───────────────┐         ┌──────────────────┐
│ ffmpeg → RGBA │         │                  │
│ Arc<ImageData>│──┐   ┌──┤ frame_snapshot_  │
│  +            │  │   │  │   get_view()     │
│ upload → GPU  │  │   │  │       ↓          │
│ queue_publish │──┴───┴──┤ SyphonServer::   │
│  _node_output │         │   publish(&view) │
└───────────────┘         └──────────────────┘
    GPU texture in GpuTextureCache
    (read directly by downstream)
```

- Producer (`Video In`) publishes a `wgpu::Texture` into `GpuTextureCache` via `queue_publish_node_output` (already how `Camera` works).
- Consumer (`Video Out` with `VideoSink::Syphon`) resolves the upstream view via `frame_snapshot_get_view((src_node, src_port))` and hands the `TextureView` to Syphon.
- `needs_cpu_image_input` returns `false`, so the eval loop never triggers readback.

### 6.2 CPU‑required chain (NDI)

```
Video In (Camera)         Video Out (NDI)
┌───────────────┐         ┌──────────────────┐
│ … same as 6.1 │──┐   ┌──┤ readback_node_   │
│ plus CPU RGBA │  │   │  │   output(src,..) │
│   (already    │  │   │  │       ↓          │
│   produced)   │──┴───┴──┤ ndi::Send::send  │
│               │         │  (BGRA/UYVY)     │
└───────────────┘         └──────────────────┘
```

- Video Out (`Ndi`) returns `needs_cpu_image_input = true`. The eval loop unions this with any other downstream consumers; if nobody else needs bytes, only one readback happens. Video In (Camera) already has CPU pixels, so no additional readback: just a BGRA swizzle.
- For chains where CPU bytes *are* absent (WGSL Viewer → Video Out NDI), the WGSL Viewer's existing `readback_texture()` path fires exactly once.

### 6.3 Fan‑out (one source, three sinks)

```
            ┌─→ Video Out (Window, Display 2)    [GPU]
Video In ───┼─→ Video Out (Syphon "PatchWork")    [GPU]
            └─→ Video Out (NDI "PatchWork")       [CPU — single readback]
```

- Eval loop unions `needs_cpu_image_input` across all three consumers; result is `true` (because NDI).
- Camera already produces CPU bytes, so readback cost = 0 extra.
- Had the producer been a WGSL Viewer, one readback would happen, its bytes shared with NDI; the other two consumers stay on GPU.

## 7. Per‑bridge specs

### 7.1 Syphon (macOS, Phase 2) — **Shipped** ✓

**Status**: Shipped in Phase 2 (M1–M7). End-to-end verified with TouchDesigner + OBS loopback on Apple Silicon.

**Restart-reconnect behaviour**: when PatchWork quits and reopens, Syphon generates a new internal UUID for the server. Consumer apps (TouchDesigner's `Syphon Spout In`, OBS's Syphon Client source, Resolume, …) keep the old name displayed in their picker but stop receiving frames until the user re-picks the sender in the consumer dropdown. This is standard Syphon ecosystem behaviour — Resolume, Isadora, Modul8, Millumin all work the same way. Persistent UUIDs would require forking Syphon-Framework (no public option key) and are out of scope; a tooltip next to the Video Out publish-name field explains the convention.



| | |
|---|---|
| **Transport** | `IOSurface` shared between app processes, Metal on both sides |
| **Latency** | Effectively zero (same GPU memory) |
| **Threading** | Server publish must run on the `MTLDevice`'s queue = our main egui‑wgpu thread. Client new‑frame callback runs on a Syphon‑owned thread; we hop back to the UI thread via `egui::Context::request_repaint` + a `Mutex<Option<MTLTexture>>` latch. |
| **Framework** | Vendored (`vendor/Syphon-Framework/`, Apache‑2.0). Built by `build.rs` via `xcodebuild`; pre‑built fallback committed to `vendor/` for environments without Xcode. |
| **Bindings** | Raw `objc2` FFI. No wrapper crate (abandoned options are documented in plan). |
| **Key surface** | `SyphonMetalServer::publishFrameTexture:imageRegion:textureDimensions:flipped:`. `SyphonMetalClient::initWithServerDescription:options:newFrameHandler:`. |
| **wgpu bridge** | Export: `wgpu::Texture::as_hal::<wgpu::hal::metal::Api, _>(|tex| tex.raw_handle())` → `MTLTexture`. Import: `wgpu_hal::metal::Device::texture_from_raw(raw, ..)` wrapped as `wgpu::Texture`. Exact signatures pinned to the `wgpu` version in `Cargo.lock` at implementation time. |

### 7.2 NDI (macOS v1, Phase 3) — **Shipped** ✓

| | |
|---|---|
| **Transport** | NDI Runtime's own reliable UDP multicast / unicast on the LAN; same‑machine optimised path (mDNS) |
| **Latency** | ~17 ms (send) + ~10–15 ms (encode) + network. Full self‑loopback at 1080p: ~50 ms end‑to‑end. |
| **Threading** | `NdiSender` is `Send`; we publish from the UI thread. `NdiReceiver` runs its blocking `recv_capture_v3(100ms)` loop on a per‑node background thread that writes frames into an `Arc<Mutex<Option<Arc<ImageData>>>>` slot the UI thread drains each tick. |
| **Runtime** | `libndi.dylib` installed by the user from [ndi.video/tools/](https://ndi.video/tools/) (NewTek EULA — can't bundle). Loaded via `libloading` at first use, cached in a `OnceLock`. Search path: `$NDI_RUNTIME_DIR_V6 / V5` env vars → `/usr/local/lib/libndi.dylib` → `/Library/NDI SDK for Apple/lib/macOS/libndi.dylib` → `/Applications/*.app/Contents/Frameworks/libndi.dylib` (catches NDI Tools 5/6 which embed libndi per app bundle rather than installing globally) → dyld default. |
| **Missing‑runtime UX** | `VideoSource::Ndi.available()` / `VideoSink::Ndi.available()` return `false`; the dropdown entries show `(install NDI Runtime)` with a hover tooltip pointing at ndi.video. Selecting NDI in either node surfaces an inline `⚠ NDI Runtime not installed` banner with the install URL. `system_log::warn` fires once at first load attempt. |
| **Crate** | Hand‑rolled `libloading` FFI in `src/video_io/ndi.rs` (`NdiLibrary` function‑pointer table + safe `NdiSender` / `NdiReceiver` / `NdiFinder` wrappers). Considered `grafton-ndi` (compile‑time link, requires SDK on build machine) and `ndi-sdk` (libloading but router‑only, no send/recv). Hand‑rolled matches the Phase 4 Spout plan exactly (same `libloading` pattern, same "user installs runtime" shape). |
| **Pixel format** | Sender: `readback_texture_bgra` (fused) pulls BGRA straight out of the GPU texture — zero swizzle when upstream is already `Bgra8UnormSrgb` (Transform), one fused swap when upstream is `Rgba8UnormSrgb` (Camera/NDI In/uploads). Receiver: `capture_video` fills a BGRA `CapturedFrame`, worker thread swizzles BGRA → RGBA once into `Arc<ImageData>` via `pixel_swizzle::bgra_to_rgba`. |
| **Discovery** | `NdiFinder` instance kept on each Video In node for the lifetime of the source-picker session; `sources()` queried at 1 Hz via a TTL cache (same pattern as `syphon_servers_cache`). Refresh ↻ forces immediate re‑poll. |
| **FPS cap** | Explicit `fps_cap: f32` DragValue on Video Out (default 60, range 5–120). Last‑send timestamp gates publishes; UI shows `last send: N ms ago` heartbeat so silent-NDI failures are immediately visible. |
| **Staleness detection** | Video In tracks `ndi_last_frame_at`; indicator cycles `Connecting… → Receiving → Source offline — waiting for frames` based on elapsed time. Worker stays alive so an offline sender coming back reconnects automatically. |
| **Dual‑consumer** | NDI (like Syphon) is publish‑subscribe, so multiple Video In nodes subscribing to the same sender is fine. Video In (Camera) however *does* hit AVFoundation device‑exclusive — detected by the process‑wide `CAMERA_OWNERS` registry; second opener sees "Device in use by Video In #N". |
| **Restart reconnect** | Same caveat as Syphon (see §7.1): NDI senders come up with a fresh internal UUID on every PatchWork restart. Receivers that were subscribed keep the name visible but need a manual re‑pick. Documented in the Publish-as hover tooltip. |
| **Known limits** | NDI codec dominates at high res: full-HD self-loopback hits 60 fps easily, 4K self-loopback caps around 20 fps (encode on send + decode on receive + 22 MB/frame CPU work, both directions on one machine). Real deployments (sender and receiver on different machines) roughly double those numbers per side. |

### 7.3 Spout (Windows, Phase 4)

| | |
|---|---|
| **Transport** | Shared D3D11 texture handles |
| **Latency** | Zero (same GPU memory) |
| **Backend challenge** | wgpu defaults to DX12 on Windows; Spout expects DX11. v1 approach: env‑opt in (`PATCHWORK_BACKEND=dx11`) to force DX11. v2 approach: DX12 shared heap → DX11 interop (see plan). |
| **Library** | `SpoutDX.dll` loaded via `libloading`; hand‑declared FFI prototypes. No stable Rust wrapper crate. |
| **Threading** | Send/receive must happen on the thread that owns the D3D11 device; that's our wgpu thread under the DX11 backend, so same as Syphon on mac. |

### 7.4 Virtual Camera (macOS, Phase 5 — v1 modal-only) — **Shipped** ✓

**Status**: Shipped in Phase 5 v1. End-to-end verified with OBS Syphon Client → OBS Virtual Camera → Zoom on Apple Silicon.

The production path requires a separate signed and notarised Camera Extension (`.systemextension` bundle) with the CMIO entitlement. Out of v1 scope (~5 weeks of Apple Developer ID + entitlement paperwork). **v1 deliverable**: the `VideoSink::VirtualCamera` option shows a one-time modal:

> To appear as a webcam in Zoom / Meet / etc., PatchWork needs a system bridge. We recommend OBS:
>   1. Install OBS Studio.
>   2. Add Source → "Syphon Client" → pick "PatchWork".
>   3. Tools → "Start Virtual Camera".
> Zoom / Meet will now see your PatchWork output.
>
> [ ] Don't show this again

On Confirm: auto-switches the sink to `Syphon` with publish name `"PatchWork"`. On Cancel / Esc: reverts to `Window` so the user is never staring at a disabled modal-only sink.

**Dismissed flag** persists at `Settings::vcam_modal_dismissed: bool` (`~/.patchwork/settings.json`). Once true, picking Virtual Camera redirects to Syphon immediately — no UI flash. To re-enable the modal, delete the key from `settings.json`.

**Modal re-entrancy guard**: the per-node `vcam_modal_open` flag is cleared in the sink-change branch so a fast Window → VirtualCamera → Syphon sequence never strands the modal over an unrelated sink.

### 7.5 File Recorder (Phase 6) — **Shipped** ✓

**Status**: Shipped in Phase 6. Cross-platform (`Command::new("ffmpeg")`); macOS gets HW-encoded VideoToolbox codecs, Linux/Windows fall back to libx264 SW.

| | |
|---|---|
| **Transport** | `ffmpeg` subprocess; pixels arrive on stdin as `-f rawvideo -pix_fmt rgba`, encoded output goes to the user-picked file. |
| **Codecs (v1)** | H.264 (`libx264` SW, max compat), HEVC (`hevc_videotoolbox` HW, ~half the size), ProRes 422 (`prores_videotoolbox` HW, editor intermediate). VP9/AV1 deferred — niche on macOS, slow on the SW path. |
| **Containers** | `.mp4` for H.264/HEVC, `.mov` for ProRes. The codec dropdown locks the extension; switching codec in idle auto-rewrites the path's extension. |
| **Frame rate** | Locked to 30 fps (`crate::video_io::file_recorder::RECORDING_FPS`). `write_frame` silently throttles writes arriving within 1/30 s of the previous one — without this, render_with_context would push 60 wall-clock writes/sec at ffmpeg's 30 fps timestamps and the output media-clock would drift from wall-clock. Configurable cap is a v2 add. |
| **Path picker** | `rfd::FileDialog::save_file()` — native overwrite confirmation; no extra UI. Default name when blank: `~/Movies/PatchWork-{node_id}-{epoch}.{ext}` (epoch seconds for monotonicity without a date crate). |
| **Mid-record dim change** | Locked to first-frame `(w, h)`. Mismatched frames are dropped with a stderr-log warning; the recording stays alive (a Camera resolution wobble shouldn't lose 5 minutes of footage). |
| **Pause / Resume** | Drops frames during pause; single output file; no segmentation. User can splice in post if they need a cut. |
| **Stop semantics** | **Graceful** = drop stdin → wait 5 s for ffmpeg to flush moov → SIGKILL fallback (via `libc::kill` on Unix; reaper thread releases the file). Runs on a worker thread so the UI never blocks. **Drop** (node removal / sink change) = direct SIGKILL. |
| **Crash survival** | MP4 outputs use `-movflags +faststart+frag_keyframe+empty_moov` so a SIGKILL-truncated file is still playable to the last keyframe. ProRes (.mov) uses fragmented atoms by default. |
| **Pre-flight checks** | HW codecs (HEVC/ProRes) reject odd dimensions; we surface a clear error before spawn instead of waiting for ffmpeg's stderr. ffmpeg-not-on-PATH errors reuse `video_player::ffmpeg_install_hint()` (`"brew install ffmpeg"` on macOS). |
| **Status display** | Recording chip shows `● recording (NN frames, M.M MB, MM:SS)` where the size is **on-disk encoded bytes** (polled via `fs::metadata`), NOT raw RGBA pushed to ffmpeg — those numbers diverge by ~500× and confused early users. |
| **Linux v4l2loopback** | **Documented-only**, not auto-installed. After installing the `v4l2loopback` kernel module, `ffmpeg -f v4l2 /dev/video10` from a separate terminal can pipe the recorded file (or live frames via a future virtual-camera sink) to a `/dev/video*` device that Zoom / Meet pick up. |

**Persistence**: `recorder_codec: u8` and `recorder_path: String` on `VideoOutNode` serialise across save/load. `enabled` and `file_recorder` are `#[serde(skip)]` so a saved project never auto-resumes mid-recording — user must explicitly hit Start.

**Stderr drainer**: sibling thread mirroring `video_player.rs:278–308`. 1 KB cap on `stderr_log` prevents unbounded growth from a chatty ffmpeg. Latest line surfaces as an amber warning under the status row while recording.

## 8. Lifecycle

| Event | Video In | Video Out |
|---|---|---|
| Node created | No decoder until `Start` pressed | No sink until `Enabled` toggled |
| Source/sink changed via dropdown | Kill current decoder, start new one at next Start | Tear down current sink, create new one |
| `enable = true` | Spawn decoder; if fails, `status = error`, `enable = false` | Create Syphon server / NDI sender / etc.; same failure pattern |
| Upstream unwired | (N/A) | Sink stays alive; publishes nothing (skip the publish call) |
| `on_removed` | Drop decoder; `VideoDecoder::Drop` kills the ffmpeg child | Drop sink; `Drop` impls release resources |
| Project closed | `on_removed` is called via the same loop that zeroes `NodeType::Camera::active` in `src/app/io.rs:167` — extended to iterate Dynamic nodes and call `on_removed` |

## 9. Error handling & status

All sources/sinks surface a `status: String` field exposed in the node UI. Categories:

- **Empty** — not started, no error.
- **`"Capturing"` / `"Sending"`** — healthy.
- **`"Error: …"`** — permanent. Shown in red. Logged to `system_log::warn`.
- **`"Disconnected"`** — transient. Shown in amber. Auto‑clears on next successful frame.

Sink failures that would otherwise silently drop frames (Syphon server failed to create, NDI runtime missing, etc.) are surfaced into the node status row *and* via a red dot next to the destination dropdown.

## 10. Thread model

```
             ┌────────────────────────── UI thread (egui + wgpu) ───────────────────────────┐
             │  graph.evaluate → render_with_context → sink.publish(view) / source.poll()   │
             └──────▲─────────────────────────────────────────────────────▲─────────────────┘
                    │ try_recv_frame()                    request_repaint │
                    │                                                     │
┌───────────────────┴──────────┐   ┌──────────────────────────────────────┴──────────────┐
│ ffmpeg reader thread(s)      │   │ Syphon client new‑frame callback thread (mac)        │
│ (Camera / Screen / File)     │   │ NDI Find background discovery thread (2s interval)   │
│ one per active Video In      │   │ libndi worker pool (internal to SDK)                 │
│ Arc<ImageData> → mpsc::sync  │   │                                                      │
│  _channel(1) (backpressure)  │   │                                                      │
└──────────────────────────────┘   └──────────────────────────────────────────────────────┘
```

- `VideoDecoder` uses `mpsc::sync_channel(1)` — bounded; natural frame pacing, no sleeps.
- Syphon callbacks stash the `MTLTexture` in a `Mutex<Option<…>>` latch and call `ctx.request_repaint()`. The UI thread picks it up on next frame.
- NDI discovery pushes new source lists via `crossbeam_channel` — already a dep.

## 11. Testing strategy

- **Unit**: type round‑trips (serde), `VideoSource`/`VideoSink` dropdown transitions, legacy JSON migration (old `NodeType::Camera` → new `VideoInNode`).
- **Manual on‑box (mac)**:
  - P1: pop‑out Window on Display 2 without drag.
  - P2: TouchDesigner `syphonspoutin` TOP shows PatchWork Visual Out; `syphonspoutout` TOP appears in Video In dropdown.
  - P3: NDI Studio Monitor sees PatchWork; OBS NDI Out appears in Video In dropdown.
- **Manual on‑box (win)**: P4 analogue — OBS Spout In + Resolume Arena free trial.
- **Regression**: open a pre‑refactor saved project containing a `NodeType::Camera`; assert the loaded graph has one `Video In` node with `source = Camera` and matching device_id.

## 12. Open questions

1. **Visual Output pop‑out deletion**: when do we cut the existing "Pop Out" button and fullscreen toggle from `VisualOutputNode`? Proposal: keep both ways working through Phase 1, remove the old pop‑out at the start of Phase 2. Avoids breaking muscle memory during the transition.
2. **NDI receiver re‑upload format**: SDK delivers `BGRA` or `UYVY` frames. For GPU upload, do we swizzle CPU→CPU first or use a tiny compute shader to swizzle on upload? First cut: CPU swizzle (simpler, cost is negligible for typical 1920×1080 BGRA).
3. **Syphon import on wgpu**: `wgpu_hal::metal::Device::texture_from_raw` exposed as stable API? If not, fallback is `wgpu::Device::create_texture_from_hal` with a hand‑built `hal::metal::Texture`. Day 1 of Phase 2 work.
4. **Multiple Video Out of the same sink type**: two Syphon servers with the same publish name = undefined behaviour per Syphon docs. UI should validate uniqueness within the graph on toggle‑enable and refuse duplicates with an error status.
5. ~~**Recording (file output sink)**: out of scope for this doc?~~ — **Resolved** in §7.5. Phase 6 shipped with H.264 / HEVC / ProRes codecs, 30 fps cap, native save dialog, graceful + hard stop paths.
