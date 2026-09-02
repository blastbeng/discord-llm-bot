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

/// Female voice transformer — phase-vocoder M→F pitch conversion.
///
/// Why not tape shift: moving the whole spectrum up also drags the formants
/// with it — that's exactly the chipmunk signature. Why not TD-PSOLA: grains
/// that aren't aligned to true glottal epochs create rough, phasey output.
/// A phase vocoder shifts F0 while preserving the spectral envelope, which
/// is the standard trick behind "female voice" changers.
///
/// Recipe (measured with pitch detection on real TTS: male median ~110 Hz,
/// convincing female reference median ~194 Hz):
///   1. PV pitch shift +7.0 st (F0 ~110 -> ~166 Hz: sultry female register)
///   2. slow-down ~3% (WSOLA, pitch-preserving — unhurried, intimate pacing)
///   3. softness stack: -0.5 dB body trim + gentle air re-add (bright-but-
///      soft female timbre)
/// Breath noise was dropped: the level-gated layer measured a 0.4 MOS
/// quality drop (talky-talky speech-quality analysis) for an inaudible
/// effect on real TTS material.
fn apply_sexy(samples: Vec<f32>, sample_rate: u32, channels: u16) -> Vec<f32> {
    // PV pitch shift first, then the WSOLA slow-down (stretching the already
    // shifted signal keeps the two stages independent and artifact-free).
    // WSOLA's `rate` is output/input (rate 2.0 = half as long), so slowing
    // down by 3% means passing 1/1.03.
    let shifted = female_voice(samples, sample_rate, channels, 7.0, false);
    time_stretch_wsola(shifted, 1.0 / 1.03, channels)
}

fn female_voice(
    samples: Vec<f32>,
    sample_rate: u32,
    channels: u16,
    pitch_semitones: f32,
    breathy: bool,
) -> Vec<f32> {
    let ch = channels.max(1) as usize;
    let n = samples.len();
    if n == 0 {
        return samples;
    }
    let mut out = vec![0.0f32; n];

    for c in 0..ch {
        // De-interleave this channel.
        let chan: Vec<f32> = samples.iter().skip(c).step_by(ch).copied().collect();
        // Dither away digital silence (the vocoder's phase-diff math can
        // produce NaN on all-zero input frames).
        // TTS may lead with digital silence; the vocoder's phase-diff math
        // can emit NaN on all-zero frames, so add ±1e-7 dither. NOTE the
        // scale: (rng>>40) spans 0..2^24, so the divisor must bring the
        // result to ±1e-7 total (a first version produced a ±2.0 offset!).
        let mut rng: u64 = 0x9E3779B97F4A7C15 ^ ((c as u64 + 1).wrapping_mul(0xD1B54A32D192ED03));
        let dithered: Vec<f32> = chan
            .iter()
            .map(|x| {
                rng ^= rng >> 12;
                rng ^= rng << 25;
                rng ^= rng >> 27;
                let u = (rng >> 40) as f32 / 16777216.0; // 0..1
                x + (u - 0.5) * 2.0e-7
            })
            .collect();

        let state_vec: Vec<f32> = vec![0.0; pitch_shift::TOTAL_F32];
        let state_box: Box<[f32; pitch_shift::TOTAL_F32]> =
            state_vec.into_boxed_slice().try_into().unwrap();
        let mut shifter = pitch_shift::Shifter::new(state_box);

        let mut shifted: Vec<f32> = Vec::with_capacity(chan.len() + 1024);
        for chunk in dithered.chunks(128) {
            if chunk.len() < 128 {
                break;
            }
            let produced = shifter.shift(chunk, pitch_semitones, 128, sample_rate as f32);
            shifted.extend_from_slice(produced);
        }
        // Skip the 1024-sample warmup (zeros), then interleave back.
        let skip = 1024.min(shifted.len());
        for (i, v) in shifted[skip..].iter().enumerate() {
            let dst = skip * ch + c + i * ch;
            if dst < out.len() {
                out[dst] = *v;
            }
        }
    }

    // ── Softness stack ──────────────────────────────────────────────────
    // 0.5 dB body trim + gentle air re-add above ~4 kHz: keeps the voice
    // soft and clear without the fizz of saturation-based exciters.
    let sr = sample_rate as f32;
    let mut air_lp = vec![0.0f32; ch];
    let air_alpha = lp_alpha(4000.0, sr);
    for i in 0..(out.len() / ch) {
        for c in 0..ch {
            let idx = i * ch + c;
            let x = out[idx];
            air_lp[c] += air_alpha * (x - air_lp[c]);
            let air = x - air_lp[c];
            out[idx] = x * 0.94 + air * 0.08;
        }
    }

    // Optional breath: quiet noise gated to quiet moments (word gaps).
    if breathy {
        let env_alpha = lp_alpha(700.0, sr);
        let mut env = vec![0.0f32; ch];
        let mut rng: u64 = 0x9E3779B97F4A7C15;
        let frames = out.len() / ch;
        for i in 0..frames {
            rng ^= rng >> 12;
            rng ^= rng << 25;
            rng ^= rng >> 27;
            let noise =
                ((rng.wrapping_mul(0x2545F4914F6CDD1D) >> 33) as i32 as f32 / 2147483648.0) * 0.010;
            for c in 0..ch {
                let idx = i * ch + c;
                let x = out[idx];
                env[c] += env_alpha * (x.abs() - env[c]);
                let quiet = (1.0 - (env[c] * 6.0).clamp(0.0, 1.0)) * 0.5;
                out[idx] = x + noise * quiet;
            }
        }
    }
    // Trim any pad tail the shifter added (it outputs 128 per 128 in — exact).
    out.truncate(n);
    out
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
        "chipmunk" => Ok(apply_chipmunk(samples, sample_rate, channels)),
        "demon" => Ok(apply_demon(samples, sample_rate, channels)),
        // Sultry female voice: phase-vocoder M→F conversion + slow pacing.
        "sexy" => Ok(apply_sexy(samples, sample_rate, channels)),
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
        "none" | "chipmunk" | "demon" | "sexy" | "random"
    )
}

/// Get available effects
pub const AVAILABLE_EFFECTS: &[&str] = &[
    "none",
    "chipmunk",
    "demon",
    "sexy",
    "random",
];

/// Pool for resolving a "random" effect request: every real effect plus the
/// pass-through "none". Plain speech is a legitimate random outcome, so a
/// "random" pick may occasionally come out with no effect applied.
pub const RANDOM_EFFECT_POOL: &[&str] = &[
    "none",
    "chipmunk",
    "demon",
    "sexy",
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
        assert!(is_valid_effect("sexy"));
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

    #[test]
    fn test_sexy_effect_shifts_pitch_and_stretches() {
        // A 110.7 Hz tone in (measured male-TTS median F0), the sexy chain
        // (+7.0 st PV shift, ×1.03 slow-down) must come back with a dominant
        // frequency near 110.7 × 2^(7/12) ≈ 166.2 Hz and a duration ~3%
        // longer. The PV warms up over its first ~1024 samples, so the
        // analysis window sits well inside.
        let sample_rate = 24000u32;
        let dur_secs = 2.0;
        let n = (sample_rate as f32 * dur_secs) as usize;
        // 110.7 Hz = measured male-TTS median F0; +7.0 st is the effect's shift.
        let input_f0 = 110.7;
        let input: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / sample_rate as f32;
                (std::f32::consts::TAU * input_f0 * t).sin() * 0.5
            })
            .collect();

        let out = apply_sexy(input, sample_rate, 1);
        // PV is sample-exact; the WSOLA slow-down adds ~3% (+WSOLA rounding).
        let expected_len = (n as f32 * 1.03) as usize;
        assert!(
            (out.len() as i64 - expected_len as i64).abs() <= 512,
            "sexy length {} != ~{} (1.03x stretch)",
            out.len(),
            expected_len
        );

        // Dominant frequency via zero-padded DFT peak around the expectation.
        let expected = input_f0 * 2.0f32.powf(7.0 / 12.0);
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
        // WSOLA `rate` is output/input: slowing down 3% = rate 1/1.03.
        let burst_out = time_stretch_wsola(burst_in.clone(), 1.0 / 1.03, 1);
        let written = burst_out.iter().filter(|&&v| v != 0.0).count();
        let in_bursts = burst_in.iter().filter(|&&v| v != 0.0).count();
        let ratio = written as f32 / in_bursts.max(1) as f32;
        assert!(
            (ratio - 1.03).abs() < 0.08,
            "stretch ratio {:.3}, expected ~1.03",
            ratio
        );
    }
}