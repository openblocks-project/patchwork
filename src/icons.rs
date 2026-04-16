// Phosphor Icons integration — icon font loaded as egui font, rendered as text
// See https://phosphoricons.com/
// Font: assets/fonts/Phosphor.ttf (Regular weight)

use eframe::egui;

// ── Icon Constants (Unicode Private Use Area codepoints) ─────────────────────

pub const ARROW_UP: &str = "\u{e08e}";
pub const ARROW_DOWN: &str = "\u{e03e}";
pub const CARET_UP: &str = "\u{e13c}";
pub const CARET_DOWN: &str = "\u{e136}";
pub const CARET_LEFT: &str = "\u{e138}";
pub const CARET_RIGHT: &str = "\u{e13a}";
pub const PLUS: &str = "\u{e3d4}";
pub const MINUS: &str = "\u{e32a}";
pub const X: &str = "\u{e4f6}";
pub const X_CIRCLE: &str = "\u{e4f8}";
pub const TRASH: &str = "\u{e4a6}";
pub const CHECK: &str = "\u{e182}";
pub const CHECK_CIRCLE: &str = "\u{e184}";
pub const WARNING: &str = "\u{e4e0}";
pub const GEAR: &str = "\u{e270}";
pub const EYE: &str = "\u{e220}";
pub const EYE_SLASH: &str = "\u{e224}";
pub const COPY: &str = "\u{e1ca}";
pub const PLAY: &str = "\u{e3d0}";
pub const PAUSE: &str = "\u{e39e}";
pub const STOP: &str = "\u{e46c}";
pub const SPEAKER_HIGH: &str = "\u{e44a}";
pub const SPEAKER_X: &str = "\u{e45c}";
pub const MUSIC_NOTE: &str = "\u{e33c}";
pub const WAVEFORM: &str = "\u{e802}";
pub const FADERS: &str = "\u{e228}";
pub const SLIDERS: &str = "\u{e432}";
pub const PENCIL: &str = "\u{e3ae}";
pub const FOLDER: &str = "\u{e24a}";
pub const FOLDER_OPEN: &str = "\u{e256}";
pub const FLOPPY_DISK: &str = "\u{e248}";
pub const DOWNLOAD: &str = "\u{e20a}";
pub const UPLOAD: &str = "\u{e4be}";
pub const LINK: &str = "\u{e2e2}";
pub const LINK_BREAK: &str = "\u{e2e4}";
pub const PLUGS_CONNECTED: &str = "\u{eb5a}";
pub const DOTS_THREE: &str = "\u{e1fe}";
pub const DOTS_THREE_VERTICAL: &str = "\u{e208}";
pub const PUSH_PIN: &str = "\u{e3e2}";
pub const LOCK: &str = "\u{e2fa}";
pub const LOCK_OPEN: &str = "\u{e306}";
pub const ARROWS_OUT: &str = "\u{e0a6}";
pub const FUNNEL: &str = "\u{e266}";

// Category icons for Node Palette
pub const SLIDERS_HORIZONTAL: &str = "\u{e432}"; // Input
pub const MATH_OPERATIONS: &str = "\u{e316}"; // Math
pub const FILE_TEXT: &str = "\u{e23c}"; // IO
pub const MONITOR_ICON: &str = "\u{e336}"; // Output
pub const DIAMOND_FOUR: &str = "\u{e1ea}"; // Shader
pub const MUSIC_NOTES: &str = "\u{e33e}"; // MIDI
pub const USB: &str = "\u{e4c0}"; // Serial
pub const WIFI_HIGH: &str = "\u{e4ea}"; // OSC
pub const SPEAKER_SIMPLE_HIGH: &str = "\u{e44e}"; // Audio
pub const CPU: &str = "\u{e1d4}"; // Hardware
pub const GLOBE: &str = "\u{e28c}"; // Web
pub const CODE: &str = "\u{e19e}"; // Custom
pub const IMAGE: &str = "\u{e2b0}"; // Image
pub const FILM_STRIP: &str = "\u{e244}"; // Video
pub const WRENCH: &str = "\u{e4f2}"; // Utility
pub const BRAIN: &str = "\u{e110}"; // ML
pub const TIMER: &str = "\u{e494}"; // Time node
pub const PALETTE: &str = "\u{e39a}"; // Color node
pub const CURSOR_CLICK: &str = "\u{e1d8}"; // Mouse tracker
pub const KEYBOARD: &str = "\u{e2d6}"; // Key Input (keyboard icon)
pub const KEYBOARD_ALT: &str = "⌨"; // Unicode keyboard fallback
pub const TEXT_AA: &str = "\u{e484}"; // Alternative: ABC/text style
pub const CHAT_CIRCLE: &str = "\u{e164}"; // Comment
pub const TERMINAL: &str = "\u{e486}"; // Console
pub const LIGHTNING: &str = "\u{e2de}"; // Script
pub const CHART_LINE: &str = "\u{e170}"; // Display
pub const TEXT_T: &str = "\u{e484}"; // Text Editor
pub const TOGGLE_RIGHT: &str = "\u{e498}"; // Gate/Toggle (Phosphor toggle-right)
pub const CIRCLE_HALF: &str = "\u{e18e}"; // Half-circle / gate indicator

/// Font family name for Phosphor icons
pub const FONT_FAMILY: &str = "phosphor";

/// Font family name for Satoshi
pub const SATOSHI_FAMILY: &str = "satoshi";

/// Register Satoshi as primary font + Phosphor icon font. Call once during app setup.
pub fn setup(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // Load Satoshi Regular as the primary UI font
    fonts.font_data.insert(
        SATOSHI_FAMILY.to_string(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!("../assets/fonts/Satoshi-Regular.ttf"))),
    );

    // Load the Phosphor icon font
    fonts.font_data.insert(
        FONT_FAMILY.to_string(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!("../assets/fonts/Phosphor.ttf"))),
    );

    // Proportional: Satoshi first (primary text), then Phosphor (icon fallback), then system defaults
    let proportional = fonts.families
        .entry(egui::FontFamily::Proportional)
        .or_default();
    proportional.insert(0, SATOSHI_FAMILY.to_string());
    proportional.push(FONT_FAMILY.to_string());

    // Monospace: keep system default but add Satoshi as fallback for missing glyphs
    let monospace = fonts.families
        .entry(egui::FontFamily::Monospace)
        .or_default();
    monospace.push(SATOSHI_FAMILY.to_string());
    monospace.push(FONT_FAMILY.to_string());

    // Register Phosphor as its own family for explicit use
    fonts.families.insert(
        egui::FontFamily::Name(FONT_FAMILY.into()),
        vec![FONT_FAMILY.to_string()],
    );

    // Register Satoshi as its own family for explicit use
    fonts.families.insert(
        egui::FontFamily::Name(SATOSHI_FAMILY.into()),
        vec![SATOSHI_FAMILY.to_string()],
    );

    ctx.set_fonts(fonts);
}

/// Create a RichText icon at the given size
pub fn icon(glyph: &str, size: f32) -> egui::RichText {
    egui::RichText::new(glyph).size(size)
}

/// Create a colored icon
pub fn icon_colored(glyph: &str, size: f32, color: egui::Color32) -> egui::RichText {
    egui::RichText::new(glyph).size(size).color(color)
}

/// Get icon for a node category
pub fn category_icon(category: &str) -> &'static str {
    match category {
        "Input" => SLIDERS_HORIZONTAL,
        "Math" => MATH_OPERATIONS,
        "IO" => FILE_TEXT,
        "Output" => CHART_LINE,
        "Shader" => DIAMOND_FOUR,
        "MIDI" => MUSIC_NOTES,
        "Serial" => USB,
        "OSC" => WIFI_HIGH,
        "Audio" => SPEAKER_SIMPLE_HIGH,
        "Hardware" => CPU,
        "Web" => GLOBE,
        "Custom" => CODE,
        "Image" => IMAGE,
        "Video" => FILM_STRIP,
        "Utility" => WRENCH,
        "ML" => BRAIN,
        "System" => GEAR,
        _ => DOTS_THREE,
    }
}

/// Get icon for a specific node type by label
pub fn node_icon(label: &str) -> &'static str {
    match label {
        // Input
        "Slider" => SLIDERS,
        "Mouse Tracker" => CURSOR_CLICK,
        "Key Input" | "Keyboard Input" => KEYBOARD_ALT,
        "Time" => TIMER,
        "Color" => PALETTE,
        // Math
        "Add" => PLUS,
        "Multiply" => X,
        "Math" => MATH_OPERATIONS,
        // IO
        "File" => FOLDER_OPEN,
        "Folder" => FOLDER,
        "Text Editor" => TEXT_T,
        // Output
        "Display" => CHART_LINE,
        "HTML Viewer" => CODE,  // HTML = code
        // Shader
        "WGSL Viewer" => DIAMOND_FOUR,
        // MIDI
        "MIDI Out" => MUSIC_NOTE,
        "MIDI In" => MUSIC_NOTES,
        // Serial
        "Serial" => USB,
        // OSC
        "OSC Out" | "OSC In" => WIFI_HIGH,
        // Audio
        "Synth" => WAVEFORM,
        "Audio Player" => PLAY,
        "Audio Sampler" => MUSIC_NOTE,
        "Audio Device" => SPEAKER_HIGH,
        "Audio FX" => FADERS,
        "Mixer" => SLIDERS,
        // Hardware
        "OB Hub" => PLUGS_CONNECTED,
        "OB Joystick" => CURSOR_CLICK,
        "OB Wheel" => GEAR,
        // Custom
        "Script" => LIGHTNING,
        "Rust Plugin" => TERMINAL,
        // Web
        "HTTP Request" => GLOBE,
        "AI Request" => BRAIN,
        "JSON Extract" => CODE,
        // Utility
        "Theme" => PALETTE,
        "Comment" => CHAT_CIRCLE,
        "Console" => TERMINAL,
        "Monitor" | "System Profiler" => MONITOR_ICON,
        "Node Palette" => FUNNEL,
        // Image
        "Image" => IMAGE,
        "Image Effects" => FADERS,
        "Blend" => COPY,  // two layers
        "Color Curves" => CHART_LINE,
        // Video
        "Video Player" => FILM_STRIP,
        "Camera" => EYE,
        // ML
        "ML Model" => BRAIN,
        // System
        "MCP Server" => LIGHTNING,
        "File Menu" => FLOPPY_DISK,
        "Zoom Control" => ARROWS_OUT,
        _ => DOTS_THREE,
    }
}

/// Small icon button (returns true if clicked)
pub fn icon_button(ui: &mut egui::Ui, glyph: &str, tooltip: &str) -> bool {
    ui.add(egui::Button::new(icon(glyph, 14.0)).frame(false))
        .on_hover_text(tooltip)
        .clicked()
}
