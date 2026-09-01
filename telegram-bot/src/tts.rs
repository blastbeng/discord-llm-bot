use id3::{Tag, TagLike, Version};
use md5::compute as md5_compute;
use std::sync::{Arc, OnceLock};
use crate::audio_effects::compress_and_save_mp3_with_effect;

/// A reqwest DNS resolver that resolves hostnames to IPv4 addresses only.
///
/// Docker's default bridge network has no IPv6 connectivity, but the system
/// DNS inside the container can still return IPv6 addresses for hosts. When
/// a container tries to connect over IPv6 on an IPv6-less bridge, the request
/// fails or behaves unreliably. Forcing IPv4 sidesteps this entirely.
#[derive(Clone)]
struct Ipv4OnlyResolver;

impl reqwest::dns::Resolve for Ipv4OnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            let host = name.as_str().to_string();
            let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .collect();
            // Prefer IPv4 addresses; fall back to everything if no IPv4 is
            // found (so e.g. local IPv6-only services still resolve).
            let ipv4: Vec<std::net::SocketAddr> =
                addrs.iter().filter(|a| a.is_ipv4()).copied().collect();
            let chosen: Vec<std::net::SocketAddr> = if ipv4.is_empty() { addrs } else { ipv4 };
            // Coerce into the `Box<dyn Iterator<Item = SocketAddr> + Send>`
            // trait object reqwest expects (a concrete Box<IntoIter> won't
            // unify with it).
            let iter: Box<dyn Iterator<Item = std::net::SocketAddr> + Send> = Box::new(chosen.into_iter());
            Ok(iter)
        })
    }
}

/// Build a reqwest client that forces IPv4-only DNS resolution (see
/// `Ipv4OnlyResolver`) to avoid broken IPv6 attempts on the Docker bridge.
fn build_client(timeout_secs: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .dns_resolver(Arc::new(Ipv4OnlyResolver))
        .build()
        .expect("Failed to build HTTP client")
}

/// Shared reqwest client for standard HTTP requests (Google TTS, downloads).
/// Reusing a single client avoids creating a new TLS context and connection
/// pool on every request, reducing latency and resource usage.
static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

pub fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| build_client(30))
}

/// All available voices (matches Python's get_available_voices).
/// Only "Google" is a built-in voice; user-cloned voices are dynamic and
/// resolved through the voiceclone sidecar. NOTE: /random and all default
/// voice code paths must always resolve to "Google" — cloned voices are
/// opt-in only (explicit --voice).
pub const AVAILABLE_VOICES: &[&str] = &["Google"];

/// Build the cache-path token for a voice. Google voices use the voice name
/// itself; cloned voices use "clone|<name>" (normalized to '_' in file paths).
pub fn get_voice_token(voice: &str) -> String {
    if voice.starts_with("clone|") {
        return voice.to_string();
    }
    if let Some(name) = voice.strip_prefix("clone:") {
        return format!("clone|{name}");
    }
    "Google".to_string()
}

/// HTTP helpers for the voiceclone sidecar service (create/list/delete/use
/// cloned voices). The sidecar may be down; calls fail fast and the command
/// reports an error — they never degrade to a different voice.
static VC_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn vc_client() -> &'static reqwest::Client {
    VC_CLIENT.get_or_init(|| build_client(600))
}

fn voiceclone_base_url() -> Option<String> {
    match std::env::var("VOICECLONE_URL") {
        Ok(url) if !url.trim().is_empty() => Some(url.trim().trim_end_matches('/').to_string()),
        _ => None,
    }
}

pub fn voiceclone_configured() -> bool {
    voiceclone_base_url().is_some()
}

pub struct ClonedVoice {
    pub name: String,
    pub owner: String,
}

/// List all cloned voices registered on the sidecar.
pub async fn list_cloned_voices() -> Result<Vec<ClonedVoice>, String> {
    let base = voiceclone_base_url().ok_or("voiceclone not configured")?;
    let resp = vc_client().get(format!("{base}/voices")).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("voices list failed: {}", resp.status()));
    }
    #[derive(serde::Deserialize)]
    struct Row {
        name: String,
        #[serde(default)]
        owner: String,
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let rows: Vec<Row> = serde_json::from_value(body.get("voices").cloned().unwrap_or_default())
        .map_err(|e| e.to_string())?;
    Ok(rows.into_iter().map(|r| ClonedVoice { name: r.name, owner: r.owner }).collect())
}

/// Create a cloned voice from a base64-encoded audio sample (MP3/WAV).
pub async fn create_cloned_voice(name: &str, owner: &str, audio_base64: &str) -> Result<(), String> {
    let base = voiceclone_base_url().ok_or("voiceclone not configured")?;
    let resp = vc_client()
        .post(format!("{base}/voices"))
        .json(&serde_json::json!({ "name": name, "owner": owner, "audioBase64": audio_base64 }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status().is_success() {
        return Ok(());
    }
    Err(extract_sidecar_error(&resp.text().await.unwrap_or_default()))
}

/// Delete a previously created cloned voice.
pub async fn delete_cloned_voice(name: &str, owner: &str) -> Result<(), String> {
    let base = voiceclone_base_url().ok_or("voiceclone not configured")?;
    let resp = vc_client()
        .delete(format!("{base}/voices/{}", urlencoding::encode(name)))
        .query(&[("owner", owner)])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status().is_success() {
        return Ok(());
    }
    Err(extract_sidecar_error(&resp.text().await.unwrap_or_default()))
}

fn extract_sidecar_error(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
            return e.to_string();
        }
    }
    body.chars().take(160).collect()
}

/// Generate (or fetch from cache) the MP3 for a cloned voice via the sidecar.
pub async fn get_tts_cloned(voice: &str, owner: &str, text: &str) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let base = voiceclone_base_url().ok_or("voiceclone not configured")?;
    let resp = vc_client()
        .post(format!("{base}/synthesize"))
        .json(&serde_json::json!({ "voice": voice, "owner": owner, "text": text }))
        .send()
        .await?;
    if !resp.status().is_success() {
        let msg = resp.text().await.unwrap_or_default();
        return Err(extract_sidecar_error(&msg).into());
    }
    let body: serde_json::Value = resp.json().await?;
    let b64 = body.get("audioBase64").and_then(|b| b.as_str()).ok_or("voiceclone response missing audioBase64")?;
    base64_decode(b64).ok_or_else(|| "voiceclone response has invalid base64".into())
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let table: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for c in s.bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let v = table.iter().position(|&t| t == c)? as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xFF) as u8);
        }
    }
    Some(out)
}

/// Check if a voice name is valid (excluding "random"). Cloned voices are
/// shape-checked here; existence is verified against the sidecar at TTS time.
pub fn is_valid_voice(voice: &str) -> bool {
    if voice == "random" {
        return true;
    }
    if voice.starts_with("clone:") || voice.starts_with("clone|") {
        let name = voice
            .strip_prefix("clone:")
            .or_else(|| voice.strip_prefix("clone|"))
            .unwrap_or("");
        return !name.is_empty()
            && name.len() <= 64
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    }
    AVAILABLE_VOICES.contains(&voice)
}

/// Reverse-lookup a voice name from its token.
/// Used to display human-readable voice names for cached MP3 files
/// whose filenames contain the voice token (e.g. "Google_hash.mp3").
#[allow(dead_code)] // Used by the Discord bot's /random, not by the wapp-bot
pub fn get_voice_name_from_token(token: &str) -> String {
    if let Some(name) = token.strip_prefix("clone|") {
        return format!("clone:{}", name);
    }
    match token {
        "Google" => "Google".to_string(),
        _ => "Unknown".to_string(),
    }
}

/// Read the lyrics (sentence text) from an MP3 file's ID3 tags.
/// Returns None if the file doesn't exist, has no ID3 tags, or has no lyrics frame.
/// Used by /random to recover the original sentence text from cached MP3 files.
#[allow(dead_code)] // Used by the Discord bot's /random, not by the wapp-bot
pub fn read_id3_lyrics(file_path: &str) -> Option<String> {
    match Tag::read_from_path(file_path) {
        Ok(tag) => {
            // Look for a USLT (unsynchronized lyrics) frame
            tag.get("USLT")?.content().text().map(|s| s.to_string())
        }
        Err(_) => None,
    }
}

pub fn write_id3_tags(file_path: &str, artist: &str, title: &str, lyrics: &str) {
    // Use the configured language for the ID3 lyrics frame instead of
    // hardcoding "ita" — when LANG=eng the lyrics are in English.
    let lang_code = std::env::var("LANG").unwrap_or_else(|_| "ita".to_string());
    let id3_lang = match lang_code.as_str() {
        "eng" => "eng",
        _ => "ita",
    };
    let mut tag = Tag::new();
    tag.set_artist(artist);
    tag.set_title(title);
    tag.add_frame(id3::frame::Lyrics {
        lang: id3_lang.to_string(),
        description: String::new(),
        text: lyrics.to_string(),
    });
    if let Err(e) = tag.write_to_path(file_path, Version::Id3v24) {
        log::warn!("write_id3_tags: failed to write tags to {}: {}", file_path, e);
    }
}

#[allow(dead_code)] // Used by the Discord bot, not by the wapp-bot
pub fn get_file_path(voice: &str, text: &str) -> String {
    get_file_path_with_effect(voice, text, "none")
}

/// Compute the cache file path, including the effect name in the filename
/// when an effect is applied. This ensures that filtered audio is cached
/// separately from unfiltered audio, so the same text+voice+effect combo
/// is only generated once.
pub fn get_file_path_with_effect(voice: &str, text: &str, effect: &str) -> String {
    let hash = format!("{:x}", md5_compute(text));
    // Cloned-voice tokens contain a pipe separator, not filename-safe; use '_'.
    let voice_token = get_voice_token(voice).replace('|', "_");
    let file_path = if effect != "none" && effect != "random" {
        format!("audios/{}_{}_{}.mp3", voice_token, effect, hash)
    } else {
        format!("audios/{}_{}.mp3", voice_token, hash)
    };
    log::debug!("get_file_path: voice={}, text={}, effect={}, path={}", voice, text, effect, file_path);
    file_path
}



pub async fn compress_and_save_mp3(input_bytes: Vec<u8>, file_path: &str) -> std::io::Result<()> {
    crate::audio_effects::compress_and_save_mp3_with_effect(input_bytes, file_path, "none")
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))
}

pub async fn get_tts_google(text: &str) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    log::debug!("get_tts_google: requesting TTS for text length {}", text.len());
    let lang = std::env::var("LANG").unwrap_or_else(|_| "ita".to_string());
    let tts_lang = match lang.as_str() {
        "eng" => "en",
        _ => "it",
    };
    let url = format!(
        "https://translate.google.com/translate_tts?ie=UTF-8&q={}&tl={}&client=tw-ob",
        urlencoding::encode(text),
        tts_lang
    );
    let client = http_client();

    // Retry up to 3 times with short backoff to handle transient network errors
    let max_attempts = 3;
    let mut last_error: Option<String> = None;
    for attempt in 1..=max_attempts {
        let resp = match client
            .get(&url)
            .header("User-Agent", "Mozilla/5.0")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                log::warn!("get_tts_google: attempt {}/{} failed: {}", attempt, max_attempts, e);
                last_error = Some(e.to_string());
                if attempt < max_attempts {
                    tokio::time::sleep(std::time::Duration::from_millis(500 * attempt as u64)).await;
                    continue;
                }
                return Err(last_error.unwrap().into());
            }
        };

        if !resp.status().is_success() {
            log::warn!("get_tts_google: attempt {}/{} returned status {}", attempt, max_attempts, resp.status());
            last_error = Some(format!("Google TTS returned status: {}", resp.status()));
            if attempt < max_attempts {
                tokio::time::sleep(std::time::Duration::from_millis(500 * attempt as u64)).await;
                continue;
            }
            return Err(last_error.unwrap().into());
        }

        let bytes = resp.bytes().await?.to_vec();

        if bytes.len() < 100 {
            log::warn!("get_tts_google: attempt {}/{} returned only {} bytes", attempt, max_attempts, bytes.len());
            last_error = Some("Google TTS returned too few bytes, possibly an error page".to_string());
            if attempt < max_attempts {
                tokio::time::sleep(std::time::Duration::from_millis(500 * attempt as u64)).await;
                continue;
            }
            return Err(last_error.unwrap().into());
        }

        log::debug!("get_tts_google: received {} bytes", bytes.len());
        return Ok(bytes);
    }

    // Unreachable: every loop iteration either returns Ok/Err or continues.
    // Kept to satisfy the compiler's exhaustiveness check.
    #[allow(unreachable_code)]
    Err(last_error.unwrap_or_else(|| "Google TTS: all attempts failed".to_string()).into())
}

pub struct TtsResult {
    pub file_path: String,
    #[allow(dead_code)] // Part of the shared TTS API; not read by the telegram bot
    pub actual_voice: String,
}

#[allow(dead_code)] // Used by the Discord bot's background generator, not by the wapp-bot
pub async fn get_or_generate_tts(text: &str, voice: &str) -> Result<TtsResult, Box<dyn std::error::Error + Send + Sync>> {
    get_or_generate_tts_with_effect(text, voice, "none").await
}

/// Apply an audio effect to an existing (plain) audio file on-the-fly, writing
/// the filtered result to a temporary file that is scheduled for cleanup.
/// Returns the temporary file path. Effects are never persisted.
async fn apply_effect_to_temp(
    input_path: &str,
    text: &str,
    effect: &str,
    actual_voice: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let hash = format!("{:x}", md5_compute(text));
    let voice_token = get_voice_token(actual_voice).replace('|', "_");
    let temp_dir = std::env::var("TMP_DIR").unwrap_or_else(|_| "/tmp/discord-llm-bot".to_string());
    tokio::fs::create_dir_all(&temp_dir).await?;
    let temp_path = format!("{}/tts_{}_{}_{}.mp3", temp_dir, voice_token, effect, hash);
    let bytes = tokio::fs::read(input_path).await?;
    compress_and_save_mp3_with_effect(bytes, &temp_path, effect).await?;
    let path_clone = temp_path.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
        let _ = tokio::fs::remove_file(&path_clone).await;
    });
    Ok(temp_path)
}

pub async fn get_or_generate_tts_with_effect(text: &str, voice: &str, effect: &str) -> Result<TtsResult, Box<dyn std::error::Error + Send + Sync>> {
    let save_mp3 = std::env::var("SAVE_MP3_ON_DISK")
        .unwrap_or_else(|_| "false".to_string())
        .to_lowercase() == "true";
    let apply_effect = effect != "none" && effect != "random";

    // 1) CACHE CHECK — the cache stores only PLAIN (no-effect) audio. When an
    //    effect is requested we reuse the cached plain audio and apply the
    //    effect on-the-fly; the effected audio is never cached.
    if save_mp3 {
        // Plain audio for the requested voice.
        let plain_path = get_file_path(voice, text);
        log::debug!("get_or_generate_tts: checking plain cache for {}", plain_path);
        if tokio::fs::try_exists(&plain_path).await.unwrap_or(false) {
            log::info!("get_or_generate_tts: plain cache hit for {}", plain_path);
            if apply_effect {
                let temp_path = apply_effect_to_temp(&plain_path, text, effect, voice).await?;
                return Ok(TtsResult { file_path: temp_path, actual_voice: voice.to_string() });
            }
            return Ok(TtsResult { file_path: plain_path, actual_voice: voice.to_string() });
        }
    }

    // 2) GENERATE the audio. Cloned voices are delegated to the voiceclone
    //    sidecar (all other voices are Google only).
    if voice.starts_with("clone|") {
        log::info!("get_or_generate_tts: generating CLONED TTS for voice {}", voice);
        let owner = std::env::var("VOICECLONE_SHARED_OWNER").unwrap_or_default();
        let bytes = get_tts_cloned(voice.strip_prefix("clone|").unwrap_or(voice), &owner, text).await?;
        let actual_voice = voice.to_string();

        let temp_dir = std::env::var("TMP_DIR").unwrap_or_else(|_| "/tmp/discord-llm-bot".to_string());
        let hash = format!("{:x}", md5_compute(text));
        tokio::fs::create_dir_all(&temp_dir).await?;
        let temp_path = if apply_effect {
            format!("{}/tts_{}_{}_{}.mp3", temp_dir, get_voice_token(&actual_voice).replace('|', "_"), effect, hash)
        } else {
            format!("{}/tts_{}_{}.mp3", temp_dir, get_voice_token(&actual_voice).replace('|', "_"), hash)
        };
        if apply_effect {
            compress_and_save_mp3_with_effect(bytes, &temp_path, effect).await?;
        } else {
            tokio::fs::write(&temp_path, &bytes).await?;
        }
        let path_clone = temp_path.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            let _ = tokio::fs::remove_file(&path_clone).await;
        });
        return Ok(TtsResult { file_path: temp_path, actual_voice });
    }

    log::info!("get_or_generate_tts: generating TTS for voice {}", voice);
    let bytes = get_tts_google(text).await?;
    let actual_voice = "Google".to_string();

    let voice_token = get_voice_token(&actual_voice);
    let temp_dir = std::env::var("TMP_DIR").unwrap_or_else(|_| "/tmp/discord-llm-bot".to_string());
    let hash = format!("{:x}", md5_compute(text));

    // 3) PERSIST the plain audio (so future effect requests reuse it) and then
    //    apply the effect on-the-fly if requested.
    if save_mp3 {
        let plain_path = get_file_path(&actual_voice, text);
        if !tokio::fs::try_exists(&plain_path).await.unwrap_or(false) {
            if apply_effect {
                // Write the effected audio to a temp file for playback…
                tokio::fs::create_dir_all(&temp_dir).await?;
                let temp_path = format!("{}/tts_{}_{}_{}.mp3", temp_dir, voice_token, effect, hash);
                compress_and_save_mp3_with_effect(bytes.clone(), &temp_path, effect).await?;
                // …and cache the plain audio for future reuse. Write the raw
                // Google TTS bytes directly to disk instead of doing an
                // unnecessary MP3→PCM→MP3 round-trip (which can produce a
                // truncated file that symphonia can't decode).
                tokio::fs::write(&plain_path, &bytes).await?;
                // Title is the spoken text, artist is the voice — so the audio
                // shows e.g. "Google - <sentence>" in Telegram instead of
                // "Google - Google".
                write_id3_tags(&plain_path, &actual_voice, text, text);
                let path_clone = temp_path.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                    let _ = tokio::fs::remove_file(&path_clone).await;
                });
                return Ok(TtsResult { file_path: temp_path, actual_voice });
            } else {
                tokio::fs::write(&plain_path, &bytes).await?;
                // The title is the spoken text, the artist is the voice.
                write_id3_tags(&plain_path, &actual_voice, text, text);
            }
        }
        // Plain audio now cached — return it (no effect path).
        return Ok(TtsResult { file_path: plain_path, actual_voice });
    }

    // 4) Disk saving disabled — write plain or effected audio to a temp file.
    tokio::fs::create_dir_all(&temp_dir).await?;
    let temp_path = if apply_effect {
        format!("{}/tts_{}_{}_{}.mp3", temp_dir, voice_token, effect, hash)
    } else {
        format!("{}/tts_{}_{}.mp3", temp_dir, voice_token, hash)
    };
    if apply_effect {
        compress_and_save_mp3_with_effect(bytes, &temp_path, effect).await?;
    } else {
        tokio::fs::write(&temp_path, &bytes).await?;
    }
    let path_clone = temp_path.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
        let _ = tokio::fs::remove_file(&path_clone).await;
    });
    Ok(TtsResult { file_path: temp_path, actual_voice })
}
