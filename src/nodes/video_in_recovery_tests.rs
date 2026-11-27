//! P6 Stage 3.4: camera recovery in the Player, with test cameras (opened
//! through the real `VideoDecoder::open_camera` and its permission check,
//! fed by the test instead of ffmpeg). Time is simulated.

use super::*;
use crate::capability::{CapSet, Capability, Context};
use crate::nodes::video_player::test_camera::{self, Open};
use std::time::{Duration, Instant};

const NODE: NodeId = 7;
const STEP: Duration = Duration::from_millis(100);

/// Test cameras on this thread, and the rights a Player run has.
struct Rig {
    rights: Context,
    _entered: crate::capability::Entered,
}

impl Rig {
    fn new() -> Rig {
        test_camera::install();
        let rights = Context::new(
            CapSet::of(&[Capability::Process, Capability::MediaCapture]),
            "this project",
        );
        let entered = rights.enter();
        Rig {
            rights,
            _entered: entered,
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        test_camera::uninstall();
    }
}

fn camera() -> VideoInNode {
    let mut node = VideoInNode::default();
    node.start_on_play = true;
    // A device no other test claims (device ownership is process-wide).
    node.device_id = "907".into();
    node
}

/// One Player frame at `now`, as `render_camera_ui` does it.
fn frame(node: &mut VideoInNode, player: bool, now: Instant) {
    node.player_camera_tick(NODE, player, now);
    node.poll_decoder_at(NODE, None, now);
    node.player_camera_watchdog(NODE, player, now);
    assert!(
        test_camera::with(|c| c.open_now()) <= 1,
        "never two captures at once"
    );
}

fn opens() -> usize {
    test_camera::with(|c| c.opens)
}

fn open_now() -> usize {
    test_camera::with(|c| c.open_now())
}

/// The next opens succeed.
fn will_open(count: usize) {
    test_camera::with(|c| c.plan.extend((0..count).map(|_| Open::Ok)));
}

/// The latest capture sends a frame.
fn send_frame() {
    test_camera::with(|c| c.feeds.last().expect("a capture").frame());
}

/// Every capture ends (as ffmpeg exits), saying why.
fn end_captures(why: &str) {
    test_camera::with(|c| {
        for feed in &c.feeds {
            feed.stderr(why);
        }
        c.feeds.clear();
    });
}

/// Run frames from `*t` until `until`, ending each capture as soon as it
/// opens (the camera is held by another program: ffmpeg starts, then
/// fails). Returns the times (since `t0`) of the opens.
fn run_unavailable(
    node: &mut VideoInNode,
    t0: Instant,
    t: &mut Instant,
    until: Instant,
) -> Vec<f32> {
    let mut times = Vec::new();
    while *t < until {
        let before = opens();
        will_open(1);
        frame(node, true, *t);
        test_camera::with(|c| c.plan.clear());
        if opens() > before {
            times.push(t.duration_since(t0).as_secs_f32());
            end_captures("Could not run graph");
        }
        *t += STEP;
    }
    times
}

fn round(times: &[f32]) -> Vec<u32> {
    times.iter().map(|s| s.round() as u32).collect()
}

#[test]
fn a_camera_unavailable_at_start_for_over_45_s_recovers_without_a_restart() {
    let _rig = Rig::new();
    let mut node = camera();
    let t0 = Instant::now();
    let mut t = t0;
    assert_eq!(node.camera_health_at(t), CameraHealth::Starting);

    // Unavailable from the start, for 65 s.
    let attempts = run_unavailable(&mut node, t0, &mut t, t0 + Duration::from_secs(65));
    // The first schedule is unchanged (0, 2, 6, 12, 20 s), then every 10 s.
    assert_eq!(round(&attempts), [0, 2, 6, 12, 20, 30, 40, 50, 60]);
    assert_eq!(node.camera_health_at(t), CameraHealth::Unavailable);
    assert!(!node.active);
    assert_eq!(open_now(), 0);
    assert!(node.status.starts_with("Disconnected"), "{}", node.status);

    // The camera is free again: the next attempt (70 s) takes it.
    will_open(1);
    let opens_before = opens();
    while opens() == opens_before {
        frame(&mut node, true, t);
        t += STEP;
    }
    assert_eq!(t.duration_since(t0).as_secs_f32().round() as u32, 70);
    assert!(node.active);
    assert_eq!(open_now(), 1);
    // Opened but no frame yet: still not live (the output keeps waiting).
    assert_eq!(node.camera_health_at(t), CameraHealth::Unavailable);
    send_frame();
    frame(&mut node, true, t);
    assert_eq!(node.camera_health_at(t), CameraHealth::Live);
    assert!(
        node.evaluate(&[])
            .first()
            .is_some_and(|(port, _)| *port == 0)
    );
    assert_eq!(node.status, "Capturing");

    // Running: no further opens while frames keep coming.
    let opens_running = opens();
    for _ in 0..300 {
        send_frame();
        frame(&mut node, true, t);
        t += STEP;
    }
    assert_eq!(opens(), opens_running);
    assert_eq!(open_now(), 1);
}

#[test]
fn a_capture_that_ends_is_restarted_at_once_and_recovers() {
    let _rig = Rig::new();
    let mut node = camera();
    let t0 = Instant::now();
    will_open(2);
    frame(&mut node, true, t0);
    send_frame();
    frame(&mut node, true, t0);
    assert_eq!(node.camera_health_at(t0), CameraHealth::Live);

    // ffmpeg is killed mid-session.
    let t1 = t0 + Duration::from_secs(30);
    end_captures("");
    frame(&mut node, true, t1);
    assert!(!node.active);
    assert_eq!(node.camera_health_at(t1), CameraHealth::Unavailable);
    assert!(node.evaluate(&[]).is_empty(), "no stale frame is passed on");
    // The next frame starts a new capture (no waiting: it had frames).
    frame(&mut node, true, t1 + STEP);
    assert_eq!(opens(), 2);
    assert_eq!(open_now(), 1);
    send_frame();
    frame(&mut node, true, t1 + STEP * 2);
    assert_eq!(node.camera_health_at(t1 + STEP * 2), CameraHealth::Live);
}

#[test]
fn a_capture_that_stops_sending_frames_is_restarted_without_a_second_one() {
    let _rig = Rig::new();
    let mut node = camera();
    let t0 = Instant::now();
    will_open(2);
    frame(&mut node, true, t0);
    send_frame();
    frame(&mut node, true, t0);

    // The capture stays open but sends nothing (hangs).
    let mut t = t0;
    while opens() == 1 {
        t += STEP;
        frame(&mut node, true, t);
        if t.duration_since(t0) > Duration::from_secs(2) + STEP {
            assert_eq!(node.camera_health_at(t), CameraHealth::Unavailable);
        }
        assert!(t.duration_since(t0) < Duration::from_secs(12), "restarted");
    }
    let after = t.duration_since(t0).as_secs_f32();
    assert!((10.0..=10.5).contains(&after), "restarted after {after} s");
    // The hung capture was closed before the new one opened.
    assert_eq!(open_now(), 1);
    send_frame();
    frame(&mut node, true, t + STEP);
    assert_eq!(node.camera_health_at(t + STEP), CameraHealth::Live);
}

#[test]
fn a_first_capture_that_never_sends_a_frame_is_restarted() {
    let _rig = Rig::new();
    let mut node = camera();
    // Its own device: the claim it checks below is process-wide, and the
    // other tests (running in parallel) claim device 907.
    node.device_id = "908".into();
    let t0 = Instant::now();
    will_open(1);
    frame(&mut node, true, t0);
    assert!(node.active);
    // Starting (not yet a failure) within the grace period...
    assert_eq!(
        node.camera_health_at(t0 + Duration::from_secs(5)),
        CameraHealth::Starting
    );
    // ...unavailable after it.
    assert_eq!(
        node.camera_health_at(t0 + Duration::from_secs(9)),
        CameraHealth::Unavailable
    );
    frame(&mut node, true, t0 + Duration::from_secs(11));
    assert!(!node.active, "closed by the watchdog");
    // Everything it held is released before the retry: the capture and
    // the device claim.
    assert_eq!(open_now(), 0);
    let claimed = CAMERA_OWNERS
        .lock()
        .unwrap()
        .get(&(VideoSource::Camera, node.device_index()))
        .copied();
    assert_eq!(claimed, None);
    // The retry then takes the device again.
    will_open(1);
    frame(&mut node, true, t0 + Duration::from_secs(11) + STEP);
    assert!(node.active);
    assert_eq!(open_now(), 1);
}

#[test]
fn a_valid_frame_resets_the_retry_state() {
    let _rig = Rig::new();
    let mut node = camera();
    let t0 = Instant::now();
    let mut t = t0;
    // Into the slow phase.
    run_unavailable(&mut node, t0, &mut t, t0 + Duration::from_secs(35));
    assert!(node.in_slow_recovery());

    // Recovers, sends a frame: the retry state is reset.
    will_open(1);
    t = t0 + Duration::from_secs(40);
    frame(&mut node, true, t);
    send_frame();
    frame(&mut node, true, t);
    assert_eq!(node.camera_health_at(t), CameraHealth::Live);
    assert_eq!(node.player_start_attempts, 0);
    assert_eq!(node.player_retry_at, None);

    // A later loss starts the first schedule again (0, 2, 6 s), not 10 s.
    end_captures("");
    let lost = t + Duration::from_secs(60);
    let mut t2 = lost;
    let attempts = run_unavailable(&mut node, lost, &mut t2, lost + Duration::from_secs(7));
    assert_eq!(round(&attempts), [0, 2, 6]);
}

#[test]
fn recovery_stops_when_the_camera_is_no_longer_required() {
    let _rig = Rig::new();
    let t0 = Instant::now();

    // "Start when played" turned off during recovery.
    let mut node = camera();
    let mut t = t0;
    run_unavailable(&mut node, t0, &mut t, t0 + Duration::from_secs(25));
    node.start_on_play = false;
    let before = opens();
    run_unavailable(&mut node, t0, &mut t, t0 + Duration::from_secs(120));
    assert_eq!(opens(), before);

    // Another source chosen.
    let mut node = camera();
    let until = t + Duration::from_secs(25);
    run_unavailable(&mut node, t0, &mut t, until);
    node.source = VideoSource::File;
    let before = opens();
    let until = t + Duration::from_secs(60);
    run_unavailable(&mut node, t0, &mut t, until);
    assert_eq!(opens(), before);

    // The project closes (the node is dropped): its capture ends with it.
    let mut node = camera();
    will_open(1);
    frame(&mut node, true, t);
    assert_eq!(open_now(), 1);
    drop(node);
    assert_eq!(open_now(), 0);
}

#[test]
fn a_permission_revoked_during_recovery_is_enforced_on_every_restart() {
    let rig = Rig::new();
    let mut node = camera();
    let t0 = Instant::now();
    let mut t = t0;
    run_unavailable(&mut node, t0, &mut t, t0 + Duration::from_secs(25));
    let reached = opens();
    assert!(reached >= 5);

    // The rights are withdrawn while the camera is unavailable.
    rig.rights.revoke();
    let denied_before = rig.rights.denied_count(Capability::Process);
    run_unavailable(&mut node, t0, &mut t, t0 + Duration::from_secs(90));
    // Restarts were still attempted on schedule, and each one was refused
    // before reaching the camera.
    assert_eq!(opens(), reached, "no capture opened without permission");
    assert!(rig.rights.denied_count(Capability::Process) >= denied_before + 6);
    assert!(!node.active);
    assert_eq!(open_now(), 0);
    assert_eq!(node.camera_health_at(t), CameraHealth::Unavailable);
}

#[test]
fn the_editor_keeps_its_camera_behaviour() {
    let _rig = Rig::new();
    let mut node = camera();
    let t0 = Instant::now();

    // The editor never starts a camera by itself, set to start or not.
    for i in 0..600 {
        frame(&mut node, false, t0 + STEP * i);
    }
    assert_eq!(opens(), 0);

    // Start (the button) opens it; a capture that stops sending frames is
    // left alone (no watchdog), and one that ends is not restarted.
    will_open(1);
    node.open_decoder(NODE, 0.0);
    assert!(node.active);
    send_frame();
    frame(&mut node, false, t0);
    for i in 1..=300 {
        frame(&mut node, false, t0 + STEP * i);
    }
    assert!(node.active, "no watchdog in the editor");
    end_captures("I/O error");
    for i in 301..=900 {
        frame(&mut node, false, t0 + STEP * i);
    }
    assert!(!node.active);
    assert_eq!(opens(), 1, "not restarted in the editor");
    assert_eq!(node.status, "Disconnected: I/O error");
    // And its health is never published as required.
    assert!(!node.player_camera_required(false));
}

#[test]
fn the_device_list_is_refreshed_only_when_it_may_be_needed() {
    // Editor, camera stopped / starting / lost: refreshed (choosing a
    // device, or a camera plugged in).
    assert!(VideoInNode::refreshes_device_list(false, false));
    // A live camera: no background listing.
    assert!(!VideoInNode::refreshes_device_list(false, true));
    // The Player: its camera starts list the devices themselves.
    assert!(!VideoInNode::refreshes_device_list(true, false));
    assert!(!VideoInNode::refreshes_device_list(true, true));
}

#[test]
fn closing_the_project_ends_its_camera_capture() {
    // P6 Stage 3.6: when the Player exits, its graph is dropped with it; a
    // running camera's capture (its ffmpeg) ends then. (The OS also ends
    // every child ffmpeg with the Player process: `video_io::child_job`.)
    let _rig = Rig::new();
    let mut node = camera();
    let t0 = Instant::now();
    will_open(1);
    frame(&mut node, true, t0);
    send_frame();
    frame(&mut node, true, t0);
    assert_eq!(node.camera_health_at(t0), CameraHealth::Live);
    let mut graph = crate::graph::Graph::new();
    graph.add_node(
        crate::graph::NodeType::Dynamic {
            inner: crate::graph::DynNode {
                node: Box::new(node),
            },
        },
        [0.0, 0.0],
    );
    assert_eq!(open_now(), 1);
    drop(graph);
    assert_eq!(open_now(), 0, "the capture ended with the project");
}
