//! Live voice recording for the hidden owner-only /clone command.
//!
//! Uses songbird's voice-receive feature: a global `VoiceTick` handler fires
//! every 20ms with decoded PCM for each active speaker. We capture the ticks
//! belonging to the target user into an MP3 buffer in memory until enough
//! SPEECH time (not wall time — silence doesn't count) is collected, then the
//! base64 result is handed to fish.audio exactly like an uploaded
//! sample.
//!
//! Decoded voice ticks are 2ch/48kHz s16le PCM (interleaved L/R). We downmix
//! to mono for cloning.

use async_trait::async_trait;
use songbird::events::{Event, EventContext, EventHandler as SongbirdEventHandler};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Discord user id used for SSRC mapping. songbird's `Speaking` payload
/// carries a `serenity_voice_model` UserId (newtype over u64), which is what
/// the map stores.
pub type DiscordUserId = u64;

/// How much speech (seconds) to collect before cloning.
pub const TARGET_SPEECH_SECS: f32 = 25.0;
/// Wall-clock cap: stop recording after this long even without speech.
pub const MAX_RECORD_SECS: u64 = 180;
/// Eavesdrop recording: much shorter — we only need a snippet of what the
/// user said, and playback must stay timely on the Pi.
pub const EAVESDROP_SPEECH_SECS: f32 = 12.0;
/// Eavesdrop wall-clock cap (shorter than the clone recording cap).
pub const EAVESDROP_MAX_SECS: u64 = 90;
/// Sample rate of songbird's decoded voice ticks.
const TICK_SAMPLE_RATE: u32 = 48000;

/// Shared sink collecting the target user's PCM while the recording session
/// is active.
pub struct RecordSink {
    /// UserId string form of the recording target (kept for logging).
    #[allow(dead_code)]
    pub target_user: String,
    /// Mono f32 samples accumulated so far.
    pub samples: std::sync::Mutex<Vec<f32>>,
    /// Set when the collector decides recording is over.
    pub done: AtomicBool,
    /// Wall-clock deadline for the whole session.
    pub deadline: std::time::Instant,
}

impl RecordSink {
    pub fn speech_secs(&self) -> f32 {
        self.samples.lock().unwrap().len() as f32 / TICK_SAMPLE_RATE as f32
    }

    /// Downmix a decoded stereo tick to mono and append.
    fn push_tick(&self, pcm: &[i16]) {
        let mut samples = self.samples.lock().unwrap();
        // 2-channel interleaved; average pairs. Guard against odd lengths.
        let frames = pcm.len() / 2;
        samples.reserve(frames);
        for frame in pcm.chunks_exact(2) {
            samples.push((frame[0] as f32 + frame[1] as f32) / 2.0 / 32768.0);
        }
    }
}

/// Songbird global event handler capturing voice ticks for one user.
///
/// Voice ticks are keyed by SSRC, not user id. `ssrc_map` is maintained by
/// [`SsrcTracker`] from `SpeakingStateUpdate` events (which carry the user id)
/// and lets us route only the target user's audio into the sink.
/// Voice ticks are keyed by SSRC, not user id. `ssrc_map` is maintained by
/// [`SsrcTracker`] from `SpeakingStateUpdate` events (which carry the user id)
/// and lets us route only the target user's audio into the sink.
pub struct CaptureHandler {
    pub target_user: DiscordUserId,
    pub sink: Arc<RecordSink>,
    /// Shared SSRC -> UserId registry (maintained by SsrcTracker).
    pub ssrc_map: SsrcMap,
}

#[async_trait]
impl SongbirdEventHandler for CaptureHandler {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if self.sink.done.load(Ordering::Relaxed) {
            return None;
        }
        if let EventContext::VoiceTick(tick) = ctx {
            let map = self.ssrc_map.lock().unwrap();
            for (ssrc, data) in tick.speaking.iter() {
                // Only accept audio belonging to the recording target.
                if map.get(ssrc) != Some(&self.target_user) {
                    continue;
                }
                if let Some(pcm) = &data.decoded_voice {
                    self.sink.push_tick(pcm);
                }
            }
        }
        None
    }
}

// SSRC -> UserId mapping captured from SpeakingStateUpdate ticks. Voice ticks
// don't carry user ids, so /clone registers a SsrcTracker alongside the
// CaptureHandler for the whole session.
pub type SsrcMap = Arc<std::sync::Mutex<std::collections::HashMap<u32, DiscordUserId>>>;

/// Handler tracking which SSRC belongs to which Discord user.
pub struct SsrcTracker {
    pub map: SsrcMap,
}

#[async_trait]
impl SongbirdEventHandler for SsrcTracker {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::SpeakingStateUpdate(speaking) = ctx {
            if let Some(user_id) = speaking.user_id {
                // songbird's model UserId is a newtype over u64.
                self.map.lock().unwrap().insert(speaking.ssrc, user_id.0);
            }
        }
        None
    }
}

// SSRC -> UserId mapping captured from SpeakingStateUpdate ticks. Voice ticks
// don't carry user ids, so /clone registers a SsrcTracker alongside the
// CaptureHandler for the whole session.

/// Songbird global event handler that copies EVERY active speaker's decoded
/// audio into a shared map keyed by user id. Used by the eavesdrop feature,
/// which does not know in advance who will speak (unlike /clone's
/// CaptureHandler, which routes one fixed target).
///
/// Sinks are created on demand: the first voice tick from an SSRC-mapped user
/// allocates their buffer. The deadline stops collection once the recording
/// window is over (handlers are removed right after anyway).
pub struct ListenerHandler {
    /// Per-user mono sample buffers, one entry per speaking user.
    pub sinks: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<DiscordUserId, Arc<RecordSink>>>>,
    /// Shared SSRC -> UserId registry (maintained by SsrcTracker).
    pub ssrc_map: SsrcMap,
    /// Wall-clock end of the capture session.
    pub deadline: std::time::Instant,
}

#[async_trait]
impl SongbirdEventHandler for ListenerHandler {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if std::time::Instant::now() >= self.deadline {
            return None;
        }
        if let EventContext::VoiceTick(tick) = ctx {
            let map = self.ssrc_map.lock().unwrap();
            let mut sinks = self.sinks.lock().unwrap();
            for (ssrc, data) in tick.speaking.iter() {
                let Some(user_id) = map.get(ssrc) else { continue };
                if let Some(pcm) = &data.decoded_voice {
                    // Create the sink on the user's first audible tick — the
                    // set of speakers is unknown until someone talks.
                    let sink = sinks
                        .entry(*user_id)
                        .or_insert_with(|| {
                            Arc::new(RecordSink {
                                target_user: user_id.to_string(),
                                samples: std::sync::Mutex::new(Vec::new()),
                                done: AtomicBool::new(false),
                                deadline: self.deadline,
                            })
                        });
                    sink.push_tick(pcm);
                }
            }
        }
        None
    }
}

/// Encode accumulated mono samples to MP3 bytes. Source ticks are 48kHz;
/// decimate to 24kHz (PocketTTS's native rate) by averaging pairs.
pub fn encode_samples_to_mp3(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>, String> {
    // Simple 2:1 decimation when downsampling 48k -> 24k.
    let (rate, data): (u32, &[f32]) = if sample_rate == 48000 {
        let out: Vec<f32> = samples.chunks_exact(2).map(|p| (p[0] + p[1]) * 0.5).collect();
        (24000, Box::leak(out.into_boxed_slice()))
    } else {
        (sample_rate, samples)
    };
    encode_mono(data, rate)
}

fn encode_mono(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>, String> {
    let mut encoder = mp3lame_encoder::Builder::new()
        .ok_or("encoder init")?
        .with_num_channels(1)
        .map_err(|e| format!("{e:?}"))?
        .with_sample_rate(sample_rate)
        .map_err(|e| format!("{e:?}"))?
        .with_brate(mp3lame_encoder::Bitrate::Kbps128)
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

/// Record `target_secs` of speech from ANY user in the bot's current voice
/// channel and return the collected samples per user (48kHz mono).
///
/// Used by the eavesdrop feature: attaches a ListenerHandler + SsrcTracker to
/// the driver, polls until the user has spoken enough (or the wall-clock cap
/// hits), then detaches and restores the cheaper Decrypt decode mode. The
/// bot's own audio (playback) is not captured — songbird only delivers
/// received remote voice.
///
/// Returns an empty map when nothing could be captured (nobody spoke, bot not
/// in a channel, ...). Never fails hard — eavesdrop treats "no audio" as "say
/// nothing this round".
pub async fn record_user_speech(
    handler_lock: &std::sync::Arc<tokio::sync::Mutex<songbird::Call>>,
    target_secs: f32,
    max_secs: u64,
) -> std::collections::HashMap<DiscordUserId, Vec<f32>> {
    // Enable PCM decoding for the session and register the handlers.
    {
        let mut handler = handler_lock.lock().await;
        let cfg = {
            let mut c = handler.config().clone();
            c.decode_mode =
                songbird::driver::DecodeMode::Decode(songbird::driver::DecodeConfig::default());
            c
        };
        handler.set_config(cfg);

        let ssrc_map: SsrcMap = Default::default();
        let sinks: std::sync::Arc<
            std::sync::Mutex<std::collections::HashMap<DiscordUserId, Arc<RecordSink>>>,
        > = Default::default();

        handler.add_global_event(
            songbird::events::Event::Core(songbird::events::CoreEvent::SpeakingStateUpdate),
            SsrcTracker { map: ssrc_map.clone() },
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(max_secs);
        handler.add_global_event(
            songbird::events::Event::Core(songbird::events::CoreEvent::VoiceTick),
            ListenerHandler { sinks: sinks.clone(), ssrc_map, deadline },
        );

        // Release the Call guard while waiting — holding it for the whole
        // recording would deadlock playback.
        drop(handler);

        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let done = {
                let sinks = sinks.lock().unwrap();
                sinks.values().any(|s| s.speech_secs() >= target_secs)
                    || std::time::Instant::now() >= deadline
            };
            if done {
                break;
            }
        }

        // Stop capturing and restore the cheaper decode mode.
        let mut handler = handler_lock.lock().await;
        handler.remove_all_global_events();
        let mut cfg = handler.config().clone();
        cfg.decode_mode = songbird::driver::DecodeMode::Decrypt;
        handler.set_config(cfg);

        let mut out = std::collections::HashMap::new();
        for (user_id, sink) in sinks.lock().unwrap().drain() {
            let samples = sink.samples.lock().unwrap().clone();
            if !samples.is_empty() {
                out.insert(user_id, samples);
            }
        }
        out
    }
}

/// True if the samples look like actual speech (simple RMS gate) — guards
/// against creating a clone from dead air. Requires ~5s of audible signal.
pub fn has_speech(samples: &[f32]) -> bool {
    if samples.is_empty() {
        return false;
    }
    // Loud 100ms windows at the capture rate (48kHz), requiring ~5s total.
    let window = TICK_SAMPLE_RATE as usize / 10;
    let loud_secs: usize = samples
        .chunks(window)
        .map(|c| c.iter().map(|s| s * s).sum::<f32>() / window as f32)
        .filter(|&rms| rms > 0.0003) // ~-35dBFS
        .count();
    loud_secs as f32 * 0.1 >= 5.0
}

