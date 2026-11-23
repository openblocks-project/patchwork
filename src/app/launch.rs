//! Program start, shared by the two executables built from this crate.
//!
//! - `patchwork` ([`Binary::Editor`], `src/main.rs`): the editor, a console
//!   program so `patchwork catalogue …` and `--player` errors print to the
//!   terminal. A `player.json` beside it still makes it that export's
//!   Player (P6 Stage 2.1).
//! - `patchwork-player` ([`Binary::Player`], `src/player_main.rs`, P6 Stage
//!   2.3): the runtime of exported apps. It is built for the Windows GUI
//!   subsystem (no console window) and only plays the project its
//!   `player.json` names: it has no editor, no catalogue tool and ignores
//!   its arguments. Without a usable `player.json` it shows why.

use std::path::Path;
use std::sync::Arc;

use eframe::egui;

use super::player::{LaunchMode, PlayerStart};

/// Which executable is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binary {
    Editor,
    Player,
}

impl Binary {
    /// The name recorded in the executable's build information.
    pub const fn flavor(self) -> &'static str {
        match self {
            Binary::Editor => "editor",
            Binary::Player => "player",
        }
    }
}

/// What the program starts as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// `patchwork catalogue …`: run the tool and exit with its code.
    Catalogue(Vec<String>),
    Player(PlayerStart),
    Editor,
    /// Print and exit with code 2 (editor binary only).
    Fail(String),
}

/// Decide how `binary` starts, from its arguments and its folder.
pub fn decide(binary: Binary, args: &[String], exe_dir: Option<&Path>) -> Start {
    match binary {
        Binary::Editor => {
            // Without knowing where it runs from it cannot tell whether it
            // is an exported app: refuse rather than guess.
            let Some(dir) = exe_dir else {
                return Start::Fail("cannot locate the Patchwork executable".to_string());
            };
            match super::player::launch_mode_at(args, dir) {
                Ok(LaunchMode::Catalogue(rest)) => Start::Catalogue(rest),
                Ok(LaunchMode::Player(launch)) | Ok(LaunchMode::Exported(launch)) => {
                    Start::Player(PlayerStart::Launch(launch))
                }
                Ok(LaunchMode::ExportInvalid { export_dir, error }) => {
                    Start::Player(PlayerStart::Invalid { export_dir, error })
                }
                Ok(LaunchMode::Editor) => Start::Editor,
                Err(error) => Start::Fail(error),
            }
        }
        Binary::Player => Start::Player(player_binary_start(args, exe_dir)),
    }
}

/// The Player runtime only ever plays its bundled project.
fn player_binary_start(args: &[String], exe_dir: Option<&Path>) -> PlayerStart {
    let not_an_app = |dir: &Path| PlayerStart::Invalid {
        export_dir: dir.to_path_buf(),
        error: "This is the Patchwork Player runtime of exported apps. It plays the project \
                named by the player.json beside it, and there is none. (It has no editor; \
                use Patchwork to open projects.)"
            .to_string(),
    };
    let Some(dir) = exe_dir else {
        return not_an_app(Path::new(""));
    };
    if !super::player::manifest::manifest_present(dir) {
        return not_an_app(dir);
    }
    match super::player::launch_mode_at(args, dir) {
        Ok(LaunchMode::Exported(launch)) => PlayerStart::Launch(launch),
        Ok(LaunchMode::ExportInvalid { export_dir, error }) => {
            PlayerStart::Invalid { export_dir, error }
        }
        // A present manifest never yields another mode; refuse if it did.
        Ok(_) | Err(_) => not_an_app(dir),
    }
}

/// Run `binary` (the body of both `main` functions).
pub fn run(binary: Binary) -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let player_launch = match decide(binary, &args, exe_dir.as_deref()) {
        Start::Catalogue(rest) => {
            std::process::exit(crate::library::marketplace::repository::cli::run(rest));
        }
        Start::Fail(error) => {
            eprintln!("error: {error}");
            std::process::exit(2);
        }
        Start::Player(start) => Some(start),
        Start::Editor => None,
    };
    // P6 Stage 2.2/2.3: keep the build markers the exporter reads.
    super::export::pe::touch_profile_marker();
    crate::audio::clap_host::init_main_thread();
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../../assets/icons/icon.png"))
        .expect("Failed to decode embedded icon PNG");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_icon(Arc::new(icon))
            .with_inner_size([1280.0, 800.0])
            .with_title(if player_launch.is_some() {
                "Patchwork Player"
            } else {
                "PatchWork"
            }),
        renderer: eframe::Renderer::Wgpu,
        vsync: false,
        ..Default::default()
    };

    // The Player has its own app name, so it never shares the editor's
    // saved window state.
    let app_name = if player_launch.is_some() {
        "PatchWork Player"
    } else {
        "PatchWork"
    };
    eframe::run_native(
        app_name,
        options,
        Box::new(move |cc| {
            cc.egui_ctx.options_mut(|options| {
                options.screen_reader = false;
            });

            Ok(Box::new(match player_launch {
                Some(launch) => super::PatchworkApp::new_player(cc, launch),
                None => super::PatchworkApp::new(cc),
            }))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pw_launch_{tag}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn player_binary_never_starts_the_editor_or_tools() {
        let dir = temp_dir("noapp");
        for list in [
            &[][..],
            &["catalogue", "validate"][..],
            &["--player", "C:\\elsewhere"][..],
            &["anything"][..],
        ] {
            match decide(Binary::Player, &args(list), Some(&dir)) {
                Start::Player(PlayerStart::Invalid { error, .. }) => {
                    assert!(error.contains("player.json"), "{error}");
                }
                other => panic!("{list:?}: {other:?}"),
            }
        }
        assert!(matches!(
            decide(Binary::Player, &[], None),
            Start::Player(PlayerStart::Invalid { .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn player_binary_with_a_broken_manifest_shows_why() {
        let dir = temp_dir("broken");
        std::fs::write(dir.join("player.json"), "{ not json").unwrap();
        match decide(Binary::Player, &args(&["catalogue"]), Some(&dir)) {
            Start::Player(PlayerStart::Invalid { export_dir, .. }) => assert_eq!(export_dir, dir),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editor_binary_keeps_its_modes() {
        let dir = temp_dir("editor");
        assert_eq!(decide(Binary::Editor, &[], Some(&dir)), Start::Editor);
        assert_eq!(
            decide(Binary::Editor, &args(&["catalogue", "x"]), Some(&dir)),
            Start::Catalogue(args(&["x"]))
        );
        assert!(matches!(
            decide(Binary::Editor, &args(&["--output", "3"]), Some(&dir)),
            Start::Fail(_)
        ));
        assert!(matches!(decide(Binary::Editor, &[], None), Start::Fail(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The exported runtime is a separate Windows GUI-subsystem build of the
    /// same crate (no console window), built with the same modules.
    #[test]
    fn player_binary_is_a_gui_build_of_the_same_modules() {
        let editor = include_str!("../main.rs");
        let player = include_str!("../player_main.rs");
        let manifest = include_str!("../../Cargo.toml");
        assert!(player.contains("#![cfg_attr(windows, windows_subsystem = \"windows\")]"));
        assert!(!editor.contains("windows_subsystem"));
        assert!(
            player.contains("const BINARY: app::launch::Binary = app::launch::Binary::Player;")
        );
        assert!(
            editor.contains("const BINARY: app::launch::Binary = app::launch::Binary::Editor;")
        );
        assert!(
            player.contains("app::launch::run(BINARY)")
                && editor.contains("app::launch::run(BINARY)")
        );
        let modules = |text: &str| -> Vec<String> {
            text.lines()
                .map(str::trim)
                .filter(|l| l.starts_with("mod ") || l.starts_with("pub mod "))
                .map(str::to_string)
                .collect()
        };
        assert!(!modules(editor).is_empty());
        assert_eq!(modules(editor), modules(player));
        let bin = manifest
            .split("[[bin]]")
            .skip(1)
            .find(|section| section.contains("\"patchwork-player\""))
            .expect("patchwork-player binary declared");
        assert!(bin.contains("path = \"src/player_main.rs\""));
        assert!(bin.contains("test = false"));
    }
}
