//! P6 Stage 2.8: tracking throttle rules (automated) and MediaPipe
//! performance measurements (manual: real models, real inference).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::graph::{ImageData, MlPreset};
use crate::nodes::ml_model::{
    BUNDLED_FACE_SENTINEL, BUNDLED_HAND_SENTINEL, BUNDLED_POSE_SENTINEL, InferenceThrottle,
    MlInferenceRequest, MlInferenceResult, run_inference,
};

fn frame() -> Arc<ImageData> {
    Arc::new(ImageData::new(2, 2, vec![0; 16]))
}

// ── Throttle rules ──────────────────────────────────────────────────────────

#[test]
fn a_new_frame_runs_once_per_interval() {
    let mut t = InferenceThrottle::default();
    let a = frame();
    let b = frame();
    assert!(t.ready(0.0, 100, &a, 0.5), "the first frame runs");
    t.finished(false);
    assert!(
        !t.ready(1.0, 100, &a, 0.5),
        "the same frame never runs again"
    );
    assert!(
        !t.ready(0.05, 100, &b, 0.5),
        "a new frame waits for the interval"
    );
    assert!(t.ready(0.1, 100, &b, 0.5), "and then runs");
}

#[test]
fn nothing_is_sent_while_the_previous_inference_is_out() {
    let mut t = InferenceThrottle::default();
    let (a, b) = (frame(), frame());
    assert!(t.ready(0.0, 50, &a, 0.5));
    assert!(
        !t.ready(1.0, 50, &b, 0.5),
        "busy: the new frame is not dropped into a busy node"
    );
    t.finished(false);
    assert!(
        t.ready(1.0, 50, &b, 0.5),
        "sent as soon as the result is back"
    );
    // A result that never comes stops blocking after the timeout.
    let c = frame();
    assert!(!t.ready(
        1.0 + InferenceThrottle::IN_FLIGHT_TIMEOUT_SECS - 1.0,
        50,
        &c,
        0.5
    ));
    assert!(t.ready(1.0 + InferenceThrottle::IN_FLIGHT_TIMEOUT_SECS, 50, &c, 0.5));
}

#[test]
fn a_failed_run_or_a_new_confidence_runs_the_same_frame_again() {
    let mut t = InferenceThrottle::default();
    let a = frame();
    assert!(t.ready(0.0, 100, &a, 0.5));
    t.finished(true);
    assert!(t.ready(0.2, 100, &a, 0.5), "retried after a failure");
    t.finished(false);
    assert!(!t.ready(0.4, 100, &a, 0.5));
    assert!(t.ready(0.4, 100, &a, 0.7), "re-run with the new confidence");
}

#[test]
fn tracking_nodes_keep_their_settings_and_ports() {
    // Existing projects: the same saved settings, ports and defaults.
    for (tag, outputs, default_interval) in [
        ("hand_detection", 5, 100),
        ("face_detection", 4, 100),
        ("pose_detection", 4, 33),
    ] {
        let mut node = crate::node_trait::NODE_REGISTRY
            .lock()
            .unwrap()
            .create(
                tag,
                &serde_json::json!({ "confidence": 0.7, "interval_ms": 250 }),
            )
            .unwrap();
        let saved = node.save_state();
        assert_eq!(saved["confidence"], serde_json::json!(0.7f32), "{tag}");
        assert_eq!(saved["interval_ms"], 250, "{tag}");
        assert_eq!(node.inputs().len(), 1, "{tag}");
        assert_eq!(node.outputs().len(), outputs, "{tag}");
        node.load_state(&serde_json::json!({}));
        assert_eq!(node.save_state()["interval_ms"], 250, "{tag}");
        let fresh = crate::node_trait::NODE_REGISTRY
            .lock()
            .unwrap()
            .create(tag, &serde_json::json!({}))
            .unwrap();
        assert_eq!(
            fresh.save_state()["interval_ms"],
            default_interval,
            "{tag}: default unchanged"
        );
    }
}

// ── Measurements (manual) ───────────────────────────────────────────────────

fn camera_frame_from(dir: &std::path::Path) -> ImageData {
    let img = image::open(dir.join("portrait.jpg")).expect("portrait.jpg");
    let img = image::imageops::resize(
        &img.to_rgba8(),
        640,
        480,
        image::imageops::FilterType::Triangle,
    );
    ImageData::new(640, 480, img.into_raw())
}

fn request(sentinel: &str, preset: MlPreset, image: Arc<ImageData>) -> MlInferenceRequest {
    MlInferenceRequest {
        model_path: sentinel.to_string(),
        labels_path: String::new(),
        confidence: 0.5,
        preset,
        image,
        node_id: 1,
    }
}

fn pipelines() -> [(&'static str, &'static str, MlPreset); 3] {
    [
        ("hand", BUNDLED_HAND_SENTINEL, MlPreset::HandDetection),
        ("face", BUNDLED_FACE_SENTINEL, MlPreset::FaceDetection),
        ("pose", BUNDLED_POSE_SENTINEL, MlPreset::PoseEstimation),
    ]
}

/// Manual: time from start to the first full (hand → face → pose) output,
/// cold, in a fresh process. PATCHWORK_COLD_MODE=chain: models load as the
/// chained nodes first run (current behaviour without preloading);
/// =preload: all six are preloaded at start (the Player now). Needs
/// PATCHWORK_ML_IMAGES (with portrait.jpg).
#[test]
#[ignore = "manual: cold MediaPipe start (one mode per process)"]
fn manual_cold_start() {
    let (Some(dir), Ok(mode)) = (
        std::env::var_os("PATCHWORK_ML_IMAGES").map(std::path::PathBuf::from),
        std::env::var("PATCHWORK_COLD_MODE"),
    ) else {
        eprintln!("set PATCHWORK_ML_IMAGES and PATCHWORK_COLD_MODE=chain|preload");
        return;
    };
    let frame = Arc::new(camera_frame_from(&dir));
    let start = Instant::now();
    if mode == "preload" {
        for tag in ["hand_detection", "face_detection", "pose_detection"] {
            crate::ml::session_cache::preload_bundled(crate::ml::bundled::tracking_models(tag));
        }
    }
    let mut input = frame;
    let mut stages = Vec::new();
    for (name, sentinel, preset) in pipelines() {
        let t = Instant::now();
        let result = run_inference(&request(sentinel, preset, input.clone()));
        stages.push(format!(
            "{name} {:.1}s ({})",
            t.elapsed().as_secs_f64(),
            result.status
        ));
        input = result.annotated_frame.expect("annotated");
    }
    let total = start.elapsed().as_secs_f64();
    let loads: Vec<String> = crate::ml::session_cache::bundled_statuses()
        .into_iter()
        .map(|(key, state)| match state {
            crate::ml::session_cache::ModelState::Ready {
                provider,
                load_time,
                ..
            } => format!("{key}={:.1}s/{}", load_time.as_secs_f64(), provider.label()),
            other => format!("{key}={other:?}"),
        })
        .collect();
    println!(
        "COLD mode={mode} first full output after {total:.1}s | {} | loads: {}",
        stages.join(", "),
        loads.join(" ")
    );
}

/// One stage of the simulated chain.
struct Stage {
    sentinel: &'static str,
    preset: MlPreset,
    throttle: InferenceThrottle,
    last_start: f64,
    busy: Option<(std::thread::JoinHandle<MlInferenceResult>, Arc<ImageData>)>,
    output: Option<Arc<ImageData>>,
    runs: u32,
    dropped: u32,
}

/// Manual: the tracking chain (camera → hand → face → pose → output) with
/// the nodes' scheduling, real models and real inference. A 30 fps camera
/// (new frames of a real photo) and a 60 fps UI loop, as in the Player.
/// Compares the previous rule (a request every interval, dropped while the
/// node is busy) with `InferenceThrottle`, at the nodes' default intervals
/// and at the fastest the hand and face nodes allow. Needs
/// PATCHWORK_ML_IMAGES (with portrait.jpg).
#[test]
#[ignore = "manual: MediaPipe chain scheduling with real inference"]
fn manual_chain_scheduling() {
    let Some(dir) = std::env::var_os("PATCHWORK_ML_IMAGES").map(std::path::PathBuf::from) else {
        eprintln!("set PATCHWORK_ML_IMAGES");
        return;
    };
    let source = camera_frame_from(&dir);
    // Warm: models built and run once.
    let mut input = Arc::new(source.clone());
    for (_, sentinel, preset) in pipelines() {
        input = run_inference(&request(sentinel, preset, input))
            .annotated_frame
            .unwrap();
    }
    let seconds = 12.0;
    // Intervals per stage (hand, face, pose): the nodes' defaults, then the
    // fastest the hand and face nodes allow.
    for (rule, intervals) in [
        ("old", [100, 100, 33]),
        ("new", [100, 100, 33]),
        ("old", [50, 50, 33]),
        ("new", [50, 50, 33]),
    ] {
        let mut stages: Vec<Stage> = pipelines()
            .into_iter()
            .map(|(_, sentinel, preset)| Stage {
                sentinel,
                preset,
                throttle: InferenceThrottle::default(),
                last_start: -999.0,
                busy: None,
                output: None,
                runs: 0,
                dropped: 0,
            })
            .collect();
        // Image identity -> the camera time it derives from.
        let mut origin: HashMap<usize, f64> = HashMap::new();
        let mut camera: Option<Arc<ImageData>> = None;
        let mut next_frame = 0.0;
        let mut camera_frames = 0;
        let mut updates = 0u32;
        let mut latencies = Vec::new();
        let start = Instant::now();
        loop {
            let now = start.elapsed().as_secs_f64();
            if now >= seconds {
                break;
            }
            if now >= next_frame {
                let frame = Arc::new(source.clone());
                origin.insert(Arc::as_ptr(&frame) as usize, now);
                camera = Some(frame);
                camera_frames += 1;
                next_frame += 1.0 / 30.0;
            }
            for i in 0..stages.len() {
                let interval = intervals[i];
                // Collect a finished result.
                if stages[i]
                    .busy
                    .as_ref()
                    .is_some_and(|(h, _)| h.is_finished())
                {
                    let (handle, consumed) = stages[i].busy.take().unwrap();
                    let result = handle.join().unwrap();
                    stages[i].throttle.finished(false);
                    stages[i].runs += 1;
                    if let Some(out) = result.annotated_frame {
                        let born = origin
                            .get(&(Arc::as_ptr(&consumed) as usize))
                            .copied()
                            .unwrap_or(now);
                        origin.insert(Arc::as_ptr(&out) as usize, born);
                        if i == stages.len() - 1 {
                            updates += 1;
                            latencies.push(now - born);
                        }
                        stages[i].output = Some(out);
                    }
                }
                let input = if i == 0 {
                    camera.clone()
                } else {
                    stages[i - 1].output.clone()
                };
                let Some(input) = input else { continue };
                let stage = &mut stages[i];
                let send = match rule {
                    "old" => {
                        if now - stage.last_start >= f64::from(interval) / 1000.0 {
                            stage.last_start = now;
                            if stage.busy.is_some() {
                                stage.dropped += 1;
                                false
                            } else {
                                true
                            }
                        } else {
                            false
                        }
                    }
                    _ => stage.busy.is_none() && stage.throttle.ready(now, interval, &input, 0.5),
                };
                if send {
                    let req = request(stage.sentinel, stage.preset.clone(), input.clone());
                    stage.busy =
                        Some((crate::capability::spawn(move || run_inference(&req)), input));
                }
            }
            std::thread::sleep(Duration::from_millis(16));
        }
        for stage in &mut stages {
            if let Some((h, _)) = stage.busy.take() {
                let _ = h.join();
            }
        }
        latencies.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pct = |p: f64| {
            latencies
                .get(((latencies.len() as f64 - 1.0) * p).round() as usize)
                .map(|v| v * 1000.0)
                .unwrap_or(f64::NAN)
        };
        println!(
            "CHAIN rule={rule} intervals={intervals:?}ms camera={:.1}fps output={:.1}/s \
             latency median={:.0}ms p90={:.0}ms | inferences/s hand={:.1} face={:.1} pose={:.1} \
             | dropped hand={} face={} pose={}",
            f64::from(camera_frames) / seconds,
            f64::from(updates) / seconds,
            pct(0.5),
            pct(0.9),
            f64::from(stages[0].runs) / seconds,
            f64::from(stages[1].runs) / seconds,
            f64::from(stages[2].runs) / seconds,
            stages[0].dropped,
            stages[1].dropped,
            stages[2].dropped,
        );
    }
    let stats = crate::nodes::ml_model::inference_stats();
    println!("STATS {stats:?}");
}
