// Audio decoding / synthesis / encoding for the voiceclone sidecar.
//
// Decoding: MP3 via minimp3 (same crate the bots already use), or a plain
// 16-bit PCM WAV header — both are trivially available without external deps.
// Synthesis: sherpa-onnx PocketTTS zero-shot cloning (int8, 2 diffusion steps).
// Encoding: MP3 via the same mp3lame-encoder setup as the bots' effects code.

use mp3lame_encoder::Builder;
use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsPocketModelConfig,
};

/// Decode recorded bytes (MP3 or 16-bit PCM WAV) to mono f32 samples.
pub fn decode_to_mono(bytes: &[u8]) -> Option<Vec<f32>> {
    // Try MP3 first (most common when users forward voice notes / audio files).
    if let Ok((samples, _rate, channels)) = decode_mp3(bytes) {
        if samples.is_empty() {
            return None;
        }
        return Some(to_mono(samples, channels as usize));
    }
    // Fall back to a plain 16-bit PCM WAV.
    if let Some((samples, _rate, channels)) = decode_wav(bytes) {
        if samples.is_empty() {
            return None;
        }
        return Some(to_mono(samples, channels as usize));
    }
    None
}

fn decode_mp3(bytes: &[u8]) -> Result<(Vec<f32>, u32, u16), ()> {
    let mut decoder = minimp3::Decoder::new(std::io::Cursor::new(bytes));
    let mut all = Vec::new();
    let mut rate = 0u32;
    let mut channels = 0u16;
    loop {
        match decoder.next_frame() {
            Ok(frame) => {
                rate = frame.sample_rate as u32;
                channels = frame.channels as u16;
                all.extend(frame.data.iter().map(|&s| s as f32 / 32768.0));
            }
            Err(minimp3::Error::Eof) => break,
            Err(_) => {
                if all.is_empty() {
                    return Err(());
                }
                break;
            }
        }
    }
    if all.is_empty() {
        return Err(());
    }
    Ok((all, rate, channels))
}

/// Minimal 16-bit PCM WAV parser (RIFF header + data chunk, mono or stereo).
fn decode_wav(bytes: &[u8]) -> Option<(Vec<f32>, u32, u16)> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut i = 12usize;
    let mut data: Option<&[u8]> = None;
    let mut channels = 1u16;
    let mut rate = 24000u32;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let size = u32::from_le_bytes(bytes[i + 4..i + 8].try_into().ok()?) as usize;
        match id {
            b"fmt " => {
                if i + 8 + size > bytes.len() || size < 16 {
                    return None;
                }
                channels = u16::from_le_bytes(bytes[i + 10..i + 12].try_into().ok()?).max(1);
                rate = u32::from_le_bytes(bytes[i + 12..i + 16].try_into().ok()?);
            }
            b"data" => {
                let end = (i + 8 + size).min(bytes.len());
                data = Some(&bytes[i + 8..end]);
            }
            _ => {}
        }
        i += 8 + size + (size % 2);
    }
    let data = data?;
    let samples: Vec<f32> = data
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
        .collect();
    Some((samples, rate, channels))
}

/// Average the channels of interleaved samples down to mono.
fn to_mono(samples: Vec<f32>, channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return samples;
    }
    samples
        .chunks_exact(channels)
        .map(|c| c.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Create the OfflineTts engine with the PocketTTS int8 model.
pub fn create_engine() -> Option<OfflineTts> {
    let dir = std::env::var("VOICECLONE_MODEL_DIR")
        .unwrap_or_else(|_| "models/pocket-tts".to_string());
    let num_threads: i32 = std::env::var("VOICECLONE_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);

    let config = OfflineTtsConfig {
        model: sherpa_onnx::OfflineTtsModelConfig {
            pocket: OfflineTtsPocketModelConfig {
                lm_flow: Some(format!("{dir}/lm_flow.int8.onnx")),
                lm_main: Some(format!("{dir}/lm_main.int8.onnx")),
                encoder: Some(format!("{dir}/encoder.onnx")),
                decoder: Some(format!("{dir}/decoder.int8.onnx")),
                text_conditioner: Some(format!("{dir}/text_conditioner.onnx")),
                vocab_json: Some(format!("{dir}/vocab.json")),
                token_scores_json: Some(format!("{dir}/token_scores.json")),
                ..Default::default()
            },
            num_threads,
            debug: false,
            ..Default::default()
        },
        ..Default::default()
    };
    OfflineTts::create(&config)
}

/// Synthesize `text` cloned from the given reference sample (mono f32).
pub fn generate(
    engine: &OfflineTts,
    text: String,
    reference: Vec<f32>,
    speed: f32,
) -> Result<Vec<f32>, String> {
    let gen_config = GenerationConfig {
        speed: speed.clamp(0.8, 1.4),
        reference_audio: Some(reference),
        reference_sample_rate: 24000,
        num_steps: std::env::var("VOICECLONE_STEPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2),
        ..Default::default()
    };
    let audio = engine
        .generate_with_config(&text, &gen_config, Some(|_: &[f32], _: f32| true))
        .ok_or("generation failed")?;
    Ok(audio.samples().to_vec())
}

/// Encode mono f32 samples to MP3 bytes (64 kbps mono — plenty for voice).
pub fn encode_mono_mp3(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>, String> {
    let mut encoder = Builder::new()
        .ok_or("encoder init")?
        .with_num_channels(1)
        .map_err(|e| format!("{e:?}"))?
        .with_sample_rate(sample_rate)
        .map_err(|e| format!("{e:?}"))?
        .with_brate(mp3lame_encoder::Bitrate::Kbps64)
        .map_err(|e| format!("{e:?}"))?
        .with_quality(mp3lame_encoder::Quality::Good)
        .map_err(|e| format!("{e:?}"))?
        .build()
        .map_err(|e| format!("{e:?}"))?;

    let pcm: Vec<i16> = samples
        .iter()
        .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
        .collect();

    let mut out = Vec::with_capacity(mp3lame_encoder::max_required_buffer_size(pcm.len()));
    let n = encoder
        .encode(mp3lame_encoder::MonoPcm(&pcm), out.spare_capacity_mut())
        .map_err(|e| format!("{e:?}"))?;
    unsafe { out.set_len(out.len() + n) };
    let n = encoder
        .flush::<mp3lame_encoder::FlushNoGap>(out.spare_capacity_mut())
        .map_err(|e| format!("{e:?}"))?;
    unsafe { out.set_len(out.len() + n) };
    Ok(out)
}