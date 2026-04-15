//! Global transport clock — a dedicated thread running at ~1ms resolution
//! that maintains monotonically increasing time/beat counters.
//!
//! Multiple Clock nodes share the same transport via the global TRANSPORT static.
//! First Clock node to initialize spawns the thread. Play/Pause/Stop/BPM are global.
//! Each Clock node applies its own subdivision locally.

use std::sync::atomic::{AtomicU64, AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

// ── Global transport ─────────────────────────────────────────────────────────

/// Shared transport + command sender. Lazily initialized by the first Clock node.
pub struct GlobalTransport {
    pub state: Arc<TransportState>,
    pub tx: crossbeam_channel::Sender<TransportCommand>,
    pub ref_count: AtomicUsize,
}

static TRANSPORT: OnceLock<GlobalTransport> = OnceLock::new();

/// Get or create the global transport. First call spawns the clock thread.
pub fn get_or_create_transport(default_bpm: f64) -> &'static GlobalTransport {
    TRANSPORT.get_or_init(|| {
        let state = Arc::new(TransportState::new(default_bpm));
        let (tx, rx) = crossbeam_channel::unbounded();
        let clock_state = Arc::clone(&state);

        std::thread::Builder::new()
            .name("patchwork-clock".into())
            .spawn(move || ClockThread { state: clock_state, commands: rx }.run())
            .expect("spawn clock thread");

        crate::system_log::log("Transport clock thread started (1ms resolution)");

        GlobalTransport {
            state,
            tx,
            ref_count: AtomicUsize::new(0),
        }
    })
}

/// Get existing transport (returns None if no Clock node has been created yet).
pub fn get_transport() -> Option<&'static GlobalTransport> {
    TRANSPORT.get()
}

// ── AtomicF64 ────────────────────────────────────────────────────────────────

pub struct AtomicF64(AtomicU64);

impl AtomicF64 {
    pub fn new(v: f64) -> Self { Self(AtomicU64::new(v.to_bits())) }
    #[inline]
    pub fn load(&self) -> f64 { f64::from_bits(self.0.load(Ordering::Relaxed)) }
    #[inline]
    pub fn store(&self, v: f64) { self.0.store(v.to_bits(), Ordering::Relaxed); }
}

// ── TransportState ───────────────────────────────────────────────────────────

pub struct TransportState {
    pub transport_secs: AtomicF64,
    pub transport_beats: AtomicF64,
    pub bpm: AtomicF64,
    pub running: AtomicBool,
    pub beat_count: AtomicU64,
}

impl TransportState {
    pub fn new(bpm: f64) -> Self {
        Self {
            transport_secs: AtomicF64::new(0.0),
            transport_beats: AtomicF64::new(0.0),
            bpm: AtomicF64::new(bpm),
            running: AtomicBool::new(false),
            beat_count: AtomicU64::new(0),
        }
    }
}

// ── TransportCommand ─────────────────────────────────────────────────────────

pub enum TransportCommand {
    Play,
    Pause,
    Stop,
    SetBpm(f64),
    Shutdown,
}

// ── ClockThread ──────────────────────────────────────────────────────────────

pub struct ClockThread {
    pub state: Arc<TransportState>,
    pub commands: crossbeam_channel::Receiver<TransportCommand>,
}

impl ClockThread {
    pub fn run(self) {
        let mut last_instant = std::time::Instant::now();
        let mut accumulated_secs: f64 = 0.0;
        let mut accumulated_beats: f64 = 0.0;
        let mut last_beat_int: u64 = 0;

        loop {
            std::thread::sleep(std::time::Duration::from_millis(1));

            while let Ok(cmd) = self.commands.try_recv() {
                match cmd {
                    TransportCommand::Play => {
                        self.state.running.store(true, Ordering::Relaxed);
                        last_instant = std::time::Instant::now();
                    }
                    TransportCommand::Pause => {
                        self.state.running.store(false, Ordering::Relaxed);
                    }
                    TransportCommand::Stop => {
                        self.state.running.store(false, Ordering::Relaxed);
                        accumulated_secs = 0.0;
                        accumulated_beats = 0.0;
                        last_beat_int = 0;
                        self.state.transport_secs.store(0.0);
                        self.state.transport_beats.store(0.0);
                        self.state.beat_count.store(0, Ordering::Relaxed);
                    }
                    TransportCommand::SetBpm(bpm) => {
                        self.state.bpm.store(bpm.max(1.0));
                    }
                    TransportCommand::Shutdown => return,
                }
            }

            if self.state.running.load(Ordering::Relaxed) {
                let now = std::time::Instant::now();
                let dt = now.duration_since(last_instant).as_secs_f64().min(0.25);
                last_instant = now;

                let bpm = self.state.bpm.load();
                accumulated_secs += dt;
                accumulated_beats += dt * (bpm / 60.0);

                self.state.transport_secs.store(accumulated_secs);
                self.state.transport_beats.store(accumulated_beats);

                let current_beat_int = accumulated_beats.floor() as u64;
                if current_beat_int > last_beat_int {
                    self.state.beat_count.store(current_beat_int, Ordering::Relaxed);
                    last_beat_int = current_beat_int;
                }
            } else {
                last_instant = std::time::Instant::now();
            }
        }
    }
}
