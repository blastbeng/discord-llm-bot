use id3::{Tag, TagLike, Version};
use md5::compute as md5_compute;
use std::sync::OnceLock;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Shared reqwest client for standard HTTP requests (Google TTS, downloads).
/// Reusing a single client avoids creating a new TLS context and connection
/// pool on every request, reducing latency and resource usage.
static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// Shared reqwest client with a generous timeout for FakeYou jobs, which
/// can take a while to complete. Uses a cookie store so that if we login
/// with FakeYou credentials, the session cookie persists across requests.
static FAKEYOU_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// FakeYou session token (cookie value) obtained via login. Stored after
/// a successful login so subsequent TTS requests are authenticated.
static FAKEYOU_SESSION_COOKIE: OnceLock<String> = OnceLock::new();

pub fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| reqwest::Client::new())
}

fn fakeyou_client() -> &'static reqwest::Client {
    FAKEYOU_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .cookie_store(true)
            .build()
            .expect("Failed to build FakeYou HTTP client")
    })
}

/// Login to FakeYou if credentials are configured (FAKEYOU_USERNAME and
/// FAKEYOU_PASSWORD env vars). This authenticates the session so the
/// shared client's cookie jar contains the session cookie for all
/// subsequent TTS requests. If credentials are not set, TTS proceeds
/// without authentication (may face stricter rate limits).
async fn fakeyou_login_if_configured() {
    let username = std::env::var("FAKEYOU_USERNAME").unwrap_or_default();
    let password = std::env::var("FAKEYOU_PASSWORD").unwrap_or_default();

    if username.is_empty() || password.is_empty() {
        log::info!("FakeYou: no credentials configured, using unauthenticated requests");
        return;
    }

    log::info!("FakeYou: logging in as {}", username);
    let client = fakeyou_client();
    let login_body = serde_json::json!({
        "username_or_email": username,
        "password": password,
    });

    match client
        .post("https://api.fakeyou.com/v1/login")
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .json(&login_body)
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            // Extract session cookie from Set-Cookie header
            if let Some(set_cookie) = resp.headers().get("set-cookie") {
                if let Ok(cookie_str) = set_cookie.to_str() {
                    // Parse "session=VALUE; ..." to extract the session token
                    if let Some(token) = cookie_str
                        .split(';')
                        .next()
                        .and_then(|s| s.split('=').nth(1))
                    {
                        let _ = FAKEYOU_SESSION_COOKIE.set(token.to_string());
                        log::info!("FakeYou: login successful (status {}), session cookie stored", status);
                        return;
                    }
                }
            }
            // If we couldn't extract a cookie but login returned 200, the
            // cookie jar in the reqwest client should still have it.
            if status.is_success() {
                log::info!("FakeYou: login returned success ({}), cookie jar updated", status);
            } else {
                let body = resp.text().await.unwrap_or_default();
                log::error!("FakeYou: login failed with status {}: {}", status, body);
            }
        }
        Err(e) => {
            log::error!("FakeYou: login request failed: {}", e);
        }
    }
}

/// Initialize FakeYou — call once at startup to login if credentials are configured.
pub async fn init_fakeyou() {
    fakeyou_login_if_configured().await;
}

/// All available voices (matches Python's get_available_voices)
pub const AVAILABLE_VOICES: &[&str] = &[
    "Google",
    "Goku (FakeYou.com)",
    "Gerry Scotti (FakeYou.com)",
    "Homer Simpson (FakeYou.com)",
    "Peter Griffin (FakeYou.com)",
    "Papa Francesco (FakeYou.com)",
    "Silvio Berlusconi (FakeYou.com)",
];

pub fn get_voice_token(voice: &str) -> &str {
    match voice {
        "Papa Francesco (FakeYou.com)" => "weight_gc8gsr41974q5ax35gvttr85v",
        "Silvio Berlusconi (FakeYou.com)" => "weight_324nvat7xvaawe146na154gwh",
        "Goku (FakeYou.com)" => "weight_wn689844yyr08jny6jyyvkwcp",
        "Gerry Scotti (FakeYou.com)" => "weight_ms1kzt5m09cfw1yn666cxhy88",
        "Peter Griffin (FakeYou.com)" => "weight_t0y9rpba3qjnq02da44ynfs45",
        "Homer Simpson (FakeYou.com)" => "weight_zw97bw3hbtm07qwkd2exna15b",
        _ => "Google",
    }
}

/// Check if a voice name is valid (excluding "random")
pub fn is_valid_voice(voice: &str) -> bool {
    voice == "random" || AVAILABLE_VOICES.contains(&voice)
}

/// Reverse-lookup a voice name from its token.
/// Used to display human-readable voice names for cached MP3 files
/// whose filenames contain the voice token (e.g. "weight_abc123_hash.mp3").
#[allow(dead_code)] // Used by the Discord bot's /random, not by the wapp-bot
pub fn get_voice_name_from_token(token: &str) -> &'static str {
    match token {
        "weight_gc8gsr41974q5ax35gvttr85v" => "Papa Francesco (FakeYou.com)",
        "weight_324nvat7xvaawe146na154gwh" => "Silvio Berlusconi (FakeYou.com)",
        "weight_wn689844yyr08jny6jyyvkwcp" => "Goku (FakeYou.com)",
        "weight_ms1kzt5m09cfw1yn666cxhy88" => "Gerry Scotti (FakeYou.com)",
        "weight_t0y9rpba3qjnq02da44ynfs45" => "Peter Griffin (FakeYou.com)",
        "weight_zw97bw3hbtm07qwkd2exna15b" => "Homer Simpson (FakeYou.com)",
        "Google" => "Google",
        _ => "Unknown",
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
        "demon" => Some("asetrate=44100*0.7,aresample=44100,atempo=1.4286,bass=g=8".to_string()),
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

#[derive(serde::Deserialize)]
struct FakeYouJobResponse {
    success: bool,
    inference_job_token: Option<String>,
    #[serde(default)]
    error_type: Option<String>,
    #[serde(default)]
    error_reason: Option<String>,
    #[serde(default)]
    error_message: Option<String>,
}

#[derive(serde::Deserialize)]
struct FakeYouStatusResponse {
    success: bool,
    state: Option<FakeYouJobState>,
    #[serde(default)]
    error_type: Option<String>,
    #[serde(default)]
    error_reason: Option<String>,
    #[serde(default)]
    error_message: Option<String>,
}

#[derive(serde::Deserialize)]
struct FakeYouJobState {
    status: Option<String>,
    maybe_public_bucket_wav_audio_path: Option<String>,
    #[serde(default)]
    maybe_extra_status_description: Option<String>,
}

pub async fn get_tts_fakeyou(text: &str, voice: &str) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    log::debug!("get_tts_fakeyou: starting job for voice {}", voice);
    let voice_token = get_voice_token(voice);
    if voice_token == "Google" {
        return Err(format!("Invalid or non-FakeYou voice: {}", voice).into());
    }

    // Use a shared client with a generous timeout (FakeYou jobs can take a while)
    let client = fakeyou_client();

    log::info!("get_tts_fakeyou: submitting inference job for voice {}", voice);

    // Submit the inference job, with one retry on rate limit (429).
    // Regenerate the idempotency token on each attempt so retries aren't
    // rejected as duplicate requests by FakeYou's deduplication logic.
    let mut attempts = 0;
    let resp_json = loop {
        attempts += 1;
        let body = serde_json::json!({
            "uuid_idempotency_token": uuid::Uuid::new_v4().to_string(),
            "tts_model_token": voice_token,
            "inference_text": text
        });
        log::debug!("get_tts_fakeyou: POST tts/inference (attempt {}) voice_token={}", attempts, voice_token);
        let resp = client.post("https://api.fakeyou.com/tts/inference")
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = resp.status();

        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            if attempts >= 2 {
                log::error!("get_tts_fakeyou: rate limited (429) after {} attempts", attempts);
                return Err("FakeYou rate limited (429), try again later".into());
            }
            log::warn!("get_tts_fakeyou: rate limited on inference (429), waiting 10s and retrying once");
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            continue;
        }

        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            log::error!(
                "get_tts_fakeyou: inference request failed with status {}: {}",
                status, body_text
            );
            return Err(format!("FakeYou inference API returned status {}: {}", status, body_text).into());
        }

        let json = match resp.json::<FakeYouJobResponse>().await {
            Ok(j) => j,
            Err(e) => {
                log::error!("get_tts_fakeyou: failed to parse inference response JSON: {}", e);
                return Err(format!("Failed to parse FakeYou inference response: {}", e).into());
            }
        };

        if !json.success {
            let error_type = json.error_type.as_deref().unwrap_or("unknown");
            let error_reason = json.error_reason.as_deref().unwrap_or("unknown");
            let error_msg = json.error_message.as_deref().unwrap_or("no error message");
            log::error!(
                "get_tts_fakeyou: inference failed — error_type={}, error_reason={}, error_message={}",
                error_type, error_reason, error_msg
            );
            return Err(format!(
                "FakeYou inference failed: {} — {} ({})",
                error_type, error_msg, error_reason
            ).into());
        }

        log::info!("get_tts_fakeyou: inference job accepted (attempt {})", attempts);
        break json;
    };

    let job_token = resp_json.inference_job_token.ok_or("No inference job token received")?;
    log::info!("get_tts_fakeyou: inference job token {}", job_token);

    poll_fakeyou_job(&client, &job_token).await
}

/// Poll a FakeYou inference job until completion, then download the audio.
async fn poll_fakeyou_job(
    client: &reqwest::Client,
    job_token: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let max_retries = 60; // 120 seconds max (2s per poll)
    let mut retries = 0;

    loop {
        if retries >= max_retries {
            log::error!("poll_fakeyou_job: timed out after {} retries (120s)", max_retries);
            return Err("FakeYou job timed out after 120 seconds".into());
        }
        retries += 1;

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        log::debug!("poll_fakeyou_job: polling status (attempt {}/{})", retries, max_retries);

        let status_resp = client.get(format!("https://api.fakeyou.com/tts/job/{}", job_token))
            .header("Accept", "application/json")
            .send()
            .await?;

        let status_code = status_resp.status();

        // Handle rate limiting on polling with a backoff
        if status_code == reqwest::StatusCode::TOO_MANY_REQUESTS {
            log::warn!("poll_fakeyou_job: rate limited on polling (429), waiting 10s");
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            continue;
        }

        if !status_code.is_success() {
            let body_text = status_resp.text().await.unwrap_or_default();
            log::error!(
                "poll_fakeyou_job: polling request failed with status {}: {}",
                status_code, body_text
            );
            return Err(format!("FakeYou polling returned status {}: {}", status_code, body_text).into());
        }

        let status_json = match status_resp.json::<FakeYouStatusResponse>().await {
            Ok(j) => j,
            Err(e) => {
                log::error!("poll_fakeyou_job: failed to parse status response JSON: {}", e);
                return Err(format!("Failed to parse FakeYou status response: {}", e).into());
            }
        };

        if !status_json.success {
            let error_type = status_json.error_type.as_deref().unwrap_or("unknown");
            let error_reason = status_json.error_reason.as_deref().unwrap_or("unknown");
            let error_msg = status_json.error_message.as_deref().unwrap_or("no error message");
            log::error!(
                "poll_fakeyou_job: status request failed — error_type={}, error_reason={}, error_message={}",
                error_type, error_reason, error_msg
            );
            return Err(format!(
                "FakeYou job status request failed: {} — {} ({})",
                error_type, error_msg, error_reason
            ).into());
        }

        if let Some(state) = status_json.state {
            if let Some(status_str) = &state.status {
                let extra = state.maybe_extra_status_description.as_deref().unwrap_or("");
                log::debug!(
                    "poll_fakeyou_job: job status = {}{}",
                    status_str,
                    if extra.is_empty() { String::new() } else { format!(" ({})", extra) }
                );
                match status_str.as_str() {
                    "complete_success" => {
                        if let Some(wav_path) = &state.maybe_public_bucket_wav_audio_path {
                            // The Python library (fakeyou.py v1.3.0) constructs the
                            // download URL as: https://cdn-2.fakeyou.com + wav_path
                            let media_url = format!("https://cdn-2.fakeyou.com{}", wav_path);
                            log::info!("poll_fakeyou_job: downloading from {}", media_url);
                            let media_resp = client.get(&media_url).send().await?;
                            let media_status = media_resp.status();
                            if !media_status.is_success() {
                                let body_text = media_resp.text().await.unwrap_or_default();
                                log::error!(
                                    "poll_fakeyou_job: audio download failed with status {}: {}",
                                    media_status, body_text
                                );
                                return Err(format!(
                                    "Failed to download audio: status {}: {}",
                                    media_status, body_text
                                ).into());
                            }
                            let bytes = media_resp.bytes().await?.to_vec();
                            if bytes.len() < 100 {
                                log::error!("poll_fakeyou_job: downloaded audio too small: {} bytes", bytes.len());
                                return Err(format!("Downloaded audio too small: {} bytes", bytes.len()).into());
                            }
                            log::info!("poll_fakeyou_job: downloaded {} bytes", bytes.len());
                            return Ok(bytes);
                        } else {
                            log::error!("poll_fakeyou_job: job completed but no audio path provided");
                            return Err("Job completed but no audio path provided".into());
                        }
                    }
                    "complete_failure" | "dead" => {
                        log::error!("poll_fakeyou_job: job failed with status: {}", status_str);
                        return Err(format!("FakeYou job failed with status: {}", status_str).into());
                    }
                    "attempt_failed" => {
                        // The Python library raises an exception immediately on attempt_failed.
                        // FakeYou does not retry after this status.
                        log::error!("poll_fakeyou_job: job attempt failed");
                        return Err("FakeYou job attempt failed".into());
                    }
                    _ => {
                        // "pending" or "started" — keep polling
                    }
                }
            } else {
                log::debug!("poll_fakeyou_job: state has no status field (attempt {})", retries);
            }
        } else {
            log::debug!("poll_fakeyou_job: response has no state (attempt {})", retries);
        }
    }
}

pub struct TtsResult {
    pub file_path: String,
    #[allow(dead_code)] // Used by the Discord bot, not by the wapp-bot
    pub actual_voice: String,
    #[allow(dead_code)] // Used by the Discord bot, not by the wapp-bot
    pub fallback: bool,
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
                return Ok(TtsResult { file_path: temp_path, actual_voice: voice.to_string(), fallback: false });
            }
            return Ok(TtsResult { file_path: plain_path, actual_voice: voice.to_string(), fallback: false });
        }

        // FakeYou voice: reuse a previously-cached Google fallback plain audio.
        if voice != "Google" {
            let google_plain = get_file_path("Google", text);
            if tokio::fs::try_exists(&google_plain).await.unwrap_or(false) {
                log::info!("get_or_generate_tts: reusing cached Google fallback {}", google_plain);
                if apply_effect {
                    let temp_path = apply_effect_to_temp(&google_plain, text, effect, "Google").await?;
                    return Ok(TtsResult { file_path: temp_path, actual_voice: "Google".to_string(), fallback: true });
                }
                return Ok(TtsResult { file_path: google_plain, actual_voice: "Google".to_string(), fallback: true });
            }
        }
    }

    // 2) GENERATE the audio (with fallback to Google).
    log::info!("get_or_generate_tts: generating TTS for voice {}", voice);
    let (bytes, actual_voice, fallback) = if voice == "Google" {
        (get_tts_google(text).await?, "Google".to_string(), false)
    } else {
        match get_tts_fakeyou(text, voice).await {
            Ok(b) => {
                log::info!("get_or_generate_tts: FakeYou succeeded for voice {}", voice);
                (b, voice.to_string(), false)
            }
            Err(e) => {
                log::warn!(
                    "get_or_generate_tts: FakeYou failed for voice '{}', falling back to Google: {}",
                    voice, e
                );
                (get_tts_google(text).await?, "Google".to_string(), true)
            }
        }
    };

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
                let (artist, title) = if actual_voice == "Google" {
                    ("Google", "Google")
                } else {
                    (actual_voice.as_str(), get_voice_token(&actual_voice))
                };
                write_id3_tags(&plain_path, artist, title, text);
                let path_clone = temp_path.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                    let _ = tokio::fs::remove_file(&path_clone).await;
                });
                return Ok(TtsResult { file_path: temp_path, actual_voice, fallback });
            } else {
                compress_and_save_mp3(bytes, &plain_path).await?;
                let (artist, title) = if actual_voice == "Google" {
                    ("Google", "Google")
                } else {
                    (actual_voice.as_str(), get_voice_token(&actual_voice))
                };
                write_id3_tags(&plain_path, artist, title, text);
            }
        }
        // Plain audio now cached — return it (no effect path).
        return Ok(TtsResult { file_path: plain_path, actual_voice, fallback });
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
    Ok(TtsResult { file_path: temp_path, actual_voice, fallback })
}
