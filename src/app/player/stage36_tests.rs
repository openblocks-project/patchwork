//! P6 Stage 3.6: the kiosk option of exported Windows apps. The window
//! itself (fullscreen on a real display, Alt+F4) needs manual testing.

use super::manifest::{ExportInfo, parse_manifest};
use super::*;

const DIGEST: &str = "5639a54fa89280f0559ff8d3bc713d4900c0f3f308010a343179d2adb26211be";

fn manifest_text(extra: &str) -> String {
    format!(
        r#"{{"schema":"patchwork.player","format":1,"project":"project","output":5,
            "title":"MediaPipe Show","patchwork_version":"0.0.10",
            "project_sha256":"{DIGEST}"{extra}}}"#
    )
}

#[test]
fn the_kiosk_setting_is_read_from_the_manifest() {
    assert!(
        parse_manifest(&manifest_text(r#","kiosk":true"#))
            .unwrap()
            .kiosk
    );
    assert!(
        !parse_manifest(&manifest_text(r#","kiosk":false"#))
            .unwrap()
            .kiosk
    );
    // Only a boolean.
    assert!(parse_manifest(&manifest_text(r#","kiosk":"yes""#)).is_err());
    assert!(parse_manifest(&manifest_text(r#","kiosk":1"#)).is_err());
    // Other unknown fields are still refused.
    assert!(parse_manifest(&manifest_text(r#","kiosk_password":"x""#)).is_err());
}

#[test]
fn older_manifests_without_the_setting_are_normal_players() {
    // Exactly the format-1 manifest earlier exports wrote.
    let manifest = parse_manifest(&manifest_text("")).unwrap();
    assert!(!manifest.kiosk);
    assert_eq!(manifest.format, 1);
    assert_eq!(manifest.output, 5);
}

#[test]
fn a_normal_player_leaves_fullscreen_on_escape_and_ignores_f11() {
    // Escape in fullscreen: leave it (P6.1, unchanged).
    assert_eq!(
        fullscreen_key_change(true, false, Some(true), false),
        Some(false)
    );
    // Not fullscreen, or no key: nothing.
    assert_eq!(fullscreen_key_change(true, false, Some(false), false), None);
    assert_eq!(fullscreen_key_change(false, false, Some(true), false), None);
    // F11 never did anything in the Player, and still does not.
    assert_eq!(fullscreen_key_change(false, true, Some(true), false), None);
    assert_eq!(fullscreen_key_change(false, true, Some(false), false), None);
}

#[test]
fn a_kiosk_does_not_leave_fullscreen_on_escape() {
    assert_eq!(fullscreen_key_change(true, false, Some(true), true), None);
    assert_eq!(fullscreen_key_change(true, false, None, true), None);
}

#[test]
fn a_kiosk_does_not_toggle_fullscreen_on_f11() {
    assert_eq!(fullscreen_key_change(false, true, Some(true), true), None);
    assert_eq!(fullscreen_key_change(false, true, Some(false), true), None);
    assert_eq!(fullscreen_key_change(true, true, Some(true), true), None);
}

#[test]
fn a_kiosk_starts_fullscreen_and_a_normal_player_as_the_video_out_says() {
    assert!(starts_fullscreen(false, true));
    assert!(starts_fullscreen(true, true));
    assert!(starts_fullscreen(true, false));
    assert!(!starts_fullscreen(false, false));
}

fn launch(kiosk: Option<bool>) -> PlayerLaunch {
    PlayerLaunch {
        project_dir: PathBuf::from("project"),
        project_json: PathBuf::from("project").join(PROJECT_FILE),
        output: Some(5),
        export: kiosk.map(|kiosk| ExportInfo {
            export_dir: PathBuf::from("."),
            title: "MediaPipe Show".to_string(),
            patchwork_version: "0.0.10".to_string(),
            expected_digest: DIGEST.to_string(),
            kiosk,
        }),
    }
}

#[test]
fn only_an_exported_app_set_to_kiosk_runs_as_one() {
    assert!(PlayerState::new(launch(Some(true)), Ok(5)).kiosk());
    assert!(!PlayerState::new(launch(Some(false)), Ok(5)).kiosk());
    // A Player started on a project from the command line never is.
    assert!(!PlayerState::new(launch(None), Ok(5)).kiosk());
}

fn display(id: &str, origin: (i32, i32), primary: bool) -> crate::display::Display {
    crate::display::Display {
        id: id.to_string(),
        name: id.to_string(),
        origin,
        size: (1920, 1080),
        is_primary: primary,
    }
}

#[test]
fn a_kiosk_uses_the_video_out_display_or_falls_back_to_the_primary() {
    let displays = [
        display("DISPLAY1", (0, 0), true),
        display("DISPLAY2", (1920, 0), false),
    ];
    // The display the Video Out names.
    assert_eq!(
        choose_display(&displays, "DISPLAY2").unwrap().id,
        "DISPLAY2"
    );
    // Disconnected: the primary display.
    assert_eq!(
        choose_display(&displays, "DISPLAY3").unwrap().id,
        "DISPLAY1"
    );
    // None named: the primary display.
    assert_eq!(choose_display(&displays, "").unwrap().id, "DISPLAY1");
    // No primary reported: the first.
    let unflagged = [display("A", (0, 0), false), display("B", (1920, 0), false)];
    assert_eq!(choose_display(&unflagged, "C").unwrap().id, "A");
    // Nothing enumerated: the system places the window (primary display).
    assert!(choose_display(&[], "DISPLAY2").is_none());
}

#[test]
fn the_export_dialog_offers_the_kiosk_option_off_by_default() {
    use crate::app::export_dialog::{KIOSK_CHECKBOX, KIOSK_EXPLANATION};
    assert_eq!(KIOSK_CHECKBOX, "Run as fullscreen kiosk");
    for said in ["starts fullscreen", "Escape", "Alt+F4"] {
        assert!(KIOSK_EXPLANATION.contains(said), "{said}");
    }
}
