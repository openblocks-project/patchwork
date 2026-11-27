//! Program start, shared by the two executables built from this crate.
//!
//! - `patchwork` ([`Binary::Editor`], `src/main.rs`): the editor, a console
//!   program so `patchwork catalogue …` and `--player` errors print to the
//!   terminal. A `player.json` beside it still makes it that export's
//!   Player (P6 Stage 2.1). Installer builds (`--features
//!   windows-gui-editor`, P6 Stage 3.12) are GUI programs instead, which
//!   write to the terminal they are started from
//!   ([`attach_parent_console`]).
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

/// Whether this executable is a Windows GUI-subsystem editor (an installer
/// build, `--features windows-gui-editor`; P6 Stage 3.12).
#[cfg(windows)]
const GUI_EDITOR: bool = cfg!(all(windows, feature = "windows-gui-editor"));

/// P6 Stage 3.12: a GUI-subsystem editor has no console of its own.
///
/// - Output redirected by whoever started it (a CI shell's pipes, `cargo
///   run`, `> file`) is used as it is.
/// - Started from a terminal, it writes to that terminal. (Windows shells
///   do not wait for a GUI program at an interactive prompt, so the output
///   follows the next prompt and the exit code is not shown; `start /wait`
///   or `Start-Process -Wait` waits.)
/// - Otherwise (a shortcut) there is no console and nothing opens one.
///
/// Used only for the console modes ([`run`]): the running editor's log goes
/// to the in-app system log, or to redirected output.
///
/// Returns whether standard error now leads somewhere.
#[cfg(windows)]
fn attach_parent_console() -> bool {
    use std::ffi::c_void;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ_WRITE: u32 = 0x1 | 0x2;
    const OPEN_EXISTING: u32 = 3;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn SetStdHandle(which: u32, handle: *mut c_void) -> i32;
        fn AttachConsole(process: u32) -> i32;
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security: *mut c_void,
            disposition: u32,
            flags: u32,
            template: *mut c_void,
        ) -> *mut c_void;
    }
    let usable = |h: *mut c_void| !h.is_null() && h as isize != -1;
    // SAFETY: plain Win32 calls with valid arguments; handles are only
    // stored as the process's standard handles.
    unsafe {
        let missing: Vec<u32> = [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]
            .into_iter()
            .filter(|&which| !usable(GetStdHandle(which)))
            .collect();
        if missing.is_empty() {
            return true;
        }
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return usable(GetStdHandle(STD_ERROR_HANDLE));
        }
        let name: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
        for which in missing {
            let console = CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );
            if usable(console) {
                SetStdHandle(which, console);
            }
        }
        usable(GetStdHandle(STD_ERROR_HANDLE))
    }
}

/// Show a start-up error to someone who has no console to read it in.
#[cfg(windows)]
fn show_error_box(text: &str) {
    use std::ffi::c_void;
    const MB_ICONERROR: u32 = 0x10;
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(owner: *mut c_void, text: *const u16, caption: *const u16, kind: u32)
        -> i32;
    }
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (text, caption) = (wide(text), wide("Patchwork"));
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            caption.as_ptr(),
            MB_ICONERROR,
        );
    }
}

/// Run `binary` (the body of both `main` functions).
pub fn run(binary: Binary) -> eframe::Result {
    // P6 Stage 3.12: only the console modes of a GUI editor (the catalogue
    // tool and start-up errors) use the terminal it was started from. The
    // editor and the Player themselves never attach to one, so closing that
    // terminal or pressing Ctrl+C in it cannot end them.
    let console_mode = || -> bool {
        #[cfg(windows)]
        if GUI_EDITOR && binary == Binary::Editor {
            return attach_parent_console();
        }
        true
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let player_launch = match decide(binary, &args, exe_dir.as_deref()) {
        Start::Catalogue(rest) => {
            console_mode();
            std::process::exit(crate::library::marketplace::repository::cli::run(rest));
        }
        Start::Fail(error) => {
            let has_console = console_mode();
            eprintln!("error: {error}");
            #[cfg(windows)]
            if !has_console {
                show_error_box(&format!("Patchwork could not start: {error}"));
            }
            let _ = has_console;
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
        let lf = |text: &str| text.replace("\r\n", "\n");
        let editor = &lf(include_str!("../main.rs"));
        let player = &lf(include_str!("../player_main.rs"));
        let manifest = &lf(include_str!("../../Cargo.toml"));
        assert!(player.contains("#![cfg_attr(windows, windows_subsystem = \"windows\")]"));
        // The editor is a GUI program only in installer builds, never in
        // its tests.
        assert_eq!(editor.matches("windows_subsystem").count(), 1);
        assert!(editor.contains(
            "all(windows, feature = \"windows-gui-editor\", not(test)),\n    \
             windows_subsystem = \"windows\""
        ));
        assert!(manifest.contains("\n[features]\n"));
        assert!(manifest.contains("\nwindows-gui-editor = []\n"));
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
