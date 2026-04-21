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
