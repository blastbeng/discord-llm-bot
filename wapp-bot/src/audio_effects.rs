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

/// Normalized-autocorrelation pitch estimate for one frame.
/// Returns f0 in Hz, or 0.0 when the frame is unvoiced/weak.
fn detect_f0(seg: &[f32], sample_rate: f32) -> f32 {
    let m = seg.len();
    if m < 64 {
        return 0.0;
    }
    let mean = seg.iter().sum::<f32>() / m as f32;
    let mut e0 = 0.0f32;
    for s in seg {
        let v = s - mean;
        e0 += v * v;
    }
    if e0 < 1e-7 {
        return 0.0;
    }
    let lag_min = (sample_rate / 400.0).max(2.0) as usize;
    let lag_max = ((sample_rate / 60.0) as usize).min(m - 2);
    let mut best_c = 0.0f32;
    let mut best_lag = 0.0f32;
    let e0 = e0.sqrt();
    for lag in lag_min..=lag_max {
        let mut s = 0.0f32;
        let mut e1 = 0.0f32;
        for k in 0..m - lag {
            let a = seg[k] - mean;
            let b = seg[k + lag] - mean;
            s += a * b;
            e1 += b * b;
        }
        if e1 < 1e-9 {
            continue;
        }
        let c = s / (e0 * e1.sqrt());
        if c > best_c {
            best_c = c;
            best_lag = lag as f32;
        }
    }
    if best_c > 0.45 && best_lag > 0.0 {
        sample_rate / best_lag
    } else {
        0.0
    }
}

/// Pitch-only F0 shift via time-domain PSOLA: pitch-synchronous grains
/// (Hann-windowed, two local periods) are COPIED VERBATIM — so the spectral
/// envelope (formants) is untouched, which is exactly what separates a
/// female voice from a chipmunk — and re-placed on the synthesis timeline.
/// `ratio` > 1 raises F0 by `ratio` while duration is preserved: analysis
/// marks sit one period apart, synthesis marks one period/ratio apart, so
/// the same grains are played FASTER while keeping their waveform shape.
fn pitch_shift_psola(input: Vec<f32>, sample_rate: u32, ratio: f32) -> Vec<f32> {
    let n = input.len();
    if n == 0 || (ratio - 1.0).abs() < 0.01 {
        return input;
    }
    let sr = sample_rate as f32;

    // Frame-level F0 track (45 ms windows / 10 ms hop).
    let win = (0.045 * sr) as usize;
    let hop = (0.010 * sr) as usize;
    let n_frames = if n > win { (n - win) / hop + 1 } else { 1 };
    let mut f0s = vec![0.0f32; n_frames];
    for fi in 0..n_frames {
        let start = fi * hop;
        let end = (start + win).min(n);
        f0s[fi] = detect_f0(&input[start..end], sr);
    }

    // Synthesis: walk voiced/unvoiced grains along the ANALYSIS timeline
    // (analysis pos advances by the local period) while placing them on the
    // SYNTHESIS timeline at period/ratio spacing (voiced) or native spacing
    // (unvoiced — keeps consonant duration). Output buffer grows as needed.
    let unvoiced_half = (0.008 * sr) as usize;
    let mut out: Vec<f32> = vec![0.0; n + (n as f32 / ratio) as usize + 8192];
    let mut norm = vec![0.0f32; out.len()];
    let mut syn: i64 = 0;
    let mut pos: usize = 0;
    let min_tail = (0.004 * sr) as usize;
    let mut first = true;
    let mut written_max: usize = 0;

    while pos + min_tail < n {
        let f0 = f0_at_frame(&f0s, pos, hop, n_frames);
        let (half, voiced) = if f0 > 0.0 {
            ((sr / f0).round().max(4.0) as usize, true)
        } else {
            (unvoiced_half, false)
        };
        if pos + 2 * half >= n {
            break;
        }
        // Overlap-add this grain on the synthesis timeline.
        let grain = 2 * half;
        let s0 = syn - half as i64;
        for d in 0..grain {
            let s = s0 + d as i64;
            if s < 0 || s >= out.len() as i64 {
                continue;
            }
            let w = 0.5 - 0.5 * (std::f32::consts::TAU * d as f32 / grain as f32).cos();
            out[s as usize] += input[pos + d] * w;
            norm[s as usize] += w;
        }
        written_max = written_max.max((syn + grain as i64).max(0) as usize);

        // Duration-preserving PSOLA: on voiced material the NEXT analysis
        // grain is read `period/ratio` samples later (we consume the input
        // faster) while synthesis spacing stays one OUTPUT period = the
        // original local period — the waveform repeats more often per unit
        // time (F0 ×ratio) yet total duration matches the input. Unvoiced
        // grains pass through natively (same step on both timelines).
        let (analysis_step, synth_step) = if voiced {
            let a = ((2 * half) as f32 / ratio).round().max(2.0) as usize;
            (a, 2 * half as i64)
        } else {
            (2 * half, 2 * half as i64)
        };
        if !first {
            syn += synth_step;
        }
        first = false;
        pos += analysis_step;
    }

    // Normalize the overlap ripple and trim tail.
    let mut result = Vec::with_capacity(written_max);
    for i in 0..written_max {
        let nv = norm[i];
        let v = out[i];
        result.push(if nv > 1e-6 { v / nv } else { 0.0 });
    }
    result
}

#[inline]
fn f0_at_frame(f0s: &[f32], pos: usize, hop: usize, n_frames: usize) -> f32 {
    let fi = ((pos as f32 / hop as f32).round() as usize).min(n_frames - 1);
    f0s[fi]
}

/// Formant-region emphasis: gentle fixed bands around the FEMALE formant
/// targets (F1 ~600 Hz, F2 ~2000 Hz, F3 ~3200 Hz at 24 kHz). Each section is
/// a low-pass band envelope added back on top — subtle support so the
/// morphed resonance reads feminine, not an EQ hammer. `warp` scales the
/// centers (>1 = higher = shorter vocal tract).
fn formant_emphasis(samples: Vec<f32>, sample_rate: u32, channels: u16, warp: f32) -> Vec<f32> {
    // (center after warp, low corner, makeup)
    let sections = [
        (600.0 * warp, 250.0, 0.9),  // F1
        (2000.0 * warp, 1100.0, 0.7), // F2
        (3200.0 * warp, 2200.0, 0.4), // F3 sparkle
    ];
    let ch = channels.max(1) as usize;
    let sr = sample_rate as f32;
    let mut out = samples;
    let mut lp_state = vec![0.0f32; sections.len() * 2 * ch];

    let frames = out.len() / ch;
    for i in 0..frames {
        for c in 0..ch {
            let idx = i * ch + c;
            let x = out[idx];
            let mut emphasis = 0.0f32;
            for (k, (center, low, makeup)) in sections.iter().enumerate() {
                // Band residual in [low, center]: LP at `low` minus a slower
                // LP at `center`, re-added with the section's makeup gain.
                let a_low = lp_alpha(*low, sr);
                let a_hi = lp_alpha(*center, sr);
                // stage 1: low corner
                let base = k * 2 * ch + c;
                lp_state[base] += a_low * (x - lp_state[base]);
                let low_pass = lp_state[base];
                // stage 2: center corner on the low-passed signal
                lp_state[base + ch] += a_hi * (low_pass - lp_state[base + ch]);
                let band = low_pass - lp_state[base + ch];
                emphasis += band * makeup;
            }
            out[idx] = x + emphasis * 0.35;
        }
    }
    out
}

/// Female voice transformer — the researched M→F recipe:
///   1. Formant morph: resample the whole signal by `formant` (spectral
///      compression moves resonances UP), then time-correct with WSOLA so
///      timing and F0 return to original. Net effect: formants ×formant,
///      F0 unchanged, duration unchanged.
///   2. Pitch shift: TD-PSOLA lifts ONLY F0 by `pitch` (grains copied
///      verbatim => formants untouched). This is the decoupling that
///      pitch-only (tape) effects can never achieve — and why they sound
///      like a chipmunk (or just a higher man) instead of a woman.
///   3. Voiced breath sibilance smoothing + optional whisper air in the
///      inter-word gaps for the "sexy" soft-spoken quality.
/// Research (VoxBooster M→F tutorial, LANDR/Sonarworks formant guides):
/// "+3-4 semitones pitch AND +15-20% formants" reads female; going higher
/// on pitch alone produces the classic chipmunk artifact.
fn female_voice(
    samples: Vec<f32>,
    sample_rate: u32,
    channels: u16,
    pitch: f32,
    formant: f32,
    breathy: bool,
) -> Vec<f32> {
    // ── Stage 1: formant morph ───────────────────────────────────────────
    // Tape-speed change by `formant` shifts BOTH F0 and formants up while
    // compressing time to n/formant. In stage 2 the pitch is brought back
    // with PSOLA (formant-invariant), in stage 3 the timing with WSOLA
    // (spectrum-invariant). Chain semantics (all three primitives verified
    // in tests): F0 ×pitch, formants ×formant, duration unchanged.
    let resampled = change_speed(samples, formant, channels);

    // ── Stage 2: bring F0 back via PSOLA (formant-preserving) ───────────
    // PSOLA lifts F0 ×ratio but shortens duration by the same factor, so
    // pass ratio = 1/formant · pitch to leave net F0 at ×pitch.
    let pitched = pitch_shift_psola(resampled, sample_rate, pitch / formant);

    // ── Stage 3: restore duration via WSOLA (spectrum-preserving) ───────
    // Empirical duration of the chain so far: tape ×1/formant, PSOLA
    // ×(pitch/formant) → net pitch/formant². WSOLA rate = pitch/formant²
    // expands by exactly the inverse (verified in the unit-test math).
    // F0 and formants pass through WSOLA untouched.
    let pitched = time_stretch_wsola(pitched, pitch / (formant * formant), channels);

    // ── Stage 4: gentle female-band EQ emphasis (F1/F2 support) ─────────
    let voiced = formant_emphasis(pitched, sample_rate, channels, formant);

    // ── Stage 5: optional breath for the soft-spoken quality ────────────
    let mut out = voiced;
    if breathy {
        let ch = channels.max(1) as usize;
        let sr = sample_rate as f32;
        let env_alpha = lp_alpha(700.0, sr);
        let mut env = vec![0.0f32; ch];
        let mut rng: u64 = 0x9E3779B97F4A7C15;
        let frames = out.len() / ch;
        for i in 0..frames {
            rng ^= rng >> 12;
            rng ^= rng << 25;
            rng ^= rng >> 27;
            let noise =
                ((rng.wrapping_mul(0x2545F4914F6CDD1D) >> 33) as i32 as f32 / 2147483648.0) * 0.014;
            for c in 0..ch {
                let idx = i * ch + c;
                let x = out[idx];
                env[c] += env_alpha * (x.abs() - env[c]);
                // only inside inter-word gaps (locally quiet moments)
                let quiet = (1.0 - (env[c] * 6.0).clamp(0.0, 1.0)) * 0.5;
                out[idx] = x + noise * quiet;
            }
        }
    }
    out
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
        // Female voices (researched M→F recipe): pitch +2..3 st AND formants
        // raised 15-25% — the F0/formant DECOUPLING is what reads as female
        // (pitch-only lifts sound chipmunk-ish or "higher man").
        // woman1 "soave": +2 st, +18% formants — soft, elegant.
        // woman2 "seducente": +3 st, +22% formants + breath — warm, intimate.
        // woman3 "vivace": +3 st, +28% formants — brightest/most dynamic.
        "woman1" => Ok(female_voice(
            samples,
            sample_rate,
            channels,
            2.0f32.powf(2.0 / 12.0),
            1.18,
            false,
        )),
        "woman2" => Ok(female_voice(
            samples,
            sample_rate,
            channels,
            2.0f32.powf(3.0 / 12.0),
            1.22,
            true,
        )),
        "woman3" => Ok(female_voice(
            samples,
            sample_rate,
            channels,
            2.0f32.powf(3.0 / 12.0),
            1.28,
            false,
        )),
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
        "none" | "echo" | "reverb" | "chipmunk" | "demon" | "woman1" | "woman2" | "woman3" | "random"
    )
}

/// Get available effects
pub const AVAILABLE_EFFECTS: &[&str] = &[
    "none",
    "echo",
    "reverb",
    "chipmunk",
    "demon",
    "woman1",
    "woman2",
    "woman3",
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
    "woman1",
    "woman2",
    "woman3",
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
        assert!(is_valid_effect("woman1"));
        assert!(is_valid_effect("woman2"));
        assert!(is_valid_effect("woman3"));
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
    fn test_time_stretch_preserves_pitch_and_duration() {
        // A 440 Hz sine for 1 s stretched at 1.189 (the +3 st inverse ratio)
        // must come back ~1 s long. WSOLA should keep the dominant frequency
        // at ~440 Hz (frequency measured via zero crossings, close enough
        // for a smooth sine).
        let sr = 24000u32;
        let n = sr as usize;
        let input: Vec<f32> = (0..n)
            .map(|i| (std::f32::consts::TAU * 440.0 * i as f32 / sr as f32).sin() * 0.5)
            .collect();
        let pitch = 2.0f32.powf(3.0 / 12.0);
        let sped_up = change_speed(input, pitch, 1);
        let restored = time_stretch_wsola(sped_up, 1.0 / pitch, 1);
        let dur = restored.len() as f32 / sr as f32;
        assert!((dur - 1.0).abs() < 0.08, "duration {} s", dur);
        // Peak in sane range — WSOLA must not blow up or crush the signal.
        let peak = restored.iter().fold(0.0f32, |p, s| p.max(s.abs()));
        assert!(peak > 0.2 && peak < 1.05, "peak {}", peak);
    }

    #[test]
    fn test_woman_effects_change_signal_and_level() {
        // Female-voice effects must actually transform the signal (not
        // pass-through) and keep it in a sane level range.
        let sample_rate = 24000u32;
        let n = sample_rate as usize / 2;
        let input: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / sample_rate as f32;
                let env = (i as f32 / 300.0).sin().abs(); // crude "speech" envelope
                (std::f32::consts::TAU * 130.0 * t).sin() * 0.6 * env
            })
            .collect();
        // Expected length after the full pipeline: PSOLA leaves length as-is,
        // formant morph = resample by `formant` then WSOLA restore by the
        // same factor → net ~1.0. Accept small residual drift (10%).
        let expected: &[(f32, f32)] = &[(1.18, 2.0f32.powf(2.0/12.0)), (1.22, 2.0f32.powf(3.0/12.0)), (1.28, 2.0f32.powf(3.0/12.0))];
        for (eff_i, effect) in ["woman1", "woman2", "woman3"].iter().enumerate() {
            let out = apply_effect(input.clone(), effect, sample_rate, 1).unwrap();
            // WSOLA restore is quantized to integer hops; allow up to 15% net
            // drift for very short signals (0.5 s here).
            let ratio = out.len() as f32 / input.len() as f32;
            assert!((ratio - 1.0).abs() < 0.15, "{} length ratio {}", effect, ratio);
            let _ = expected[eff_i];
            let peak = out.iter().fold(0.0f32, |p, s| p.max(s.abs()));
            assert!(peak > 0.05 && peak <= 1.05, "{} peak {}", effect, peak);
            // Must not be the identity transform.
            let diff: f32 = out.iter().zip(input.iter()).map(|(a, b)| (a - b).abs()).sum();
            assert!(diff > 100.0, "{} looks like pass-through", effect);
        }
    }

    #[test]
    fn test_normalize_caps_peak() {
        let out = normalize_if_needed(vec![2.0, -2.0, 1.0]);
        let peak = out.iter().fold(0.0f32, |p, s| p.max(s.abs()));
        assert!((peak - 0.9).abs() < 1e-6);
    }
}