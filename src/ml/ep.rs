//! Execution-provider configuration for ONNX sessions.
//!
//! `session_builder()` returns an `ort::SessionBuilder` with the best
//! available hardware accelerator already registered:
//!
//!   * macOS  → CoreML (Neural Engine + GPU + CPU via `ComputeUnits::All`)
//!   * Windows → CUDA, falling back to DirectML
//!   * other  → CPU only
//!
//! Registration is always best-effort: if an EP fails to attach at runtime
//! (feature compiled in but binary / driver missing, unsupported GPU, etc.)
//! the builder is recovered and returned without that provider, which lets
//! ort fall back to its CPU provider. Callers therefore never need to handle
//! EP-specific failure — they just call `session_builder()?` and proceed.

use ort::session::builder::SessionBuilder;

pub fn session_builder() -> ort::Result<SessionBuilder> {
    let builder = SessionBuilder::new()?;
    Ok(attach_accelerators(builder))
}

#[cfg(target_os = "macos")]
fn attach_accelerators(builder: SessionBuilder) -> SessionBuilder {
    use ort::ep::coreml::{ComputeUnits, CoreML};
    match builder.with_execution_providers([CoreML::default()
        .with_compute_units(ComputeUnits::All)
        .build()])
    {
        Ok(b) => b,
        Err(e) => {
            eprintln!("[ml/ep] CoreML register failed, CPU fallback: {e}");
            e.recover()
        }
    }
}

#[cfg(target_os = "windows")]
fn attach_accelerators(builder: SessionBuilder) -> SessionBuilder {
    use ort::ep::{cuda::CUDA, directml::DirectML};
    match builder.with_execution_providers([CUDA::default().build(), DirectML::default().build()]) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("[ml/ep] CUDA/DirectML register failed, CPU fallback: {e}");
            e.recover()
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn attach_accelerators(builder: SessionBuilder) -> SessionBuilder {
    builder
}
