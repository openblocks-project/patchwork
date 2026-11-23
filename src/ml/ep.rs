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
//!
//! P6 Stage 2.8: each provider is registered on its own with
//! `error_on_failure`, so which one took (or why none did) is known and can
//! be shown ([`Provider`]); previously a failure was only logged, and a CPU
//! fallback was silent.

use ort::session::builder::SessionBuilder;

/// The execution provider a session was built with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    Cuda,
    DirectMl,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    CoreMl,
    Cpu,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Provider::Cuda => "CUDA (GPU)",
            Provider::DirectMl => "DirectML (GPU)",
            Provider::CoreMl => "CoreML",
            Provider::Cpu => "CPU",
        }
    }
}

/// A builder and the provider registered on it. `fallback` says why the
/// accelerator was not used, when it was expected and is missing.
pub struct Built {
    pub builder: SessionBuilder,
    pub provider: Provider,
    pub fallback: Option<String>,
}

pub fn session_builder() -> ort::Result<SessionBuilder> {
    Ok(session_builder_with_provider()?.builder)
}

/// [`session_builder`], saying which provider was registered.
pub fn session_builder_with_provider() -> ort::Result<Built> {
    let builder = SessionBuilder::new()?;
    Ok(attach_accelerators(builder))
}

#[cfg(target_os = "macos")]
fn attach_accelerators(builder: SessionBuilder) -> Built {
    use ort::ep::coreml::{ComputeUnits, CoreML};
    match builder.with_execution_providers([CoreML::default()
        .with_compute_units(ComputeUnits::All)
        .build()
        .error_on_failure()])
    {
        Ok(builder) => Built {
            builder,
            provider: Provider::CoreMl,
            fallback: None,
        },
        Err(e) => {
            let why = format!("CoreML: {e}");
            eprintln!("[ml/ep] CoreML register failed, CPU fallback: {e}");
            Built {
                builder: e.recover(),
                provider: Provider::Cpu,
                fallback: Some(why),
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn attach_accelerators(builder: SessionBuilder) -> Built {
    use ort::ep::{cuda::CUDA, directml::DirectML};
    // CUDA first (when present), then DirectML; each on its own so the one
    // that took is known.
    let builder =
        match builder.with_execution_providers([CUDA::default().build().error_on_failure()]) {
            Ok(builder) => {
                return Built {
                    builder,
                    provider: Provider::Cuda,
                    fallback: None,
                };
            }
            Err(e) => e.recover(),
        };
    match builder.with_execution_providers([DirectML::default().build().error_on_failure()]) {
        Ok(builder) => Built {
            builder,
            provider: Provider::DirectMl,
            fallback: None,
        },
        Err(e) => {
            let why = format!("DirectML: {e}");
            eprintln!("[ml/ep] CUDA/DirectML register failed, CPU fallback: {e}");
            Built {
                builder: e.recover(),
                provider: Provider::Cpu,
                fallback: Some(why),
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn attach_accelerators(builder: SessionBuilder) -> Built {
    Built {
        builder,
        provider: Provider::Cpu,
        fallback: None,
    }
}
