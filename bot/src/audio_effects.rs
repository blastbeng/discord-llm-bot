use std::io::Cursor;
use thiserror::Error;
use mp3lame_encoder::Builder;
use minimp3::Decoder;
use oximedia_effects::{
    AudioEffect,
    delay::delay::{DelayConfig, MonoDelay, FeedbackSaturationMode},
    filter::state_variable::{StateVariableConfig, StateVariableFilter, FilterMode},
    pitch::shifter::{PitchShifter, PitchShifterConfig},
    reverb::freeverb::Freeverb,
    ReverbConfig,
};

// `chunks_exact(2)` is used to split interleaved stereo samples into left/right
// channels. The constant chunk size is intentional for the split-then-recombine
// pattern used below; `as_chunks` doesn't compose well with `.unzip()`.
#[allow(clippy::chunks_exact_to_as_chunks)]

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
/// This replaces the ffmpeg-based approach with pure Rust DSP processing.
pub async fn apply_effect_to_mp3(
    input_bytes: Vec<u8>,
    effect: &str,
    sample_rate: u32,
) -> Result<Vec<u8>, AudioEffectError> {
    // 1. Decode MP3 to raw PCM samples
    let (mut samples, decoded_sample_rate, channels) = decode_mp3(&input_bytes)?;

    // Resample if needed (Google TTS is typically 24kHz or 44.1kHz)
    if decoded_sample_rate != sample_rate {
        samples = resample_audio(samples, decoded_sample_rate, sample_rate, channels);
    }

    // 2. Apply the requested effect
    let processed_samples = apply_effect(samples, effect, sample_rate, channels)?;

    // 3. Encode back to MP3
    let output_bytes = encode_mp3(processed_samples, sample_rate, channels)?;

    Ok(output_bytes)
}

/// Decode MP3 bytes to interleaved f32 samples (-1.0 to 1.0)
fn decode_mp3(data: &[u8]) -> Result<(Vec<f32>, u32, u16), AudioEffectError> {
    let mut decoder = Decoder::new(Cursor::new(data));
    let mut all_samples = Vec::new();
    let mut sample_rate = 0;
    let mut channels = 0;

    while let Ok(frame) = decoder.next_frame() {
        sample_rate = frame.sample_rate as u32;
        channels = frame.channels as u16;
        for sample in frame.data {
            all_samples.push(sample as f32 / 32768.0);
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
        .with_brate(mp3lame_encoder::Bitrate::Kbps64)
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

/// Simple linear resampling (basic, for demo - can be improved with rubato)
fn resample_audio(samples: Vec<f32>, from_rate: u32, to_rate: u32, channels: u16) -> Vec<f32> {
    if from_rate == to_rate {
        return samples;
    }

    let ratio = from_rate as f32 / to_rate as f32;
    let frames = samples.len() / channels as usize;
    let new_frames = (frames as f32 / ratio) as usize;
    let mut output = Vec::with_capacity(new_frames * channels as usize);

    for ch in 0..channels as usize {
        for i in 0..new_frames {
            let src_idx = (i as f32 * ratio) as usize * channels as usize + ch;
            if src_idx < samples.len() {
                output.push(samples[src_idx]);
            } else {
                output.push(0.0);
            }
        }
    }

    output
}

/// Apply a specific effect to the audio samples
fn apply_effect(
    samples: Vec<f32>,
    effect: &str,
    sample_rate: u32,
    channels: u16,
) -> Result<Vec<f32>, AudioEffectError> {
    match effect {
        "none" | "random" => Ok(samples),
        "echo" => apply_echo(samples, sample_rate, channels),
        "reverb" => apply_reverb(samples, sample_rate, channels),
        "bass" => apply_bass_boost(samples, sample_rate, channels),
        "chipmunk" => apply_chipmunk(samples, sample_rate, channels),
        "demon" => apply_demon(samples, sample_rate, channels),
        "telephone" => apply_telephone(samples, sample_rate, channels),
        "underwater" => apply_underwater(samples, sample_rate, channels),
        _ => Err(AudioEffectError::EffectProcessing(format!("Unknown effect: {}", effect))),
    }
}

/// Apply echo using MonoDelay from oximedia-effects
fn apply_echo(samples: Vec<f32>, sample_rate: u32, channels: u16) -> Result<Vec<f32>, AudioEffectError> {
    let config = DelayConfig {
        delay_ms: 300.0,
        feedback: 0.4,
        wet: 0.3,
        dry: 1.0,
        tone: 0.7,
        saturation: FeedbackSaturationMode::None,
        saturation_drive: 1.0,
    };

    let mut delay = MonoDelay::new(config, sample_rate as f32);
    let mut output = samples.clone();
    if channels == 1 {
        delay.process(&mut output);
    } else {
        let (mut left, mut right): (Vec<f32>, Vec<f32>) = samples
            .chunks_exact(2)
            .map(|chunk| (chunk[0], chunk[1]))
            .unzip();
        delay.process_stereo(&mut left, &mut right);
        output = left.into_iter().zip(right).flat_map(|(l, r)| [l, r]).collect();
    }
    Ok(output)
}

/// Apply reverb using Freeverb from oximedia-effects
fn apply_reverb(samples: Vec<f32>, sample_rate: u32, channels: u16) -> Result<Vec<f32>, AudioEffectError> {
    let config = ReverbConfig {
        room_size: 0.7,
        damping: 0.5,
        wet: 0.3,
        dry: 0.7,
        width: 0.5,
        predelay_ms: 20.0,
    };
    let mut reverb = Freeverb::new(config, sample_rate as f32);
    let mut output = samples.clone();
    if channels == 1 {
        reverb.process(&mut output);
    } else {
        let (mut left, mut right): (Vec<f32>, Vec<f32>) = samples
            .chunks_exact(2)
            .map(|chunk| (chunk[0], chunk[1]))
            .unzip();
        reverb.process_stereo(&mut left, &mut right);
        output = left.into_iter().zip(right).flat_map(|(l, r)| [l, r]).collect();
    }
    Ok(output)
}

/// Apply bass boost using low-pass filter with high resonance
fn apply_bass_boost(samples: Vec<f32>, sample_rate: u32, channels: u16) -> Result<Vec<f32>, AudioEffectError> {
    let config = StateVariableConfig {
        frequency: 200.0,
        resonance: 5.0,
        mode: FilterMode::LowPass,
    };
    let mut filter = StateVariableFilter::new(config, sample_rate as f32);
    let mut output = samples.clone();
    if channels == 1 {
        filter.process(&mut output);
    } else {
        let (mut left, mut right): (Vec<f32>, Vec<f32>) = samples
            .chunks_exact(2)
            .map(|chunk| (chunk[0], chunk[1]))
            .unzip();
        filter.process(&mut left);
        filter.process_stereo(&mut left, &mut right);
        output = left.into_iter().zip(right).flat_map(|(l, r)| [l, r]).collect();
    }
    Ok(output)
}

/// Apply chipmunk effect: pitch up 7 semitones and tempo up
fn apply_chipmunk(samples: Vec<f32>, sample_rate: u32, channels: u16) -> Result<Vec<f32>, AudioEffectError> {
    let mut shifter = PitchShifter::new(
        PitchShifterConfig {
            semitones: 7.0,
            cents: 0.0,
            mix: 1.0,
        },
        sample_rate as f32,
    );
    let mut output = samples.clone();
    if channels == 1 {
        shifter.process(&mut output);
    } else {
        let (mut left, mut right): (Vec<f32>, Vec<f32>) = samples
            .chunks_exact(2)
            .map(|chunk| (chunk[0], chunk[1]))
            .unzip();
        shifter.process_stereo(&mut left, &mut right);
        output = left.into_iter().zip(right).flat_map(|(l, r)| [l, r]).collect();
    }
    output = change_tempo(output, 1.5, channels);
    Ok(output)
}

/// Apply demon effect: pitch down 12 semitones and tempo down
fn apply_demon(samples: Vec<f32>, sample_rate: u32, channels: u16) -> Result<Vec<f32>, AudioEffectError> {
    let mut shifter = PitchShifter::new(
        PitchShifterConfig {
            semitones: -12.0,
            cents: 0.0,
            mix: 1.0,
        },
        sample_rate as f32,
    );
    let mut output = samples.clone();
    if channels == 1 {
        shifter.process(&mut output);
    } else {
        let (mut left, mut right): (Vec<f32>, Vec<f32>) = samples
            .chunks_exact(2)
            .map(|chunk| (chunk[0], chunk[1]))
            .unzip();
        shifter.process_stereo(&mut left, &mut right);
        output = left.into_iter().zip(right).flat_map(|(l, r)| [l, r]).collect();
    }
    output = change_tempo(output, 0.7, channels);
    output = apply_bass_boost(output, sample_rate, channels)?;
    Ok(output)
}

/// Apply telephone effect: bandpass filter 300-3400 Hz
fn apply_telephone(samples: Vec<f32>, sample_rate: u32, channels: u16) -> Result<Vec<f32>, AudioEffectError> {
    let hp_config = StateVariableConfig {
        frequency: 300.0,
        resonance: 0.707,
        mode: FilterMode::HighPass,
    };
    let mut hp_filter = StateVariableFilter::new(hp_config, sample_rate as f32);

    let lp_config = StateVariableConfig {
        frequency: 3400.0,
        resonance: 0.707,
        mode: FilterMode::LowPass,
    };
    let mut lp_filter = StateVariableFilter::new(lp_config, sample_rate as f32);

    let mut output = samples.clone();
    if channels == 1 {
        hp_filter.process(&mut output);
        lp_filter.process(&mut output);
    } else {
        let (mut left, mut right): (Vec<f32>, Vec<f32>) = samples
            .chunks_exact(2)
            .map(|chunk| (chunk[0], chunk[1]))
            .unzip();
        hp_filter.process(&mut left);
        hp_filter.process_stereo(&mut left, &mut right);
        lp_filter.process(&mut left);
        lp_filter.process_stereo(&mut left, &mut right);
        output = left.into_iter().zip(right).flat_map(|(l, r)| [l, r]).collect();
    }
    Ok(output)
}

/// Apply underwater effect: low-pass + slow tempo
fn apply_underwater(samples: Vec<f32>, sample_rate: u32, channels: u16) -> Result<Vec<f32>, AudioEffectError> {
    let config = StateVariableConfig {
        frequency: 400.0,
        resonance: 1.0,
        mode: FilterMode::LowPass,
    };
    let mut filter = StateVariableFilter::new(config, sample_rate as f32);
    let mut output = samples.clone();
    if channels == 1 {
        filter.process(&mut output);
    } else {
        let (mut left, mut right): (Vec<f32>, Vec<f32>) = samples
            .chunks_exact(2)
            .map(|chunk| (chunk[0], chunk[1]))
            .unzip();
        filter.process(&mut left);
        filter.process_stereo(&mut left, &mut right);
        output = left.into_iter().zip(right).flat_map(|(l, r)| [l, r]).collect();
    }
    output = change_tempo(output, 0.8, channels);
    Ok(output)
}

/// Simple tempo change by resampling
fn change_tempo(samples: Vec<f32>, factor: f32, channels: u16) -> Vec<f32> {
    if (factor - 1.0).abs() < 0.01 {
        return samples;
    }
    let frames = samples.len() / channels as usize;
    let new_frames = (frames as f32 / factor) as usize;
    let mut output = Vec::with_capacity(new_frames * channels as usize);

    for ch in 0..channels as usize {
        for i in 0..new_frames {
            let src_idx = (i as f32 * factor) as usize * channels as usize + ch;
            if src_idx < samples.len() {
                output.push(samples[src_idx]);
            } else {
                output.push(0.0);
            }
        }
    }
    output
}

/// Simplified API for common use case: compress and save MP3 with effect
pub async fn compress_and_save_mp3_with_effect(
    input_bytes: Vec<u8>,
    file_path: &str,
    effect: &str,
) -> Result<(), AudioEffectError> {
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
        "none" | "echo" | "reverb" | "bass" | "chipmunk" | "demon" | "telephone" | "underwater" | "random"
    )
}

/// Get available effects
pub const AVAILABLE_EFFECTS: &[&str] = &[
    "none",
    "echo",
    "reverb",
    "bass",
    "chipmunk",
    "demon",
    "telephone",
    "underwater",
    "random",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_effect() {
        assert!(is_valid_effect("none"));
        assert!(is_valid_effect("echo"));
        assert!(is_valid_effect("reverb"));
        assert!(is_valid_effect("bass"));
        assert!(is_valid_effect("chipmunk"));
        assert!(is_valid_effect("demon"));
        assert!(is_valid_effect("telephone"));
        assert!(is_valid_effect("underwater"));
        assert!(is_valid_effect("random"));
        assert!(!is_valid_effect("invalid"));
    }

    #[test]
    fn test_available_effects_contains_expected() {
        assert!(AVAILABLE_EFFECTS.contains(&"echo"));
        assert!(AVAILABLE_EFFECTS.contains(&"reverb"));
    }
}
