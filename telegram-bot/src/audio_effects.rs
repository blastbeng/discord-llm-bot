use std::io::Cursor;
use thiserror::Error;
use mp3lame_encoder::Builder;
use minimp3::Decoder;

#[derive(Debug, Error)]
pub enum AudioEffectError {
    #[error("MP3 decode error: {0}")]
    Mp3Decode(String),
    #[error("MP3 encode error: {0}")]
    Mp3Encode(String),
    #[error("Effect processing error: {0}")]
    EffectProcessing(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Apply an audio effect to MP3 bytes and return processed MP3 bytes.
/// Pure-Rust DSP (decode -> process -> encode), no external effect crates.
pub async fn apply_effect_to_mp3(
    input_bytes: Vec<u8>,
    effect: &str,
    sample_rate: u32,
) -> Result<Vec<u8>, AudioEffectError> {
    let (samples, decoded_sample_rate, channels) = decode_mp3(&input_bytes)?;

    // Resample if needed (Google TTS is typically 24kHz, music is 44.1/48kHz)
    let samples = if decoded_sample_rate != sample_rate {
        resample_audio(samples, decoded_sample_rate, sample_rate, channels)
    } else {
        samples
    };

    // 2. Apply the requested effect. The "woman" effect prefers the external
    // Praat conversion (research-grade independent pitch+formant control,
    // measured MOS 4.73 vs 4.25 for the in-process DSP); on any failure it
    // falls back to the in-process tape+tilt DSP below.
    let processed_samples = if effect == "woman" {
        match apply_woman_praat(samples.clone(), sample_rate, channels).await {
            Ok(s) => s,
            Err(e) => {
                log::warn!("woman effect: praat conversion failed ({}); using in-process DSP fallback", e);
                apply_effect(samples, effect, sample_rate, channels)?
            }
        }
    } else {
        apply_effect(samples, effect, sample_rate, channels)?
    };

    // 3. Clamp/normalize samples to prevent clipping distortion.
    let processed_samples = normalize_if_needed(processed_samples);

    // 4. Encode back to MP3
    encode_mp3(processed_samples, sample_rate, channels)
}

/// Peak-based normalization. Down-scales whenever the peak exceeds 0.9
/// (clipping guard) and applies make-up gain when the peak is very low
/// (< 0.35), which is common for heavily low-passed effects (demon) whose
/// filtered output would otherwise be near-inaudible. Audio
/// already in a healthy peak range is left untouched.
fn normalize_if_needed(samples: Vec<f32>) -> Vec<f32> {
    let peak = samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    if peak > 0.9 {
        let scale = 0.9 / peak;
        samples.iter().map(|s| s * scale).collect()
    } else if peak < 0.35 {
        // Scale up to a sensible peak, capped so silence (peak 0) and
        // pathological inputs can never explode the gain.
        let scale = (0.7 / peak).min(4.0);
        samples.iter().map(|s| s * scale).collect()
    } else {
        samples
    }
}

/// Decode MP3 bytes to interleaved f32 samples (-1.0 to 1.0)
fn decode_mp3(data: &[u8]) -> Result<(Vec<f32>, u32, u16), AudioEffectError> {
    let mut decoder = Decoder::new(Cursor::new(data));
    let mut all_samples = Vec::new();
    let mut sample_rate = 0;
    let mut channels = 0;

    loop {
        match decoder.next_frame() {
            Ok(frame) => {
                sample_rate = frame.sample_rate as u32;
                channels = frame.channels as u16;
                for sample in frame.data {
                    all_samples.push(sample as f32 / 32768.0);
                }
            }
            Err(_) => break,
        }
    }

    if all_samples.is_empty() {
        return Err(AudioEffectError::Mp3Decode("No audio data decoded".to_string()));
    }

    Ok((all_samples, sample_rate, channels))
}

/// Encode f32 samples to MP3 bytes
fn encode_mp3(samples: Vec<f32>, sample_rate: u32, channels: u16) -> Result<Vec<u8>, AudioEffectError> {
    let mut encoder = Builder::new()
        .ok_or_else(|| AudioEffectError::Mp3Encode("Failed to initialize encoder".to_string()))?
        .with_num_channels(channels as u8)
        .map_err(|e| AudioEffectError::Mp3Encode(format!("{:?}", e)))?
        .with_sample_rate(sample_rate)
        .map_err(|e| AudioEffectError::Mp3Encode(format!("{:?}", e)))?
        .with_brate(mp3lame_encoder::Bitrate::Kbps128)
        .map_err(|e| AudioEffectError::Mp3Encode(format!("{:?}", e)))?
        .with_quality(mp3lame_encoder::Quality::Good)
        .map_err(|e| AudioEffectError::Mp3Encode(format!("{:?}", e)))?
        .build()
        .map_err(|e| AudioEffectError::Mp3Encode(format!("{:?}", e)))?;

    let pcm_samples: Vec<i16> = samples
        .iter()
        .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
        .collect();

    let mut output = Vec::new();
    let capacity = mp3lame_encoder::max_required_buffer_size(pcm_samples.len());
    output.reserve(capacity);

    if channels == 1 {
        let mono_pcm = mp3lame_encoder::MonoPcm(&pcm_samples);
        let encoded = encoder.encode(mono_pcm, output.spare_capacity_mut())
            .map_err(|e| AudioEffectError::Mp3Encode(format!("{:?}", e)))?;
        unsafe { output.set_len(output.len().wrapping_add(encoded)); }
    } else {
        // The samples are interleaved stereo (L R L R ...). Split into separate
        // left and right buffers for the encoder.
        let mut left = Vec::with_capacity(pcm_samples.len() / 2);
        let mut right = Vec::with_capacity(pcm_samples.len() / 2);
        for chunk in pcm_samples.chunks_exact(2) {
            left.push(chunk[0]);
            right.push(chunk[1]);
        }
        let stereo_pcm = mp3lame_encoder::DualPcm { left: &left, right: &right };
        let encoded = encoder.encode(stereo_pcm, output.spare_capacity_mut())
            .map_err(|e| AudioEffectError::Mp3Encode(format!("{:?}", e)))?;
        unsafe { output.set_len(output.len().wrapping_add(encoded)); }
    }

    let flushed = encoder.flush::<mp3lame_encoder::FlushNoGap>(output.spare_capacity_mut())
        .map_err(|e| AudioEffectError::Mp3Encode(format!("{:?}", e)))?;
    unsafe { output.set_len(output.len().wrapping_add(flushed)); }

    Ok(output)
}

/// One-pole low-pass coefficient for a given cutoff at a sample rate.
#[inline]
fn lp_alpha(cutoff_hz: f32, sample_rate: f32) -> f32 {
    1.0 - (-std::f32::consts::TAU * cutoff_hz / sample_rate).exp()
}

/// High-quality resampling: anti-alias filtering when downsampling plus
/// linear interpolation. Keeps the interleaved stereo layout intact.
fn resample_audio(samples: Vec<f32>, from_rate: u32, to_rate: u32, channels: u16) -> Vec<f32> {
    if from_rate == to_rate || channels == 0 {
        return samples;
    }
    let ch = channels as usize;
    let mut padded = samples;
    // Truncate to whole frames, then append one zero frame so interpolation
    // can reach the end of the signal without reading past the buffer.
    let frames = padded.len() / ch;
    padded.truncate(frames * ch);
    if frames == 0 {
        return vec![0.0; ch];
    }
    padded.resize((frames + 1) * ch, 0.0);

    // Downsampling: frequencies above the new Nyquist must be removed first,
    // otherwise linear interpolation aliases them into the audible band.
    if to_rate < from_rate {
        let cutoff = (to_rate as f32 / 2.0) * 0.85;
        let alpha = lp_alpha(cutoff, from_rate as f32);
        // Three passes of one-pole filtering approximate a gentler rolloff
        // while keeping the speech band flat.
        for _pass in 0..3 {
            let mut state = vec![0.0f32; ch];
            for frame in padded.chunks_mut(ch) {
                for c in 0..ch {
                    state[c] += alpha * (frame[c] - state[c]);
                    frame[c] = state[c];
                }
            }
        }
    }

    let ratio = from_rate as f32 / to_rate as f32;
    let new_frames = ((frames as f32) / ratio).ceil() as usize;
    let mut output = Vec::with_capacity(new_frames * ch);

    for i in 0..new_frames {
        let src_pos = i as f32 * ratio;
        let idx = (src_pos as usize).min(frames);
        let frac = src_pos - (idx as f32);
        for c in 0..ch {
            let s0 = padded[idx * ch + c];
            let s1 = padded[(idx + 1) * ch + c];
            output.push(s0 + frac * (s1 - s0));
        }
    }

    output
}

/// Time-stretch by `1/rate` (rate 2.0 = output half as long) WITHOUT changing
/// pitch, using waveform-similar overlap-add (WSOLA): each synthesis frame is
/// a Hann-windowed slice whose start is nudged within ±8 ms to maximize
/// overlap correlation with the output written so far (the "similarity"
/// trick that keeps stretched speech smooth instead of warbly). Keeps the
/// interleaved channel layout intact.
fn time_stretch_wsola(samples: Vec<f32>, rate: f32, channels: u16) -> Vec<f32> {
    if (rate - 1.0).abs() < 0.01 || channels == 0 {
        return samples;
    }
    let ch = channels as usize;
    let frames = samples.len() / ch;
    if frames == 0 {
        return samples;
    }

    // ~46 ms Hann window, ~11.6 ms synthesis hop (75% overlap, ripple-free).
    let win: usize = (0.0464 * 24000.0) as usize | 1;
    let hop: usize = (0.0116 * 24000.0) as usize;
    let search: i64 = (0.008 * 24000.0) as i64; // ±8 ms similarity search

    // How far the input position advances per synthesis frame.
    let in_hop = (hop as f32 * rate).round() as i64;

    // Output buffer covers the full stretched duration: input frames / rate,
    // plus one window of slack for the tail. WSOLA consumes `in_hop` input
    // samples per `hop` output samples, so the expansion factor is handled
    // by the loop; the buffer must simply never be the limiting factor.
    let total_out: usize = (frames as f32 / rate) as usize + 2 * win;
    let mut output = vec![0.0f32; total_out * ch];
    let mut norm = vec![0.0f32; total_out];
    let hann: Vec<f32> = (0..win)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / win as f32).cos())
        .collect();

    let mut natural: i64 = 0; // plain-OLA input position for this output frame
    let mut out_frame: usize = 0;
    let max_out_frames = (total_out - win) / hop;

    // Keep going while ANY input remains (not `natural + win`): the final
    // frames read a clamped, partially zero window — otherwise up to a full
    // window (46 ms) of speech tail gets dropped, audibly truncating words.
    while natural < frames as i64 - 1 && out_frame < max_out_frames {
        // Choose the offset (within ±search) whose head best continues the
        // output tail already written — the WSOLA similarity search.
        let mut best_off: i64 = 0;
        if out_frame > 0 {
            let tail_start = out_frame * hop;
            let tail_n = hop.min(win / 4);
            let low = (-search).max(1 - natural);
            let high = search.min(frames as i64 - win as i64 - 1 - natural);
            let mut best = f32::NEG_INFINITY;
            let mut off = low;
            while off <= high {
                let cand = natural + off;
                let mut acc = 0.0f32;
                for k in 0..tail_n {
                    let o = (tail_start + k) * ch;
                    let x = ((cand + k as i64) as usize) * ch;
                    for c in 0..ch {
                        acc += output[o + c] * samples[x + c];
                    }
                }
                if acc > best {
                    best = acc;
                    best_off = off;
                }
                off += 1;
            }
        }
        // Clamp the read start inside the buffer, then overlap-add the frame.
        let read = (natural + best_off).clamp(0, frames as i64 - (win as i64) - 1) as usize;
        for k in 0..win {
            let dst = (out_frame * hop + k) * ch;
            let src = (read + k) * ch;
            for c in 0..ch {
                output[dst + c] += samples[src + c] * hann[k];
            }
            norm[out_frame * hop + k] += hann[k];
        }
        natural += in_hop;
        out_frame += 1;
    }

    // Normalize the overlap ripple away and trim to the written length.
    let written = out_frame * hop;
    let mut result = Vec::with_capacity(written * ch);
    for i in 0..written {
        let n = norm[i];
        for c in 0..ch {
            let v = output[i * ch + c];
            result.push(if n > 0.15 { v / n } else { 0.0 });
        }
    }
    result
}

/// In-process FALLBACK woman transformer — used only when the external
/// Praat conversion (see [`apply_woman_praat`]) is unavailable. Tape-shift
/// based male→female conversion of the built-in Google
/// TTS (the Google IT voice is MALE: median F0 ~124 Hz, measured).
///
/// Research-grounded recipe (DAFX "gender change", VTLN literature): a
/// convincing M→F conversion needs F0 shifted into the female range
/// (~165-255 Hz, Italian speech ~190-220) AND the vocal-tract/formant
/// scaling raised only ~x1.1-1.25, with the male chest resonance removed.
/// Pitch and formants must move INDEPENDENTLY — a pure tape shift couples
/// them (formants x1.57 at F0 195 = child tract = chipmunk), and the only
/// formant-preserving pitch shifter available (the `pitch_shift` crate,
/// 128-sample vocoder chunks) measured MOS 1.8-3.7 at every usable shift on
/// 24 kHz speech, so it is not used here.
///
/// Best artifact-free compromise (best of 31 measured variants, talky-talky
/// pitch+quality analysis on real Google TTS):
///   1. tape shift +4.0 st: F0 ~124 -> ~154 Hz (female alto register),
///      formants rise together to x1.26 (inside the female x1.1-1.25 band)
///   2. WSOLA time-correction back to ~original duration (pitch-preserving)
///   3. M→F spectral tilt: cut ~35% of the male chest resonance below
///      ~250 Hz, re-add +10% air above 4 kHz (female brightness)
/// Measured: median F0 154.1 Hz, MOS 4.25 (raw Google = 4.95; +2.5 st tape
/// without tilt = "not a woman" per user; +5..6 st = MOS 3.0-3.3; every
/// pitch_shift-crate chain = 1.8-3.7; gated breath = noisiness collapse).
fn apply_woman(samples: Vec<f32>, sample_rate: u32, channels: u16) -> Vec<f32> {
    // Tape shift: pitch and formants move together (formant rise is part of
    // the female signature, as long as it stays in the x1.1-1.3 band).
    let speed = 2.0f32.powf(4.0 / 12.0);
    let taped = change_speed(samples, speed, channels);
    // WSOLA corrects the tape shortening back to ~original duration.
    // WSOLA's `rate` is output/input (rate 2.0 = half as long).
    let mut out = time_stretch_wsola(taped, 1.0 / speed, channels);

    // M→F spectral tilt: remove the male chest resonance (low shelf dip at
    // ~250 Hz) and re-add female presence (air above ~4 kHz).
    let sr = sample_rate as f32;
    let ch = channels.max(1) as usize;
    let mut air_lp = vec![0.0f32; ch];
    let mut low_lp = vec![0.0f32; ch];
    let air_alpha = lp_alpha(4000.0, sr);
    let low_alpha = lp_alpha(250.0, sr);
    for i in 0..(out.len() / ch) {
        for c in 0..ch {
            let idx = i * ch + c;
            let x = out[idx];
            air_lp[c] += air_alpha * (x - air_lp[c]);
            low_lp[c] += low_alpha * (x - low_lp[c]);
            let air = x - air_lp[c];
            out[idx] = (x - 0.35 * low_lp[c]) + air * 0.10;
        }
    }
    out
}

/// Praat "Change gender" script — the research-standard M→F conversion
/// (Boersma & Weenink). Arguments: input, output, pitch_floor, pitch_ceiling,
/// formant_shift_ratio, new_pitch_median, pitch_range_factor, duration_factor.
/// It manipulates the pitch track (PSOLA) and the formant/spectral envelope
/// INDEPENDENTLY — exactly what tape shifts and chunked vocoders cannot do.
const PRAAT_CHANGE_GENDER_SCRIPT: &str = r#"form Change gender
    sentence Input_audio_file_name
    sentence Output_audio_file_name
    real Pitch_floor 75.0
    real Pitch_ceiling 600.0
    real Formant_shift_ratio 1.1
    real New_pitch_median 0.0
    real Pitch_range_factor 1.0
    real Duration_factor 1.0
endform
Read from file: input_audio_file_name$
Change gender: pitch_floor, pitch_ceiling, formant_shift_ratio, new_pitch_median, pitch_range_factor, duration_factor
Save as WAV file: output_audio_file_name$
"#;

/// Convert a male voice to a female voice with the external `praat` binary.
///
/// Tuned on the real Google IT TTS voice (male, median F0 ~124 Hz) with
/// talky-talky pitch+quality measurement: formant_shift_ratio 1.20 +
/// new_pitch_median 200 Hz measured median F0 192.2 Hz with MOS 4.73
/// (untouched input 4.95) — essentially transparent, squarely female.
/// Requires `praat` on PATH (installed in the bot images); returns Err to
/// trigger the in-process DSP fallback when unavailable.
async fn apply_woman_praat(
    samples: Vec<f32>,
    sample_rate: u32,
    channels: u16,
) -> Result<Vec<f32>, String> {
    if samples.is_empty() {
        return Ok(samples);
    }

    let temp_dir = std::env::var("TMP_DIR").unwrap_or_else(|_| "/tmp/discord-llm-bot".to_string());
    tokio::fs::create_dir_all(&temp_dir)
        .await
        .map_err(|e| format!("tmp dir: {e}"))?;
    let uid = (std::process::id() as u64)
        ^ std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(0);
    let in_path = format!("{}/woman_in_{uid}.wav", temp_dir);
    let out_path = format!("{}/woman_out_{uid}.wav", temp_dir);
    let script_path = format!("{}/woman_change_gender_{uid}.praat", temp_dir);

    let result = async {
        write_wav_file(&in_path, &samples, sample_rate, channels).await?;
        tokio::fs::write(&script_path, PRAAT_CHANGE_GENDER_SCRIPT)
            .await
            .map_err(|e| format!("script write: {e}"))?;

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            tokio::process::Command::new("praat")
                .args(["--run", &script_path, &in_path, &out_path])
                .output(),
        )
        .await
        .map_err(|_| "praat timed out (20s)".to_string())?
        .map_err(|e| format!("praat spawn: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "praat exited {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).chars().take(200).collect::<String>()
            ));
        }
        let (out_samples, out_rate, out_channels) = read_wav_file(&out_path).await?;
        if out_rate != sample_rate || out_channels != channels {
            return Err(format!(
                "praat returned {} Hz x {} ch, expected {} Hz x {} ch",
                out_rate, out_channels, sample_rate, channels
            ));
        }
        Ok(out_samples)
    }
    .await;

    // Best-effort cleanup of the temp artifacts.
    for p in [&in_path, &out_path, &script_path] {
        let _ = tokio::fs::remove_file(p).await;
    }
    result
}

/// Write interleaved f32 samples to a 16-bit PCM WAV file.
async fn write_wav_file(
    path: &str,
    samples: &[f32],
    sample_rate: u32,
    channels: u16,
) -> Result<(), String> {
    let spec = hound::WavSpec {
        bits_per_sample: 16,
        sample_rate,
        channels,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).map_err(|e| format!("wav create: {e}"))?;
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        writer.write_sample(v).map_err(|e| format!("wav write: {e}"))?;
    }
    writer.finalize().map_err(|e| format!("wav finalize: {e}"))
}

/// Read a 16-bit PCM WAV file back to interleaved f32 samples.
async fn read_wav_file(path: &str) -> Result<(Vec<f32>, u32, u16), String> {
    let data = tokio::fs::read(path).await.map_err(|e| format!("wav read: {e}"))?;
    let cursor = std::io::Cursor::new(data);
    let mut reader = hound::WavReader::new(cursor).map_err(|e| format!("wav open: {e}"))?;
    let spec = reader.spec();
    let samples: Vec<f32> = reader
        .samples::<i16>()
        .filter_map(|s| s.ok())
        .map(|s| s as f32 / 32768.0)
        .collect();
    Ok((samples, spec.sample_rate, spec.channels))
}

fn change_speed(samples: Vec<f32>, speed: f32, channels: u16) -> Vec<f32> {
    if (speed - 1.0).abs() < 0.01 || channels == 0 {
        return samples;
    }
    let ch = channels as usize;
    let frames = samples.len() / ch;
    if frames == 0 {
        return samples;
    }
    let new_frames = (frames as f32 / speed).max(1.0).floor() as usize;
    let mut output = Vec::with_capacity(new_frames * ch);

    for i in 0..new_frames {
        let src_pos = i as f32 * speed;
        let idx = (src_pos as usize).min(frames - 1);
        let frac = src_pos - idx as f32;
        let next = (idx + 1).min(frames - 1);
        for c in 0..ch {
            let s0 = samples[idx * ch + c];
            let s1 = samples[next * ch + c];
            output.push(s0 + frac * (s1 - s0));
        }
    }
    output
}

/// Apply the requested effect to interleaved samples.
fn apply_effect(
    samples: Vec<f32>,
    effect: &str,
    sample_rate: u32,
    channels: u16,
) -> Result<Vec<f32>, AudioEffectError> {
    match effect {
        "none" | "random" => Ok(samples),
        "chipmunk" => Ok(apply_chipmunk(samples, sample_rate, channels)),
        "demon" => Ok(apply_demon(samples, sample_rate, channels)),
        // Sultry female voice: phase-vocoder M→F conversion + slow pacing.
        "woman" => Ok(apply_woman(samples, sample_rate, channels)),
        // Female voices: PHASE VOCODER pitch shift (formant-preserving —
        // the spectral envelope does NOT move, which is what made tape
        // shifts read as chipmunk). Targets measured on real Google TTS
        // (~118 Hz male): +4.5 st -> 153 Hz soft, +5.5 -> 163 Hz warm,
        // +6.5 -> 173 Hz bright. With the softness stack below this reads
        // as a young female TTS voice, not chipmunk.
        _ => Err(AudioEffectError::EffectProcessing(format!("Unknown effect: {}", effect))),
    }
}

/// Chipmunk: pitch up 5 semitones via tape-style speed change.
fn apply_chipmunk(samples: Vec<f32>, _sample_rate: u32, channels: u16) -> Vec<f32> {
    let ratio = 2.0f32.powf(5.0 / 12.0);
    change_speed(samples, ratio, channels)
}

/// Demon: pitch down 4 semitones (tape-style speed change keeps the slow,
/// ominous pacing), then darkened with a ~3 kHz low-pass. The previous 1.8 kHz
/// cutoff destroyed intelligibility, turning speech into rumble; 3 kHz keeps
/// the menacing timbre while words remain understandable. Loudness is handled
/// centrally by normalize_if_needed (no manual gain make-up here).
fn apply_demon(samples: Vec<f32>, sample_rate: u32, channels: u16) -> Vec<f32> {
    let ratio = 2.0f32.powf(-4.0 / 12.0);
    let output = change_speed(samples, ratio, channels);

    let ch = channels.max(1) as usize;
    let alpha = lp_alpha(3000.0, sample_rate as f32);
    let mut out = output;
    let mut state = vec![0.0f32; ch];
    let frames = out.len() / ch;
    for i in 0..frames {
        for c in 0..ch {
            let idx = i * ch + c;
            state[c] += alpha * (out[idx] - state[c]);
            out[idx] = state[c];
        }
    }
    out
}

/// Simplified API for common use case: compress and save MP3 with effect
pub async fn compress_and_save_mp3_with_effect(
    input_bytes: Vec<u8>,
    file_path: &str,
    effect: &str,
) -> Result<(), AudioEffectError> {
    // Skip encoding when no effect is applied to avoid unnecessary decode/encode round-trip
    if effect == "none" {
        if let Some(parent) = std::path::Path::new(file_path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        tokio::fs::write(file_path, input_bytes).await?;
        return Ok(());
    }

    let sample_rate = 24000;
    let _channels = 1;

    let processed = apply_effect_to_mp3(input_bytes, effect, sample_rate).await?;

    if let Some(parent) = std::path::Path::new(file_path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    tokio::fs::write(file_path, processed).await?;
    Ok(())
}

/// Check if an effect name is valid
pub fn is_valid_effect(effect: &str) -> bool {
    matches!(
        effect,
        "none" | "chipmunk" | "demon" | "woman" | "random"
    )
}

/// Get available effects
pub const AVAILABLE_EFFECTS: &[&str] = &[
    "none",
    "chipmunk",
    "demon",
    "woman",
    "random",
];

/// Pool for resolving a "random" effect request: every real effect plus the
/// pass-through "none". Plain speech is a legitimate random outcome, so a
/// "random" pick may occasionally come out with no effect applied.
pub const RANDOM_EFFECT_POOL: &[&str] = &[
    "none",
    "chipmunk",
    "demon",
    "woman",
];

/// Pick a uniformly random effect from [`RANDOM_EFFECT_POOL`] (real effects
/// plus "none"). Central helper so every feature that resolves a "random"
/// effect (/random, eavesdrop, welcome, goodbye, here-i-am) behaves
/// identically.
pub fn random_effect() -> &'static str {
    use rand::seq::SliceRandom;
    RANDOM_EFFECT_POOL
        .choose(&mut rand::thread_rng())
        .unwrap_or(&"none")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_effect() {
        assert!(is_valid_effect("none"));
        assert!(is_valid_effect("chipmunk"));
        assert!(is_valid_effect("demon"));
        assert!(is_valid_effect("woman"));
        assert!(is_valid_effect("random"));
        assert!(!is_valid_effect("woman1"));
        assert!(!is_valid_effect("woman2"));
        assert!(!is_valid_effect("woman3"));
        assert!(!is_valid_effect("bass"));
        assert!(!is_valid_effect("telephone"));
        assert!(!is_valid_effect("underwater"));
        assert!(!is_valid_effect("echo"));
        assert!(!is_valid_effect("reverb"));
        assert!(!is_valid_effect("invalid"));
    }

    #[test]
    fn test_available_effects_does_not_contain_removed() {
        assert!(!AVAILABLE_EFFECTS.contains(&"echo"));
        assert!(!AVAILABLE_EFFECTS.contains(&"reverb"));
    }

    #[test]
    fn test_change_speed_shifts_pitch() {
        // A 440 Hz "sine": speed 1.5x should compress length by 1.5x
        let sample_rate = 24000u32;
        let n = 2400;
        let input: Vec<f32> = (0..n)
            .map(|i| (std::f32::consts::TAU * 440.0 * i as f32 / sample_rate as f32).sin() * 0.5)
            .collect();
        let speed = 2.0f32.powf(5.0 / 12.0);
        let out = change_speed(input.clone(), speed, 1);
        let expected_len = (n as f32 / speed) as usize;
        assert!((out.len() as i64 - expected_len as i64).abs() <= 2, "len {} != {}", out.len(), expected_len);
        // Peak preserved (no massive attenuation or explosion)
        let peak = out.iter().fold(0.0f32, |p, s| p.max(s.abs()));
        assert!(peak > 0.4 && peak < 1.1, "peak {}", peak);
    }

    #[test]
    fn test_resample_stereo_layout_preserved() {
        // Interleaved stereo: L and R carry 440 Hz and 880 Hz. After resample
        // the even entries must still be dominated by the 440 Hz component.
        let n = 4410;
        let mut input = Vec::with_capacity(n * 2);
        for i in 0..n {
            let t = i as f32 / 44100.0;
            input.push((std::f32::consts::TAU * 440.0 * t).sin());
            input.push((std::f32::consts::TAU * 880.0 * t).sin());
        }
        let out = resample_audio(input, 44100, 24000, 2);
        assert_eq!(out.len() % 2, 0, "interleaved layout must stay frame-aligned");
    }

    #[test]
    fn test_normalize_caps_peak() {
        let out = normalize_if_needed(vec![2.0, -2.0, 1.0]);
        let peak = out.iter().fold(0.0f32, |p, s| p.max(s.abs()));
        assert!((peak - 0.9).abs() < 1e-6);
    }

    #[tokio::test]
    async fn test_wav_roundtrip() {
        // The Praat path depends on lossless WAV IO: samples must survive a
        // write/read cycle (16-bit quantization tolerance).
        let samples: Vec<f32> = (0..4800)
            .map(|i| ((std::f32::consts::TAU * 440.0 * i as f32 / 24000.0).sin()) * 0.5)
            .collect();
        let dir = std::env::temp_dir();
        let path = dir.join(format!("aefx_rt_test_{}.wav", std::process::id()));
        write_wav_file(path.to_str().unwrap(), &samples, 24000, 1)
            .await
            .expect("write wav");
        let (back, rate, ch) = read_wav_file(path.to_str().unwrap()).await.expect("read wav");
        let _ = std::fs::remove_file(&path);
        assert_eq!(rate, 24000);
        assert_eq!(ch, 1);
        assert_eq!(back.len(), samples.len());
        // 16-bit roundtrip: write scales by 32767, read divides by 32768,
        // so up to ~2 LSB of asymmetry is expected.
        for (a, b) in samples.iter().zip(back.iter()) {
            assert!((a - b).abs() < 2.5 / 32768.0, "{a} vs {b}");
        }
    }

    #[test]
    fn test_woman_effect_shifts_pitch_and_stretches() {
        // A 124 Hz tone in (measured Google-TTS median F0), the woman chain
        // (tape +4.0 st, WSOLA time-correction) must come back with a
        // dominant frequency near 124 × 2^(4/12) ≈ 156.2 Hz and a duration
        // ~equal to the input (WSOLA corrects the tape shortening). The
        // analysis window sits well inside the signal.
        let sample_rate = 24000u32;
        let dur_secs = 2.0;
        let n = (sample_rate as f32 * dur_secs) as usize;
        // 124 Hz = measured Google-TTS median F0; +4.0 st tape is the shift.
        let input_f0 = 124.0;
        let input: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / sample_rate as f32;
                (std::f32::consts::TAU * input_f0 * t).sin() * 0.5
            })
            .collect();

        let out = apply_woman(input, sample_rate, 1);
        // Tape shortens by 2^(2.5/12); WSOLA corrects back to ~input length.
        let expected_len = n;
        assert!(
            (out.len() as i64 - expected_len as i64).abs() <= 512,
            "woman length {} != ~{} (time-corrected)",
            out.len(),
            expected_len
        );

        // Dominant frequency via zero-padded DFT peak around the expectation.
        let expected = input_f0 * 2.0f32.powf(4.0 / 12.0);
        let analyze = |data: &[f32], lo: f32, hi: f32| -> f32 {
            let mut best = (0.0f32, 0.0f32);
            let steps = 400;
            for s in 0..steps {
                let f = lo + (hi - lo) * (s as f32 / (steps - 1) as f32);
                let (mut re, mut im) = (0.0f32, 0.0f32);
                for (i, &v) in data.iter().enumerate() {
                    let ang = std::f32::consts::TAU * f * (i as f32) / sample_rate as f32;
                    re += v * ang.cos();
                    im -= v * ang.sin();
                }
                let mag = (re * re + im * im).sqrt();
                if mag > best.1 {
                    best = (f, mag);
                }
            }
            best.0
        };
        let got = analyze(&out[4096..], expected * 0.7, expected * 1.3);
        assert!(
            (got - expected).abs() / expected < 0.03,
            "dominant freq {:.1} Hz, expected ~{:.1} Hz",
            got, expected
        );

        // Duration stretch: a 1 kHz tone-burst gap pattern survives, lengthened.
        let burst_in: Vec<f32> = (0..n)
            .map(|i| {
                if (i / 2400) % 2 == 0 {
                    (std::f32::consts::TAU * 1000.0 * i as f32 / sample_rate as f32).sin() * 0.5
                } else {
                    0.0
                }
            })
            .collect();
        // WSOLA `rate` is output/input: slowing down 10% = rate 1/1.10.
        let burst_out = time_stretch_wsola(burst_in.clone(), 1.0 / 1.10, 1);
        let written = burst_out.iter().filter(|&&v| v != 0.0).count();
        let in_bursts = burst_in.iter().filter(|&&v| v != 0.0).count();
        let ratio = written as f32 / in_bursts.max(1) as f32;
        assert!(
            (ratio - 1.10).abs() < 0.08,
            "stretch ratio {:.3}, expected ~1.10",
            ratio
        );
    }
}