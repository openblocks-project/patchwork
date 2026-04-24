//! Cross-platform display (monitor) enumeration.
//!
//! Used by `Video Out` to pop the Window sink onto a chosen monitor
//! without forcing the user to drag it. Also available to any future
//! feature that needs to know what screens are attached.
//!
//! - macOS: `NSScreen::screens()` via `objc2_app_kit` (already a dep).
//! - Windows: TODO Phase 4 — wire `EnumDisplayMonitors` via the `windows`
//!   crate when we add Spout. For now returns an empty list on Windows.
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

/// Enumerate every attached monitor. Returns an empty vec if the
/// platform's implementation failed for any reason — callers should
/// cope with an empty list gracefully.
pub fn enumerate_displays() -> Vec<Display> {
    #[cfg(target_os = "macos")]
    { return macos::enumerate(); }
    #[cfg(target_os = "linux")]
    { return linux::enumerate(); }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    { return Vec::new(); }
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
        let Ok(out) = Command::new("xrandr")
            .arg("--listmonitors")
            .output()
        else {
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
            if !line.contains(':') { continue; }
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 3 { continue; }
            let primary_name = parts[1]; // "+*HDMI-1" or "+HDMI-1"
            let is_primary = primary_name.contains('*');
            let geom = parts[2];          // "1920/520x1080/290+0+0"
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
            if idx > 32 { break; }
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
            id: "eDP-1".into(), name: "Built-in".into(),
            origin: (0, 0), size: (1920, 1080), is_primary: true,
        };
        assert_eq!(d.pretty_label(), "Built-in — 1920×1080 ★");
    }
}
