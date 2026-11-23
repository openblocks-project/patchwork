//! P6 Stage 3.4: the Player's "camera unavailable" screen. Only an output
//! fed (through any chain of nodes) by a required camera that is not
//! delivering frames waits; the output comes back when frames do.

use super::*;
use crate::graph::Connection;
use crate::nodes::video_in_node::CameraHealth;

const CAMERA: NodeId = 1;
const HANDS: NodeId = 2;
const OVERLAY: NodeId = 3;
const OUTPUT: NodeId = 4;
const PATTERN: NodeId = 5;
const OTHER_OUTPUT: NodeId = 6;

fn wire(from_node: NodeId, to_node: NodeId, to_port: usize) -> Connection {
    Connection {
        from_node,
        from_port: 0,
        to_node,
        to_port,
        label: String::new(),
    }
}

/// Camera → hand tracking → overlay → output, with a generated pattern
/// also drawn by the overlay; and the pattern alone on a second output.
fn exhibit() -> Vec<Connection> {
    vec![
        wire(CAMERA, HANDS, 0),
        wire(CAMERA, OVERLAY, 0),
        wire(HANDS, OVERLAY, 1),
        wire(PATTERN, OVERLAY, 2),
        wire(OVERLAY, OUTPUT, 0),
        wire(PATTERN, OTHER_OUTPUT, 0),
    ]
}

fn camera_is(state: Option<CameraHealth>) -> impl Fn(NodeId) -> Option<CameraHealth> {
    move |id| if id == CAMERA { state } else { None }
}

#[test]
fn an_output_fed_by_an_unavailable_camera_shows_the_camera_unavailable_screen() {
    let wires = exhibit();
    let lost = camera_is(Some(CameraHealth::Unavailable));
    assert!(output_waits_for_camera(&wires, OUTPUT, &lost));
    // Its last image is not shown as if it were live.
    assert_eq!(output_view(true, true, true), OutputView::CameraUnavailable);
    assert_eq!(
        output_view(true, true, false),
        OutputView::CameraUnavailable
    );
    assert_eq!(CAMERA_UNAVAILABLE, "Camera unavailable — reconnecting…");
}

#[test]
fn unrelated_graphics_keep_being_shown() {
    let wires = exhibit();
    let lost = camera_is(Some(CameraHealth::Unavailable));
    // An output that does not depend on the camera is unaffected.
    assert!(!output_waits_for_camera(&wires, OTHER_OUTPUT, &lost));
    // A camera the Player does not require (not "Start when played"; no
    // health published) never blanks an output.
    assert!(!output_waits_for_camera(&wires, OUTPUT, camera_is(None)));
}

#[test]
fn the_output_comes_back_when_camera_frames_resume() {
    let wires = exhibit();
    assert!(output_waits_for_camera(
        &wires,
        OUTPUT,
        camera_is(Some(CameraHealth::Unavailable))
    ));
    let live = camera_is(Some(CameraHealth::Live));
    assert!(!output_waits_for_camera(&wires, OUTPUT, &live));
    assert_eq!(output_view(true, false, true), OutputView::Image);
    // A camera starting for the first time does not show the screen.
    assert!(!output_waits_for_camera(
        &wires,
        OUTPUT,
        camera_is(Some(CameraHealth::Starting))
    ));
}

#[test]
fn the_screen_never_overrides_the_device_permission() {
    assert_eq!(output_view(false, true, true), OutputView::NotPermitted);
    assert_eq!(output_view(false, false, true), OutputView::NotPermitted);
    assert_eq!(output_view(true, false, false), OutputView::Nothing);
}

#[test]
fn feedback_loops_are_followed_once() {
    let mut wires = exhibit();
    wires.push(wire(OVERLAY, HANDS, 2));
    wires.push(wire(OUTPUT, OVERLAY, 3));
    assert!(output_waits_for_camera(
        &wires,
        OUTPUT,
        camera_is(Some(CameraHealth::Unavailable))
    ));
    assert!(!output_waits_for_camera(
        &wires,
        OTHER_OUTPUT,
        camera_is(Some(CameraHealth::Unavailable))
    ));
}

#[test]
fn the_published_health_is_read_per_node() {
    let ctx = egui::Context::default();
    use crate::nodes::video_in_node::VideoInNode;
    assert_eq!(published_camera_health(&ctx, CAMERA), None);
    ctx.data_mut(|d| {
        d.insert_temp(
            VideoInNode::camera_health_id(CAMERA),
            Some(CameraHealth::Unavailable),
        )
    });
    assert_eq!(
        published_camera_health(&ctx, CAMERA),
        Some(CameraHealth::Unavailable)
    );
    assert_eq!(published_camera_health(&ctx, HANDS), None);
}
