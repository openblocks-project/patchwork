//! P6 Stage 2.8: the Player's diagnostics.
//!
//! - Tracking models (hand, face, pose) used by the project are built in the
//!   background as soon as the project is allowed to run (never before), so
//!   they are ready when the first frames arrive; meanwhile a small
//!   "Preparing …" line says so.
//! - Which inference provider each model runs on. A CPU fallback (DirectML
//!   unavailable) is shown as a small, persistent warning instead of
//!   silently running slower.
//! - F12 shows details: providers and model load times, inference times,
//!   camera, output and display rates.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::graph::{Graph, ImageData, NodeId, NodeType, PortValue};
use crate::ml::ep::Provider;
use crate::ml::session_cache::ModelState;
use crate::nodes::ml_model::InferenceStat;

/// The key that shows / hides the details.
pub const DIAGNOSTICS_KEY: egui::Key = egui::Key::F12;

/// Events per second over the last second.
#[derive(Debug, Default)]
pub struct RateCounter {
    events: VecDeque<Instant>,
}

impl RateCounter {
    pub fn tick(&mut self, now: Instant) {
        self.events.push_back(now);
        self.trim(now);
    }

    fn trim(&mut self, now: Instant) {
        while self
            .events
            .front()
            .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(1))
        {
            self.events.pop_front();
        }
    }

    pub fn rate(&mut self, now: Instant) -> usize {
        self.trim(now);
        self.events.len()
    }
}

/// Counts how often an image changes (a new frame, by identity).
#[derive(Debug, Default)]
pub struct FrameRate {
    last: Option<usize>,
    counter: RateCounter,
}

impl FrameRate {
    pub fn observe(&mut self, image: Option<&Arc<ImageData>>, now: Instant) {
        // GPU images have no pixels here and are new every frame: not a
        // meaningful frame identity.
        let Some(image) = image.filter(|i| !i.pixels.is_empty()) else {
            return;
        };
        let id = Arc::as_ptr(image) as usize;
        if self.last != Some(id) {
            self.last = Some(id);
            self.counter.tick(now);
        }
    }

    pub fn rate(&mut self, now: Instant) -> usize {
        self.counter.rate(now)
    }
}

#[derive(Debug, Default)]
pub struct PlayerDiagnostics {
    pub open: bool,
    /// Tracking models were sent for preloading (once per run).
    pub preloaded: bool,
    pub display: RateCounter,
    pub output: FrameRate,
    pub camera: FrameRate,
}

/// The tracking node types in `graph` (hand, face, pose), each once.
pub fn tracking_types(graph: &Graph) -> Vec<&'static str> {
    let mut found = Vec::new();
    for node in graph.nodes.values() {
        if let NodeType::Dynamic { inner } = &node.node_type {
            for tag in ["hand_detection", "face_detection", "pose_detection"] {
                if inner.node.type_tag() == tag && !found.contains(&tag) {
                    found.push(tag);
                }
            }
        }
    }
    found
}

/// The cameras' states (running, status), from what each camera node
/// published this frame.
pub fn camera_states(ctx: &egui::Context, graph: &Graph) -> Vec<(bool, String)> {
    graph
        .nodes
        .iter()
        .filter(|(_, node)| {
            matches!(&node.node_type, NodeType::Dynamic { inner } if inner.node.type_tag() == "video_in")
        })
        .filter_map(|(id, _)| {
            ctx.data(|d| {
                d.get_temp::<(bool, String)>(
                    crate::nodes::video_in_node::VideoInNode::camera_status_id(*id),
                )
            })
        })
        .collect()
}

/// The note for a camera that is not running, if any (pure; tested).
pub fn camera_note(states: &[(bool, String)]) -> Option<String> {
    states
        .iter()
        .find(|(running, status)| !running && !status.trim().is_empty())
        .map(|(_, status)| format!("Camera not running: {status}"))
}

/// The first Video In's current frame.
pub fn camera_frame(
    graph: &Graph,
    values: &HashMap<(NodeId, usize), PortValue>,
) -> Option<Arc<ImageData>> {
    graph
        .nodes
        .iter()
        .find_map(|(id, node)| match &node.node_type {
            NodeType::Dynamic { inner } if inner.node.type_tag() == "video_in" => {
                match values.get(&(*id, 0)) {
                    Some(PortValue::Image(img)) => Some(img.clone()),
                    _ => None,
                }
            }
            _ => None,
        })
}

/// What the diagnostics say, from the model states (pure; tested).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Summary {
    /// Models still being prepared (readable names).
    pub loading: Vec<&'static str>,
    /// The CPU-fallback warning, when any model runs on the CPU although an
    /// accelerator was expected.
    pub cpu_warning: Option<String>,
    /// One line per model: name, provider / state, load time.
    pub models: Vec<String>,
}

pub fn summarize(states: &[(&'static str, ModelState)]) -> Summary {
    let mut summary = Summary::default();
    let mut fallback: Option<&str> = None;
    for (key, state) in states {
        let name = crate::ml::bundled::model_label(key);
        match state {
            ModelState::Loading { since } => {
                summary.loading.push(name);
                summary.models.push(format!(
                    "{name}: preparing ({:.0} s)",
                    since.elapsed().as_secs_f64()
                ));
            }
            ModelState::Ready {
                provider,
                fallback: why,
                load_time,
            } => {
                if *provider == Provider::Cpu {
                    if let Some(why) = why {
                        fallback.get_or_insert(why);
                    }
                }
                summary.models.push(format!(
                    "{name}: {} (ready in {:.1} s)",
                    provider.label(),
                    load_time.as_secs_f64()
                ));
            }
            ModelState::Failed(e) => summary.models.push(format!("{name}: failed — {e}")),
        }
    }
    if let Some(why) = fallback {
        summary.cpu_warning = Some(format!(
            "Tracking is running on the CPU because the GPU (DirectML) is unavailable, so \
             it is slower. {why}"
        ));
    }
    summary
}

/// The names of the pipelines' timings, for the details.
fn stat_lines(stats: &[(&'static str, InferenceStat)]) -> Vec<String> {
    stats
        .iter()
        .map(|(name, s)| {
            format!(
                "{name} tracking: {:.0} ms average, {:.0} ms last ({} runs)",
                s.average_ms, s.last_ms, s.runs
            )
        })
        .collect()
}

const AMBER: egui::Color32 = egui::Color32::from_rgb(240, 170, 60);

impl PlayerDiagnostics {
    /// Count this frame and draw the indicators (and the details if open).
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        output: Option<&Arc<ImageData>>,
        camera: Option<&Arc<ImageData>>,
        cameras: &[(bool, String)],
    ) {
        let now = Instant::now();
        if ctx.input(|i| i.key_pressed(DIAGNOSTICS_KEY)) {
            self.open = !self.open;
        }
        self.display.tick(now);
        self.output.observe(output, now);
        self.camera.observe(camera, now);
        let summary = summarize(&crate::ml::session_cache::bundled_statuses());

        let mut notes: Vec<(String, egui::Color32)> = Vec::new();
        if !summary.loading.is_empty() {
            notes.push((
                format!(
                    "Preparing {} — the first start can take several seconds…",
                    summary.loading.join(", ").to_lowercase()
                ),
                egui::Color32::from_gray(220),
            ));
        }
        if let Some(note) = camera_note(cameras) {
            notes.push((note, AMBER));
        }
        if summary.cpu_warning.is_some() {
            notes.push((
                "Tracking is running on the CPU (GPU unavailable) and is slower. F12: details"
                    .to_string(),
                AMBER,
            ));
        }
        if !notes.is_empty() {
            egui::Area::new(egui::Id::new("player_diagnostics_notes"))
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::RIGHT_BOTTOM, [-8.0, -8.0])
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::NONE
                        .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 170))
                        .inner_margin(egui::Margin::symmetric(6, 3))
                        .show(ui, |ui| {
                            for (text, color) in &notes {
                                ui.label(egui::RichText::new(text).size(11.0).color(*color));
                            }
                        });
                });
        }
        if !self.open {
            return;
        }
        let mut lines = vec![
            format!("Display: {} fps", self.display.rate(now)),
            format!("Output updates: {} per second", self.output.rate(now)),
            format!("Camera frames: {} per second", self.camera.rate(now)),
        ];
        if let Some(camera) = camera {
            lines.push(format!("Camera image: {}×{}", camera.width, camera.height));
        }
        for (running, status) in cameras {
            lines.push(format!(
                "Camera: {}{}",
                if *running { "running" } else { "not running" },
                if status.is_empty() {
                    String::new()
                } else {
                    format!(" ({status})")
                }
            ));
        }
        lines.extend(stat_lines(&crate::nodes::ml_model::inference_stats()));
        if summary.models.is_empty() {
            lines.push("No tracking models loaded.".to_string());
        }
        lines.extend(summary.models.iter().cloned());
        egui::Area::new(egui::Id::new("player_diagnostics"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::LEFT_TOP, [8.0, 8.0])
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 200))
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new("Diagnostics (F12)")
                                .strong()
                                .color(egui::Color32::WHITE),
                        );
                        for line in &lines {
                            ui.label(
                                egui::RichText::new(line)
                                    .size(12.0)
                                    .color(egui::Color32::from_gray(220)),
                            );
                        }
                        if let Some(warning) = &summary.cpu_warning {
                            ui.label(egui::RichText::new(warning).size(12.0).color(AMBER));
                        }
                    });
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_camera_that_is_not_running_is_noted() {
        assert_eq!(camera_note(&[]), None);
        // Starting (no status yet) or running: no note.
        assert_eq!(camera_note(&[(false, String::new())]), None);
        assert_eq!(camera_note(&[(true, "Capturing".to_string())]), None);
        assert_eq!(
            camera_note(&[(false, "Disconnected: Could not run graph".to_string())]).as_deref(),
            Some("Camera not running: Disconnected: Could not run graph")
        );
    }

    #[test]
    fn rates_count_the_last_second() {
        let mut counter = RateCounter::default();
        let start = Instant::now();
        for i in 0..30 {
            counter.tick(start + Duration::from_millis(i * 50));
        }
        // At 1.45 s, the events of the last second (0.45 s .. 1.45 s).
        assert_eq!(counter.rate(start + Duration::from_millis(1450)), 21);
        assert_eq!(counter.rate(start + Duration::from_secs(5)), 0);
    }

    #[test]
    fn a_frame_counts_once_however_often_it_is_shown() {
        let mut rate = FrameRate::default();
        let now = Instant::now();
        let a = Arc::new(ImageData::new(2, 2, vec![0; 16]));
        let b = Arc::new(ImageData::new(2, 2, vec![0; 16]));
        for _ in 0..5 {
            rate.observe(Some(&a), now);
        }
        rate.observe(Some(&b), now);
        rate.observe(None, now);
        // GPU images (no pixels) are not counted.
        rate.observe(Some(&Arc::new(ImageData::new(2, 2, Vec::new()))), now);
        assert_eq!(rate.rate(now), 2);
    }

    #[test]
    fn loading_and_cpu_fallback_are_reported() {
        let states = vec![
            (
                crate::ml::bundled::HAND_DETECT_KEY,
                ModelState::Loading {
                    since: Instant::now(),
                },
            ),
            (
                crate::ml::bundled::FACE_DETECT_KEY,
                ModelState::Ready {
                    provider: Provider::DirectMl,
                    fallback: None,
                    load_time: Duration::from_millis(1500),
                },
            ),
        ];
        let summary = summarize(&states);
        assert_eq!(summary.loading, vec!["Hand detector"]);
        assert!(summary.cpu_warning.is_none());
        assert!(
            summary
                .models
                .contains(&"Face detector: DirectML (GPU) (ready in 1.5 s)".to_string())
        );

        let states = vec![(
            crate::ml::bundled::POSE_DETECT_KEY,
            ModelState::Ready {
                provider: Provider::Cpu,
                fallback: Some("DirectML: no adapter".to_string()),
                load_time: Duration::from_millis(200),
            },
        )];
        let summary = summarize(&states);
        assert!(summary.loading.is_empty());
        let warning = summary.cpu_warning.unwrap();
        assert!(warning.contains("CPU") && warning.contains("DirectML: no adapter"));
        assert_eq!(summary.models, vec!["Pose detector: CPU (ready in 0.2 s)"]);

        // A plain CPU build (no accelerator expected) is not a fallback.
        let states = vec![(
            crate::ml::bundled::POSE_DETECT_KEY,
            ModelState::Ready {
                provider: Provider::Cpu,
                fallback: None,
                load_time: Duration::from_millis(200),
            },
        )];
        assert!(summarize(&states).cpu_warning.is_none());
    }

    #[test]
    fn every_tracking_node_type_has_its_two_models() {
        for tag in ["hand_detection", "face_detection", "pose_detection"] {
            let models = crate::ml::bundled::tracking_models(tag);
            assert_eq!(models.len(), 2, "{tag}");
            for (key, bytes) in models {
                assert!(bytes.len() > 1000, "{key}");
                assert_ne!(crate::ml::bundled::model_label(key), "Model");
            }
        }
        assert!(crate::ml::bundled::tracking_models("video_in").is_empty());
        let mut graph = Graph::new();
        for tag in ["pose_detection", "hand_detection", "pose_detection"] {
            let node = crate::node_trait::NODE_REGISTRY
                .lock()
                .unwrap()
                .create(tag, &serde_json::json!({}))
                .unwrap();
            graph.add_node(
                NodeType::Dynamic {
                    inner: crate::graph::DynNode { node },
                },
                [0.0, 0.0],
            );
        }
        let mut found = tracking_types(&graph);
        found.sort();
        assert_eq!(found, vec!["hand_detection", "pose_detection"]);
    }
}
