use mp3lame_encoder::{Encoder, Interleaving, MonoPcm, StereoPcm};
use minimp3::{Decoder, Frame};
use oximedia_effects::{
    AudioEffect,
    delay::{MonoDelay, DelayConfig},
    filter::state_variable::{StateVariableFilter, StateVariableConfig, FilterMode},
    pitch::shifter::{PitchShifter, PitchShifterConfig},
    reverb::room_reverb::ReverbProcessor,
};
use std::io::Cursor;
use thiserror::Error;

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

    for frame in decoder {
        let frame = frame.map_err(|e| AudioEffectError::Mp3Decode(e.to_string()))?;
        
        sample_rate = frame.sample_rate as u32;
        channels = frame.channels as u16;
        
        // Convert i16 samples to f32 (-1.0 to 1.0)
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
    let mut encoder = Encoder::new()
        .map_err(|e| AudioEffectError::Mp3Encode(e.to_string()))?;
    
    encoder.set_sample_rate(sample_rate)
        .map_err(|e| AudioEffectError::Mp3Encode(e.to_string()))?;
    
    encoder.set_channels(channels)
        .map_err(|e| AudioEffectError::Mp3Encode(e.to_string()))?;
    
    encoder.set_brate(64)
        .map_err(|e| AudioEffectError::Mp3Encode(e.to_string()))?;
    
    encoder.set_quality(2) // 0=best, 9=worst
        .map_err(|e| AudioEffectError::Mp3Encode(e.to_string()))?;
    
    encoder.set_interleaving(Interleaving::Interleaved)
        .map_err(|e| AudioEffectError::Mp3Encode(e.to_string()))?;

    // Convert f32 to i16
    let pcm_samples: Vec<i16> = samples
        .iter()
        .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
        .collect();

    let mut output = Vec::new();
    let mut encoder_buffer = vec![0u8; pcm_samples.len() * 2]; // rough estimate

    if channels == 1 {
        let mono_pcm = MonoPcm::new(&pcm_samples);
        let encoded = encoder
            .encode(mono_pcm)
            .map_err(|e| AudioEffectError::Mp3Encode(e.to_string()))?;
        output.extend_from_slice(&encoded);
    } else {
        let stereo_pcm = StereoPcm::new(&pcm_samples);
        let encoded = encoder
            .encode(stereo_pcm)
            .map_err(|e| AudioEffectError::Mp3Encode(e.to_string()))?;
        output.extend_from_slice(&encoded);
    }

    // Flush encoder
    let flushed = encoder
        .flush()
        .map_err(|e| AudioEffectError::Mp3Encode(e.to_string()))?;
    output.extend_from_slice(&flushed);

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
        
        // Echo effect using delay
        "echo" => apply_echo(samples, sample_rate, channels),
        
        // Reverb effect
        "reverb" => apply_reverb(samples, sample_rate, channels),
        
        // Bass boost using low-pass filter
        "bass" => apply_bass_boost(samples, sample_rate, channels),
        
        // Chipmunk: pitch up + speed up
        "chipmunk" => apply_chipmunk(samples, sample_rate, channels),
        
        // Demon: pitch down + slow down
        "demon" => apply_demon(samples, sample_rate, channels),
        
        // Telephone: bandpass filter
        "telephone" => apply_telephone(samples, sample_rate, channels),
        
        // Underwater: low-pass + slow down
        "underwater" => apply_underwater(samples, sample_rate, channels),
        
        _ => Err(AudioEffectError::EffectProcessing(format!("Unknown effect: {}", effect))),
    }
}

/// Apply echo using MonoDelay from oximedia-effects
fn apply_echo(samples: Vec<f32>, sample_rate: u32, channels: u16) -> Result<Vec<f32>, AudioEffectError> {
    let delay_samples = (sample_rate as f32 * 0.3) as usize; // 300ms delay
    let config = DelayConfig {
        delay_samples,
        feedback: 0.4,
        lowpass: 0.7,
        highpass: 0.0,
        wet: 0.3,
        dry: 1.0,
    };
    
    let mut delay = MonoDelay::new(config, sample_rate as f32);
    
    let mut output = samples.clone();
    if channels == 1 {
        delay.process(&mut output);
    } else {
        // For stereo, process left and right separately
        let (mut left, mut right): (Vec<f32>, Vec<f32>) = samples
            .chunks_exact(2)
            .map(|chunk| (chunk[0], chunk[1]))
            .unzip();
        delay.process(&mut left);
        delay.process(&mut right);
        // Re-interleave
        output = left.into_iter().zip(right).flat_map(|(l, r)| [l, r]).collect();
    }
    
    Ok(output)
}

/// Apply reverb using ReverbProcessor from oximedia-effects
fn apply_reverb(samples: Vec<f32>, sample_rate: u32, channels: u16) -> Result<Vec<f32>, AudioEffectError> {
    let mut reverb = ReverbProcessor::new(
        0.8,  // room_size
        0.5,  // damping
        0.3,  // wet
        1.0,  // dry
        0.5,  // width
        20.0, // predelay_ms
        sample_rate as f32,
    );
    
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

/// Apply bass boost using a low-shelf filter
fn apply_bass_boost(samples: Vec<f32>, sample_rate: u32, channels: u16) -> Result<Vec<f32>, AudioEffectError> {
    let config = StateVariableConfig {
        frequency: 100.0, // 100 Hz
        q: 1.0,
        mode: FilterMode::LowShelf,
        gain_db: 10.0, // +10dB boost
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
        filter.process(&mut right);
        output = left.into_iter().zip(right).flat_map(|(l, r)| [l, r]).collect();
    }
    
    Ok(output)
}

/// Apply chipmunk effect: pitch up 50% and tempo up 33%
fn apply_chipmunk(samples: Vec<f32>, sample_rate: u32, channels: u16) -> Result<Vec<f32>, AudioEffectError> {
    // Pitch shift up by 1.5x (about 7 semitones)
    let mut shifter = PitchShifter::new(PitchShifterConfig {
        pitch_shift: 1.5,  // 1.5x pitch
        formant_shift: 1.0, // preserve formants
        fft_size: 2048,
        overlap: 4,
        sample_rate: sample_rate as f32,
    });
    
    let mut output = samples.clone();
    if channels == 1 {
        shifter.process(&mut output);
    } else {
        let (mut left, mut right): (Vec<f32>, Vec<f32>) = samples
            .chunks_exact(2)
            .map(|chunk| (chunk[0], chunk[1]))
            .unzip();
        shifter.process(&mut left);
        shifter.process(&mut right);
        output = left.into_iter().zip(right).flat_map(|(l, r)| [l, r]).collect();
    }
    
    // Also speed up (tempo change)
    output = change_tempo(output, 1.5, channels);
    
    Ok(output)
}

/// Apply demon effect: pitch down 50% and tempo down
fn apply_demon(samples: Vec<f32>, sample_rate: u32, channels: u16) -> Result<Vec<f32>, AudioEffectError> {
    // Pitch shift down by 0.5x (about -12 semitones)
    let mut shifter = PitchShifter::new(PitchShifterConfig {
        pitch_shift: 0.5,
        formant_shift: 1.0,
        fft_size: 4096, // larger for lower pitch
        overlap: 8,
        sample_rate: sample_rate as f32,
    });
    
    let mut output = samples.clone();
    if channels == 1 {
        shifter.process(&mut output);
    } else {
        let (mut left, mut right): (Vec<f32>, Vec<f32>) = samples
            .chunks_exact(2)
            .map(|chunk| (chunk[0], chunk[1]))
            .unzip();
        shifter.process(&mut left);
        shifter.process(&mut right);
        output = left.into_iter().zip(right).flat_map(|(l, r)| [l, r]).collect();
    }
    
    // Slow down tempo
    output = change_tempo(output, 0.7, channels);
    
    // Add bass boost for demonic feel
    output = apply_bass_boost(output, sample_rate, channels)?;
    
    Ok(output)
}

/// Apply telephone effect: bandpass filter 300-3400 Hz
fn apply_telephone(samples: Vec<f32>, sample_rate: u32, channels: u16) -> Result<Vec<f32>, AudioEffectError> {
    // High-pass at 300 Hz
    let hp_config = StateVariableConfig {
        frequency: 300.0,
        q: 0.707,
        mode: FilterMode::HighPass,
        gain_db: 0.0,
    };
    let mut hp_filter = StateVariableFilter::new(hp_config, sample_rate as f32);
    
    // Low-pass at 3400 Hz
    let lp_config = StateVariableConfig {
        frequency: 3400.0,
        q: 0.707,
        mode: FilterMode::LowPass,
        gain_db: 0.0,
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
        hp_filter.process(&mut right);
        lp_filter.process(&mut left);
        lp_filter.process(&mut right);
        output = left.into_iter().zip(right).flat_map(|(l, r)| [l, r]).collect();
    }
    
    Ok(output)
}

/// Apply underwater effect: low-pass + slow tempo
fn apply_underwater(samples: Vec<f32>, sample_rate: u32, channels: u16) -> Result<Vec<f32>, AudioEffectError> {
    // Low-pass at 400 Hz
    let config = StateVariableConfig {
        frequency: 400.0,
        q: 1.0,
        mode: FilterMode::LowPass,
        gain_db: 15.0, // boost bass
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
        filter.process(&mut right);
        output = left.into_iter().zip(right).flat_map(|(l, r)| [l, r]).collect();
    }
    
    // Slow down tempo
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
/// This replaces the ffmpeg-based compress_and_save_mp3_with_effect
pub async fn compress_and_save_mp3_with_effect(
    input_bytes: Vec<u8>,
    file_path: &str,
    effect: &str,
) -> Result<(), AudioEffectError> {
    // Use standard TTS sample rate (Google TTS is typically 24kHz)
    let sample_rate = 24000;
    let channels = 1; // Mono for TTS
    
    let processed = apply_effect_to_mp3(input_bytes, effect, sample_rate).await?;
    
    // Ensure directory exists
    if let Some(parent) = std::path::Path::new(file_path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    
    // Write to file
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
];