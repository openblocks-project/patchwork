use crate::graph::*;
use eframe::egui;
use std::collections::HashMap;
use std::sync::Arc;

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
    if *preset == MlPreset::Classification || *preset == MlPreset::ObjectDetection || *preset == MlPreset::Custom {
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
pub struct MlInferenceResult {
    pub node_id: NodeId,
    pub result_text: String,
    pub result_json: String,
    pub annotated_frame: Option<Arc<ImageData>>,
    pub status: String,
}

/// Run ONNX inference on an image (called from background thread)
pub fn run_inference(req: &MlInferenceRequest) -> MlInferenceResult {
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

    if expected_dims.len() < 3 { return None; }

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

    let mut session = Session::builder()
        .map_err(|e| format!("Session builder: {}", e))?
        .commit_from_file(&req.model_path)
        .map_err(|e| format!("Load model: {}", e))?;

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

    let mut session2 = Session::builder()
        .map_err(|e| format!("Session builder: {}", e))?
        .commit_from_file(&req.model_path)
        .map_err(|e| format!("Reload model: {}", e))?;
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
