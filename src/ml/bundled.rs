//! Bundled ONNX model bytes + dispatch sentinels.
//!
//! `include_bytes!` paths are relative to this source file; the file sits at
//! `src/ml/bundled.rs`, so `../../assets/...` resolves to the project root.

pub static BUNDLED_HAND_MODEL: &[u8] =
    include_bytes!("../../assets/presets/ml/MediaPipeHandDetector.onnx");

pub static BUNDLED_HAND_LANDMARK_MODEL: &[u8] =
    include_bytes!("../../assets/presets/ml/MediaPipeHandLandmark.onnx");

pub const BUNDLED_HAND_SENTINEL: &str = "__bundled_hand__";

pub static BUNDLED_FACE_MODEL: &[u8] =
    include_bytes!("../../assets/presets/ml/MediaPipeFaceDetector.onnx");

pub static BUNDLED_FACE_LANDMARK_MODEL: &[u8] =
    include_bytes!("../../assets/presets/ml/MediaPipeFaceLandmarkDetector.onnx");

pub const BUNDLED_FACE_SENTINEL: &str = "__bundled_face__";

pub static BUNDLED_POSE_MODEL: &[u8] =
    include_bytes!("../../assets/presets/ml/MediaPipePoseDetector.onnx");

pub static BUNDLED_POSE_LANDMARK_MODEL: &[u8] =
    include_bytes!("../../assets/presets/ml/MediaPipePoseLandmark.onnx");

pub const BUNDLED_POSE_SENTINEL: &str = "__bundled_pose__";

/// Session-cache keys of the tracking pipelines' models (P6 Stage 2.8: one
/// name per model, shared by the pipelines and by preloading).
pub const HAND_DETECT_KEY: &str = "hand_palm_detect";
pub const HAND_LANDMARK_KEY: &str = "hand_landmark";
pub const FACE_DETECT_KEY: &str = "face_detect";
pub const FACE_LANDMARK_KEY: &str = "face_landmark";
pub const POSE_DETECT_KEY: &str = "pose_detect";
pub const POSE_LANDMARK_KEY: &str = "pose_landmark";

/// P6 Stage 2.8: the models a tracking node type (`hand_detection`,
/// `face_detection`, `pose_detection`) runs, as (session key, bytes).
pub fn tracking_models(type_tag: &str) -> &'static [(&'static str, &'static [u8])] {
    use std::sync::OnceLock;
    static HAND: OnceLock<[(&str, &[u8]); 2]> = OnceLock::new();
    static FACE: OnceLock<[(&str, &[u8]); 2]> = OnceLock::new();
    static POSE: OnceLock<[(&str, &[u8]); 2]> = OnceLock::new();
    match type_tag {
        "hand_detection" => HAND.get_or_init(|| {
            [
                (HAND_DETECT_KEY, BUNDLED_HAND_MODEL),
                (HAND_LANDMARK_KEY, BUNDLED_HAND_LANDMARK_MODEL),
            ]
        }),
        "face_detection" => FACE.get_or_init(|| {
            [
                (FACE_DETECT_KEY, BUNDLED_FACE_MODEL),
                (FACE_LANDMARK_KEY, BUNDLED_FACE_LANDMARK_MODEL),
            ]
        }),
        "pose_detection" => POSE.get_or_init(|| {
            [
                (POSE_DETECT_KEY, BUNDLED_POSE_MODEL),
                (POSE_LANDMARK_KEY, BUNDLED_POSE_LANDMARK_MODEL),
            ]
        }),
        _ => &[],
    }
}

/// A readable name for a tracking model's session key.
pub fn model_label(key: &str) -> &'static str {
    match key {
        HAND_DETECT_KEY => "Hand detector",
        HAND_LANDMARK_KEY => "Hand landmarks",
        FACE_DETECT_KEY => "Face detector",
        FACE_LANDMARK_KEY => "Face landmarks",
        POSE_DETECT_KEY => "Pose detector",
        POSE_LANDMARK_KEY => "Pose landmarks",
        _ => "Model",
    }
}
