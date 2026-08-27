use id3::{Tag, TagLike, Version};
use md5::compute as md5_compute;
use std::sync::{Arc, OnceLock};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

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

/// All available voices (matches Python's get_available_voices)
pub const AVAILABLE_VOICES: &[&str] = &["Google"];

pub fn get_voice_token(_voice: &str) -> &str {
    "Google"
}

/// Check if a voice name is valid (excluding "random")
pub fn is_valid_voice(voice: &str) -> bool {
    voice == "random" || AVAILABLE_VOICES.contains(&voice)
}

/// Reverse-lookup a voice name from its token.
/// Used to display human-readable voice names for cached MP3 files
/// whose filenames contain the voice token (e.g. "Google_hash.mp3").
pub fn get_voice_name_from_token(token: &str) -> &'static str {
    match token {
        "Google" => "Google",
        _ => "Unknown",
    }
}

/// Read the lyrics (sentence text) from an MP3 file's ID3 tags.
/// Returns None if the file doesn't exist, has no ID3 tags, or has no lyrics frame.
/// Used by /random to recover the original sentence text from cached MP3 files.
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

pub fn get_file_path(voice: &str, text: &str) -> String {
    get_file_path_with_effect(voice, text, "none")
}

/// Compute the cache file path, including the effect name in the filename
/// when an effect is applied. This ensures that filtered audio is cached
/// separately from unfiltered audio, so the same text+voice+effect combo
/// is only generated once.
pub fn get_file_path_with_effect(voice: &str, text: &str, effect: &str) -> String {
    let hash = format!("{:x}", md5_compute(text));
    let voice_token = get_voice_token(voice);
    let file_path = if effect != "none" && effect != "random" {
        format!("audios/{}_{}_{}.mp3", voice_token, effect, hash)
    } else {
        format!("audios/{}_{}.mp3", voice_token, hash)
    };
    log::debug!("get_file_path: voice={}, text={}, effect={}, path={}", voice, text, effect, file_path);
    file_path
}

/// Available voice effects that can be applied to TTS audio.
/// Each maps to an ffmpeg audio filter chain.
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

/// Get the ffmpeg filter string for a given effect name.
/// Returns None for "none" (no filtering applied).
pub fn get_effect_filter(effect: &str) -> Option<String> {
    match effect {
        "echo" => Some("aecho=0.8:0.9:1000:0.3".to_string()),
        "reverb" => Some("aecho=0.7:0.5:1800:0.3,aecho=0.7:0.5:600:0.2".to_string()),
        "bass" => Some("bass=g=10,equalizer=f=80:t=q:w=1:g=5".to_string()),
        "chipmunk" => Some("asetrate=44100*1.5,aresample=44100,atempo=0.6667".to_string()),
        // Demon voice: drops pitch to ~50% and keeps the audio slow so the
        // speech actually sounds deep and rumbling instead of just sped-up.
        // The previous filter did `asetrate=44100*0.6` (pitch down) then
        // `aresample=44100,atempo=1.6667` which raised the tempo back to
        // normal, so the net result was a bass-boosted speed-up rather than
        // a demonic voice. We omit the atempo stage here, add light reverb
        // for a cavernous feel, and boost the low end to thicken the tone.
        "demon" => Some("asetrate=44100*0.5,aresample=44100,bass=g=18,aecho=0.8:0.7:1000:0.3".to_string()),
        "telephone" => Some("highpass=f=300,lowpass=f=3400".to_string()),
        "underwater" => Some("lowpass=f=400,bass=g=15,atempo=0.8".to_string()),
        _ => None,
    }
}

/// Check if an effect name is valid.
pub fn is_valid_effect(effect: &str) -> bool {
    AVAILABLE_EFFECTS.contains(&effect) || effect == "random"
}

#[allow(dead_code)]
pub async fn compress_and_save_mp3(input_bytes: Vec<u8>, file_path: &str) -> std::io::Result<()> {
    compress_and_save_mp3_with_effect(input_bytes, file_path, "none").await
}

/// Compress and save MP3 with an optional audio effect applied via ffmpeg.
/// When effect is "none", the behavior is identical to compress_and_save_mp3.
pub async fn compress_and_save_mp3_with_effect(input_bytes: Vec<u8>, file_path: &str, effect: &str) -> std::io::Result<()> {
    // Compress to 64k bitrate, mono channel to save disk space
    tokio::fs::create_dir_all("audios").await?;
    log::debug!("compress_and_save_mp3: saving to {} with effect: {}", file_path, effect);
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-i", "pipe:0", "-b:a", "64k", "-ac", "1", "-y"]);

    // Apply audio effect if one is specified (and not "none")
    if let Some(filter) = get_effect_filter(effect) {
        log::info!("compress_and_save_mp3: applying ffmpeg filter: {}", filter);
        cmd.args(["-af", &filter]);
    }

    cmd.arg(file_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&input_bytes).await?;
    }
    // Capture stderr so we can log it if ffmpeg fails — previously it was
    // discarded, making it impossible to diagnose compression failures.
    let stderr = child.stderr.take();
    let output = child.wait().await?;
    let exit_code = output.code();

    if let Some(mut stderr) = stderr {
        use tokio::io::AsyncReadExt;
        let mut buf = String::new();
        let _ = stderr.read_to_string(&mut buf).await;
        if !output.success() {
            log::error!(
                "compress_and_save_mp3: ffmpeg exited with code {:?} for {}: {}",
                exit_code,
                file_path,
                buf.trim()
            );
            return Err(std::io::Error::other(format!(
                "ffmpeg exited with code {:?}: {}",
                exit_code,
                buf.trim()
            )));
        } else {
            log::debug!("compress_and_save_mp3: completed for {}", file_path);
        }
    } else if !output.success() {
        log::error!(
            "compress_and_save_mp3: ffmpeg exited with code {:?} for {} (no stderr captured)",
            exit_code,
            file_path
        );
        return Err(std::io::Error::other(format!(
            "ffmpeg exited with code {:?} (no stderr captured)",
            exit_code
        )));
    } else {
        log::debug!("compress_and_save_mp3: completed for {}", file_path);
    }
    Ok(())
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
    pub actual_voice: String,
}

pub async fn get_or_generate_tts_with_effect(text: &str, voice: &str, effect: &str) -> Result<TtsResult, Box<dyn std::error::Error + Send + Sync>> {
    get_or_generate_tts_inner(text, voice, effect).await
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
    let voice_token = get_voice_token(actual_voice);
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

async fn get_or_generate_tts_inner(text: &str, voice: &str, effect: &str) -> Result<TtsResult, Box<dyn std::error::Error + Send + Sync>> {
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

    // 2) GENERATE the audio.
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
                // …and cache the plain audio for future reuse.
                compress_and_save_mp3(bytes, &plain_path).await?;
                // Title is the spoken text, artist is the voice — so the audio
                // shows e.g. "Google - <sentence>" instead of "Google - Google".
                write_id3_tags(&plain_path, &actual_voice, text, text);
                let path_clone = temp_path.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                    let _ = tokio::fs::remove_file(&path_clone).await;
                });
                return Ok(TtsResult { file_path: temp_path, actual_voice });
            } else {
                compress_and_save_mp3(bytes, &plain_path).await?;
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
        compress_and_save_mp3(bytes, &temp_path).await?;
    }
    let path_clone = temp_path.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
        let _ = tokio::fs::remove_file(&path_clone).await;
    });
    Ok(TtsResult { file_path: temp_path, actual_voice })
}
