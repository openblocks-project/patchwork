use eframe::egui;
use crate::nodes::ScrollAreaExt;
use crate::graph::{NodeId, PortValue, Connection, Graph, PortKind};
use crate::http::HttpAction;
use std::collections::HashMap;
use std::sync::Arc;
use crate::graph::ImageData;

// ── Style presets ─────────────────────────────────────────────────────────────

pub struct StylePreset {
    pub label: &'static str,
    pub system: &'static str,
}

pub const TEXT_PRESETS: &[StylePreset] = &[
    StylePreset {
        label: "Concise",
        system: "Reply briefly and directly. Skip filler words. Plain text, no markdown unless asked.",
    },
    StylePreset {
        label: "Creative",
        system: "Be imaginative and playful. Vary sentence rhythm. Surprise the reader with unexpected metaphors.",
    },
    StylePreset {
        label: "Rust expert",
        system: "You are a senior Rust engineer. Reply with idiomatic, well-typed Rust. Prefer iterators and pattern matching. Brief explanations only when non-obvious.",
    },
    StylePreset {
        label: "WGSL shader",
        system: "You are a WGSL shader expert writing fragment shaders that drop directly into PatchWork's WGSL Viewer node.\n\
                 \n\
                 STRICT OUTPUT FORMAT:\n\
                 - Output ONLY the shader source. No markdown fences, no prose, no comments outside the code.\n\
                 - Provide a `@fragment fn fs_main(in: VertexOutput) -> @location(0) vec4<f32>` function. DO NOT write a vertex shader — the viewer provides one.\n\
                 - Define `struct VertexOutput { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32>, };` at the top so the fragment input type resolves.\n\
                 \n\
                 UNIFORMS (auto-injected by the viewer — DO NOT redeclare):\n\
                 - The viewer prepends a `struct Uniforms` and `@group(0) @binding(0) var<uniform> u: Uniforms;` above your code. Reference uniforms as `u.<name>`.\n\
                 - Always available: `u.time: f32`, `u.resolution: vec2<f32>`, `u.mouse: vec2<f32>`.\n\
                 - To expose a NEW knob to the user, just reference it: e.g. `u.speed`, `u.intensity`. The viewer scans your shader for `u.xxx` and auto-creates an input port + slider for each. Use snake_case names.\n\
                 - To expose a COLOR knob, reference all three of `u.<name>_r`, `u.<name>_g`, `u.<name>_b` — the viewer groups them into a single color picker.\n\
                 \n\
                 CONVENTIONS:\n\
                 - Use `in.uv` (already in [0,1], y-flipped so origin is top-left) as your normalized coordinate. Convert to centered with `let p = in.uv * 2.0 - 1.0;` and fix aspect with `p.x *= u.resolution.x / u.resolution.y;`.\n\
                 - Animate with `u.time`. Keep movement smooth.\n\
                 - Return `vec4<f32>(rgb, 1.0)` from `fs_main`.\n\
                 - Prefer `mix`, `smoothstep`, `fract`, `sin`, `length` over heavy loops. Keep it 60fps-friendly.\n\
                 - No textures, no storage buffers, no compute. The viewer only binds `@group(0) @binding(0)` for uniforms.\n\
                 \n\
                 If the user asks for a knob/slider/parameter, expose it as `u.<snake_name>` so it appears as an input port. If they ask for a color, use the `_r`/`_g`/`_b` triplet pattern.",
    },
    StylePreset {
        label: "JSON only",
        system: "Reply with a single valid JSON object. No prose, no code fences, no commentary.",
    },
];

pub const IMAGE_PRESETS: &[StylePreset] = &[
    StylePreset {
        label: "Anime",
        system: "High-quality anime illustration, expressive characters, vibrant colors, detailed line work, cel-shaded.",
    },
    StylePreset {
        label: "Photoreal",
        system: "Photorealistic, 50mm lens, natural lighting, sharp focus, high dynamic range, no text or watermark.",
    },
    StylePreset {
        label: "Pixel art",
        system: "16-bit pixel art, limited palette, crisp pixels, retro game aesthetic, no anti-aliasing.",
    },
    StylePreset {
        label: "Watercolor",
        system: "Soft watercolor painting, visible paper texture, delicate washes, hand-painted feel, muted palette.",
    },
    StylePreset {
        label: "Cinematic 3D",
        system: "Cinematic 3D render, dramatic lighting, shallow depth of field, ray-traced reflections, octane render quality.",
    },
];

// ── Provider / model tables ───────────────────────────────────────────────────

/// Returns model presets for `(provider, mode)`. `mode` 0 = Text, 1 = Image.
/// Returns an empty list if the provider has no models for that mode (e.g.
/// Anthropic + Image).
pub fn models_for(provider: &str, mode: u8) -> Vec<(&'static str, &'static str)> {
    match (provider, mode) {
        // ── Text models (vision-capable) ─────────────────────────
        ("anthropic", 0) => vec![
            ("claude-sonnet-4-20250514", "Claude Sonnet 4"),
            ("claude-haiku-35-20241022", "Claude Haiku 3.5"),
            ("claude-opus-4-20250514", "Claude Opus 4"),
        ],
        ("openai", 0) => vec![
            ("gpt-4o", "GPT-4o"),
            ("gpt-4o-mini", "GPT-4o Mini"),
            ("gpt-4-turbo", "GPT-4 Turbo"),
        ],
        ("google", 0) => vec![
            ("gemini-2.0-flash", "Gemini 2.0 Flash"),
            ("gemini-2.0-pro", "Gemini 2.0 Pro"),
            ("gemini-1.5-flash", "Gemini 1.5 Flash"),
        ],
        // ── Image-generation models ──────────────────────────────
        // ("openai", 1) => vec![
        //     ("dall-e-3", "DALL·E 3"),
        // ],
        ("openai", 1) => vec![
    ("gpt-image-2", "GPT Image 2"),
    ("gpt-image-1-mini", "GPT Image 1 Mini"),
],
        // ("google", 1) => vec![
        //     ("imagen-3-generate-002", "Imagen 3"),
        // ],
        ("google", 1) => vec![
    ("imagen-3-generate-002", "Imagen 3"),
],
        // anthropic + image: empty (no native image generation API)
        _ => vec![],
    }
}

pub fn provider_label(provider: &str) -> &'static str {
    match provider {
        "anthropic" => "Anthropic",
        "openai" => "OpenAI",
        "google" => "Google",
        _ => "Unknown",
    }
}

pub const TEXT_FORMATS: &[&str] = &["Text", "JSON", "Code", "WGSL", "HTML"];

// ── Inline base64 encoder (avoid pulling a dep just for one encode) ───────────

pub fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(((data.len() + 2) / 3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// JPEG-encode (quality 85) an `ImageData` and return base64 of the bytes.
/// Returns `None` on encode failure (e.g. zero-size image).
pub fn encode_image_to_jpeg_b64(img: &ImageData) -> Option<String> {
    if img.width == 0 || img.height == 0 || img.pixels.is_empty() {
        return None;
    }
    let buf: image::RgbaImage =
        image::ImageBuffer::from_raw(img.width, img.height, img.pixels.clone())?;
    let rgb = image::DynamicImage::ImageRgba8(buf).to_rgb8();
    let mut out: Vec<u8> = Vec::with_capacity(64 * 1024);
    {
        use image::codecs::jpeg::JpegEncoder;
        let mut enc = JpegEncoder::new_with_quality(&mut out, 85);
        enc.encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .ok()?;
    }
    Some(base64_encode(&out))
}

// ── render ────────────────────────────────────────────────────────────────────

pub fn render(
    ui: &mut egui::Ui,
    provider: &mut String,
    model: &mut String,
    system_prompt: &mut String,
    user_prompt: &mut String,
    response: &str,
    status: &str,
    max_tokens: &mut u32,
    temperature: &mut f32,
    api_key: &mut String,
    response_type: &mut u8,
    mode: &mut u8,
    last_trigger: &mut f32,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    is_pending: bool,
    actions: &mut Vec<HttpAction>,
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {
    // ── Auto-migrate Anthropic + Image (no native image API) ─────
    if *mode == 1 && provider == "anthropic" {
        *provider = "openai".into();
        if let Some((id, _)) = models_for(provider, *mode).first() {
            *model = id.to_string();
        }
    }

    let system_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 0);
    let prompt_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 1);
    let trigger_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 2);
    let image_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 3);

    // ── Mode toggle (Text / Image) at the very top ───────────────
    {
        let prev_mode = *mode;
        ui.horizontal(|ui| {
            let avail = ui.available_width();
            let half = (avail - 6.0) * 0.5;
            if ui
                .add_sized(
                    [half, 24.0],
                    egui::SelectableLabel::new(*mode == 0, "Text"),
                )
                .clicked()
            {
                *mode = 0;
            }
            if ui
                .add_sized(
                    [half, 24.0],
                    egui::SelectableLabel::new(*mode == 1, "Image"),
                )
                .clicked()
            {
                *mode = 1;
            }
        });
        if *mode != prev_mode {
            // Validate (provider, model) for new mode
            if *mode == 1 && provider == "anthropic" {
                *provider = "openai".into();
            }
            let new_models = models_for(provider, *mode);
            let model_valid = new_models.iter().any(|(id, _)| *id == model.as_str());
            if !model_valid {
                if let Some((id, _)) = new_models.first() {
                    *model = id.to_string();
                }
            }
            // Clear stale "Image" sentinel from old response_type slot
            if *mode == 0 && *response_type >= TEXT_FORMATS.len() as u8 {
                *response_type = 0;
            }
        }
    }

    ui.add_space(2.0);

    // ── Input ports ──
    ui.horizontal(|ui| {
        super::inline_port_circle(ui, node_id, 0, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Text);
        ui.label(egui::RichText::new("System").small());
        ui.add_space(4.0);
        super::inline_port_circle(ui, node_id, 1, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Text);
        ui.label(egui::RichText::new("Prompt").small());
        ui.add_space(4.0);
        super::inline_port_circle(ui, node_id, 2, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Trigger);
        ui.label(egui::RichText::new("Send").small());
    });
    // Image input port — separate row so the indicator doesn't overflow
    ui.horizontal(|ui| {
        super::inline_port_circle(ui, node_id, 3, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Image);
        ui.label(egui::RichText::new("Image").small());
        if image_wired {
            // Show tiny dimension indicator if we can read the value
            let dims = Graph::static_input_value(connections, values, node_id, 3)
                .image_dimensions();
            if let Some((w, h)) = dims {
                ui.label(
                    egui::RichText::new(format!("[{}×{}]", w, h))
                        .small()
                        .color(egui::Color32::from_rgb(140, 140, 155)),
                );
            }
            if *mode == 1 {
                ui.label(
                    egui::RichText::new("(text mode only)")
                        .small()
                        .italics()
                        .color(egui::Color32::from_rgb(140, 140, 155)),
                );
            }
        }
    });

    ui.separator();

    // ── Provider dropdown (Anthropic hidden in Image mode) ──
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Provider").small());
        let prev_provider = provider.clone();
        egui::ComboBox::from_id_salt(format!("ai_prov_{}", node_id))
            .selected_text(provider_label(provider))
            .width(120.0)
            .show_ui(ui, |ui| {
                if *mode == 0 {
                    ui.selectable_value(provider, "anthropic".into(), "Anthropic");
                }
                ui.selectable_value(provider, "openai".into(), "OpenAI");
                ui.selectable_value(provider, "google".into(), "Google");
            });
        if *provider != prev_provider {
            if let Some((id, _)) = models_for(provider, *mode).first() {
                *model = id.to_string();
            }
        }
    });

    // ── Model dropdown ──
    let models = models_for(provider, *mode);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Model").small());
        let display_name = models
            .iter()
            .find(|(id, _)| *id == model.as_str())
            .map(|(_, name)| *name)
            .unwrap_or(model.as_str());
        egui::ComboBox::from_id_salt(format!("ai_model_{}", node_id))
            .selected_text(display_name)
            .width(140.0)
            .show_ui(ui, |ui| {
                for (id, name) in &models {
                    ui.selectable_value(model, id.to_string(), *name);
                }
            });
    });

    // ── API Key (password field) ──
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("API Key").small());
        ui.add(
            egui::TextEdit::singleline(api_key)
                .password(true)
                .desired_width(140.0)
                .hint_text("sk-..."),
        );
    });

    ui.separator();

    // ── Style preset chips (mode-aware) ──
    if !system_wired {
        let presets: &[StylePreset] = if *mode == 1 { IMAGE_PRESETS } else { TEXT_PRESETS };
        ui.label(
            egui::RichText::new("Style")
                .small()
                .color(egui::Color32::from_rgb(140, 140, 155)),
        );
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for preset in presets {
                if ui
                    .add(egui::Button::new(egui::RichText::new(preset.label).small()))
                    .on_hover_text(preset.system)
                    .clicked()
                {
                    *system_prompt = preset.system.to_string();
                }
            }
        });
    }

    // ── System Prompt (textarea, unless wired) ──
    if !system_wired {
        ui.label(
            egui::RichText::new("System Prompt")
                .small()
                .color(egui::Color32::from_rgb(140, 140, 155)),
        );
        egui::ScrollArea::vertical()
            .max_height(60.0)
            .id_salt(format!("ai_sys_scroll_{}", node_id))
            .show_pannable(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(system_prompt)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY)
                        .hint_text("You are a helpful assistant...")
                        .font(egui::TextStyle::Small),
                );
            });
    }

    // ── User Prompt (textarea, unless wired) ──
    if !prompt_wired {
        ui.label(
            egui::RichText::new("Prompt")
                .small()
                .color(egui::Color32::from_rgb(140, 140, 155)),
        );
        ui.add(
            egui::TextEdit::multiline(user_prompt)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text("Ask something...")
                .font(egui::TextStyle::Small),
        );
    }

    ui.separator();

    // ── Temperature ──
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Temp").small());
        ui.add(egui::Slider::new(temperature, 0.0..=2.0).step_by(0.1).show_value(true));
    });

    // ── Format dropdown (Text mode only) ──
    if *mode == 0 {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Format").small());
            let rt = (*response_type as usize).min(TEXT_FORMATS.len() - 1);
            let rt_label = TEXT_FORMATS[rt];
            egui::ComboBox::from_id_salt(format!("ai_rt_{}", node_id))
                .selected_text(rt_label)
                .width(80.0)
                .show_ui(ui, |ui| {
                    for (i, name) in TEXT_FORMATS.iter().enumerate() {
                        if ui.selectable_label(rt == i, *name).clicked() {
                            *response_type = i as u8;
                        }
                    }
                });
        });
    }

    // ── Resolve effective prompts (wired overrides inline) ──
    let eff_system = if system_wired {
        match Graph::static_input_value(connections, values, node_id, 0) {
            PortValue::Text(s) => s,
            _ => system_prompt.clone(),
        }
    } else {
        system_prompt.clone()
    };
    let eff_prompt = if prompt_wired {
        match Graph::static_input_value(connections, values, node_id, 1) {
            PortValue::Text(s) => s,
            _ => user_prompt.clone(),
        }
    } else {
        user_prompt.clone()
    };

    // ── Resolve optional vision image (Text mode only, when wired) ──
    let eff_image: Option<Arc<ImageData>> = if image_wired && *mode == 0 {
        match Graph::static_input_value(connections, values, node_id, 3) {
            PortValue::Image(img) => Some(img),
            _ => None,
        }
    } else {
        None
    };

    // Frame-local dispatch guard. `is_pending` only reflects requests
    // the HttpManager has *already accepted*; an action pushed earlier
    // in this same frame is still invisible to it. Without this flag,
    // a Send-button click + a trigger-port rising edge + an MCP trigger
    // could all fire in the same frame and queue duplicate API calls.
    let dispatched = std::cell::Cell::new(false);

    // Helper to dispatch a request. Single chokepoint for all three
    // entry points (button / trigger port / MCP), so duplicate
    // suppression lives in exactly one place.
    let dispatch =
        |actions: &mut Vec<HttpAction>| {
            if is_pending || dispatched.get() {
                return;
            }
            let img_b64 = eff_image.as_ref().and_then(|img| encode_image_to_jpeg_b64(img));
            let (url, headers, body) = build_request(
                provider,
                model,
                api_key,
                &eff_system,
                &eff_prompt,
                *max_tokens,
                *temperature,
                *mode,
                img_b64.as_deref(),
            );
            actions.push(HttpAction::SendRequest {
                node_id,
                url,
                method: "POST".into(),
                headers,
                body,
            });
            dispatched.set(true);
        };

    // ── Send button ──
    ui.horizontal(|ui| {
        // Visual gate: also disables the button if we already
        // dispatched earlier this frame (e.g. from a trigger port).
        let in_flight = is_pending || dispatched.get();
        let can_send = !eff_prompt.is_empty() && !api_key.is_empty() && !in_flight;
        let btn_text = if in_flight { "\u{23f3} Thinking..." } else { "\u{25b6} Send" };
        if ui.add_enabled(can_send, egui::Button::new(btn_text)).clicked() {
            dispatch(actions);
        }

        // Status indicator
        if eff_prompt.is_empty() {
            ui.label(egui::RichText::new("(type a prompt)").small().color(egui::Color32::GRAY));
        } else if api_key.is_empty() {
            ui.label(
                egui::RichText::new("(need API key)")
                    .small()
                    .color(egui::Color32::from_rgb(200, 80, 80)),
            );
        } else {
            let (color, text) = if status.contains("done") {
                (egui::Color32::from_rgb(80, 200, 80), "done")
            } else if status.contains("error") {
                (egui::Color32::from_rgb(220, 80, 80), status)
            } else if is_pending {
                (egui::Color32::from_rgb(200, 200, 80), "thinking...")
            } else {
                (egui::Color32::GRAY, "ready")
            };
            ui.label(egui::RichText::new(text).small().color(color));
        }
    });

    // ── Trigger port auto-send (rising edge) ──
    // No need to gate on `is_pending` here — `dispatch` handles
    // that internally and also de-dups against same-frame dispatches.
    if trigger_wired {
        let trigger_val = match Graph::static_input_value(connections, values, node_id, 2) {
            PortValue::Float(v) => v,
            _ => 0.0,
        };
        if trigger_val > 0.5
            && *last_trigger <= 0.5
            && !eff_prompt.is_empty()
            && !api_key.is_empty()
        {
            dispatch(actions);
        }
        *last_trigger = trigger_val as f32;
    }

    // ── MCP auto-trigger ──
    if status == "mcp_trigger" && !eff_prompt.is_empty() && !api_key.is_empty() {
        dispatch(actions);
        // Only mark the trigger as consumed if we actually dispatched —
        // otherwise a request mid-flight would silently swallow the MCP
        // event and the model would never run.
        if dispatched.get() {
            ui.ctx().data_mut(|d| {
                d.insert_temp(egui::Id::new(("mcp_ai_triggered", node_id)), true)
            });
        }
    }

    // ── Output ports ──
    ui.separator();
    {
        let val_str = if response.is_empty() {
            "\u{2014}".to_string()
        } else {
            format!("{}ch", response.len())
        };
        super::output_port_row(
            ui, "Response", &val_str, node_id, 0, port_positions, dragging_from, connections,
            pending_disconnects, PortKind::Text,
        );
    }
    {
        let val_str = if status.is_empty() { "\u{2014}".to_string() } else { status.to_string() };
        super::output_port_row(
            ui, "Status", &val_str, node_id, 1, port_positions, dragging_from, connections,
            pending_disconnects, PortKind::Text,
        );
    }

    // ── Response preview ──
    if !response.is_empty() {
        ui.collapsing(format!("Response ({} chars)", response.len()), |ui| {
            egui::ScrollArea::vertical().max_height(150.0).show_pannable(ui, |ui| {
                if *mode == 1 {
                    // Image mode: show as a hyperlink
                    ui.hyperlink(response);
                } else {
                    let rt = *response_type;
                    if rt == 2 || rt == 3 || rt == 4 {
                        // Code/WGSL/HTML — show as code
                        ui.add(
                            egui::TextEdit::multiline(&mut response.to_string())
                                .code_editor()
                                .desired_width(f32::INFINITY)
                                .interactive(false),
                        );
                    } else {
                        // Text/JSON — show as wrapped text
                        ui.add(
                            egui::TextEdit::multiline(&mut response.to_string())
                                .desired_width(f32::INFINITY)
                                .interactive(false),
                        );
                    }
                }
            });
        });
    }
}

// ── Request builder ───────────────────────────────────────────────────────────

pub fn build_request(
    provider: &str,
    model: &str,
    api_key: &str,
    system_prompt: &str,
    user_prompt: &str,
    max_tokens: u32,
    temperature: f32,
    mode: u8,
    image_b64: Option<&str>,
) -> (String, Vec<(String, String)>, String) {
    let is_image_mode = mode == 1;

    match provider {
        // ── Anthropic ────────────────────────────────────────────
        "anthropic" => {
            // Image mode is unreachable (provider hidden) — fall through to
            // text endpoint with an error sentinel response that the user
            // would never actually trigger via UI. This only protects against
            // hand-edited project files.
            if is_image_mode {
                return (
                    "about:anthropic-no-image".into(),
                    vec![],
                    "{}".into(),
                );
            }
            let url = "https://api.anthropic.com/v1/messages".into();
            let headers = vec![
                ("x-api-key".into(), api_key.into()),
                ("anthropic-version".into(), "2023-06-01".into()),
                ("content-type".into(), "application/json".into()),
            ];
            // Build user message content. With image: array of parts;
            // without: a plain string.
            let user_content: serde_json::Value = if let Some(b64) = image_b64 {
                serde_json::json!([
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": "image/jpeg",
                            "data": b64,
                        }
                    },
                    { "type": "text", "text": user_prompt }
                ])
            } else {
                serde_json::Value::String(user_prompt.into())
            };
            let mut body = serde_json::json!({
                "model": model,
                "max_tokens": max_tokens,
                "temperature": temperature,
                "messages": [{"role": "user", "content": user_content}],
            });
            if !system_prompt.is_empty() {
                body["system"] = serde_json::json!(system_prompt);
            }
            (url, headers, body.to_string())
        }

        // ── Google (Gemini + Imagen) ─────────────────────────────
        "google" => {
            let url = format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
                model, api_key
            );
            let headers = vec![("Content-Type".into(), "application/json".into())];
            if is_image_mode {
                // Imagen / image-gen Gemini models — request inline image
                // bytes back. Imagen-3 returns base64 PNG in
                // candidates[0].content.parts[0].inlineData.data.
                let body = serde_json::json!({
                    "contents": [{"role": "user", "parts": [{"text": user_prompt}]}],
                    "generationConfig": {
                        "responseModalities": ["IMAGE"],
                    }
                });
                (url, headers, body.to_string())
            } else {
                // Text mode (with optional vision input)
                let mut parts: Vec<serde_json::Value> = Vec::new();
                if let Some(b64) = image_b64 {
                    parts.push(serde_json::json!({
                        "inlineData": {
                            "mimeType": "image/jpeg",
                            "data": b64,
                        }
                    }));
                }
                parts.push(serde_json::json!({"text": user_prompt}));
                let mut body = serde_json::json!({
                    "contents": [{"role": "user", "parts": parts}],
                    "generationConfig": {
                        "maxOutputTokens": max_tokens,
                        "temperature": temperature,
                    }
                });
                if !system_prompt.is_empty() {
                    body["systemInstruction"] = serde_json::json!({
                        "parts": [{"text": system_prompt}]
                    });
                }
                (url, headers, body.to_string())
            }
        }

        // ── OpenAI (default fallback for unknown providers too) ──
        "openai" | _ => {
            if is_image_mode {
                // DALL·E / gpt-image-1 — /v1/images/generations
                let url = "https://api.openai.com/v1/images/generations".into();
                let headers = vec![
                    ("Authorization".into(), format!("Bearer {}", api_key)),
                    ("Content-Type".into(), "application/json".into()),
                ];
                // Hardcoded v1 defaults: 1024x1024, n=1, standard quality.
                // dall-e-3 wants response_format=url; gpt-image-1 returns
                // base64 by default but also accepts the field.
                let body = serde_json::json!({
                    "model": model,
                    "prompt": user_prompt,
                    "n": 1,
                    "size": "1024x1024",
                    // "response_format": "url",
                });
                (url, headers, body.to_string())
            } else {
                let url = "https://api.openai.com/v1/chat/completions".into();
                let headers = vec![
                    ("Authorization".into(), format!("Bearer {}", api_key)),
                    ("Content-Type".into(), "application/json".into()),
                ];
                let mut messages: Vec<serde_json::Value> = Vec::new();
                if !system_prompt.is_empty() {
                    messages.push(serde_json::json!({
                        "role": "system",
                        "content": system_prompt,
                    }));
                }
                let user_content: serde_json::Value = if let Some(b64) = image_b64 {
                    serde_json::json!([
                        { "type": "text", "text": user_prompt },
                        {
                            "type": "image_url",
                            "image_url": { "url": format!("data:image/jpeg;base64,{}", b64) }
                        }
                    ])
                } else {
                    serde_json::Value::String(user_prompt.into())
                };
                messages.push(serde_json::json!({
                    "role": "user",
                    "content": user_content,
                }));
                let body = serde_json::json!({
                    "model": model,
                    "messages": messages,
                    "max_tokens": max_tokens,
                    "temperature": temperature,
                });
                (url, headers, body.to_string())
            }
        }
    }
}

// ── Response extraction ───────────────────────────────────────────────────────

/// Strip markdown code fences from AI responses.
/// Handles ```lang\n...\n``` and ```\n...\n```
pub fn strip_code_fences(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.starts_with("```") {
        let after_opening = if let Some(newline_pos) = trimmed.find('\n') {
            &trimmed[newline_pos + 1..]
        } else {
            return trimmed.to_string();
        };
        let result = if after_opening.trim_end().ends_with("```") {
            let end = after_opening.trim_end();
            &end[..end.len() - 3]
        } else {
            after_opening
        };
        result.trim().to_string()
    } else {
        text.to_string()
    }
}

/// Tiny base64 decoder (counterpart to `base64_encode` above). Used by the
/// Imagen response path to materialise inline image bytes.
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes: Vec<u8> = s
        .bytes()
        .filter(|b| !matches!(*b, b'\n' | b'\r' | b' ' | b'\t' | b'='))
        .collect();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let mut i = 0;
    while i + 4 <= bytes.len() {
        let v0 = val(bytes[i])?;
        let v1 = val(bytes[i + 1])?;
        let v2 = val(bytes[i + 2])?;
        let v3 = val(bytes[i + 3])?;
        out.push((v0 << 2) | (v1 >> 4));
        out.push((v1 << 4) | (v2 >> 2));
        out.push((v2 << 6) | v3);
        i += 4;
    }
    let rem = bytes.len() - i;
    if rem >= 2 {
        let v0 = val(bytes[i])?;
        let v1 = val(bytes[i + 1])?;
        out.push((v0 << 2) | (v1 >> 4));
        if rem == 3 {
            let v2 = val(bytes[i + 2])?;
            out.push((v1 << 4) | (v2 >> 2));
        }
    }
    Some(out)
}

/// If `body` contains an Imagen base64 inline image, decode it to a temp
/// PNG file and return a `file://` URL string. Returns None if the body
/// doesn't have inline image data.
pub fn try_save_imagen_image(body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let b64 = json
        .get("candidates")?
        .get(0)?
        .get("content")?
        .get("parts")?
        .as_array()?
        .iter()
        .find_map(|p| {
            p.get("inlineData")
                .and_then(|d| d.get("data"))
                .and_then(|d| d.as_str())
        })?;
    let bytes = base64_decode(b64)?;
    // Imagen returns PNG bytes; write to a temp file with a unique name.
    let mut path = std::env::temp_dir();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    path.push(format!("patchwork_imagen_{}.png", stamp));
    std::fs::write(&path, &bytes).ok()?;
    Some(format!("file://{}", path.to_string_lossy()))
}

/// Extract the text content (or image URL) from a provider's response JSON.
pub fn extract_ai_response(provider: &str, body: &str) -> String {
    // DALL-E / gpt-image-1 URL response
    // if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
    //     if let Some(url) = json
    //         .get("data")
    //         .and_then(|d| d.get(0))
    //         .and_then(|d| d.get("url"))
    //         .and_then(|u| u.as_str())
    //     {
    //         return url.to_string();
    //     }
    // }

    // OpenAI image response — URL (legacy) or b64_json (gpt-image-*)
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(url) = json
            .get("data")
            .and_then(|d| d.get(0))
            .and_then(|d| d.get("url"))
            .and_then(|u| u.as_str())
        {
            return url.to_string();
        }
        if let Some(b64) = json
            .get("data")
            .and_then(|d| d.get(0))
            .and_then(|d| d.get("b64_json"))
            .and_then(|u| u.as_str())
        {
            if let Some(bytes) = base64_decode(b64) {
                let mut path = std::env::temp_dir();
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                path.push(format!("patchwork_openai_{}.png", stamp));
                if std::fs::write(&path, &bytes).is_ok() {
                    return format!("file://{}", path.to_string_lossy());
                }
            }
        }
    }
    // Imagen inline image response
    if let Some(file_url) = try_save_imagen_image(body) {
        return file_url;
    }

    let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
        return strip_code_fences(body);
    };
    let raw = match provider {
        "anthropic" => json
            .get("content")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or(body)
            .to_string(),
        "google" => json
            .get("candidates")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.get(0))
            .and_then(|p| p.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or(body)
            .to_string(),
        "openai" | _ => json
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|t| t.as_str())
            .unwrap_or(body)
            .to_string(),
    };
    strip_code_fences(&raw)
}
