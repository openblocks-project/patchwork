//! Cross-platform display (monitor) enumeration.
//!
//! Used by `Video Out` to pop the Window sink onto a chosen monitor
//! without forcing the user to drag it. Also available to any future
//! feature that needs to know what screens are attached.
//!
//! - macOS: `NSScreen::screens()` via `objc2_app_kit` (already a dep).
//! - Windows (P6.2): `EnumDisplayMonitors` + `GetMonitorInfoW` (user32)
//!   and `GetDpiForMonitor` (shcore), declared directly: no new crate.
//! - Linux: `xrandr --listmonitors` shell-out, primary-only fallback.
//!
//! All coordinates are in **logical points**, matching egui's coordinate
//! system — so `ViewportBuilder::with_position(origin)` lands where you
//! expect on hi-DPI displays.

use serde::{Deserialize, Serialize};

/// One monitor / display attached to the system.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Display {
    /// Platform-stable id. On macOS this is the `NSScreen.localizedName`
    /// (stable across unplug/replug for the same physical monitor; falls
    /// back to an index string for unnamed ones). On Linux it's the
    /// xrandr output name ("HDMI-1", "eDP-1"). On Windows: placeholder
    /// until Phase 4.
    pub id: String,
    /// Human-friendly label for UI. Often identical to `id`.
    pub name: String,
    /// Top-left corner in logical points.
    pub origin: (i32, i32),
    /// Width / height in logical points.
    pub size: (u32, u32),
    /// `true` if this is the primary display (where the menu bar lives
    /// on macOS, the taskbar on Windows, etc.). Exactly one entry should
    /// have this set; callers use it as the sensible default selection.
    pub is_primary: bool,
}

impl Display {
    /// Label shown in the Video Out display picker dropdown.
    pub fn pretty_label(&self) -> String {
        let star = if self.is_primary { " ★" } else { "" };
        format!("{} — {}×{}{}", self.name, self.size.0, self.size.1, star)
    }
}

/// The display a saved `destination` names, falling back to the primary
/// display (then the first) when it is empty or no longer attached. The
/// one rule shared by the Video Out node and the Player (P6.2).
pub fn choose<'a>(displays: &'a [Display], destination: &str) -> Option<&'a Display> {
    displays
        .iter()
        .find(|d| !destination.is_empty() && d.id == destination)
        .or_else(|| displays.iter().find(|d| d.is_primary))
        .or_else(|| displays.first())
}

/// Enumerate every attached monitor. Returns an empty vec if the
/// platform's implementation failed for any reason — callers should
/// cope with an empty list gracefully.
pub fn enumerate_displays() -> Vec<Display> {
    #[cfg(target_os = "macos")]
    {
        return macos::enumerate();
    }
    #[cfg(target_os = "linux")]
    {
        return linux::enumerate();
    }
    #[cfg(target_os = "windows")]
    {
        return windows::enumerate();
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        return Vec::new();
    }
}

/// One monitor as Windows reports it: device name (`\\.\DISPLAY2`), its
/// rectangle in physical pixels on the virtual desktop (left, top, right,
/// bottom; negative left of / above the primary) and the primary flag.
#[derive(Debug, Clone, PartialEq)]
pub struct RawMonitor {
    pub device: String,
    pub rect: (i32, i32, i32, i32),
    pub primary: bool,
}

/// Convert Windows monitors to [`Display`]s in egui points.
///
/// egui places windows at `points × pixels_per_point`, where that factor
/// is the **primary** monitor's scale for a new viewport (the Video Out
/// window) and the window's own for a move (the Player, which opens on the
/// primary). Dividing every monitor's physical rectangle by the primary's
/// scale therefore lands each window on its monitor, including monitors
/// with another resolution or scale and monitors at negative coordinates.
/// Empty rectangles are skipped; ids are the device names.
pub fn displays_from_raw(raw: &[RawMonitor], primary_scale: f32) -> Vec<Display> {
    let scale = if primary_scale.is_finite() && primary_scale > 0.0 {
        primary_scale
    } else {
        1.0
    };
    let points = |v: i32| (v as f32 / scale).round() as i32;
    let mut seen_primary = false;
    raw.iter()
        .filter(|m| m.rect.2 > m.rect.0 && m.rect.3 > m.rect.1)
        .enumerate()
        .map(|(index, m)| {
            let (left, top, right, bottom) = m.rect;
            // Exactly one primary, even from inconsistent input.
            let is_primary = m.primary && !seen_primary;
            seen_primary |= is_primary;
            Display {
                id: m.device.clone(),
                name: display_name(&m.device, index),
                origin: (points(left), points(top)),
                size: (
                    points(right - left).max(1) as u32,
                    points(bottom - top).max(1) as u32,
                ),
                is_primary,
            }
        })
        .collect()
}

/// "Display 2" for `\\.\DISPLAY2`, else "Display <position>".
fn display_name(device: &str, index: usize) -> String {
    let digits = device.len() - device.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        format!("Display {}", index + 1)
    } else {
        format!("Display {}", &device[device.len() - digits..])
    }
}

// ── Windows ─────────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod windows {
    //! Minimal Win32 bindings: user32 (already linked by winit) and shcore
    //! (Windows 8.1+), declared here instead of adding a crate.
    use super::{Display, RawMonitor, displays_from_raw};
    use std::ffi::c_void;

    type Hmonitor = *mut c_void;
    type Hdc = *mut c_void;
    type Bool = i32;
    type Lparam = isize;

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: i32,
        y: i32,
    }

    /// `MONITORINFOEXW`.
    #[repr(C)]
    struct MonitorInfoExW {
        cb_size: u32,
        rc_monitor: Rect,
        rc_work: Rect,
        dw_flags: u32,
        sz_device: [u16; 32],
    }

    const MONITORINFOF_PRIMARY: u32 = 1;
    const MONITOR_DEFAULTTOPRIMARY: u32 = 1;
    const MDT_EFFECTIVE_DPI: i32 = 0;

    type MonitorEnumProc = unsafe extern "system" fn(Hmonitor, Hdc, *mut Rect, Lparam) -> Bool;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn EnumDisplayMonitors(
            hdc: Hdc,
            clip: *const Rect,
            callback: Option<MonitorEnumProc>,
            data: Lparam,
        ) -> Bool;
        fn GetMonitorInfoW(monitor: Hmonitor, info: *mut MonitorInfoExW) -> Bool;
        fn MonitorFromPoint(point: Point, flags: u32) -> Hmonitor;
    }

    #[link(name = "shcore")]
    unsafe extern "system" {
        fn GetDpiForMonitor(monitor: Hmonitor, kind: i32, dpi_x: *mut u32, dpi_y: *mut u32) -> i32;
    }

    unsafe extern "system" fn collect(
        monitor: Hmonitor,
        _hdc: Hdc,
        _rect: *mut Rect,
        data: Lparam,
    ) -> Bool {
        // SAFETY: `data` is the `&mut Vec<RawMonitor>` passed by `enumerate`,
        // alive for the whole (synchronous) enumeration.
        let out = unsafe { &mut *(data as *mut Vec<RawMonitor>) };
        let mut info = MonitorInfoExW {
            cb_size: std::mem::size_of::<MonitorInfoExW>() as u32,
            rc_monitor: Rect::default(),
            rc_work: Rect::default(),
            dw_flags: 0,
            sz_device: [0; 32],
        };
        // SAFETY: `info` is a correctly sized MONITORINFOEXW.
        if unsafe { GetMonitorInfoW(monitor, &mut info) } != 0 {
            let len = info
                .sz_device
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(info.sz_device.len());
            let r = info.rc_monitor;
            out.push(RawMonitor {
                device: String::from_utf16_lossy(&info.sz_device[..len]),
                rect: (r.left, r.top, r.right, r.bottom),
                primary: info.dw_flags & MONITORINFOF_PRIMARY != 0,
            });
        }
        // Continue: a monitor that cannot be read (just unplugged) is skipped.
        1
    }

    /// The primary monitor's scale (effective DPI / 96), or 1.0.
    fn primary_scale() -> f32 {
        // SAFETY: plain Win32 calls with valid out-pointers.
        unsafe {
            let primary = MonitorFromPoint(Point { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            if primary.is_null() {
                return 1.0;
            }
            let (mut x, mut y) = (0u32, 0u32);
            if GetDpiForMonitor(primary, MDT_EFFECTIVE_DPI, &mut x, &mut y) == 0 && x > 0 {
                x as f32 / 96.0
            } else {
                1.0
            }
        }
    }

    pub fn enumerate() -> Vec<Display> {
        let mut raw: Vec<RawMonitor> = Vec::new();
        // SAFETY: synchronous enumeration; `collect` only pushes to `raw`.
        let ok = unsafe {
            EnumDisplayMonitors(
                std::ptr::null_mut(),
                std::ptr::null(),
                Some(collect),
                &mut raw as *mut Vec<RawMonitor> as Lparam,
            )
        };
        if ok == 0 {
            return Vec::new();
        }
        displays_from_raw(&raw, primary_scale())
    }
}

// ── macOS ───────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod macos {
    use super::Display;
    use objc2_app_kit::NSScreen;
    use objc2_foundation::MainThreadMarker;

    pub fn enumerate() -> Vec<Display> {
        // `NSScreen::screens` must be called on the main thread. Nodes
        // render on the UI thread where this is true. If somehow called
        // off-thread, bail out gracefully rather than panic.
        let Some(mtm) = MainThreadMarker::new() else {
            return Vec::new();
        };
        let screens = NSScreen::screens(mtm);
        let primary_id = NSScreen::mainScreen(mtm).and_then(|s| screen_localized_name(&s));

        screens
            .iter()
            .enumerate()
            .map(|(idx, screen)| {
                let frame = screen.frame();
                let name = screen_localized_name(&screen)
                    .unwrap_or_else(|| format!("Display {}", idx + 1));
                let is_primary = primary_id
                    .as_deref()
                    .map(|p| p == name.as_str())
                    .unwrap_or(idx == 0);
                Display {
                    id: name.clone(),
                    name,
                    origin: (frame.origin.x as i32, frame.origin.y as i32),
                    size: (
                        frame.size.width.max(0.0) as u32,
                        frame.size.height.max(0.0) as u32,
                    ),
                    is_primary,
                }
            })
            .collect()
    }

    /// `NSScreen.localizedName` — String? in Swift; None on 10.15 and
    /// earlier, but we target 11.0+ (`minimum-system-version` in
    /// Cargo.toml packager metadata) so it should always return Some.
    /// Fall back silently otherwise.
    fn screen_localized_name(screen: &NSScreen) -> Option<String> {
        // SAFETY: main-thread only; checked by caller. `localizedName`
        // returns an autoreleased NSString we copy out immediately.
        let name = unsafe { screen.localizedName() };
        Some(name.to_string())
    }
}

// ── Linux ───────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod linux {
    use super::Display;
    use std::process::Command;

    pub fn enumerate() -> Vec<Display> {
        // Try xrandr first — common on X11 + XWayland sessions.
        let Ok(out) = Command::new("xrandr").arg("--listmonitors").output() else {
            return Vec::new();
        };
        if !out.status.success() {
            return Vec::new();
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut displays = Vec::new();
        for (idx, line) in stdout.lines().enumerate() {
            // Format: " 0: +*HDMI-1 1920/520x1080/290+0+0  HDMI-1"
            //          idx primary geometry                  name
            let line = line.trim();
            if !line.contains(':') {
                continue;
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 3 {
                continue;
            }
            let primary_name = parts[1]; // "+*HDMI-1" or "+HDMI-1"
            let is_primary = primary_name.contains('*');
            let geom = parts[2]; // "1920/520x1080/290+0+0"
            let (size, origin) = parse_xrandr_geom(geom).unwrap_or(((1920, 1080), (0, 0)));
            let name = parts.last().unwrap_or(&"").to_string();
            displays.push(Display {
                id: name.clone(),
                name,
                origin,
                size,
                is_primary,
            });
            // Safety net against pathological xrandr output.
            if idx > 32 {
                break;
            }
        }
        displays
    }

    /// Parse xrandr's `WIDTH/PW_MMxHEIGHT/PH_MM+X+Y` geometry string.
    /// Returns `(size, origin)` in logical points.
    fn parse_xrandr_geom(s: &str) -> Option<((u32, u32), (i32, i32))> {
        let (wh, xy) = s.split_once('+')?;
        let (w_part, h_part) = wh.split_once('x')?;
        let w: u32 = w_part.split('/').next()?.parse().ok()?;
        let h: u32 = h_part.split('/').next()?.parse().ok()?;
        let (x_str, y_str) = xy.split_once('+')?;
        let x: i32 = x_str.parse().ok()?;
        let y: i32 = y_str.parse().ok()?;
        Some(((w, h), (x, y)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerate_returns_at_least_primary_on_real_desktop() {
        // In CI with no display this will be empty — that's OK.
        let displays = enumerate_displays();
        // At most one primary flag set
        let primaries = displays.iter().filter(|d| d.is_primary).count();
        assert!(primaries <= 1, "more than one display flagged primary");
    }

    #[test]
    fn pretty_label_formats() {
        let d = Display {
            id: "eDP-1".into(),
            name: "Built-in".into(),
            origin: (0, 0),
            size: (1920, 1080),
            is_primary: true,
        };
        assert_eq!(d.pretty_label(), "Built-in — 1920×1080 ★");
    }

    // ── P6.2: Windows conversion and the shared choice ─────────────────

    fn raw(device: &str, rect: (i32, i32, i32, i32), primary: bool) -> RawMonitor {
        RawMonitor {
            device: device.to_string(),
            rect,
            primary,
        }
    }

    #[test]
    fn a_single_monitor_is_the_primary_at_the_origin() {
        let d = displays_from_raw(&[raw(r"\\.\DISPLAY1", (0, 0, 1920, 1080), true)], 1.0);
        assert_eq!(
            d,
            vec![Display {
                id: r"\\.\DISPLAY1".into(),
                name: "Display 1".into(),
                origin: (0, 0),
                size: (1920, 1080),
                is_primary: true,
            }]
        );
    }

    #[test]
    fn monitors_left_of_and_above_the_primary_have_negative_origins() {
        let d = displays_from_raw(
            &[
                raw(r"\\.\DISPLAY1", (0, 0, 2560, 1440), true),
                raw(r"\\.\DISPLAY2", (-1920, 200, 0, 1280), false),
                raw(r"\\.\DISPLAY3", (0, -1080, 1920, 0), false),
            ],
            1.0,
        );
        assert_eq!(d.len(), 3);
        // Different resolutions are kept per monitor.
        assert_eq!((d[0].origin, d[0].size), ((0, 0), (2560, 1440)));
        assert_eq!((d[1].origin, d[1].size), ((-1920, 200), (1920, 1080)));
        assert_eq!((d[2].origin, d[2].size), ((0, -1080), (1920, 1080)));
        assert_eq!(d.iter().filter(|d| d.is_primary).count(), 1);
        assert!(d[0].is_primary);
        assert_eq!(d[1].name, "Display 2");
    }

    #[test]
    fn coordinates_are_in_points_of_the_primary_scale() {
        // Primary at 150%: a 3840×2160 primary and a 1920×1080 monitor to
        // its left. egui multiplies by 1.5 again, landing on the pixels.
        let d = displays_from_raw(
            &[
                raw(r"\\.\DISPLAY1", (0, 0, 3840, 2160), true),
                raw(r"\\.\DISPLAY2", (-1920, 0, 0, 1080), false),
            ],
            1.5,
        );
        assert_eq!((d[0].origin, d[0].size), ((0, 0), (2560, 1440)));
        assert_eq!((d[1].origin, d[1].size), ((-1280, 0), (1280, 720)));
        // An unusable scale falls back to 1.0 rather than dividing by zero.
        let d = displays_from_raw(&[raw("X", (0, 0, 800, 600), true)], 0.0);
        assert_eq!(d[0].size, (800, 600));
        let d = displays_from_raw(&[raw("X", (0, 0, 800, 600), true)], f32::NAN);
        assert_eq!(d[0].size, (800, 600));
    }

    #[test]
    fn unreadable_monitors_are_skipped_and_one_primary_is_kept() {
        let d = displays_from_raw(
            &[
                raw("A", (0, 0, 0, 0), true), // disconnected mid-enumeration
                raw("B", (0, 0, 1280, 1024), true),
                raw("C", (1280, 0, 2560, 1024), true),
            ],
            1.0,
        );
        assert_eq!(d.len(), 2);
        assert_eq!(d.iter().filter(|d| d.is_primary).count(), 1);
        assert!(d[0].is_primary);
        // A device without a number is named by position.
        assert_eq!(d[0].name, "Display 1");
        assert!(displays_from_raw(&[], 1.0).is_empty());
    }

    #[test]
    fn the_saved_display_is_chosen_else_the_primary_else_the_first() {
        let d = displays_from_raw(
            &[
                raw(r"\\.\DISPLAY2", (-1920, 0, 0, 1080), false),
                raw(r"\\.\DISPLAY1", (0, 0, 1920, 1080), true),
            ],
            1.0,
        );
        // Saved and attached: kept, even at negative coordinates.
        assert_eq!(choose(&d, r"\\.\DISPLAY2").unwrap().origin, (-1920, 0));
        // Saved but disconnected, or never chosen: the primary.
        assert_eq!(choose(&d, r"\\.\DISPLAY9").unwrap().id, r"\\.\DISPLAY1");
        assert_eq!(choose(&d, "").unwrap().id, r"\\.\DISPLAY1");
        // No primary flagged: the first. None attached: nothing.
        let no_primary = displays_from_raw(&[raw("A", (0, 0, 10, 10), false)], 1.0);
        assert_eq!(choose(&no_primary, "Z").unwrap().id, "A");
        assert!(choose(&[], "A").is_none());
    }

    /// The real monitors of this machine (Windows): at least one, exactly
    /// one primary, all with a size. Headless machines may report none.
    #[cfg(target_os = "windows")]
    #[test]
    fn windows_reports_the_attached_monitors() {
        let displays = enumerate_displays();
        if displays.is_empty() {
            eprintln!("no monitors reported (headless session)");
            return;
        }
        eprintln!("monitors: {displays:?}");
        assert_eq!(displays.iter().filter(|d| d.is_primary).count(), 1);
        assert!(displays.iter().all(|d| d.size.0 > 0 && d.size.1 > 0));
        assert!(displays.iter().all(|d| d.id.starts_with(r"\\.\")));
        assert!(choose(&displays, "missing").unwrap().is_primary);
    }
}
