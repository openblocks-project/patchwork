//! P6 Stage 2.9: camera-only capture, console-free ffmpeg, and GPU objects
//! released at exit.

use crate::nodes::video_in_node::{MicChange, VideoInNode};

/// (camera running, Audio output wired, microphone capturing) -> change.
fn mic(running: bool, wired: bool, capturing: bool) -> Option<MicChange> {
    VideoInNode::camera_mic_change(running, wired, capturing)
}

#[test]
fn a_video_only_camera_never_opens_the_microphone() {
    // Frame output used, Audio output not wired: before, while and after
    // the camera runs, the microphone is never started.
    for running in [false, true] {
        assert_eq!(mic(running, false, false), None);
    }
}

#[test]
fn a_camera_whose_audio_is_used_captures_the_microphone() {
    // Audio-only use (only the Audio output wired; Video In always runs the
    // camera itself) and combined use (Frame and Audio wired) are the same
    // here: the Audio output is wired.
    assert_eq!(mic(true, true, false), Some(MicChange::Start));
    assert_eq!(mic(true, true, true), None, "started once, not every frame");
    // Not before the camera runs.
    assert_eq!(mic(false, true, false), None);
}

#[test]
fn microphone_capture_follows_the_wiring_and_the_camera() {
    // Wired while running: starts; unwired while running: stops.
    assert_eq!(mic(true, true, false), Some(MicChange::Start));
    assert_eq!(mic(true, false, true), Some(MicChange::Stop));
    // The camera stops (or drops out) while audio is wired: stops too, and
    // starts again with the camera.
    assert_eq!(mic(false, true, true), Some(MicChange::Stop));
    assert_eq!(mic(true, true, false), Some(MicChange::Start));
}

fn source_files() -> Vec<(String, String)> {
    fn visit(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push((
                    path.to_string_lossy().replace('\\', "/"),
                    std::fs::read_to_string(&path).unwrap(),
                ));
            }
        }
    }
    let mut out = Vec::new();
    visit(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut out,
    );
    out
}

/// Every ffmpeg Patchwork runs is built by `ffmpeg_command` (no console
/// window on Windows), recorder and web streams included.
#[test]
fn every_ffmpeg_is_started_without_a_console_window() {
    let mut direct = Vec::new();
    for (path, text) in source_files() {
        for (i, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if code.contains("Command::new(") && code.contains("ffmpeg_bin()") {
                direct.push(format!("{path}:{}", i + 1));
            }
        }
    }
    // The builder itself is the one place allowed to.
    direct.retain(|place| {
        !place.contains("nodes/video_player.rs") && !place.contains("stage29_tests.rs")
    });
    assert!(direct.is_empty(), "ffmpeg started directly: {direct:?}");
}

/// The app releases GPU objects held in thread-locals when it exits, so
/// they are not destroyed during process exit (Intel Vulkan driver crash).
#[test]
fn gpu_objects_in_thread_locals_are_released_on_exit() {
    let app = include_str!("../mod.rs");
    let on_exit = app
        .split("fn on_exit(")
        .nth(1)
        .expect("on_exit")
        .split("\n    }")
        .next()
        .unwrap();
    assert!(on_exit.contains("release_thread_gpu_objects()"));
    let gpu = include_str!("../../gpu_image.rs");
    let release = gpu
        .split("pub fn release_thread_gpu_objects()")
        .nth(1)
        .unwrap()
        .split("\n}")
        .next()
        .unwrap();
    // Every thread-local holding wgpu objects is cleared.
    for name in [
        "FRAME_TEX_SNAPSHOT.",
        "FRAME_TEX_SNAPSHOT_TEXTURE.",
        "PENDING_CACHE_PUBLISHES.",
    ] {
        assert!(release.contains(name), "{name} is not released");
    }
    // Running it on a thread with nothing cached is harmless.
    crate::gpu_image::release_thread_gpu_objects();
}
