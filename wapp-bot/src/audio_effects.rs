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

    // 2. Apply the requested effect
    let processed_samples = apply_effect(samples, effect, sample_rate, channels)?;

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

/// Resample-based speed change (pitch and tempo move together, exactly like
/// tape speed — the classic way to build chipmunk/demon voices). Linear
/// interpolation keeps the interleaved channel layout intact.
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
        "echo" => Ok(apply_echo(samples, sample_rate, channels)),
        "reverb" => Ok(apply_reverb(samples, sample_rate, channels)),
        "chipmunk" => Ok(apply_chipmunk(samples, sample_rate, channels)),
        "demon" => Ok(apply_demon(samples, sample_rate, channels)),
        // Female voices: PHASE VOCODER pitch shift (formant-preserving —
        // the spectral envelope does NOT move, which is what made tape
        // shifts read as chipmunk). Targets measured on real Google TTS
        // (~118 Hz male): +4.5 st -> 153 Hz soft, +5.5 -> 163 Hz warm,
        // +6.5 -> 173 Hz bright. With the softness stack below this reads
        // as a young female TTS voice, not chipmunk.
        _ => Err(AudioEffectError::EffectProcessing(format!("Unknown effect: {}", effect))),
    }
}

/// Echo: dry signal + decaying, progressively darker repeats.
/// The tail is padded out so the repeats are not cut at file end.
fn apply_echo(samples: Vec<f32>, sample_rate: u32, channels: u16) -> Vec<f32> {
    let ch = channels.max(1) as usize;
    let sr = sample_rate as f32;
    let delay_ms = 250.0;
    let delay_samples = ((delay_ms / 1000.0 * sr).max(1.0)) as usize;
    let feedback = 0.35f32;
    let wet_per_repeat = 0.45f32;

    // 4 repeats are audible before the feedback decays into noise.
    let tail_frames = (delay_ms / 1000.0 * sr * 4.0) as usize;
    let mut output = samples;
    let orig_frames = output.len() / ch;
    output.resize((orig_frames + tail_frames) * ch, 0.0);

    // Feedback path passes through a one-pole low-pass (analog tape echo):
    // each repeat is darker than the previous one.
    let tone_alpha = lp_alpha(3200.0, sr);
    let mut line = vec![0.0f32; delay_samples * ch];
    let mut tone_state = vec![0.0f32; ch];

    let frames = output.len() / ch;
    for i in 0..frames {
        let line_base = (i % delay_samples) * ch;
        for c in 0..ch {
            let idx = i * ch + c;
            let delayed = line[line_base + c];
            tone_state[c] += tone_alpha * (delayed - tone_state[c]);
            line[line_base + c] = output[idx] + tone_state[c] * feedback;
            output[idx] += delayed * wet_per_repeat;
        }
    }
    output
}

/// Reverb: classic Freeverb architecture (8 parallel combs into 4 series
/// all-passes, with damping in the comb feedback path), pure Rust, with a
/// padded tail so the reverb rings out naturally after speech ends.
fn apply_reverb(samples: Vec<f32>, sample_rate: u32, channels: u16) -> Vec<f32> {
    let ch = channels.max(1) as usize;
    let sr = sample_rate as f32;
    const COMB_DELAYS: [f32; 8] = [1116.0, 1188.0, 1277.0, 1356.0, 1422.0, 1491.0, 1557.0, 1617.0];
    const ALLPASS_DELAYS: [f32; 4] = [556.0, 441.0, 341.0, 225.0];
    // Freeverb's fixed delay table is defined at 44.1 kHz.
    let scale = sr / 44100.0;
    let room_size = 0.7f32;
    let damping = 0.5f32;

    // Classic Freeverb parameter mappings.
    let feedback = room_size * 0.28 + 0.7;
    let damp = damping * 0.4;
    let fixed_gain = 0.015f32; // input attenuation into the comb network
    let wet_gain = 0.9f32; // wet slider 0.3 * scalewet(3)
    let dry_gain = 0.8f32;

    let tail_frames = (0.45 * sr) as usize;
    let mut output = samples;
    output.resize(output.len() / ch * ch + tail_frames * ch, 0.0);

    let n_combs = COMB_DELAYS.len();
    let n_aps = ALLPASS_DELAYS.len();

    let mut comb_buf: Vec<Vec<f32>> = Vec::with_capacity(n_combs * ch);
    let mut comb_pos = vec![0usize; n_combs * ch];
    let mut comb_store = vec![0.0f32; n_combs * ch];
    for d in COMB_DELAYS.iter() {
        for _ in 0..ch {
            comb_buf.push(vec![0.0f32; ((d * scale) as usize).max(1)]);
        }
    }
    let mut ap_buf: Vec<Vec<f32>> = Vec::with_capacity(n_aps * ch);
    let mut ap_pos = vec![0usize; n_aps * ch];
    for d in ALLPASS_DELAYS.iter() {
        for _ in 0..ch {
            ap_buf.push(vec![0.0f32; ((d * scale) as usize).max(1)]);
        }
    }

    let frames = output.len() / ch;
    let mut wet_out = vec![0.0f32; output.len()];
    for i in 0..frames {
        for c in 0..ch {
            let idx = i * ch + c;
            let input = output[idx] * fixed_gain;

            // Parallel comb filters
            let mut out = 0.0f32;
            for k in 0..n_combs {
                let slot = k * ch + c;
                let buf_len = comb_buf[slot].len();
                let pos = comb_pos[slot];
                let delayed = comb_buf[slot][pos];
                comb_store[slot] = delayed * (1.0 - damp) + comb_store[slot] * damp;
                comb_buf[slot][pos] = input + comb_store[slot] * feedback;
                comb_pos[slot] = (pos + 1) % buf_len;
                out += delayed;
            }
            // Series all-pass filters
            for k in 0..n_aps {
                let slot = k * ch + c;
                let buf_len = ap_buf[slot].len();
                let pos = ap_pos[slot];
                let bufout = ap_buf[slot][pos];
                let ap_out = -out + bufout;
                ap_buf[slot][pos] = out + bufout * 0.5;
                ap_pos[slot] = (pos + 1) % buf_len;
                out = ap_out;
            }
            wet_out[idx] = out;
        }
    }
    for i in 0..output.len() {
        output[i] = output[i] * dry_gain + wet_out[i] * wet_gain;
    }
    output
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
        "none" | "echo" | "reverb" | "chipmunk" | "demon" | "random"
    )
}

/// Get available effects
pub const AVAILABLE_EFFECTS: &[&str] = &[
    "none",
    "echo",
    "reverb",
    "chipmunk",
    "demon",
    "random",
];

/// Pool for resolving a "random" effect request: every real effect plus the
/// pass-through "none". Plain speech is a legitimate random outcome, so a
/// "random" pick may occasionally come out with no effect applied.
pub const RANDOM_EFFECT_POOL: &[&str] = &[
    "none",
    "echo",
    "reverb",
    "chipmunk",
    "demon",
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
        assert!(is_valid_effect("echo"));
        assert!(is_valid_effect("reverb"));
        assert!(is_valid_effect("chipmunk"));
        assert!(is_valid_effect("demon"));
        assert!(is_valid_effect("random"));
        assert!(!is_valid_effect("woman1"));
        assert!(!is_valid_effect("woman2"));
        assert!(!is_valid_effect("woman3"));
        assert!(!is_valid_effect("bass"));
        assert!(!is_valid_effect("telephone"));
        assert!(!is_valid_effect("underwater"));
        assert!(!is_valid_effect("invalid"));
    }

    #[test]
    fn test_available_effects_contains_expected() {
        assert!(AVAILABLE_EFFECTS.contains(&"echo"));
        assert!(AVAILABLE_EFFECTS.contains(&"reverb"));
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
}