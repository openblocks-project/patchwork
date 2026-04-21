use crate::graph::*;
use eframe::egui;
use std::collections::HashMap;
use std::sync::Arc;

pub use crate::ml::bundled::{
    BUNDLED_FACE_LANDMARK_MODEL, BUNDLED_FACE_MODEL, BUNDLED_FACE_SENTINEL,
    BUNDLED_HAND_LANDMARK_MODEL, BUNDLED_HAND_MODEL, BUNDLED_HAND_SENTINEL,
    BUNDLED_POSE_LANDMARK_MODEL, BUNDLED_POSE_MODEL, BUNDLED_POSE_SENTINEL,
};

pub fn render(
    ui: &mut egui::Ui,
    node_id: NodeId,
    node_type: &mut NodeType,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
) {
    let (model_path, labels_path, confidence, preset, result_text, status, last_input_hash, interval_secs, last_inference_secs) = match node_type {
        NodeType::MlModel { model_path, labels_path, confidence, preset, result_text, status,
            last_input_hash, interval_secs, last_inference_secs, .. } => {
            (model_path, labels_path, confidence, preset, result_text, status, last_input_hash, interval_secs, last_inference_secs)
        }
        _ => return,
    };

    // ── Preset selector ───────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label("Preset:");
        egui::ComboBox::from_id_salt(egui::Id::new(("ml_preset", node_id)))
            .selected_text(preset.name())
            .width(130.0)
            .show_ui(ui, |ui| {
                for p in MlPreset::all() {
                    if ui.selectable_label(*preset == *p, p.name()).clicked() {
                        *preset = p.clone();
                    }
                }
            });
    });

    // ── Model file selector ───────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label("Model:");
        if ui.button("Load .onnx").clicked() {
            if let Some(p) = rfd::FileDialog::new()
                .add_filter("ONNX", &["onnx"])
                .pick_file()
            {
                *model_path = p.display().to_string();
                *status = "Model loaded".into();
            }
        }
    });
    if !model_path.is_empty() {
        let short = if model_path.len() > 30 {
            format!("...{}", &model_path[model_path.len()-30..])
        } else {
            model_path.clone()
        };
        ui.label(egui::RichText::new(short).small().monospace());
    }

    // Labels file (optional, mainly for Classification)
    if matches!(*preset, MlPreset::Classification | MlPreset::ObjectDetection | MlPreset::Custom) {
        ui.horizontal(|ui| {
            ui.label("Labels:");
            if ui.button("Load .txt").clicked() {
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("Text", &["txt", "csv"])
                    .pick_file()
                {
                    *labels_path = p.display().to_string();
                }
            }
        });
        if !labels_path.is_empty() {
            let short = if labels_path.len() > 30 {
                format!("...{}", &labels_path[labels_path.len()-30..])
            } else {
                labels_path.clone()
            };
            ui.label(egui::RichText::new(short).small().monospace());
        }
    }

    // Confidence threshold
    ui.horizontal(|ui| {
        ui.label("Threshold:");
        ui.add(egui::Slider::new(confidence, 0.01..=1.0).step_by(0.01));
    });

    // Run interval (FPS control)
    ui.horizontal(|ui| {
        ui.label("Every:");
        ui.add(egui::Slider::new(interval_secs, 0.05..=5.0)
            .step_by(0.05)
            .suffix("s")
            .logarithmic(true));
        let fps = 1.0 / *interval_secs;
        ui.label(egui::RichText::new(format!("({:.1} fps)", fps)).small()
            .color(ui.visuals().widgets.noninteractive.fg_stroke.color));
    });

    // Input size info
    ui.label(egui::RichText::new(format!("Input: {}×{}", preset.input_size(), preset.input_size())).small().color(egui::Color32::from_rgb(140, 140, 160)));

    // Status
    if !status.is_empty() {
        let color = if status.starts_with("Error") || status.starts_with("error") {
            egui::Color32::from_rgb(255, 100, 100)
        } else if status.contains("Running") {
            egui::Color32::from_rgb(200, 200, 80)
        } else {
            egui::Color32::from_rgb(80, 200, 80)
        };
        ui.colored_label(color, egui::RichText::new(&*status).small());
    }

    // Show text result
    if !result_text.is_empty() {
        ui.separator();
        ui.label(egui::RichText::new("Results:").small().strong());
        for line in result_text.lines().take(10) {
            ui.label(egui::RichText::new(line).small().monospace());
        }
    }

    // Check for input image and trigger inference
    let input_val = Graph::static_input_value(connections, values, node_id, 0);
    if let PortValue::Image(img) = &input_val {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        img.width.hash(&mut h);
        img.height.hash(&mut h);
        if img.pixels.len() >= 32 {
            img.pixels[..16].hash(&mut h);
            img.pixels[img.pixels.len()-16..].hash(&mut h);
        }
        let hash = h.finish();
        let now = ui.ctx().input(|i| i.time);

        // Only trigger if: image changed OR enough time has passed since last inference
        let time_ok = now - *last_inference_secs >= *interval_secs as f64;
        if time_ok && !model_path.is_empty() {
            *last_input_hash = hash;
            *last_inference_secs = now;
            *status = "Running inference...".into();

            let inference_id = egui::Id::new(("ml_inference", node_id));
            ui.ctx().data_mut(|d| d.insert_temp(inference_id, MlInferenceRequest {
                model_path: model_path.clone(),
                labels_path: labels_path.clone(),
                confidence: *confidence,
                preset: preset.clone(),
                image: img.clone(),
                node_id,
            }));
        }
    }
}

/// Request for background inference
#[derive(Clone)]
pub struct MlInferenceRequest {
    pub model_path: String,
    pub labels_path: String,
    pub confidence: f32,
    pub preset: MlPreset,
    pub image: Arc<ImageData>,
    pub node_id: NodeId,
}

/// Result from background inference
#[derive(Clone)]
pub struct MlInferenceResult {
    pub node_id: NodeId,
    pub result_text: String,
    pub result_json: String,
    pub annotated_frame: Option<Arc<ImageData>>,
    pub status: String,
}

/// Run ONNX inference on an image (called from background thread)
pub fn run_inference(req: &MlInferenceRequest) -> MlInferenceResult {
    // The bundled sentinels trigger the full 2-stage tracking pipelines.
    if req.model_path == BUNDLED_HAND_SENTINEL {
        return run_hand_tracking(req);
    }
    if req.model_path == BUNDLED_FACE_SENTINEL {
        return run_face_tracking(req);
    }
    if req.model_path == BUNDLED_POSE_SENTINEL {
        return run_pose_tracking(req);
    }
    match run_inference_inner(req) {
        Ok(result) => result,
        Err(e) => MlInferenceResult {
            node_id: req.node_id,
            result_text: String::new(),
            result_json: String::new(),
            annotated_frame: None,
            status: format!("Error: {}", e),
        },
    }
}

/// Build the input tensor for a given size and layout. Returns the tensor and
/// the backing data (must be kept alive while tensor is in use).
fn build_input_tensor(
    req: &MlInferenceRequest,
    target_size: u32,
    is_nchw: bool,
) -> (ort::value::Tensor<f32>, Box<[f32]>) {
    let ts = target_size as usize;
    let resized = resize_image(&req.image, target_size, target_size);

    let use_imagenet = req.preset.imagenet_norm();
    let mean = if use_imagenet { [0.485f32, 0.456, 0.406] } else { [0.0; 3] };
    let std_dev = if use_imagenet { [0.229f32, 0.224, 0.225] } else { [1.0; 3] };

    let mut input_data = vec![0.0f32; 3 * ts * ts];
    for y in 0..ts {
        for x in 0..ts {
            let idx = (y * ts + x) * 4;
            if idx + 2 >= resized.len() { continue; }
            let r = resized[idx] as f32 / 255.0;
            let g = resized[idx + 1] as f32 / 255.0;
            let b = resized[idx + 2] as f32 / 255.0;
            if is_nchw {
                input_data[0 * ts * ts + y * ts + x] = (r - mean[0]) / std_dev[0];
                input_data[1 * ts * ts + y * ts + x] = (g - mean[1]) / std_dev[1];
                input_data[2 * ts * ts + y * ts + x] = (b - mean[2]) / std_dev[2];
            } else {
                input_data[(y * ts + x) * 3 + 0] = (r - mean[0]) / std_dev[0];
                input_data[(y * ts + x) * 3 + 1] = (g - mean[1]) / std_dev[1];
                input_data[(y * ts + x) * 3 + 2] = (b - mean[2]) / std_dev[2];
            }
        }
    }

    let shape = if is_nchw {
        vec![1usize, 3, ts, ts]
    } else {
        vec![1usize, ts, ts, 3]
    };
    let boxed = input_data.into_boxed_slice();
    let tensor = match ort::value::Tensor::from_array((shape, boxed.clone())) {
        Ok(t) => t,
        Err(e) => {
            crate::system_log::error(format!("ML tensor creation failed: {}", e));
            // Return a minimal 1x1 tensor as fallback — shape [1,3,1,1] with 3 zeros
            // cannot fail since shape and data length match exactly.
            let fallback_shape = vec![1usize, 3, 1, 1];
            let fallback_data = vec![0.0f32; 3].into_boxed_slice();
            match ort::value::Tensor::from_array((fallback_shape, fallback_data)) {
                Ok(t) => return (t, boxed),
                Err(e2) => {
                    // ORT runtime is fundamentally broken — create the simplest possible tensor
                    crate::system_log::error(format!("ML fallback tensor also failed: {}", e2));
                    let t = ort::value::Tensor::from_array(([1usize, 1, 1, 1], vec![0.0f32].into_boxed_slice()))
                        .expect("1-element tensor must succeed");
                    return (t, boxed);
                }
            }
        }
    };
    (tensor, boxed)
}

/// Parse ONNX Runtime dimension error to extract the expected input shape.
/// Handles concatenated output like "Expected: 3index: 2 Got: 224 Expected: 128"
/// where entries run together without newlines.
/// Returns (spatial_size, is_nchw) if parseable.
fn parse_expected_shape(err: &str) -> Option<(usize, bool)> {
    // Use regex to find all "index: N Got: N Expected: N" patterns,
    // even when concatenated without separators.
    let mut expected_dims: Vec<(usize, usize)> = Vec::new();

    // Find all occurrences of "Expected: <number>" preceded by "index: <number>"
    // The error format is: "index: I Got: G Expected: E" possibly concatenated
    let err_lower = err.to_string();
    let mut search_from = 0;
    while let Some(pos) = err_lower[search_from..].find("Expected: ") {
        let abs_pos = search_from + pos;
        let after = &err_lower[abs_pos + 10..]; // skip "Expected: "
        // Extract the number (may be followed by "index" or newline or other text)
        let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(expected_val) = num_str.parse::<usize>() {
            // Now find the corresponding "index: N" before this "Expected:"
            // Search backward from abs_pos for "index: "
            if let Some(idx_pos) = err_lower[..abs_pos].rfind("index: ") {
                let idx_after = &err_lower[idx_pos + 7..]; // skip "index: "
                let idx_str: String = idx_after.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(idx) = idx_str.parse::<usize>() {
                    if !expected_dims.iter().any(|(i, _)| *i == idx) {
                        expected_dims.push((idx, expected_val));
                    }
                }
            }
        }
        search_from = abs_pos + 10 + num_str.len().max(1);
    }

    if expected_dims.len() < 2 { return None; }

    expected_dims.sort_by_key(|(i, _)| *i);

    let dim1 = expected_dims.iter().find(|(i, _)| *i == 1).map(|(_, v)| *v);
    let dim2 = expected_dims.iter().find(|(i, _)| *i == 2).map(|(_, v)| *v);
    let dim3 = expected_dims.iter().find(|(i, _)| *i == 3).map(|(_, v)| *v);

    match (dim1, dim2, dim3) {
        (Some(3), Some(h), Some(w)) if h == w => Some((h, true)),   // NCHW [1,3,H,W]
        (Some(3), Some(h), _) => Some((h, true)),                    // NCHW [1,3,H,W]
        (Some(h), Some(w), Some(3)) if h == w => Some((h, false)),  // NHWC [1,H,W,3]
        (Some(h), _, Some(3)) => Some((h, false)),                   // NHWC [1,H,W,3]
        (_, Some(h), Some(w)) if h == w => Some((h, true)),         // guess NCHW
        _ => None,
    }
}

fn run_inference_inner(req: &MlInferenceRequest) -> Result<MlInferenceResult, String> {
    use ort::session::Session;

    let mut session = {
        let mut builder = Session::builder()
            .map_err(|e| format!("Session builder: {}", e))?;
        if req.model_path == BUNDLED_HAND_SENTINEL {
            builder.commit_from_memory(BUNDLED_HAND_MODEL)
                .map_err(|e| format!("Load bundled hand model: {}", e))?
        } else {
            builder.commit_from_file(&req.model_path)
                .map_err(|e| format!("Load model: {}", e))?
        }
    };

    let input_name = session.inputs().first().ok_or("No inputs in model")?.name().to_string();

    // Try preset defaults first. If the model rejects the shape, parse the error
    // to discover the correct dimensions, then retry. Works with ANY model.
    let mut target_size = req.preset.input_size();
    let mut is_nchw = !req.preset.is_nhwc();

    // First attempt
    let (tensor, _data) = build_input_tensor(req, target_size, is_nchw);
    let first_err = match session.run(ort::inputs![&input_name => tensor]) {
        Ok(out) => {
            return finish_inference(req, &out, target_size);
        }
        Err(e) => e.to_string(),
    };
    // Session is now consumed/dropped — we can create a new one.

    // Parse expected shape from the error and retry
    if let Some((detected_size, detected_nchw)) = parse_expected_shape(&first_err) {
        target_size = detected_size as u32;
        is_nchw = detected_nchw;
    } else {
        return Err(format!("Run: {}", first_err));
    }

    let mut session2 = {
        let mut builder = Session::builder()
            .map_err(|e| format!("Session builder: {}", e))?;
        if req.model_path == BUNDLED_HAND_SENTINEL {
            builder.commit_from_memory(BUNDLED_HAND_MODEL)
                .map_err(|e| format!("Reload bundled hand model: {}", e))?
        } else {
            builder.commit_from_file(&req.model_path)
                .map_err(|e| format!("Reload model: {}", e))?
        }
    };
    let input_name2 = session2.inputs().first().ok_or("No inputs")?.name().to_string();
    let (tensor2, _data2) = build_input_tensor(req, target_size, is_nchw);
    let outputs = session2.run(ort::inputs![&input_name2 => tensor2])
        .map_err(|e| format!("Run (retry {}x{} {}): {}",
            target_size, target_size, if is_nchw { "NCHW" } else { "NHWC" }, e))?;

    finish_inference(req, &outputs, target_size)
}

fn finish_inference(req: &MlInferenceRequest, outputs: &ort::session::SessionOutputs, _input_size: u32) -> Result<MlInferenceResult, String> {
    match req.preset {
        MlPreset::Classification => parse_classification(req, outputs),
        MlPreset::ObjectDetection => parse_object_detection(req, outputs),
        MlPreset::PoseEstimation => parse_pose_estimation(req, outputs),
        MlPreset::HandDetection => parse_hand_detection(req, outputs),
        MlPreset::FaceDetection => parse_hand_detection(req, outputs), // unused — face uses bundled pipeline
        MlPreset::Custom => parse_classification(req, outputs),
    }
}

// ── Classification ──────────────────────────────────────────────────────────

fn parse_classification(req: &MlInferenceRequest, outputs: &ort::session::SessionOutputs) -> Result<MlInferenceResult, String> {
    let output = &outputs[0];
    let (_shape, data) = output.try_extract_tensor::<f32>()
        .map_err(|e| format!("Extract: {}", e))?;
    let scores: Vec<f32> = data.to_vec();

    // Softmax
    let max_score = scores.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let exp_scores: Vec<f32> = scores.iter().map(|s| (s - max_score).exp()).collect();
    let sum: f32 = exp_scores.iter().sum();
    let probs: Vec<f32> = exp_scores.iter().map(|s| s / sum).collect();

    let labels = load_labels(&req.labels_path, probs.len());

    let mut indexed: Vec<(usize, f32)> = probs.iter().enumerate().map(|(i, &p)| (i, p)).collect();
    indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let mut result = String::new();
    let mut json_items: Vec<String> = Vec::new();
    for (i, prob) in indexed.iter().take(5) {
        if *prob < req.confidence { break; }
        let label = labels.get(*i).map(|s| s.as_str()).unwrap_or("unknown");
        result.push_str(&format!("{}: {:.1}%\n", label, prob * 100.0));
        json_items.push(format!("{{\"label\":\"{}\",\"confidence\":{:.4}}}", label, prob));
    }
    if result.is_empty() {
        result = format!("No results above {:.0}% threshold", req.confidence * 100.0);
    }
    let json = format!("[{}]", json_items.join(","));

    Ok(MlInferenceResult {
        node_id: req.node_id,
        result_text: result,
        result_json: json,
        annotated_frame: None, // Classification doesn't annotate the image
        status: "Done".into(),
    })
}

// ── Object Detection (YOLO-style) ───────────────────────────────────────────

fn parse_object_detection(req: &MlInferenceRequest, outputs: &ort::session::SessionOutputs) -> Result<MlInferenceResult, String> {
    let output = &outputs[0];
    let (shape, data) = output.try_extract_tensor::<f32>()
        .map_err(|e| format!("Extract: {}", e))?;
    let raw: Vec<f32> = data.to_vec();
    let dims: Vec<usize> = shape.iter().map(|&d| d as usize).collect();

    let labels = load_labels(&req.labels_path, 80); // COCO 80 classes default

    // YOLO output formats:
    // YOLOv5: [1, num_detections, 5+C]  (x,y,w,h, objectness, class_scores...)
    // YOLOv8: [1, 4+C, num_detections]  (x,y,w,h, class_scores...) — NO objectness
    let (num_detections, num_values) = if dims.len() == 3 {
        if dims[1] > dims[2] {
            (dims[1], dims[2]) // [1, N, 5+C] → v5 row-major
        } else {
            (dims[2], dims[1]) // [1, 4+C, N] → v8 transposed
        }
    } else if dims.len() == 2 {
        (dims[0], dims[1])
    } else {
        return Err(format!("Unexpected output shape: {:?}. Expected 2D or 3D tensor.", dims));
    };

    let transposed = dims.len() == 3 && dims[1] < dims[2];

    // Auto-detect YOLOv5 (5+C with objectness) vs YOLOv8 (4+C, no objectness).
    // If labels file is loaded, check which offset matches the label count.
    // Otherwise: if num_values matches 4+common_count, assume v8.
    let num_labels = labels.len();
    let is_v8 = if num_labels > 0 {
        (num_values as i64 - 4) == num_labels as i64  // v8: 4+C
    } else {
        // Common COCO sizes: v8 → 84 (4+80), v5 → 85 (5+80)
        num_values == 84 || (num_values >= 4 && num_values % 2 == 0)
    };

    let class_offset: usize = if is_v8 { 4 } else { 5 };
    let num_classes = num_values.saturating_sub(class_offset);

    let img_w = req.image.width as f32;
    let img_h = req.image.height as f32;
    let input_size = req.preset.input_size() as f32;

    // Detect whether model outputs normalized (0-1) or pixel-space (0-input_size) coords.
    // Sample the first 20 cx/cy values — if all are <= 1.5, treat as normalized.
    let sample_count = num_detections.min(20);
    let mut max_cx: f32 = 0.0;
    for i in 0..sample_count {
        let cx_sample = if transposed { raw[0 * num_detections + i] } else { raw[i * num_values] };
        max_cx = max_cx.max(cx_sample.abs());
    }
    let coord_scale = if max_cx <= 1.5 { input_size } else { 1.0 };

    let mut detections: Vec<Detection> = Vec::new();

    for i in 0..num_detections {
        let get = |j: usize| -> f32 {
            if transposed { raw[j * num_detections + i] } else { raw[i * num_values + j] }
        };

        let (cx, cy, w, h) = (get(0) * coord_scale, get(1) * coord_scale,
                               get(2) * coord_scale, get(3) * coord_scale);

        // v5 has objectness at index 4; v8 has no objectness (class score IS confidence)
        let obj_conf = if is_v8 { 1.0 } else { get(4) };

        // Find best class score
        let (mut best_class, mut best_score) = (0usize, 0.0f32);
        for c in 0..num_classes {
            let score = get(class_offset + c) * obj_conf;
            if score > best_score {
                best_score = score;
                best_class = c;
            }
        }

        if best_score < req.confidence { continue; }

        // Convert from model pixel space (0..input_size) to original image pixel coords
        let x1 = ((cx - w / 2.0) / input_size * img_w).max(0.0);
        let y1 = ((cy - h / 2.0) / input_size * img_h).max(0.0);
        let x2 = ((cx + w / 2.0) / input_size * img_w).min(img_w);
        let y2 = ((cy + h / 2.0) / input_size * img_h).min(img_h);

        detections.push(Detection {
            x1, y1, x2, y2,
            class: best_class,
            confidence: best_score,
        });
    }

    // NMS: simple greedy non-max suppression
    detections.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));
    let detections = nms(&detections, 0.45);

    // Build annotated image
    let mut annotated = req.image.pixels.clone();
    let mut result = String::new();
    let mut json_items: Vec<String> = Vec::new();

    let colors: &[[u8; 3]] = &[
        [255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0], [255, 0, 255],
        [0, 255, 255], [255, 128, 0], [128, 0, 255], [0, 255, 128], [255, 64, 64],
    ];

    for det in &detections {
        let label = labels.get(det.class).map(|s| s.as_str()).unwrap_or("?");
        let color = colors[det.class % colors.len()];
        result.push_str(&format!("{}: {:.1}% [{:.0},{:.0},{:.0},{:.0}]\n",
            label, det.confidence * 100.0, det.x1, det.y1, det.x2, det.y2));
        json_items.push(format!(
            "{{\"label\":\"{}\",\"confidence\":{:.4},\"bbox\":[{:.1},{:.1},{:.1},{:.1}]}}",
            label, det.confidence, det.x1, det.y1, det.x2, det.y2
        ));

        // Draw bounding box (3px thick)
        draw_rect(&mut annotated, req.image.width, req.image.height,
                   det.x1 as i32, det.y1 as i32, det.x2 as i32, det.y2 as i32,
                   color[0], color[1], color[2], 3);

        // Draw label text with background strip above the box
        let tag = format!("{} {:.0}%", label, det.confidence * 100.0);
        let char_w = 6i32;
        let char_h = 8i32;
        let text_w = tag.len() as i32 * char_w;
        let tx = det.x1 as i32;
        let ty = (det.y1 as i32 - char_h - 2).max(0);
        // Filled background rect
        draw_filled_rect(&mut annotated, req.image.width, req.image.height,
            tx, ty, tx + text_w, ty + char_h + 2,
            color[0], color[1], color[2]);
        // Text in white (or dark if color is bright)
        let brightness = color[0] as u16 + color[1] as u16 + color[2] as u16;
        let (tr, tg, tb) = if brightness > 400 { (0u8, 0u8, 0u8) } else { (255u8, 255u8, 255u8) };
        draw_text(&mut annotated, req.image.width, req.image.height,
            tx + 1, ty + 1, &tag, tr, tg, tb);
    }

    if result.is_empty() {
        result = format!("No detections above {:.0}%", req.confidence * 100.0);
    }
    let json = format!("[{}]", json_items.join(","));

    Ok(MlInferenceResult {
        node_id: req.node_id,
        result_text: result,
        result_json: json,
        annotated_frame: Some(Arc::new(ImageData {
            width: req.image.width,
            height: req.image.height,
            pixels: annotated,
        })),
        status: format!("Done ({} detections)", detections.len()),
    })
}

// ── Pose Estimation ─────────────────────────────────────────────────────────

fn parse_pose_estimation(req: &MlInferenceRequest, outputs: &ort::session::SessionOutputs) -> Result<MlInferenceResult, String> {
    let img_w = req.image.width as f32;
    let img_h = req.image.height as f32;

    // Collect all output tensors — pose models often have multiple outputs
    // (detections, keypoints, scores, etc.)
    let mut all_outputs: Vec<(Vec<usize>, Vec<f32>)> = Vec::new();
    let mut output_info = String::new();
    for (idx, output) in outputs.iter().enumerate() {
        match output.1.try_extract_tensor::<f32>() {
            Ok((shape, data)) => {
                let dims: Vec<usize> = shape.iter().map(|&d| d as usize).collect();
                let raw: Vec<f32> = data.to_vec();
                output_info.push_str(&format!("out[{}]: {:?} ({} vals)\n", idx, dims, raw.len()));
                all_outputs.push((dims, raw));
            }
            Err(_) => {} // Skip non-float outputs
        }
    }

    // Try to find keypoints in the outputs.
    // Strategy: look for the output with the most values that could be keypoints.
    // Common formats:
    //   MoveNet:   [1, 1, 17, 3] → 17 keypoints (y, x, conf) in 0-1
    //   MediaPipe: [1, 195]      → 39 keypoints × 5 values (flattened)
    //   YOLO-Pose: [1, 56, N]    → transposed, 4+1+17*3 per detection
    //   Generic:   [1, K, 3]     → K keypoints (x, y, conf)

    let mut keypoints: Vec<(f32, f32, f32)> = Vec::new();

    // Find the output tensor that looks most like keypoints
    for (dims, raw) in &all_outputs {
        let total = raw.len();
        if total < 4 { continue; }

        let (num_kp, vpk, flat_offset) = if dims.len() == 4 && dims[2] > 0 && dims[3] >= 2 {
            // [1, 1, K, V] e.g. MoveNet
            (dims[2], dims[3], 0usize)
        } else if dims.len() == 3 && dims[1] > 0 && dims[2] >= 2 && dims[2] <= 5 {
            // [1, K, V]
            (dims[1], dims[2], 0)
        } else if dims.len() == 2 && dims[1] > 6 {
            // [1, N] flat — try groups of 5 (MediaPipe: x,y,z,vis,presence) or 3
            let vpk = if dims[1] % 5 == 0 { 5 } else if dims[1] % 3 == 0 { 3 } else { 3 };
            (dims[1] / vpk, vpk, 0)
        } else if dims.len() == 1 && total > 6 {
            let vpk = if total % 5 == 0 { 5 } else { 3 };
            (total / vpk, vpk, 0)
        } else {
            continue;
        };

        if num_kp < 4 || num_kp > 200 { continue; } // sanity check

        for k in 0..num_kp {
            let off = flat_offset + k * vpk;
            if off + 1 >= raw.len() { break; }
            let (mut x, mut y) = (raw[off], raw[off + 1]);
            let conf = if vpk >= 4 && off + 3 < raw.len() {
                raw[off + 3] // visibility for MediaPipe (index 3)
            } else if vpk >= 3 && off + 2 < raw.len() {
                raw[off + 2] // standard confidence
            } else {
                1.0
            };

            // Scale normalized coords to image dimensions
            if x >= -0.5 && x <= 1.5 && y >= -0.5 && y <= 1.5 {
                x *= img_w;
                y *= img_h;
            }

            keypoints.push((x, y, conf));
        }

        if !keypoints.is_empty() { break; } // Use first matching output
    }

    // Standard keypoint names (COCO 17-point format)
    let coco_names = [
        "nose", "left_eye", "right_eye", "left_ear", "right_ear",
        "left_shoulder", "right_shoulder", "left_elbow", "right_elbow",
        "left_wrist", "right_wrist", "left_hip", "right_hip",
        "left_knee", "right_knee", "left_ankle", "right_ankle",
    ];
    let skeleton: &[(usize, usize)] = &[
        (0, 1), (0, 2), (1, 3), (2, 4),     // head
        (5, 6), (5, 7), (7, 9), (6, 8), (8, 10), // arms
        (5, 11), (6, 12), (11, 12),          // torso
        (11, 13), (13, 15), (12, 14), (14, 16), // legs
    ];

    let mut result = String::new();
    let mut json_items: Vec<String> = Vec::new();

    for (k, &(x, y, conf)) in keypoints.iter().enumerate() {
        if conf >= req.confidence {
            let name = if k < coco_names.len() { coco_names[k] } else { &format!("kp_{}", k) };
            result.push_str(&format!("{}: ({:.0}, {:.0}) {:.0}%\n", name, x, y, conf * 100.0));
            json_items.push(format!(
                "{{\"name\":\"{}\",\"x\":{:.1},\"y\":{:.1},\"confidence\":{:.4}}}",
                name, x, y, conf
            ));
        }
    }

    // Draw annotated image
    let mut annotated = req.image.pixels.clone();

    // Draw skeleton lines
    for &(a, b) in skeleton {
        if a < keypoints.len() && b < keypoints.len() {
            let (ax, ay, ac) = keypoints[a];
            let (bx, by, bc) = keypoints[b];
            if ac >= req.confidence && bc >= req.confidence {
                draw_line(&mut annotated, req.image.width, req.image.height,
                          ax as i32, ay as i32, bx as i32, by as i32,
                          0, 255, 128, 2);
            }
        }
    }

    // Draw keypoint circles
    for (x, y, conf) in &keypoints {
        if *conf >= req.confidence {
            draw_circle(&mut annotated, req.image.width, req.image.height,
                        *x as i32, *y as i32, 4,
                        255, 80, 80);
        }
    }

    if result.is_empty() {
        result = format!("No keypoints above {:.0}%\nOutputs: {}", req.confidence * 100.0, output_info.trim());
    }
    let json = format!("[{}]", json_items.join(","));

    let visible_count = keypoints.iter().filter(|(_, _, c)| *c >= req.confidence).count();
    Ok(MlInferenceResult {
        node_id: req.node_id,
        result_text: result,
        result_json: json,
        annotated_frame: Some(Arc::new(ImageData {
            width: req.image.width,
            height: req.image.height,
            pixels: annotated,
        })),
        status: format!("Done ({}/{} keypoints)", visible_count, keypoints.len()),
    })
}

// ── Full MediaPipe Hand Tracking (2-stage: Palm Detect → 21-point Landmark) ──
//
// Stage 1: Palm detector (256×256 NCHW, SSD)
//   → bounding box of the palm region
//
// Stage 2: Hand landmark model (224×224 NHWC)
//   → 21 keypoints × (x, y, z) normalized to the crop rect
//
// The 21 MediaPipe hand keypoints (HAND_LANDMARK_NAMES order):
//   0  wrist
//   1  thumb_cmc   2  thumb_mcp   3  thumb_ip    4  thumb_tip
//   5  index_mcp   6  index_pip   7  index_dip   8  index_tip
//   9  middle_mcp  10 middle_pip  11 middle_dip  12 middle_tip
//   13 ring_mcp    14 ring_pip    15 ring_dip    16 ring_tip
//   17 pinky_mcp   18 pinky_pip   19 pinky_dip   20 pinky_tip

const HAND_LANDMARK_NAMES: [&str; 21] = [
    "wrist",
    "thumb_cmc", "thumb_mcp", "thumb_ip", "thumb_tip",
    "index_mcp", "index_pip", "index_dip", "index_tip",
    "middle_mcp", "middle_pip", "middle_dip", "middle_tip",
    "ring_mcp", "ring_pip", "ring_dip", "ring_tip",
    "pinky_mcp", "pinky_pip", "pinky_dip", "pinky_tip",
];

// Finger skeleton: pairs of keypoint indices to draw lines between
const HAND_SKELETON: [(usize, usize); 24] = [
    // Thumb
    (0,1),(1,2),(2,3),(3,4),
    // Index
    (0,5),(5,6),(6,7),(7,8),
    // Middle
    (0,9),(9,10),(10,11),(11,12),
    // Ring
    (0,13),(13,14),(14,15),(15,16),
    // Pinky
    (0,17),(17,18),(18,19),(19,20),
    // Palm knuckle line
    (5,9),(9,13),(13,17),
    // Extra palm
    (0,17),
];

/// Crop an RGBA image to the given pixel rect and resize to new_w × new_h.
fn crop_and_resize(src: &crate::graph::ImageData, x1: u32, y1: u32, x2: u32, y2: u32, new_w: u32, new_h: u32) -> Vec<u8> {
    let cx = x2 - x1;
    let cy = y2 - y1;
    let mut out = vec![0u8; (new_w * new_h * 4) as usize];
    let x_ratio = cx as f32 / new_w as f32;
    let y_ratio = cy as f32 / new_h as f32;
    for dy in 0..new_h {
        for dx in 0..new_w {
            let sx = (dx as f32 * x_ratio) as u32 + x1;
            let sy = (dy as f32 * y_ratio) as u32 + y1;
            let sx = sx.min(src.width - 1);
            let sy = sy.min(src.height - 1);
            let si = ((sy * src.width + sx) * 4) as usize;
            let di = ((dy * new_w + dx) * 4) as usize;
            if si + 3 < src.pixels.len() && di + 3 < out.len() {
                out[di]   = src.pixels[si];
                out[di+1] = src.pixels[si+1];
                out[di+2] = src.pixels[si+2];
                out[di+3] = 255;
            }
        }
    }
    out
}

/// Full 2-stage MediaPipe hand tracking inference.
/// Stage 1 runs the palm detector; Stage 2 runs hand landmark on the crop.
pub fn run_hand_tracking(req: &MlInferenceRequest) -> MlInferenceResult {
    match run_hand_tracking_inner(req) {
        Ok(r) => r,
        Err(e) => MlInferenceResult {
            node_id: req.node_id,
            result_text: String::new(),
            result_json: "[]".into(),
            annotated_frame: Some(req.image.clone()),
            status: format!("Error: {}", e),
        },
    }
}

fn run_hand_tracking_inner(req: &MlInferenceRequest) -> Result<MlInferenceResult, String> {
    use ort::session::Session;
    use std::sync::Arc;

    let img_w = req.image.width as f32;
    let img_h = req.image.height as f32;

    // ────────────────────────────────────────────────────────────────────────
    // Stage 1 — Palm Detection (256×256 NCHW) — find up to 2 hands with NMS
    // ────────────────────────────────────────────────────────────────────────
    let palm_sz = 256u32;
    let palm_sz_f = palm_sz as f32;

    let mut palm_sess = Session::builder()
        .map_err(|e| format!("Session builder: {}", e))?
        .commit_from_memory(BUNDLED_HAND_MODEL)
        .map_err(|e| format!("Load palm model: {}", e))?;

    let palm_resized = resize_image(&req.image, palm_sz, palm_sz);
    let ts = palm_sz as usize;
    let mut palm_data = vec![0.0f32; 3 * ts * ts];
    for y in 0..ts {
        for x in 0..ts {
            let si = (y * ts + x) * 4;
            palm_data[0 * ts * ts + y * ts + x] = palm_resized[si]   as f32 / 127.5 - 1.0;
            palm_data[1 * ts * ts + y * ts + x] = palm_resized[si+1] as f32 / 127.5 - 1.0;
            palm_data[2 * ts * ts + y * ts + x] = palm_resized[si+2] as f32 / 127.5 - 1.0;
        }
    }
    let palm_tensor = ort::value::Tensor::from_array(
        ([1usize, 3, ts, ts], palm_data.into_boxed_slice())
    ).map_err(|e| format!("Palm tensor: {}", e))?;

    let palm_input_name = palm_sess.inputs().first().ok_or("No palm inputs")?.name().to_string();
    let palm_outputs = palm_sess.run(ort::inputs![&palm_input_name => palm_tensor])
        .map_err(|e| format!("Palm run: {}", e))?;

    let mut tensors: Vec<(Vec<usize>, Vec<f32>)> = Vec::new();
    for output in palm_outputs.iter() {
        if let Ok((shape, data)) = output.1.try_extract_tensor::<f32>() {
            tensors.push((shape.iter().map(|&d| d as usize).collect(), data.to_vec()));
        }
    }
    let boxes_t = tensors.iter().find(|(d,_)| d.len()==3 && d[2]==18);
    let scores_t = tensors.iter().find(|(d,_)| d.len()==3 && d[2]==1)
        .or_else(|| tensors.iter().find(|(d,_)| d.len()==2 && d[1]==1));
    let (boxes, scores) = match (boxes_t, scores_t) {
        (Some(b), Some(s)) => (b, s),
        _ => return Err("Palm detector output format unrecognized".into()),
    };

    let num_anchors = boxes.0[1].min(scores.0[if scores.0.len()>=2 { 1 } else { 0 }]);
    let anchors = generate_mediapipe_anchors(palm_sz);

    // Collect all detections above threshold, sort by score descending
    let mut candidates: Vec<(f32, usize)> = (0..num_anchors)
        .map(|i| (sigmoid(scores.1[i]), i))
        .filter(|(s, _)| *s >= req.confidence)
        .collect();
    candidates.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    if candidates.is_empty() {
        let best = (0..num_anchors).map(|i| sigmoid(scores.1[i])).fold(0.0f32, f32::max);
        return Ok(MlInferenceResult {
            node_id: req.node_id,
            result_text: format!("No hands ({:.1}%)", best * 100.0),
            result_json: "[]".into(),
            annotated_frame: Some(req.image.clone()),
            status: format!("No hands ({:.1}%)", best * 100.0),
        });
    }

    // NMS — select up to 2 non-overlapping detections
    // Uses BOTH IoU overlap AND center-distance suppression because the SSD
    // model emits 2 anchors per cell at different scales: a tight and a loose
    // box on the same hand can have IoU < 0.3 but their centers are the same.
    let decode_box = |idx: usize| -> (f32, f32, f32, f32) {
        let b = idx * 18;
        let (ax, ay) = anchors.get(idx).copied().unwrap_or((0.5, 0.5));
        let cy = (ay + boxes.1[b]   / palm_sz_f) * img_h;
        let cx = (ax + boxes.1[b+1] / palm_sz_f) * img_w;
        let h  = (boxes.1[b+2].abs() / palm_sz_f * img_h).max(1.0);
        let w  = (boxes.1[b+3].abs() / palm_sz_f * img_w).max(1.0);
        (cx - w/2.0, cy - h/2.0, cx + w/2.0, cy + h/2.0)
    };
    let box_center = |(x1,y1,x2,y2): (f32,f32,f32,f32)| -> (f32,f32) {
        ((x1+x2)/2.0, (y1+y2)/2.0)
    };
    let box_iou = |(ax1,ay1,ax2,ay2): (f32,f32,f32,f32), (bx1,by1,bx2,by2): (f32,f32,f32,f32)| -> f32 {
        let ix1 = ax1.max(bx1); let iy1 = ay1.max(by1);
        let ix2 = ax2.min(bx2); let iy2 = ay2.min(by2);
        let inter = (ix2-ix1).max(0.0) * (iy2-iy1).max(0.0);
        let a = (ax2-ax1)*(ay2-ay1); let b = (bx2-bx1)*(by2-by1);
        inter / (a + b - inter + 1e-6)
    };
    // Two hands must be at least 25% of the shorter image dimension apart
    let min_dim = img_w.min(img_h);
    let center_dist_threshold = min_dim * 0.25;

    let mut selected: Vec<(f32, usize)> = Vec::new();
    for (score, idx) in &candidates {
        let box_a = decode_box(*idx);
        let (ca_x, ca_y) = box_center(box_a);
        let suppressed = selected.iter().any(|(_, sel)| {
            let box_b = decode_box(*sel);
            // Suppress if either heavily overlapping OR centers too close
            if box_iou(box_a, box_b) > 0.3 { return true; }
            let (cb_x, cb_y) = box_center(box_b);
            let dx = ca_x - cb_x; let dy = ca_y - cb_y;
            (dx*dx + dy*dy).sqrt() < center_dist_threshold
        });
        if !suppressed { selected.push((*score, *idx)); }
        if selected.len() >= 2 { break; }
    }

    // ────────────────────────────────────────────────────────────────────────
    // Stage 2 — Run Hand Landmark for each detected palm
    // ────────────────────────────────────────────────────────────────────────
    let lm_sz = 224u32;
    let lm_ts = lm_sz as usize;
    let lm_sz_f = lm_sz as f32;

    // Load landmark session once, reuse for both hands
    let mut lm_sess = Session::builder()
        .map_err(|e| format!("Session builder: {}", e))?
        .commit_from_memory(BUNDLED_HAND_LANDMARK_MODEL)
        .map_err(|e| format!("Load landmark model: {}", e))?;
    let lm_input_name = lm_sess.inputs().first().ok_or("No landmark inputs")?.name().to_string();

    // Per-hand colour palettes for annotation
    let hand_colors: [(u8,u8,u8); 2] = [(80,200,120), (100,160,255)]; // green, blue

    let mut annotated = req.image.pixels.clone();
    let mut all_json: Vec<String> = Vec::new();
    let mut result_text = String::new();

    for (hand_idx, (hand_score, anchor_idx)) in selected.iter().enumerate() {
        let base = anchor_idx * 18;
        let (ax, ay) = anchors.get(*anchor_idx).copied().unwrap_or((0.5, 0.5));

        // Decode 7 palm keypoints for crop centering
        let mut kp_sum_x = 0.0f32; let mut kp_sum_y = 0.0f32;
        let mut kp_min_x = f32::INFINITY; let mut kp_min_y = f32::INFINITY;
        let mut kp_max_x = -f32::INFINITY; let mut kp_max_y = -f32::INFINITY;
        for k in 0..7usize {
            let ry = boxes.1[base + 4 + k * 2];
            let rx = boxes.1[base + 4 + k * 2 + 1];
            let kx = ((ax + rx / palm_sz_f) * img_w).clamp(0.0, img_w);
            let ky = ((ay + ry / palm_sz_f) * img_h).clamp(0.0, img_h);
            kp_sum_x += kx; kp_sum_y += ky;
            kp_min_x = kp_min_x.min(kx); kp_min_y = kp_min_y.min(ky);
            kp_max_x = kp_max_x.max(kx); kp_max_y = kp_max_y.max(ky);
        }
        let kp_cx = kp_sum_x / 7.0;
        let kp_cy = kp_sum_y / 7.0;
        let raw_bw = (boxes.1[base+3].abs() / palm_sz_f * img_w).max(30.0);
        let raw_bh = (boxes.1[base+2].abs() / palm_sz_f * img_h).max(40.0);

        let half_w = (raw_bw * 0.6).max((kp_max_x - kp_min_x) * 0.8).max(40.0);
        let half_h = (raw_bh * 1.2).max((kp_max_y - kp_min_y) * 1.5).max(60.0);
        let half = half_w.max(half_h);
        let crop_cx = kp_cx.max((ax + boxes.1[base+1] / palm_sz_f) * img_w);
        let crop_cy = kp_cy.min((ay + boxes.1[base]   / palm_sz_f) * img_h) - half * 0.15;

        let cx1 = (crop_cx - half * 1.1).max(0.0) as u32;
        let cy1 = (crop_cy - half * 1.2).max(0.0) as u32;
        let cx2 = (crop_cx + half * 1.1).min(img_w) as u32;
        let cy2 = (crop_cy + half * 0.8).min(img_h) as u32;
        if cx2 <= cx1 || cy2 <= cy1 { continue; }

        // Build landmark input tensor
        let cropped = crop_and_resize(&req.image, cx1, cy1, cx2, cy2, lm_sz, lm_sz);
        let mut lm_data = vec![0.0f32; lm_ts * lm_ts * 3];
        for y in 0..lm_ts {
            for x in 0..lm_ts {
                let si = (y * lm_ts + x) * 4;
                lm_data[(y * lm_ts + x) * 3 + 0] = cropped[si]   as f32 / 255.0;
                lm_data[(y * lm_ts + x) * 3 + 1] = cropped[si+1] as f32 / 255.0;
                lm_data[(y * lm_ts + x) * 3 + 2] = cropped[si+2] as f32 / 255.0;
            }
        }
        let lm_tensor = ort::value::Tensor::from_array(
            ([1usize, lm_ts, lm_ts, 3], lm_data.into_boxed_slice())
        ).map_err(|e| format!("Landmark tensor: {}", e))?;

        let lm_outputs = lm_sess.run(ort::inputs![&lm_input_name => lm_tensor])
            .map_err(|e| format!("Landmark run: {}", e))?;

        let lm_out = &lm_outputs["Identity"];
        let (_, lm_vals) = lm_out.try_extract_tensor::<f32>()
            .map_err(|e| format!("Landmark extract: {}", e))?;
        let lm: Vec<f32> = lm_vals.to_vec();
        if lm.len() < 63 { continue; }

        let crop_w = (cx2 - cx1) as f32;
        let crop_h = (cy2 - cy1) as f32;
        let max_lm = lm.iter().cloned().fold(0.0f32, f32::max);
        let lm_scale = if max_lm <= 1.5 { 1.0 } else { 1.0 / lm_sz_f };

        let mut keypoints: Vec<(f32, f32, &'static str)> = Vec::with_capacity(21);
        for i in 0..21usize {
            let kx = (lm[i*3]     * lm_scale * crop_w + cx1 as f32).clamp(0.0, img_w);
            let ky = (lm[i*3 + 1] * lm_scale * crop_h + cy1 as f32).clamp(0.0, img_h);
            keypoints.push((kx, ky, HAND_LANDMARK_NAMES[i]));
        }

        // Bbox from 21 landmarks
        let lm_min_x = keypoints.iter().map(|(x,_,_)| *x).fold(f32::INFINITY,  f32::min);
        let lm_min_y = keypoints.iter().map(|(_,y,_)| *y).fold(f32::INFINITY,  f32::min);
        let lm_max_x = keypoints.iter().map(|(x,_,_)| *x).fold(-f32::INFINITY, f32::max);
        let lm_max_y = keypoints.iter().map(|(_,y,_)| *y).fold(-f32::INFINITY, f32::max);
        let pad = ((lm_max_x - lm_min_x).max(lm_max_y - lm_min_y) * 0.08).max(8.0);
        let bx1 = (lm_min_x - pad).max(0.0);
        let by1 = (lm_min_y - pad).max(0.0);
        let bx2 = (lm_max_x + pad).min(img_w);
        let by2 = (lm_max_y + pad).min(img_h);

        // Annotate: skeleton
        let (lr, lg, lb) = hand_colors[hand_idx % 2];
        for &(a, b) in &HAND_SKELETON {
            if a < keypoints.len() && b < keypoints.len() {
                let (ax, ay, _) = keypoints[a];
                let (bx, by, _) = keypoints[b];
                draw_line(&mut annotated, req.image.width, req.image.height,
                    ax as i32, ay as i32, bx as i32, by as i32, lr, lg, lb, 2);
            }
        }
        let fingertips = [4usize, 8, 12, 16, 20];
        for (i, (kx, ky, _)) in keypoints.iter().enumerate() {
            let is_tip = fingertips.contains(&i);
            let (outer_r, inner_r, r, g, b) = if is_tip {
                (10, 6, 80, 255, 120)
            } else if i == 0 {
                (10, 6, 255, 200, 50)
            } else {
                (7, 4, lr, lg, lb)
            };
            draw_circle(&mut annotated, req.image.width, req.image.height, *kx as i32, *ky as i32, outer_r, 20, 20, 20);
            draw_circle(&mut annotated, req.image.width, req.image.height, *kx as i32, *ky as i32, inner_r, r, g, b);
            draw_circle(&mut annotated, req.image.width, req.image.height, *kx as i32, *ky as i32, 2, 255, 255, 255);
        }
        // Bbox with hand-coloured border
        let (bx1i, by1i, bx2i, by2i) = (bx1 as i32, by1 as i32, bx2 as i32, by2 as i32);
        for t in 0..2i32 {
            draw_line(&mut annotated, req.image.width, req.image.height, bx1i, by1i+t, bx2i, by1i+t, lr, lg, lb, 1);
            draw_line(&mut annotated, req.image.width, req.image.height, bx2i-t, by1i, bx2i-t, by2i, lr, lg, lb, 1);
            draw_line(&mut annotated, req.image.width, req.image.height, bx2i, by2i-t, bx1i, by2i-t, lr, lg, lb, 1);
            draw_line(&mut annotated, req.image.width, req.image.height, bx1i+t, by2i, bx1i+t, by1i, lr, lg, lb, 1);
        }

        // JSON — tag each item with hand index
        let h = hand_idx;
        result_text.push_str(&format!("Hand {} ({:.1}%)\n", h+1, hand_score * 100.0));
        all_json.push(format!(
            r#"{{"type":"hand_bbox","hand":{h},"x1":{:.1},"y1":{:.1},"x2":{:.1},"y2":{:.1},"confidence":{:.4}}}"#,
            bx1, by1, bx2, by2, hand_score
        ));
        for (kx, ky, name) in &keypoints {
            all_json.push(format!(
                r#"{{"type":"hand_landmark","hand":{h},"name":"{}","x":{:.1},"y":{:.1}}}"#,
                name, kx, ky
            ));
        }
    }

    let hand_count = selected.len();
    let best_score = selected.first().map(|(s,_)| *s).unwrap_or(0.0);
    let status = match hand_count {
        0 => "No hands".to_string(),
        1 => format!("1 hand ({:.1}%)", best_score * 100.0),
        n => format!("{} hands ({:.1}%)", n, best_score * 100.0),
    };

    Ok(MlInferenceResult {
        node_id: req.node_id,
        result_text,
        result_json: format!("[{}]", all_json.join(",")),
        annotated_frame: Some(Arc::new(crate::graph::ImageData {
            width: req.image.width,
            height: req.image.height,
            pixels: annotated,
        })),
        status,
    })
}

// ── Hand Detection (MediaPipe SSD palm detector, single stage) ───────────────
//
// MediaPipe palm detector outputs:
//   out[0]: [1, N, 18]  — per-anchor: [cy, cx, h, w, kp0y, kp0x, ..., kp6y, kp6x]
//   out[1]: [1, N, 1]   — per-anchor confidence logit (apply sigmoid)
//
// Used by the legacy ML Model node preset. Hand Tracking uses run_hand_tracking().

fn sigmoid(x: f32) -> f32 { 1.0 / (1.0 + (-x).exp()) }

/// Generate MediaPipe SSD anchor centers in [0,1] normalized space.
/// For input_sz=256, strides [8,16,32,32,32], 2 anchors/cell → exactly 2944 anchors.
/// This matches MediaPipe's SsdAnchorsCalculator configuration for the palm detector.
pub fn generate_mediapipe_anchors(input_sz: u32) -> Vec<(f32, f32)> {
    let strides = [8u32, 16, 32, 32, 32];
    let mut anchors = Vec::with_capacity(2944);
    for &stride in &strides {
        let grid = (input_sz / stride) as usize;
        for row in 0..grid {
            for col in 0..grid {
                let cx = (col as f32 + 0.5) / grid as f32;
                let cy = (row as f32 + 0.5) / grid as f32;
                // 2 anchors per cell (different aspect ratios, same center)
                anchors.push((cx, cy));
                anchors.push((cx, cy));
            }
        }
    }
    anchors
}

fn parse_hand_detection(req: &MlInferenceRequest, outputs: &ort::session::SessionOutputs) -> Result<MlInferenceResult, String> {
    let img_w = req.image.width as f32;
    let img_h = req.image.height as f32;
    // Collect raw tensors
    let mut tensors: Vec<(Vec<usize>, Vec<f32>)> = Vec::new();
    let mut output_info = String::new();
    for (idx, output) in outputs.iter().enumerate() {
        if let Ok((shape, data)) = output.1.try_extract_tensor::<f32>() {
            let dims: Vec<usize> = shape.iter().map(|&d| d as usize).collect();
            let vals: Vec<f32> = data.to_vec();
            output_info.push_str(&format!("out[{}]: {:?} ({} vals)\n", idx, dims, vals.len()));
            tensors.push((dims, vals));
        }
    }

    // Find boxes tensor [1, N, 18] and scores tensor [1, N, 1]
    let mut boxes_data: Option<(usize, &[f32])> = None; // (num_anchors, data)
    let mut scores_data: Option<(usize, &[f32])> = None;

    for (dims, vals) in &tensors {
        if dims.len() == 3 && dims[2] == 18 {
            boxes_data = Some((dims[1], vals.as_slice()));
        } else if dims.len() == 3 && dims[2] == 1 {
            scores_data = Some((dims[1], vals.as_slice()));
        } else if dims.len() == 2 && dims[1] == 1 {
            // [N, 1] — treat as scores
            scores_data = Some((dims[0], vals.as_slice()));
        }
    }

    // Fallback: if we couldn't separate, try treating largest tensor as boxes
    let (boxes, scores) = match (boxes_data, scores_data) {
        (Some(b), Some(s)) => (b, s),
        _ => {
            // Generic fallback — show raw output info
            return Ok(MlInferenceResult {
                node_id: req.node_id,
                result_text: format!("Hand Detection: unexpected output format\n{}", output_info.trim()),
                result_json: "[]".into(),
                annotated_frame: None,
                status: "No detections (format unknown)".into(),
            });
        }
    };

    let num_anchors = boxes.0.min(scores.0);

    // Find best anchor by sigmoid(score)
    let mut best_score = -f32::INFINITY;
    let mut best_idx = 0usize;
    for i in 0..num_anchors {
        let score = sigmoid(scores.1[i]);
        if score > best_score {
            best_score = score;
            best_idx = i;
        }
    }

    let palm_kp_names = ["wrist", "index_mcp", "middle_mcp", "ring_mcp", "pinky_mcp", "index_tip", "thumb_tip"];

    if best_score < req.confidence {
        return Ok(MlInferenceResult {
            node_id: req.node_id,
            result_text: format!("No hand detected (best score {:.1}%, threshold {:.0}%)\nOutputs: {}", best_score * 100.0, req.confidence * 100.0, output_info.trim()),
            result_json: "[]".into(),
            annotated_frame: None,
            status: format!("No hand ({:.1}%)", best_score * 100.0),
        });
    }

    // ── Decode keypoints using SSD anchor grid ───────────────────────────────
    // MediaPipe SSD output format per anchor (18 values):
    //   [0] cy_offset  [1] cx_offset  [2] h_offset  [3] w_offset
    //   [4] kp0_y  [5] kp0_x  [6] kp1_y  [7] kp1_x  ... (y BEFORE x each pair)
    //
    // All values are anchor-relative offsets in input pixel space.
    // Decoded: coord_normalized = anchor_center + raw_offset / input_size
    // Pixel:   coord_px = coord_normalized * image_dimension
    //
    // Anchor grid: strides [8,16,32,32,32] × 2 anchors/cell → 2944 anchors for 256×256 input.

    let base = best_idx * 18;
    let input_size = req.preset.input_size();
    let input_sz_f = input_size as f32;

    // Generate anchor grid and look up the winning anchor center
    let anchors = generate_mediapipe_anchors(input_size);
    let (ax, ay) = anchors.get(best_idx).copied().unwrap_or((0.5, 0.5));

    // Decode 7 palm keypoints — MediaPipe stores (y, x) per pair
    let mut keypoints: Vec<(f32, f32, &'static str)> = Vec::new();
    for k in 0..7usize {
        let raw_kp_y = boxes.1[base + 4 + k * 2];       // y offset comes first
        let raw_kp_x = boxes.1[base + 4 + k * 2 + 1];   // x offset comes second
        let kp_x = ((ax + raw_kp_x / input_sz_f) * img_w).clamp(0.0, img_w);
        let kp_y = ((ay + raw_kp_y / input_sz_f) * img_h).clamp(0.0, img_h);
        keypoints.push((kp_x, kp_y, palm_kp_names[k]));
    }

    // Decode bbox center — [cy, cx, h, w] order
    let raw_cy = boxes.1[base];
    let raw_cx = boxes.1[base + 1];
    let raw_h  = boxes.1[base + 2];
    let raw_w  = boxes.1[base + 3];
    let cx = ((ax + raw_cx / input_sz_f) * img_w).clamp(0.0, img_w);
    let cy = ((ay + raw_cy / input_sz_f) * img_h).clamp(0.0, img_h);
    let bw = (raw_w / input_sz_f * img_w).abs();
    let bh = (raw_h / input_sz_f * img_h).abs();

    // Bounding box: derive from keypoint extents (covers full detected region)
    // and cross-check against decoded center — use whichever is larger.
    let kp_min_x = keypoints.iter().map(|(x, _, _)| *x).fold(f32::INFINITY, f32::min);
    let kp_min_y = keypoints.iter().map(|(_, y, _)| *y).fold(f32::INFINITY, f32::min);
    let kp_max_x = keypoints.iter().map(|(x, _, _)| *x).fold(-f32::INFINITY, f32::max);
    let kp_max_y = keypoints.iter().map(|(_, y, _)| *y).fold(-f32::INFINITY, f32::max);
    // Padding = 25% of the keypoint span, at least 30px
    let pad = (((kp_max_x - kp_min_x).max(kp_max_y - kp_min_y)) * 0.25).max(30.0);
    let kp_x1 = (kp_min_x - pad).max(0.0);
    let kp_y1 = (kp_min_y - pad).max(0.0);
    let kp_x2 = (kp_max_x + pad).min(img_w);
    let kp_y2 = (kp_max_y + pad).min(img_h);
    // Merge with decoded center box (union — whichever covers more area)
    let (x1, y1, x2, y2) = if bw > 10.0 && bh > 10.0 {
        let dx1 = (cx - bw / 2.0).max(0.0);
        let dy1 = (cy - bh / 2.0).max(0.0);
        let dx2 = (cx + bw / 2.0).min(img_w);
        let dy2 = (cy + bh / 2.0).min(img_h);
        (kp_x1.min(dx1), kp_y1.min(dy1), kp_x2.max(dx2), kp_y2.max(dy2))
    } else {
        (kp_x1, kp_y1, kp_x2, kp_y2)
    };

    // Build result text + JSON
    let mut result = format!(
        "Hand detected {:.1}%\nBBox: ({:.0},{:.0})→({:.0},{:.0})\n",
        best_score * 100.0, x1, y1, x2, y2
    );
    let mut json_items: Vec<String> = Vec::new();

    json_items.push(format!(
        "{{\"type\":\"hand_bbox\",\"x1\":{:.1},\"y1\":{:.1},\"x2\":{:.1},\"y2\":{:.1},\"confidence\":{:.4}}}",
        x1, y1, x2, y2, best_score
    ));

    for (kx, ky, name) in &keypoints {
        result.push_str(&format!("{}: ({:.0}, {:.0})\n", name, kx, ky));
        json_items.push(format!(
            "{{\"type\":\"palm_keypoint\",\"name\":\"{}\",\"x\":{:.1},\"y\":{:.1}}}",
            name, kx, ky
        ));
    }

    // Draw annotated image
    let mut annotated = req.image.pixels.clone();

    // Draw bounding box (bright green, thick)
    let x1i = x1 as i32; let y1i = y1 as i32;
    let x2i = x2 as i32; let y2i = y2 as i32;
    for t in 0..3i32 {
        draw_line(&mut annotated, req.image.width, req.image.height, x1i, y1i+t, x2i, y1i+t, 0, 230, 80, 1);
        draw_line(&mut annotated, req.image.width, req.image.height, x2i-t, y1i, x2i-t, y2i, 0, 230, 80, 1);
        draw_line(&mut annotated, req.image.width, req.image.height, x2i, y2i-t, x1i, y2i-t, 0, 230, 80, 1);
        draw_line(&mut annotated, req.image.width, req.image.height, x1i+t, y2i, x1i+t, y1i, 0, 230, 80, 1);
    }

    // Draw palm keypoints (larger bright circles for visibility)
    for (kx, ky, _) in &keypoints {
        // Outer ring (dark border for contrast)
        draw_circle(&mut annotated, req.image.width, req.image.height,
                    *kx as i32, *ky as i32, 10, 30, 30, 30);
        // Filled orange circle
        draw_circle(&mut annotated, req.image.width, req.image.height,
                    *kx as i32, *ky as i32, 8, 255, 160, 0);
        // Bright white center dot
        draw_circle(&mut annotated, req.image.width, req.image.height,
                    *kx as i32, *ky as i32, 3, 255, 255, 255);
    }

    // Draw skeleton lines between keypoints (wrist→each mcp, and mcp→tip)
    let skeleton = [(0,1),(0,2),(0,3),(0,4),(0,5),(1,5),(2,5),(4,6)];
    for (a, b) in skeleton {
        if a < keypoints.len() && b < keypoints.len() {
            let (ax, ay, _) = keypoints[a];
            let (bx, by, _) = keypoints[b];
            draw_line(&mut annotated, req.image.width, req.image.height,
                      ax as i32, ay as i32, bx as i32, by as i32,
                      255, 200, 50, 1);
        }
    }

    let json = format!("[{}]", json_items.join(","));

    Ok(MlInferenceResult {
        node_id: req.node_id,
        result_text: result,
        result_json: json,
        annotated_frame: Some(Arc::new(ImageData {
            width: req.image.width,
            height: req.image.height,
            pixels: annotated,
        })),
        status: format!("Hand detected ({:.1}%)", best_score * 100.0),
    })
}

// ── Helpers ─────────────────────────────────────────────────────────────────

struct Detection {
    x1: f32, y1: f32, x2: f32, y2: f32,
    class: usize,
    confidence: f32,
}

fn load_labels(path: &str, fallback_count: usize) -> Vec<String> {
    if !path.is_empty() {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|l| l.trim().to_string())
            .collect()
    } else {
        (0..fallback_count).map(|i| format!("class_{}", i)).collect()
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// Face Tracking — 2-stage MediaPipe pipeline (BlazeFace → Face Landmark 468)
// ══════════════════════════════════════════════════════════════════════════════

/// Generate SSD anchors for BlazeFace short-range face detector (128×128).
/// Strides [8, 16], anchors per cell [2, 6] → 896 total.
fn generate_blazeface_anchors(input_sz: u32) -> Vec<(f32, f32)> {
    let configs: &[(u32, usize)] = &[(8, 2), (16, 6)];
    let mut anchors = Vec::with_capacity(896);
    for &(stride, num) in configs {
        let grid = (input_sz / stride) as usize;
        for row in 0..grid {
            for col in 0..grid {
                let cx = (col as f32 + 0.5) / grid as f32;
                let cy = (row as f32 + 0.5) / grid as f32;
                for _ in 0..num {
                    anchors.push((cx, cy));
                }
            }
        }
    }
    anchors
}

// Named face landmarks — map MediaPipe indices to human-readable names.
const FACE_NAMED_LANDMARKS: [(usize, &str); 15] = [
    (1,   "nose_tip"),
    (10,  "forehead"),
    (33,  "left_eye_inner"),
    (133, "left_eye_outer"),
    (362, "right_eye_inner"),
    (263, "right_eye_outer"),
    (61,  "mouth_left"),
    (291, "mouth_right"),
    (0,   "upper_lip"),
    (17,  "lower_lip"),
    (152, "chin"),
    (234, "left_cheek"),
    (454, "right_cheek"),
    (127, "left_temple"),
    (356, "right_temple"),
];

// Face mesh contour connections — subsets of MediaPipe face mesh topology.
// These draw the face outline, eyes, eyebrows, lips, nose, and iris.
const FACE_OVAL: [(usize,usize); 36] = [
    (10,338),(338,297),(297,332),(332,284),(284,251),(251,389),(389,356),(356,454),
    (454,323),(323,361),(361,288),(288,397),(397,365),(365,379),(379,378),(378,400),
    (400,377),(377,152),(152,148),(148,176),(176,149),(149,150),(150,136),(136,172),
    (172,58),(58,132),(132,93),(93,234),(234,127),(127,162),(162,21),(21,54),
    (54,103),(103,67),(67,109),(109,10),
];

const FACE_LIPS_OUTER: [(usize,usize); 20] = [
    (61,146),(146,91),(91,181),(181,84),(84,17),(17,314),(314,405),(405,321),
    (321,375),(375,291),(291,409),(409,270),(270,269),(269,267),(267,0),(0,37),
    (37,39),(39,40),(40,185),(185,61),
];

const FACE_LIPS_INNER: [(usize,usize); 8] = [
    (78,95),(95,88),(88,178),(178,87),(87,14),(14,317),(317,402),(402,78),
];

const FACE_LEFT_EYE: [(usize,usize); 16] = [
    (33,7),(7,163),(163,144),(144,145),(145,153),(153,154),(154,155),(155,133),
    (33,246),(246,161),(161,160),(160,159),(159,158),(158,157),(157,173),(173,133),
];

const FACE_RIGHT_EYE: [(usize,usize); 16] = [
    (362,382),(382,381),(381,380),(380,374),(374,373),(373,390),(390,249),(249,263),
    (362,398),(398,384),(384,385),(385,386),(386,387),(387,388),(388,466),(466,263),
];

const FACE_LEFT_EYEBROW: [(usize,usize); 8] = [
    (46,53),(53,52),(52,65),(65,55),(70,63),(63,105),(105,66),(66,107),
];

const FACE_RIGHT_EYEBROW: [(usize,usize); 8] = [
    (276,283),(283,282),(282,295),(295,285),(300,293),(293,334),(334,296),(296,336),
];

const FACE_NOSE_BRIDGE: [(usize,usize); 4] = [
    (168,6),(6,197),(197,195),(195,5),
];

pub fn run_face_tracking(req: &MlInferenceRequest) -> MlInferenceResult {
    match run_face_tracking_inner(req) {
        Ok(r) => r,
        Err(e) => MlInferenceResult {
            node_id: req.node_id,
            result_text: String::new(),
            result_json: "[]".into(),
            annotated_frame: Some(req.image.clone()),
            status: format!("Error: {}", e),
        },
    }
}

fn run_face_tracking_inner(req: &MlInferenceRequest) -> Result<MlInferenceResult, String> {
    use ort::session::Session;

    let img_w = req.image.width  as f32;
    let img_h = req.image.height as f32;

    // ────────────────────────────────────────────────────────────────────────
    // Stage 1 — BlazeFace short-range face detection (128×128 NHWC)
    // ────────────────────────────────────────────────────────────────────────
    let face_sz = 128u32;
    let ts = face_sz as usize;
    let face_sz_f = face_sz as f32;

    let mut face_sess = Session::builder()
        .map_err(|e| format!("Session builder: {}", e))?
        .commit_from_memory(BUNDLED_FACE_MODEL)
        .map_err(|e| format!("Load face model: {}", e))?;

    // Resize input image to 128×128
    let face_resized = resize_image(&req.image, face_sz, face_sz);

    // NHWC tensor, normalize to [-1, 1]
    let mut face_data = vec![0.0f32; ts * ts * 3];
    for y in 0..ts {
        for x in 0..ts {
            let si = (y * ts + x) * 4;
            let di = (y * ts + x) * 3;
            face_data[di]     = face_resized[si]   as f32 / 127.5 - 1.0;
            face_data[di + 1] = face_resized[si+1] as f32 / 127.5 - 1.0;
            face_data[di + 2] = face_resized[si+2] as f32 / 127.5 - 1.0;
        }
    }
    let face_tensor = ort::value::Tensor::from_array(
        ([1usize, ts, ts, 3], face_data.into_boxed_slice())
    ).map_err(|e| format!("Face tensor: {}", e))?;

    let face_input_name = face_sess.inputs().first().ok_or("No face inputs")?.name().to_string();
    let face_outputs = face_sess.run(ort::inputs![&face_input_name => face_tensor])
        .map_err(|e| format!("Face run: {}", e))?;

    // Extract output tensors
    let mut tensors: Vec<(Vec<usize>, Vec<f32>)> = Vec::new();
    for output in face_outputs.iter() {
        if let Ok((shape, data)) = output.1.try_extract_tensor::<f32>() {
            tensors.push((shape.iter().map(|&d| d as usize).collect(), data.to_vec()));
        }
    }

    let boxes_t = tensors.iter().find(|(d,_)| d.len()==3 && d[2]==16);
    let scores_t = tensors.iter().find(|(d,_)| d.len()==3 && d[2]==1)
        .or_else(|| tensors.iter().find(|(d,_)| d.len()==2 && d[1]==1));
    let (boxes, scores) = match (boxes_t, scores_t) {
        (Some(b), Some(s)) => (b, s),
        _ => return Err("Face detector output format unrecognized".into()),
    };

    let num_anchors = boxes.0[1].min(scores.0[if scores.0.len()>=2 { 1 } else { 0 }]);
    let anchors = generate_blazeface_anchors(face_sz);

    // Collect detections above threshold
    let mut candidates: Vec<(f32, usize)> = (0..num_anchors)
        .map(|i| (sigmoid(scores.1[i]), i))
        .filter(|(s, _)| *s >= req.confidence)
        .collect();
    candidates.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    if candidates.is_empty() {
        let best = (0..num_anchors).map(|i| sigmoid(scores.1[i])).fold(0.0f32, f32::max);
        return Ok(MlInferenceResult {
            node_id: req.node_id,
            result_text: format!("No faces ({:.1}%)", best * 100.0),
            result_json: "[]".into(),
            annotated_frame: Some(req.image.clone()),
            status: format!("No faces ({:.1}%)", best * 100.0),
        });
    }

    // NMS — select up to 2 non-overlapping faces
    // BlazeFace output per anchor: [cy, cx, h, w, kp0y, kp0x, ..., kp5y, kp5x] — 16 values
    let decode_box = |idx: usize| -> (f32, f32, f32, f32) {
        let b = idx * 16;
        let (ax, ay) = anchors.get(idx).copied().unwrap_or((0.5, 0.5));
        let cy = (ay + boxes.1[b]   / face_sz_f) * img_h;
        let cx = (ax + boxes.1[b+1] / face_sz_f) * img_w;
        let h  = (boxes.1[b+2].abs() / face_sz_f * img_h).max(1.0);
        let w  = (boxes.1[b+3].abs() / face_sz_f * img_w).max(1.0);
        (cx - w/2.0, cy - h/2.0, cx + w/2.0, cy + h/2.0)
    };
    let box_center = |(x1,y1,x2,y2): (f32,f32,f32,f32)| -> (f32,f32) {
        ((x1+x2)/2.0, (y1+y2)/2.0)
    };
    let box_iou = |(ax1,ay1,ax2,ay2): (f32,f32,f32,f32), (bx1,by1,bx2,by2): (f32,f32,f32,f32)| -> f32 {
        let ix1 = ax1.max(bx1); let iy1 = ay1.max(by1);
        let ix2 = ax2.min(bx2); let iy2 = ay2.min(by2);
        let inter = (ix2-ix1).max(0.0) * (iy2-iy1).max(0.0);
        let a = (ax2-ax1)*(ay2-ay1); let b = (bx2-bx1)*(by2-by1);
        inter / (a + b - inter + 1e-6)
    };
    let min_dim = img_w.min(img_h);
    let center_dist_threshold = min_dim * 0.25;

    let mut selected: Vec<(f32, usize)> = Vec::new();
    for (score, idx) in &candidates {
        let box_a = decode_box(*idx);
        let (ca_x, ca_y) = box_center(box_a);
        let suppressed = selected.iter().any(|(_, sel)| {
            let box_b = decode_box(*sel);
            if box_iou(box_a, box_b) > 0.3 { return true; }
            let (cb_x, cb_y) = box_center(box_b);
            let dx = ca_x - cb_x; let dy = ca_y - cb_y;
            (dx*dx + dy*dy).sqrt() < center_dist_threshold
        });
        if !suppressed { selected.push((*score, *idx)); }
        if selected.len() >= 1 { break; } // single face only
    }

    // ────────────────────────────────────────────────────────────────────────
    // Stage 2 — Face Landmark (192×192 NCHW) for each detected face
    // ────────────────────────────────────────────────────────────────────────
    let lm_sz = 192u32;
    let lm_ts = lm_sz as usize;
    let lm_sz_f = lm_sz as f32;

    let mut lm_sess = Session::builder()
        .map_err(|e| format!("Session builder: {}", e))?
        .commit_from_memory(BUNDLED_FACE_LANDMARK_MODEL)
        .map_err(|e| format!("Load face landmark model: {}", e))?;
    let lm_input_name = lm_sess.inputs().first().ok_or("No landmark inputs")?.name().to_string();

    // Per-face colour palettes
    let face_colors: [(u8,u8,u8); 2] = [(100,220,200), (220,120,200)]; // cyan, pink

    let mut annotated = req.image.pixels.clone();
    let mut all_json: Vec<String> = Vec::new();
    let mut result_text = String::new();

    for (face_idx, (face_score, anchor_idx)) in selected.iter().enumerate() {
        let base = anchor_idx * 16;
        let (ax, ay) = anchors.get(*anchor_idx).copied().unwrap_or((0.5, 0.5));

        // Decode 6 face keypoints for crop centering
        let mut kp_sum_x = 0.0f32; let mut kp_sum_y = 0.0f32;
        let mut kp_min_x = f32::INFINITY; let mut kp_min_y = f32::INFINITY;
        let mut kp_max_x = -f32::INFINITY; let mut kp_max_y = -f32::INFINITY;
        for k in 0..6usize {
            let ry = boxes.1[base + 4 + k * 2];
            let rx = boxes.1[base + 4 + k * 2 + 1];
            let kx = ((ax + rx / face_sz_f) * img_w).clamp(0.0, img_w);
            let ky = ((ay + ry / face_sz_f) * img_h).clamp(0.0, img_h);
            kp_sum_x += kx; kp_sum_y += ky;
            kp_min_x = kp_min_x.min(kx); kp_min_y = kp_min_y.min(ky);
            kp_max_x = kp_max_x.max(kx); kp_max_y = kp_max_y.max(ky);
        }
        let kp_cx = kp_sum_x / 6.0;
        let kp_cy = kp_sum_y / 6.0;
        let raw_bw = (boxes.1[base+3].abs() / face_sz_f * img_w).max(30.0);
        let raw_bh = (boxes.1[base+2].abs() / face_sz_f * img_h).max(30.0);

        // Face crop — square, padded generously to include full face
        let half_w = (raw_bw * 0.7).max((kp_max_x - kp_min_x) * 0.8).max(40.0);
        let half_h = (raw_bh * 0.7).max((kp_max_y - kp_min_y) * 0.8).max(40.0);
        let half = half_w.max(half_h);
        let crop_cx = kp_cx;
        let crop_cy = kp_cy;

        let cx1 = (crop_cx - half * 1.3).max(0.0) as u32;
        let cy1 = (crop_cy - half * 1.5).max(0.0) as u32;  // more space above for forehead
        let cx2 = (crop_cx + half * 1.3).min(img_w) as u32;
        let cy2 = (crop_cy + half * 1.1).min(img_h) as u32;
        if cx2 <= cx1 || cy2 <= cy1 { continue; }

        // Build landmark input tensor (NCHW for this model)
        let cropped = crop_and_resize(&req.image, cx1, cy1, cx2, cy2, lm_sz, lm_sz);
        let mut lm_data = vec![0.0f32; 3 * lm_ts * lm_ts];
        for y in 0..lm_ts {
            for x in 0..lm_ts {
                let si = (y * lm_ts + x) * 4;
                // NCHW: channel-first layout
                lm_data[0 * lm_ts * lm_ts + y * lm_ts + x] = cropped[si]   as f32 / 255.0;
                lm_data[1 * lm_ts * lm_ts + y * lm_ts + x] = cropped[si+1] as f32 / 255.0;
                lm_data[2 * lm_ts * lm_ts + y * lm_ts + x] = cropped[si+2] as f32 / 255.0;
            }
        }
        let lm_tensor = ort::value::Tensor::from_array(
            ([1usize, 3, lm_ts, lm_ts], lm_data.into_boxed_slice())
        ).map_err(|e| format!("Landmark tensor: {}", e))?;

        let lm_outputs = lm_sess.run(ort::inputs![&lm_input_name => lm_tensor])
            .map_err(|e| format!("Face landmark run: {}", e))?;

        // Output: "landmarks" [1, 468, 3] and "scores" [1]
        let lm_vals: Vec<f32> = {
            let lm_out = lm_outputs.iter()
                .find(|(name, _)| *name == "landmarks")
                .or_else(|| lm_outputs.iter().next())
                .ok_or("No landmark output")?;
            let (_, data) = lm_out.1.try_extract_tensor::<f32>()
                .map_err(|e| format!("Landmark extract: {}", e))?;
            data.to_vec()
        };

        let num_landmarks = lm_vals.len() / 3;
        if num_landmarks < 468 { continue; }

        let crop_w = (cx2 - cx1) as f32;
        let crop_h = (cy2 - cy1) as f32;

        // Determine coordinate space: landmarks may be in [0, lm_sz] pixel space or [0,1] normalized
        let max_lm = lm_vals.iter().cloned().fold(0.0f32, f32::max);
        let lm_scale = if max_lm <= 1.5 { 1.0 } else { 1.0 / lm_sz_f };

        // Map all 468 landmarks to image space
        let mut landmarks: Vec<(f32, f32, f32)> = Vec::with_capacity(num_landmarks);
        for i in 0..num_landmarks {
            let lx = (lm_vals[i*3]     * lm_scale * crop_w + cx1 as f32).clamp(0.0, img_w);
            let ly = (lm_vals[i*3 + 1] * lm_scale * crop_h + cy1 as f32).clamp(0.0, img_h);
            let lz =  lm_vals[i*3 + 2] * lm_scale; // depth, keep relative
            landmarks.push((lx, ly, lz));
        }

        // Bbox from landmarks
        let lm_min_x = landmarks.iter().map(|(x,_,_)| *x).fold(f32::INFINITY,  f32::min);
        let lm_min_y = landmarks.iter().map(|(_,y,_)| *y).fold(f32::INFINITY,  f32::min);
        let lm_max_x = landmarks.iter().map(|(x,_,_)| *x).fold(-f32::INFINITY, f32::max);
        let lm_max_y = landmarks.iter().map(|(_,y,_)| *y).fold(-f32::INFINITY, f32::max);
        let pad = ((lm_max_x - lm_min_x).max(lm_max_y - lm_min_y) * 0.05).max(4.0);
        let bx1 = (lm_min_x - pad).max(0.0);
        let by1 = (lm_min_y - pad).max(0.0);
        let bx2 = (lm_max_x + pad).min(img_w);
        let by2 = (lm_max_y + pad).min(img_h);

        // Annotate: face mesh contours
        let (lr, lg, lb) = face_colors[face_idx % 2];
        let draw_contour = |ann: &mut Vec<u8>, connections: &[(usize,usize)], r: u8, g: u8, b: u8| {
            for &(a, b_idx) in connections {
                if a < landmarks.len() && b_idx < landmarks.len() {
                    let (ax, ay, _) = landmarks[a];
                    let (bx, by, _) = landmarks[b_idx];
                    draw_line(ann, req.image.width, req.image.height,
                        ax as i32, ay as i32, bx as i32, by as i32, r, g, b, 1);
                }
            }
        };

        // Draw face oval (jawline)
        draw_contour(&mut annotated, &FACE_OVAL, lr, lg, lb);
        // Lips
        draw_contour(&mut annotated, &FACE_LIPS_OUTER, lr/2+80, lg/2+60, lb/2+60);
        draw_contour(&mut annotated, &FACE_LIPS_INNER, lr/2+80, lg/2+60, lb/2+60);
        // Eyes
        draw_contour(&mut annotated, &FACE_LEFT_EYE, 80, 220, 255);
        draw_contour(&mut annotated, &FACE_RIGHT_EYE, 80, 220, 255);
        // Eyebrows
        draw_contour(&mut annotated, &FACE_LEFT_EYEBROW, lr, lg, lb);
        draw_contour(&mut annotated, &FACE_RIGHT_EYEBROW, lr, lg, lb);
        // Nose bridge
        draw_contour(&mut annotated, &FACE_NOSE_BRIDGE, lr, lg, lb);

        // Draw dots at named landmarks
        for &(idx, _name) in &FACE_NAMED_LANDMARKS {
            if idx < landmarks.len() {
                let (kx, ky, _) = landmarks[idx];
                draw_circle(&mut annotated, req.image.width, req.image.height, kx as i32, ky as i32, 3, lr, lg, lb);
                draw_circle(&mut annotated, req.image.width, req.image.height, kx as i32, ky as i32, 1, 255, 255, 255);
            }
        }

        // Bbox
        let (bx1i, by1i, bx2i, by2i) = (bx1 as i32, by1 as i32, bx2 as i32, by2 as i32);
        for t in 0..2i32 {
            draw_line(&mut annotated, req.image.width, req.image.height, bx1i, by1i+t, bx2i, by1i+t, lr, lg, lb, 1);
            draw_line(&mut annotated, req.image.width, req.image.height, bx2i-t, by1i, bx2i-t, by2i, lr, lg, lb, 1);
            draw_line(&mut annotated, req.image.width, req.image.height, bx2i, by2i-t, bx1i, by2i-t, lr, lg, lb, 1);
            draw_line(&mut annotated, req.image.width, req.image.height, bx1i+t, by2i, bx1i+t, by1i, lr, lg, lb, 1);
        }

        // JSON output
        let f = face_idx;
        result_text.push_str(&format!("Face {} ({:.1}%)\n", f+1, face_score * 100.0));

        all_json.push(format!(
            r#"{{"type":"face_bbox","face":{f},"x1":{:.1},"y1":{:.1},"x2":{:.1},"y2":{:.1},"confidence":{:.4}}}"#,
            bx1, by1, bx2, by2, face_score
        ));

        for &(idx, name) in &FACE_NAMED_LANDMARKS {
            if idx < landmarks.len() {
                let (kx, ky, _) = landmarks[idx];
                all_json.push(format!(
                    r#"{{"type":"face_landmark","face":{f},"name":"{}","x":{:.1},"y":{:.1}}}"#,
                    name, kx, ky
                ));
            }
        }

        // Full landmark array as indexed items
        for (i, (lx, ly, lz)) in landmarks.iter().enumerate() {
            all_json.push(format!(
                r#"{{"type":"face_landmark_idx","face":{f},"index":{i},"x":{:.1},"y":{:.1},"z":{:.3}}}"#,
                lx, ly, lz
            ));
        }
    }

    let face_count = selected.len();
    let best_score = selected.first().map(|(s,_)| *s).unwrap_or(0.0);
    let status = match face_count {
        0 => "No faces".to_string(),
        1 => format!("1 face ({:.1}%)", best_score * 100.0),
        n => format!("{} faces ({:.1}%)", n, best_score * 100.0),
    };

    Ok(MlInferenceResult {
        node_id: req.node_id,
        result_text,
        result_json: format!("[{}]", all_json.join(",")),
        annotated_frame: Some(Arc::new(crate::graph::ImageData {
            width: req.image.width,
            height: req.image.height,
            pixels: annotated,
        })),
        status,
    })
}

// ══════════════════════════════════════════════════════════════════════════════
// Body/Pose Tracking — 2-stage MediaPipe pipeline (Pose Detector → Pose Landmark)
// ══════════════════════════════════════════════════════════════════════════════

// 33 MediaPipe pose landmark names (canonical order, indices 0-32)
const POSE_LANDMARK_NAMES: [&str; 33] = [
    "nose",
    "left_eye_inner", "left_eye", "left_eye_outer",
    "right_eye_inner", "right_eye", "right_eye_outer",
    "left_ear", "right_ear",
    "mouth_left", "mouth_right",
    "left_shoulder", "right_shoulder",
    "left_elbow", "right_elbow",
    "left_wrist", "right_wrist",
    "left_pinky", "right_pinky",
    "left_index", "right_index",
    "left_thumb", "right_thumb",
    "left_hip", "right_hip",
    "left_knee", "right_knee",
    "left_ankle", "right_ankle",
    "left_heel", "right_heel",
    "left_foot_index", "right_foot_index",
];

// 35 skeleton connections for body pose
const POSE_SKELETON: [(usize,usize); 35] = [
    // Face
    (0,1),(1,2),(2,3),(3,7), (0,4),(4,5),(5,6),(6,8),
    // Mouth
    (9,10),
    // Shoulders
    (11,12),
    // Left arm
    (11,13),(13,15),(15,17),(15,19),(15,21),(17,19),
    // Right arm
    (12,14),(14,16),(16,18),(16,20),(16,22),(18,20),
    // Torso
    (11,23),(12,24),(23,24),
    // Left leg
    (23,25),(25,27),(27,29),(27,31),(29,31),
    // Right leg
    (24,26),(26,28),(28,30),(28,32),(30,32),
];

pub fn run_pose_tracking(req: &MlInferenceRequest) -> MlInferenceResult {
    match run_pose_tracking_inner(req) {
        Ok(r) => r,
        Err(e) => MlInferenceResult {
            node_id: req.node_id,
            result_text: String::new(),
            result_json: "[]".into(),
            annotated_frame: Some(req.image.clone()),
            status: format!("Error: {}", e),
        },
    }
}

fn run_pose_tracking_inner(req: &MlInferenceRequest) -> Result<MlInferenceResult, String> {
    use ort::session::Session;

    let img_w = req.image.width  as f32;
    let img_h = req.image.height as f32;

    // ────────────────────────────────────────────────────────────────────────
    // Stage 1 — Pose Detection (128×128 NCHW)
    // ────────────────────────────────────────────────────────────────────────
    let pose_sz = 128u32;
    let ts = pose_sz as usize;
    let pose_sz_f = pose_sz as f32;

    let mut pose_sess = Session::builder()
        .map_err(|e| format!("Session builder: {}", e))?
        .commit_from_memory(BUNDLED_POSE_MODEL)
        .map_err(|e| format!("Load pose model: {}", e))?;

    let pose_resized = resize_image(&req.image, pose_sz, pose_sz);

    // NCHW tensor, normalize to [-1, 1]
    let mut pose_data = vec![0.0f32; 3 * ts * ts];
    for y in 0..ts {
        for x in 0..ts {
            let si = (y * ts + x) * 4;
            pose_data[0 * ts * ts + y * ts + x] = pose_resized[si]   as f32 / 127.5 - 1.0;
            pose_data[1 * ts * ts + y * ts + x] = pose_resized[si+1] as f32 / 127.5 - 1.0;
            pose_data[2 * ts * ts + y * ts + x] = pose_resized[si+2] as f32 / 127.5 - 1.0;
        }
    }
    let pose_tensor = ort::value::Tensor::from_array(
        ([1usize, 3, ts, ts], pose_data.into_boxed_slice())
    ).map_err(|e| format!("Pose tensor: {}", e))?;

    let pose_input_name = pose_sess.inputs().first().ok_or("No pose inputs")?.name().to_string();
    let pose_outputs = pose_sess.run(ort::inputs![&pose_input_name => pose_tensor])
        .map_err(|e| format!("Pose run: {}", e))?;

    let mut tensors: Vec<(Vec<usize>, Vec<f32>)> = Vec::new();
    for output in pose_outputs.iter() {
        if let Ok((shape, data)) = output.1.try_extract_tensor::<f32>() {
            tensors.push((shape.iter().map(|&d| d as usize).collect(), data.to_vec()));
        }
    }

    // boxes: [1, 896, 12] — per-anchor: [cy,cx,h,w, kp0y,kp0x, kp1y,kp1x, kp2y,kp2x, kp3y,kp3x]
    let boxes_t = tensors.iter().find(|(d,_)| d.len()==3 && d[2]==12);
    let scores_t = tensors.iter().find(|(d,_)| d.len()==3 && d[2]==1)
        .or_else(|| tensors.iter().find(|(d,_)| d.len()==2 && d[1]==1));
    let (boxes, scores) = match (boxes_t, scores_t) {
        (Some(b), Some(s)) => (b, s),
        _ => return Err("Pose detector output format unrecognized".into()),
    };

    let num_anchors = boxes.0[1].min(scores.0[if scores.0.len()>=2 { 1 } else { 0 }]);
    // Reuse BlazeFace anchor generation — same 896 config for 128×128
    let anchors = generate_blazeface_anchors(pose_sz);

    let mut candidates: Vec<(f32, usize)> = (0..num_anchors)
        .map(|i| (sigmoid(scores.1[i]), i))
        .filter(|(s, _)| *s >= req.confidence)
        .collect();
    candidates.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    if candidates.is_empty() {
        let best = (0..num_anchors).map(|i| sigmoid(scores.1[i])).fold(0.0f32, f32::max);
        return Ok(MlInferenceResult {
            node_id: req.node_id,
            result_text: format!("No bodies ({:.1}%)", best * 100.0),
            result_json: "[]".into(),
            annotated_frame: Some(req.image.clone()),
            status: format!("No bodies ({:.1}%)", best * 100.0),
        });
    }

    // NMS — up to 2 bodies
    let decode_box = |idx: usize| -> (f32, f32, f32, f32) {
        let b = idx * 12;
        let (ax, ay) = anchors.get(idx).copied().unwrap_or((0.5, 0.5));
        let cy = (ay + boxes.1[b]   / pose_sz_f) * img_h;
        let cx = (ax + boxes.1[b+1] / pose_sz_f) * img_w;
        let h  = (boxes.1[b+2].abs() / pose_sz_f * img_h).max(1.0);
        let w  = (boxes.1[b+3].abs() / pose_sz_f * img_w).max(1.0);
        (cx - w/2.0, cy - h/2.0, cx + w/2.0, cy + h/2.0)
    };
    let box_center = |(x1,y1,x2,y2): (f32,f32,f32,f32)| -> (f32,f32) {
        ((x1+x2)/2.0, (y1+y2)/2.0)
    };
    let box_iou = |(ax1,ay1,ax2,ay2): (f32,f32,f32,f32), (bx1,by1,bx2,by2): (f32,f32,f32,f32)| -> f32 {
        let ix1 = ax1.max(bx1); let iy1 = ay1.max(by1);
        let ix2 = ax2.min(bx2); let iy2 = ay2.min(by2);
        let inter = (ix2-ix1).max(0.0) * (iy2-iy1).max(0.0);
        let a = (ax2-ax1)*(ay2-ay1); let b = (bx2-bx1)*(by2-by1);
        inter / (a + b - inter + 1e-6)
    };
    let min_dim = img_w.min(img_h);
    let center_dist_threshold = min_dim * 0.25;

    let mut selected: Vec<(f32, usize)> = Vec::new();
    for (score, idx) in &candidates {
        let box_a = decode_box(*idx);
        let (ca_x, ca_y) = box_center(box_a);
        let suppressed = selected.iter().any(|(_, sel)| {
            let box_b = decode_box(*sel);
            if box_iou(box_a, box_b) > 0.3 { return true; }
            let (cb_x, cb_y) = box_center(box_b);
            let dx = ca_x - cb_x; let dy = ca_y - cb_y;
            (dx*dx + dy*dy).sqrt() < center_dist_threshold
        });
        if !suppressed { selected.push((*score, *idx)); }
        if selected.len() >= 1 { break; } // single body only
    }

    // ────────────────────────────────────────────────────────────────────────
    // Stage 2 — Pose Landmark (256×256 NHWC) for each detected body
    // ────────────────────────────────────────────────────────────────────────
    let lm_sz = 256u32;
    let lm_ts = lm_sz as usize;
    let lm_sz_f = lm_sz as f32;

    let mut lm_sess = Session::builder()
        .map_err(|e| format!("Session builder: {}", e))?
        .commit_from_memory(BUNDLED_POSE_LANDMARK_MODEL)
        .map_err(|e| format!("Load pose landmark model: {}", e))?;
    let lm_input_name = lm_sess.inputs().first().ok_or("No landmark inputs")?.name().to_string();

    let body_colors: [(u8,u8,u8); 2] = [(180,220,60), (255,160,60)]; // green-yellow, orange

    let mut annotated = req.image.pixels.clone();
    let mut all_json: Vec<String> = Vec::new();
    let mut result_text = String::new();

    for (body_idx, (body_score, anchor_idx)) in selected.iter().enumerate() {
        let base = anchor_idx * 12;
        let (ax, ay) = anchors.get(*anchor_idx).copied().unwrap_or((0.5, 0.5));

        // Decode 4 body keypoints for crop centering
        let mut kp_sum_x = 0.0f32; let mut kp_sum_y = 0.0f32;
        let mut kp_min_x = f32::INFINITY; let mut kp_min_y = f32::INFINITY;
        let mut kp_max_x = -f32::INFINITY; let mut kp_max_y = -f32::INFINITY;
        for k in 0..4usize {
            let ry = boxes.1[base + 4 + k * 2];
            let rx = boxes.1[base + 4 + k * 2 + 1];
            let kx = ((ax + rx / pose_sz_f) * img_w).clamp(0.0, img_w);
            let ky = ((ay + ry / pose_sz_f) * img_h).clamp(0.0, img_h);
            kp_sum_x += kx; kp_sum_y += ky;
            kp_min_x = kp_min_x.min(kx); kp_min_y = kp_min_y.min(ky);
            kp_max_x = kp_max_x.max(kx); kp_max_y = kp_max_y.max(ky);
        }
        let kp_cx = kp_sum_x / 4.0;
        let kp_cy = kp_sum_y / 4.0;
        let raw_bw = (boxes.1[base+3].abs() / pose_sz_f * img_w).max(40.0);
        let raw_bh = (boxes.1[base+2].abs() / pose_sz_f * img_h).max(60.0);

        // Body crop — taller than wide (full body), generous padding
        let half_w = (raw_bw * 0.6).max((kp_max_x - kp_min_x) * 0.8).max(50.0);
        let half_h = (raw_bh * 0.7).max((kp_max_y - kp_min_y) * 0.8).max(80.0);
        let half = half_w.max(half_h);

        let cx1 = (kp_cx - half * 1.3).max(0.0) as u32;
        let cy1 = (kp_cy - half * 1.5).max(0.0) as u32;
        let cx2 = (kp_cx + half * 1.3).min(img_w) as u32;
        let cy2 = (kp_cy + half * 1.3).min(img_h) as u32;
        if cx2 <= cx1 || cy2 <= cy1 { continue; }

        // Build landmark input tensor (NHWC)
        let cropped = crop_and_resize(&req.image, cx1, cy1, cx2, cy2, lm_sz, lm_sz);
        let mut lm_data = vec![0.0f32; lm_ts * lm_ts * 3];
        for y in 0..lm_ts {
            for x in 0..lm_ts {
                let si = (y * lm_ts + x) * 4;
                let di = (y * lm_ts + x) * 3;
                lm_data[di]     = cropped[si]   as f32 / 255.0;
                lm_data[di + 1] = cropped[si+1] as f32 / 255.0;
                lm_data[di + 2] = cropped[si+2] as f32 / 255.0;
            }
        }
        let lm_tensor = ort::value::Tensor::from_array(
            ([1usize, lm_ts, lm_ts, 3], lm_data.into_boxed_slice())
        ).map_err(|e| format!("Pose landmark tensor: {}", e))?;

        let lm_outputs = lm_sess.run(ort::inputs![&lm_input_name => lm_tensor])
            .map_err(|e| format!("Pose landmark run: {}", e))?;

        // Output Identity: [1, 195] = 39 landmarks × 5 (x, y, z, visibility, presence)
        let lm_vals: Vec<f32> = {
            let lm_out = lm_outputs.iter()
                .find(|(name, _)| *name == "Identity")
                .or_else(|| lm_outputs.iter().next())
                .ok_or("No pose landmark output")?;
            let (_, data) = lm_out.1.try_extract_tensor::<f32>()
                .map_err(|e| format!("Pose landmark extract: {}", e))?;
            data.to_vec()
        };

        // Use first 33 landmarks (skip 6 auxiliary at end)
        let num_raw = lm_vals.len() / 5;
        let num_landmarks = num_raw.min(33);
        if num_landmarks < 33 { continue; }

        let crop_w = (cx2 - cx1) as f32;
        let crop_h = (cy2 - cy1) as f32;

        // Coordinate scale: landmarks might be in [0, lm_sz] or [0, 1]
        let max_lm = lm_vals.iter()
            .take(33 * 5)
            .enumerate()
            .filter(|(i, _)| i % 5 < 2)  // only x,y values
            .map(|(_, v)| *v)
            .fold(0.0f32, f32::max);
        let lm_scale = if max_lm <= 1.5 { 1.0 } else { 1.0 / lm_sz_f };

        // Map 33 landmarks to image space
        let mut keypoints: Vec<(f32, f32, f32, f32, &'static str)> = Vec::with_capacity(33);
        for i in 0..33usize {
            let lx = (lm_vals[i*5]     * lm_scale * crop_w + cx1 as f32).clamp(0.0, img_w);
            let ly = (lm_vals[i*5 + 1] * lm_scale * crop_h + cy1 as f32).clamp(0.0, img_h);
            let lz =  lm_vals[i*5 + 2] * lm_scale;
            let vis = sigmoid(lm_vals[i*5 + 3]);
            keypoints.push((lx, ly, lz, vis, POSE_LANDMARK_NAMES[i]));
        }

        // Bbox from landmarks (only visible ones)
        let visible: Vec<&(f32,f32,f32,f32,&str)> = keypoints.iter()
            .filter(|(_, _, _, vis, _)| *vis > 0.3)
            .collect();
        let (bx1, by1, bx2, by2) = if visible.len() >= 4 {
            let min_x = visible.iter().map(|(x,_,_,_,_)| *x).fold(f32::INFINITY,  f32::min);
            let min_y = visible.iter().map(|(_,y,_,_,_)| *y).fold(f32::INFINITY,  f32::min);
            let max_x = visible.iter().map(|(x,_,_,_,_)| *x).fold(-f32::INFINITY, f32::max);
            let max_y = visible.iter().map(|(_,y,_,_,_)| *y).fold(-f32::INFINITY, f32::max);
            let pad = ((max_x - min_x).max(max_y - min_y) * 0.05).max(4.0);
            ((min_x - pad).max(0.0), (min_y - pad).max(0.0),
             (max_x + pad).min(img_w), (max_y + pad).min(img_h))
        } else {
            (cx1 as f32, cy1 as f32, cx2 as f32, cy2 as f32)
        };

        // ── Annotate ──────────────────────────────────────────────────────
        let (lr, lg, lb) = body_colors[body_idx % 2];

        // Draw skeleton bones (only when both joints are visible)
        for &(a, b) in &POSE_SKELETON {
            if a < keypoints.len() && b < keypoints.len() {
                let (ax, ay, _, a_vis, _) = keypoints[a];
                let (bx, by, _, b_vis, _) = keypoints[b];
                if a_vis > 0.3 && b_vis > 0.3 {
                    draw_line(&mut annotated, req.image.width, req.image.height,
                        ax as i32, ay as i32, bx as i32, by as i32, lr, lg, lb, 2);
                }
            }
        }

        // Draw joint dots
        let major_joints: [usize; 12] = [11,12,13,14,15,16,23,24,25,26,27,28];
        for (i, (kx, ky, _, vis, _)) in keypoints.iter().enumerate() {
            if *vis < 0.3 { continue; }
            let is_major = major_joints.contains(&i);
            let (outer, inner) = if is_major { (8, 5) } else { (5, 3) };
            draw_circle(&mut annotated, req.image.width, req.image.height, *kx as i32, *ky as i32, outer, 20, 20, 20);
            draw_circle(&mut annotated, req.image.width, req.image.height, *kx as i32, *ky as i32, inner, lr, lg, lb);
            draw_circle(&mut annotated, req.image.width, req.image.height, *kx as i32, *ky as i32, 2, 255, 255, 255);
        }

        // Bbox
        let (bx1i, by1i, bx2i, by2i) = (bx1 as i32, by1 as i32, bx2 as i32, by2 as i32);
        for t in 0..2i32 {
            draw_line(&mut annotated, req.image.width, req.image.height, bx1i, by1i+t, bx2i, by1i+t, lr, lg, lb, 1);
            draw_line(&mut annotated, req.image.width, req.image.height, bx2i-t, by1i, bx2i-t, by2i, lr, lg, lb, 1);
            draw_line(&mut annotated, req.image.width, req.image.height, bx2i, by2i-t, bx1i, by2i-t, lr, lg, lb, 1);
            draw_line(&mut annotated, req.image.width, req.image.height, bx1i+t, by2i, bx1i+t, by1i, lr, lg, lb, 1);
        }

        // JSON output
        let b_idx = body_idx;
        result_text.push_str(&format!("Body {} ({:.1}%)\n", b_idx+1, body_score * 100.0));

        all_json.push(format!(
            r#"{{"type":"body_bbox","body":{b_idx},"x1":{:.1},"y1":{:.1},"x2":{:.1},"y2":{:.1},"confidence":{:.4}}}"#,
            bx1, by1, bx2, by2, body_score
        ));

        for (kx, ky, kz, vis, name) in &keypoints {
            all_json.push(format!(
                r#"{{"type":"body_landmark","body":{b_idx},"name":"{}","x":{:.1},"y":{:.1},"z":{:.3},"visibility":{:.3}}}"#,
                name, kx, ky, kz, vis
            ));
        }
    }

    let body_count = selected.len();
    let best_score = selected.first().map(|(s,_)| *s).unwrap_or(0.0);
    let status = match body_count {
        0 => "No bodies".to_string(),
        1 => format!("1 body ({:.1}%)", best_score * 100.0),
        n => format!("{} bodies ({:.1}%)", n, best_score * 100.0),
    };

    Ok(MlInferenceResult {
        node_id: req.node_id,
        result_text,
        result_json: format!("[{}]", all_json.join(",")),
        annotated_frame: Some(Arc::new(crate::graph::ImageData {
            width: req.image.width,
            height: req.image.height,
            pixels: annotated,
        })),
        status,
    })
}

fn iou(a: &Detection, b: &Detection) -> f32 {
    let x1 = a.x1.max(b.x1);
    let y1 = a.y1.max(b.y1);
    let x2 = a.x2.min(b.x2);
    let y2 = a.y2.min(b.y2);
    let inter = (x2 - x1).max(0.0) * (y2 - y1).max(0.0);
    let area_a = (a.x2 - a.x1) * (a.y2 - a.y1);
    let area_b = (b.x2 - b.x1) * (b.y2 - b.y1);
    inter / (area_a + area_b - inter + 1e-6)
}

fn nms(dets: &[Detection], iou_threshold: f32) -> Vec<&Detection> {
    let mut keep: Vec<&Detection> = Vec::new();
    let mut suppressed = vec![false; dets.len()];
    for i in 0..dets.len() {
        if suppressed[i] { continue; }
        keep.push(&dets[i]);
        for j in (i + 1)..dets.len() {
            if !suppressed[j] && iou(&dets[i], &dets[j]) > iou_threshold {
                suppressed[j] = true;
            }
        }
    }
    keep
}

/// Draw a filled rectangle on RGBA pixels
fn draw_filled_rect(pixels: &mut [u8], w: u32, h: u32, x1: i32, y1: i32, x2: i32, y2: i32, r: u8, g: u8, b: u8) {
    for y in y1..=y2 {
        for x in x1..=x2 {
            set_pixel(pixels, w, h, x, y, r, g, b);
        }
    }
}

/// Draw ASCII text using a minimal 5×7 pixel font
fn draw_text(pixels: &mut [u8], w: u32, h: u32, x: i32, y: i32, text: &str, r: u8, g: u8, b: u8) {
    for (i, ch) in text.chars().enumerate() {
        let glyph = get_glyph(ch);
        let cx = x + i as i32 * 6;
        for row in 0..7i32 {
            let bits = glyph[row as usize];
            for col in 0..5i32 {
                if bits & (1 << (4 - col)) != 0 {
                    set_pixel(pixels, w, h, cx + col, y + row, r, g, b);
                }
            }
        }
    }
}

/// Minimal 5×7 bitmap font — returns 7 rows of 5-bit masks
fn get_glyph(ch: char) -> [u8; 7] {
    match ch {
        'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b00000],
        'B' => [0b11110, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110, 0b00000],
        'C' => [0b01111, 0b10000, 0b10000, 0b10000, 0b10000, 0b01111, 0b00000],
        'D' => [0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110, 0b00000],
        'E' => [0b11111, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111, 0b00000],
        'F' => [0b11111, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000, 0b00000],
        'G' => [0b01111, 0b10000, 0b10011, 0b10001, 0b10001, 0b01111, 0b00000],
        'H' => [0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001, 0b00000],
        'I' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111, 0b00000],
        'J' => [0b11111, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100, 0b00000],
        'K' => [0b10001, 0b10010, 0b11100, 0b10010, 0b10001, 0b10001, 0b00000],
        'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111, 0b00000],
        'M' => [0b10001, 0b11011, 0b10101, 0b10001, 0b10001, 0b10001, 0b00000],
        'N' => [0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b00000],
        'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110, 0b00000],
        'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b00000],
        'Q' => [0b01110, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101, 0b00000],
        'R' => [0b11110, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001, 0b00000],
        'S' => [0b01111, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110, 0b00000],
        'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00000],
        'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110, 0b00000],
        'V' => [0b10001, 0b10001, 0b10001, 0b01010, 0b01010, 0b00100, 0b00000],
        'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b11011, 0b10001, 0b00000],
        'X' => [0b10001, 0b01010, 0b00100, 0b00100, 0b01010, 0b10001, 0b00000],
        'Y' => [0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100, 0b00000],
        'Z' => [0b11111, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111, 0b00000],
        'a' => [0b00000, 0b01110, 0b00001, 0b01111, 0b10001, 0b01111, 0b00000],
        'b' => [0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b11110, 0b00000],
        'c' => [0b00000, 0b01111, 0b10000, 0b10000, 0b10000, 0b01111, 0b00000],
        'd' => [0b00001, 0b00001, 0b01111, 0b10001, 0b10001, 0b01111, 0b00000],
        'e' => [0b00000, 0b01110, 0b10001, 0b11111, 0b10000, 0b01111, 0b00000],
        'f' => [0b00110, 0b01000, 0b11100, 0b01000, 0b01000, 0b01000, 0b00000],
        'g' => [0b00000, 0b01111, 0b10001, 0b01111, 0b00001, 0b01110, 0b00000],
        'h' => [0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b10001, 0b00000],
        'i' => [0b00100, 0b00000, 0b00100, 0b00100, 0b00100, 0b00110, 0b00000],
        'j' => [0b00010, 0b00000, 0b00010, 0b00010, 0b10010, 0b01100, 0b00000],
        'k' => [0b10000, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b00000],
        'l' => [0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110, 0b00000],
        'm' => [0b00000, 0b11010, 0b10101, 0b10101, 0b10001, 0b10001, 0b00000],
        'n' => [0b00000, 0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b00000],
        'o' => [0b00000, 0b01110, 0b10001, 0b10001, 0b10001, 0b01110, 0b00000],
        'p' => [0b00000, 0b11110, 0b10001, 0b11110, 0b10000, 0b10000, 0b00000],
        'q' => [0b00000, 0b01111, 0b10001, 0b01111, 0b00001, 0b00001, 0b00000],
        'r' => [0b00000, 0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b00000],
        's' => [0b00000, 0b01111, 0b10000, 0b01110, 0b00001, 0b11110, 0b00000],
        't' => [0b01000, 0b11100, 0b01000, 0b01000, 0b01001, 0b00110, 0b00000],
        'u' => [0b00000, 0b10001, 0b10001, 0b10001, 0b10001, 0b01111, 0b00000],
        'v' => [0b00000, 0b10001, 0b10001, 0b01010, 0b01010, 0b00100, 0b00000],
        'w' => [0b00000, 0b10001, 0b10001, 0b10101, 0b10101, 0b01010, 0b00000],
        'x' => [0b00000, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b00000],
        'y' => [0b00000, 0b10001, 0b01010, 0b00100, 0b01000, 0b10000, 0b00000],
        'z' => [0b00000, 0b11111, 0b00010, 0b00100, 0b01000, 0b11111, 0b00000],
        '0' => [0b01110, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110, 0b00000],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b01110, 0b00000],
        '2' => [0b01110, 0b10001, 0b00010, 0b00100, 0b01000, 0b11111, 0b00000],
        '3' => [0b11110, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110, 0b00000],
        '4' => [0b00010, 0b00110, 0b01010, 0b11111, 0b00010, 0b00010, 0b00000],
        '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b11110, 0b00000],
        '6' => [0b00110, 0b01000, 0b11110, 0b10001, 0b10001, 0b01110, 0b00000],
        '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b00000],
        '8' => [0b01110, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110, 0b00000],
        '9' => [0b01110, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100, 0b00000],
        '%' => [0b11000, 0b11001, 0b00010, 0b00100, 0b01001, 0b00011, 0b00000],
        ' ' => [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000],
        '_' => [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b11111, 0b00000],
        '-' => [0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000, 0b00000],
        '.' => [0b00000, 0b00000, 0b00000, 0b00000, 0b00110, 0b00110, 0b00000],
        ':' => [0b00110, 0b00110, 0b00000, 0b00110, 0b00110, 0b00000, 0b00000],
        _ =>  [0b11111, 0b10001, 0b10001, 0b10001, 0b10001, 0b11111, 0b00000],
    }
}

/// Draw a rectangle outline on RGBA pixels
fn draw_rect(pixels: &mut [u8], w: u32, h: u32, x1: i32, y1: i32, x2: i32, y2: i32, r: u8, g: u8, b: u8, thickness: i32) {
    for t in 0..thickness {
        // Top & bottom edges
        for x in x1..=x2 {
            set_pixel(pixels, w, h, x, y1 + t, r, g, b);
            set_pixel(pixels, w, h, x, y2 - t, r, g, b);
        }
        // Left & right edges
        for y in y1..=y2 {
            set_pixel(pixels, w, h, x1 + t, y, r, g, b);
            set_pixel(pixels, w, h, x2 - t, y, r, g, b);
        }
    }
}

/// Draw a filled circle on RGBA pixels
fn draw_circle(pixels: &mut [u8], w: u32, h: u32, cx: i32, cy: i32, radius: i32, r: u8, g: u8, b: u8) {
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            if dx * dx + dy * dy <= radius * radius {
                set_pixel(pixels, w, h, cx + dx, cy + dy, r, g, b);
            }
        }
    }
}

/// Draw a line using Bresenham's algorithm
fn draw_line(pixels: &mut [u8], w: u32, h: u32, x0: i32, y0: i32, x1: i32, y1: i32, r: u8, g: u8, b: u8, thickness: i32) {
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let mut x = x0;
    let mut y = y0;
    let half_t = thickness / 2;
    loop {
        for t in -half_t..=half_t {
            set_pixel(pixels, w, h, x + t, y, r, g, b);
            set_pixel(pixels, w, h, x, y + t, r, g, b);
        }
        if x == x1 && y == y1 { break; }
        let e2 = 2 * err;
        if e2 >= dy { err += dy; x += sx; }
        if e2 <= dx { err += dx; y += sy; }
    }
}

fn set_pixel(pixels: &mut [u8], w: u32, h: u32, x: i32, y: i32, r: u8, g: u8, b: u8) {
    if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 { return; }
    let idx = ((y as u32 * w + x as u32) * 4) as usize;
    if idx + 3 < pixels.len() {
        pixels[idx] = r;
        pixels[idx + 1] = g;
        pixels[idx + 2] = b;
        pixels[idx + 3] = 255;
    }
}

/// Simple bilinear resize of RGBA image
fn resize_image(img: &ImageData, new_w: u32, new_h: u32) -> Vec<u8> {
    let mut out = vec![0u8; (new_w * new_h * 4) as usize];
    let x_ratio = img.width as f32 / new_w as f32;
    let y_ratio = img.height as f32 / new_h as f32;
    for y in 0..new_h {
        for x in 0..new_w {
            let src_x = (x as f32 * x_ratio).min(img.width as f32 - 1.0);
            let src_y = (y as f32 * y_ratio).min(img.height as f32 - 1.0);
            let sx = src_x as u32;
            let sy = src_y as u32;
            let src_idx = ((sy * img.width + sx) * 4) as usize;
            let dst_idx = ((y * new_w + x) * 4) as usize;
            if src_idx + 3 < img.pixels.len() && dst_idx + 3 < out.len() {
                out[dst_idx] = img.pixels[src_idx];
                out[dst_idx + 1] = img.pixels[src_idx + 1];
                out[dst_idx + 2] = img.pixels[src_idx + 2];
                out[dst_idx + 3] = img.pixels[src_idx + 3];
            }
        }
    }
    out
}
