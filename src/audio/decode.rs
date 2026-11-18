use super::buffers::FilePlayerBuffer;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

// ── Symphonia File Decode Thread ─────────────────────────────────────────────

/// Background thread function that decodes an audio file with Symphonia and
/// writes mono f32 samples into a FilePlayerBuffer ring buffer.
/// Handles seeking, pausing, looping, and stop signals via atomics.
pub(crate) fn decode_file_thread(
    path: String,
    buffer: Arc<FilePlayerBuffer>,
    output_sample_rate: f32,
    looping: Arc<AtomicBool>,
) {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;
    use symphonia::core::units::Time;

    let open_and_decode =
        |path: &str, buffer: &FilePlayerBuffer, seek_secs: f64| -> Result<(), String> {
            // P4.3.2E2: checked before every open (loops and seeks reopen).
            crate::capability::check(crate::capability::Capability::Filesystem)?;
            let file = std::fs::File::open(path).map_err(|e| format!("Open: {}", e))?;
            let mss = MediaSourceStream::new(Box::new(file), Default::default());

            let mut hint = Hint::new();
            if let Some(ext) = std::path::Path::new(path)
                .extension()
                .and_then(|e| e.to_str())
            {
                hint.with_extension(ext);
            }

            let probed = symphonia::default::get_probe()
                .format(
                    &hint,
                    mss,
                    &FormatOptions::default(),
                    &MetadataOptions::default(),
                )
                .map_err(|e| format!("Probe: {}", e))?;

            let mut reader = probed.format;

            let track = reader.default_track().ok_or("No default audio track")?;
            let track_id = track.id;
            let codec_params = track.codec_params.clone();

            let file_sr = codec_params.sample_rate.unwrap_or(44100) as f32;
            let _file_channels = codec_params.channels.map(|ch| ch.count()).unwrap_or(2) as usize;

            buffer
                .file_sample_rate
                .store(file_sr as u32, Ordering::Release);
            if let Some(n_frames) = codec_params.n_frames {
                buffer
                    .total_samples
                    .store(n_frames as usize, Ordering::Release);
            }

            let mut decoder = symphonia::default::get_codecs()
                .make(&codec_params, &DecoderOptions::default())
                .map_err(|e| format!("Decoder: {}", e))?;

            // Seek if needed (non-zero start position)
            if seek_secs > 0.01 {
                let _ = reader.seek(
                    symphonia::core::formats::SeekMode::Coarse,
                    symphonia::core::formats::SeekTo::Time {
                        time: Time::new(seek_secs as u64, seek_secs.fract()),
                        track_id: Some(track_id),
                    },
                );
            }

            // Resampling state: linear interpolation
            let resample_ratio = output_sample_rate / file_sr;
            let needs_resample = (resample_ratio - 1.0).abs() > 0.001;
            let mut resample_pos: f64 = 0.0; // fractional position in source samples

            let mut sample_buf: Option<SampleBuffer<f32>> = None;

            loop {
                // Check stop signal
                if buffer.stop_requested.load(Ordering::Relaxed) {
                    return Ok(());
                }

                // Check seek signal
                if buffer.seek_requested.load(Ordering::Relaxed) {
                    buffer.seek_requested.store(false, Ordering::Release);
                    let target = buffer.seek_target_ms.load(Ordering::Relaxed) as f64 / 1000.0;
                    let _ = reader.seek(
                        symphonia::core::formats::SeekMode::Coarse,
                        symphonia::core::formats::SeekTo::Time {
                            time: Time::new(target as u64, target.fract()),
                            track_id: Some(track_id),
                        },
                    );
                    buffer.reset();
                    let new_pos = (target * output_sample_rate as f64) as usize;
                    buffer.playback_position.store(new_pos, Ordering::Release);
                    buffer.decoded_position.store(new_pos, Ordering::Release);
                    resample_pos = 0.0;
                    decoder.reset();
                    continue;
                }

                // Check pause signal
                if buffer.paused.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }

                // Backpressure: if the ring buffer is nearly full, the audio callback
                // hasn't consumed yet.  Just wait briefly — never discard samples, as that
                // creates audible gaps ("plays, stops, plays, stops" stuttering).
                let buffered = buffer.buffered();
                if buffered > buffer.capacity * 3 / 4 {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }

                // Decode next packet
                let packet = match reader.next_packet() {
                    Ok(p) => p,
                    Err(symphonia::core::errors::Error::IoError(ref e))
                        if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                    {
                        // End of file
                        if looping.load(Ordering::Relaxed) {
                            // Seek back to beginning
                            let _ = reader.seek(
                                symphonia::core::formats::SeekMode::Coarse,
                                symphonia::core::formats::SeekTo::Time {
                                    time: Time::new(0, 0.0),
                                    track_id: Some(track_id),
                                },
                            );
                            buffer.playback_position.store(0, Ordering::Release);
                            buffer.decoded_position.store(0, Ordering::Release);
                            resample_pos = 0.0;
                            decoder.reset();
                            continue;
                        }
                        buffer.finished.store(true, Ordering::Release);
                        return Ok(());
                    }
                    Err(_) => {
                        buffer.finished.store(true, Ordering::Release);
                        return Ok(());
                    }
                };

                if packet.track_id() != track_id {
                    continue; // Skip non-audio packets
                }

                let decoded = match decoder.decode(&packet) {
                    Ok(d) => d,
                    Err(_) => continue, // Skip decode errors
                };

                // Get samples as interleaved f32
                let spec = *decoded.spec();
                let num_decoded_frames = decoded.frames();
                if num_decoded_frames == 0 {
                    continue;
                }

                let sb = sample_buf.get_or_insert_with(|| {
                    SampleBuffer::<f32>::new(num_decoded_frames as u64, spec)
                });
                // Ensure capacity
                if sb.capacity() < num_decoded_frames {
                    *sb = SampleBuffer::<f32>::new(num_decoded_frames as u64, spec);
                }
                sb.copy_interleaved_ref(decoded);
                let interleaved = sb.samples();
                let channels = spec.channels.count().max(1);

                // Downmix to mono
                let mono: Vec<f32> = interleaved
                    .chunks(channels)
                    .map(|frame| {
                        let sum: f32 = frame.iter().sum();
                        sum / channels as f32
                    })
                    .collect();

                // Resample if needed, then write to ring buffer
                if needs_resample && mono.len() > 1 {
                    // Linear interpolation resampling
                    let src_len = mono.len() as f64;
                    let mut resampled =
                        Vec::with_capacity((src_len * resample_ratio as f64) as usize + 1);
                    while resample_pos < src_len - 1.0 {
                        let idx = resample_pos as usize;
                        let frac = resample_pos - idx as f64;
                        let s0 = mono[idx];
                        let s1 = mono[(idx + 1).min(mono.len() - 1)];
                        resampled.push(s0 + (s1 - s0) * frac as f32);
                        resample_pos += 1.0 / resample_ratio as f64;
                    }
                    resample_pos -= src_len - 1.0; // carry fractional part to next packet
                    if resample_pos < 0.0 {
                        resample_pos = 0.0;
                    }
                    buffer
                        .decoded_position
                        .fetch_add(resampled.len(), Ordering::Relaxed);
                    buffer.write(&resampled);
                } else {
                    buffer
                        .decoded_position
                        .fetch_add(mono.len(), Ordering::Relaxed);
                    buffer.write(&mono);
                }
            }
        };

    if let Err(e) = open_and_decode(&path, &buffer, 0.0) {
        crate::system_log::error(format!("File decode error: {}", e));
        buffer.finished.store(true, Ordering::Release);
    }
}

/// Probe an audio file for its duration using Symphonia metadata (fast, no full decode).
pub fn probe_file_duration(path: &str) -> Option<f64> {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    crate::capability::check(crate::capability::Capability::Filesystem).ok()?;
    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
    {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()?;
    let track = probed.format.default_track()?;
    let n_frames = track.codec_params.n_frames?;
    let sr = track.codec_params.sample_rate.unwrap_or(44100) as f64;
    Some(n_frames as f64 / sr)
}
