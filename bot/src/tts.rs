use id3::{Tag, TagLike, Version};
use md5::compute as md5_compute;
use std::path::Path;
use std::sync::OnceLock;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Shared reqwest client for standard HTTP requests (Google TTS, downloads).
/// Reusing a single client avoids creating a new TLS context and connection
/// pool on every request, reducing latency and resource usage.
static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// Shared reqwest client with a generous timeout for FakeYou jobs, which
/// can take a while to complete.
static FAKEYOU_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

pub fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| reqwest::Client::new())
}

fn fakeyou_client() -> &'static reqwest::Client {
    FAKEYOU_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("Failed to build FakeYou HTTP client")
    })
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

pub fn write_id3_tags(file_path: &str, artist: &str, title: &str, lyrics: &str) {
    let mut tag = Tag::new();
    tag.set_artist(artist);
    tag.set_title(title);
    tag.add_frame(id3::frame::Lyrics {
        lang: "ita".to_string(),
        description: String::new(),
        text: lyrics.to_string(),
    });
    if let Err(e) = tag.write_to_path(file_path, Version::Id3v24) {
        log::warn!("write_id3_tags: failed to write tags to {}: {}", file_path, e);
    }
}

pub fn get_file_path(voice: &str, text: &str) -> String {
    let hash = format!("{:x}", md5_compute(text));
    let voice_token = get_voice_token(voice);
    let file_path = format!("audios/{}_{}.mp3", voice_token, hash);
    log::debug!("get_file_path: voice={}, text={}, path={}", voice, text, file_path);
    file_path
}

pub async fn compress_and_save_mp3(input_bytes: Vec<u8>, file_path: &str) -> std::io::Result<()> {
    // Compress to 64k bitrate, mono channel to save disk space
    std::fs::create_dir_all("audios")?;
    log::debug!("compress_and_save_mp3: saving to {}", file_path);
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-i", "pipe:0", "-b:a", "64k", "-ac", "1", "-y", file_path])
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
        } else {
            log::debug!("compress_and_save_mp3: completed for {}", file_path);
        }
    } else if !output.success() {
        log::error!(
            "compress_and_save_mp3: ffmpeg exited with code {:?} for {} (no stderr captured)",
            exit_code,
            file_path
        );
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

    // Should never reach here, but satisfy the compiler
    Err(last_error.unwrap_or_else(|| "Google TTS: all attempts failed".to_string()).into())
}

#[derive(serde::Deserialize)]
struct FakeYouJobResponse {
    success: bool,
    inference_job_token: Option<String>,
}

#[derive(serde::Deserialize)]
struct FakeYouStatusResponse {
    success: bool,
    state: Option<FakeYouJobState>,
}

#[derive(serde::Deserialize)]
struct FakeYouJobState {
    status: Option<String>,
    maybe_public_bucket_wav_audio_path: Option<String>,
}

pub async fn get_tts_fakeyou(text: &str, voice: &str) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    log::debug!("get_tts_fakeyou: starting job for voice {}", voice);
    let voice_token = get_voice_token(voice);
    if voice_token == "Google" {
        return Err(format!("Invalid or non-FakeYou voice: {}", voice).into());
    }

    // Use a shared client with a generous timeout (FakeYou jobs can take a while)
    let client = fakeyou_client();

    let idempotency_token = uuid::Uuid::new_v4().to_string();
    let body = serde_json::json!({
        "uuid_idempotency_token": idempotency_token,
        "tts_model_token": voice_token,
        "inference_text": text
    });

    log::info!("get_tts_fakeyou: submitting inference job for voice {}", voice);

    // Submit the inference job, with one retry on rate limit (429).
    let mut attempts = 0;
    let resp_json = loop {
        attempts += 1;
        let resp = client.post("https://api.fakeyou.com/tts/inference")
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            if attempts >= 2 {
                return Err("FakeYou rate limited (429), try again later".into());
            }
            log::warn!("get_tts_fakeyou: rate limited on inference, waiting 10s and retrying once");
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            continue;
        }

        if !resp.status().is_success() {
            return Err(format!("FakeYou API returned status: {}", resp.status()).into());
        }

        let json = resp.json::<FakeYouJobResponse>().await?;
        if !json.success {
            return Err("FakeYou API failed to start job".into());
        }
        break json;
    };

    let job_token = resp_json.inference_job_token.ok_or("No inference job token received")?;
    log::debug!("get_tts_fakeyou: inference job token {}", job_token);

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
            return Err("FakeYou job timed out after 120 seconds".into());
        }
        retries += 1;

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        log::debug!("get_tts_fakeyou: polling status (attempt {})", retries);

        let status_resp = client.get(format!("https://api.fakeyou.com/tts/job/{}", job_token))
            .header("Accept", "application/json")
            .send()
            .await?;

        // Handle rate limiting on polling with a backoff
        if status_resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            log::warn!("get_tts_fakeyou: rate limited on polling, waiting 10s");
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            continue;
        }

        if !status_resp.status().is_success() {
            return Err(format!("FakeYou polling returned status: {}", status_resp.status()).into());
        }

        let status_json = status_resp.json::<FakeYouStatusResponse>().await?;

        if !status_json.success {
            return Err("FakeYou API job status request failed".into());
        }

        if let Some(state) = status_json.state {
            if let Some(status_str) = &state.status {
                log::debug!("get_tts_fakeyou: job status = {}", status_str);
                match status_str.as_str() {
                    "complete_success" => {
                        if let Some(wav_path) = &state.maybe_public_bucket_wav_audio_path {
                            // The Python library (fakeyou.py v1.3.0) constructs the
                            // download URL as: https://cdn-2.fakeyou.com + wav_path
                            let media_url = format!("https://cdn-2.fakeyou.com{}", wav_path);
                            log::info!("get_tts_fakeyou: downloading from {}", media_url);
                            let media_resp = client.get(&media_url).send().await?;
                            if !media_resp.status().is_success() {
                                return Err(format!("Failed to download audio: status {}", media_resp.status()).into());
                            }
                            let bytes = media_resp.bytes().await?.to_vec();
                            if bytes.len() < 100 {
                                return Err(format!("Downloaded audio too small: {} bytes", bytes.len()).into());
                            }
                            log::info!("get_tts_fakeyou: downloaded {} bytes", bytes.len());
                            return Ok(bytes);
                        } else {
                            return Err("Job completed but no audio path provided".into());
                        }
                    }
                    "complete_failure" | "dead" => {
                        return Err(format!("FakeYou job failed with status: {}", status_str).into());
                    }
                    "attempt_failed" => {
                        // The Python library raises an exception immediately on attempt_failed.
                        // FakeYou does not retry after this status.
                        return Err("FakeYou job attempt failed".into());
                    }
                    _ => {
                        // "pending" or "started" — keep polling
                    }
                }
            }
        }
    }
}

pub struct TtsResult {
    pub file_path: String,
    pub actual_voice: String,
    pub fallback: bool,
}

pub async fn get_or_generate_tts(text: &str, voice: &str) -> Result<TtsResult, Box<dyn std::error::Error + Send + Sync>> {
    let save_mp3 = std::env::var("SAVE_MP3_ON_DISK")
        .unwrap_or_else(|_| "false".to_string())
        .to_lowercase() == "true";

    // When disk saving is enabled, check the cache first
    if save_mp3 {
        let file_path = get_file_path(voice, text);
        log::debug!("get_or_generate_tts: checking cache for {}", file_path);
        if Path::new(&file_path).exists() {
            log::debug!("get_or_generate_tts: cache hit for {}", file_path);
            return Ok(TtsResult { file_path, actual_voice: voice.to_string(), fallback: false });
        }

        // If the requested voice is a FakeYou voice and its specific file doesn't
        // exist, check whether a Google fallback was previously cached for the
        // same text.  When FakeYou fails and falls back to Google, the file is
        // saved as Google_{hash}.mp3 — so on the next request for the same
        // FakeYou voice we should reuse that cached fallback instead of
        // retrying FakeYou (which will likely fail again) every time.
        if voice != "Google" {
            let google_fallback_path = get_file_path("Google", text);
            if Path::new(&google_fallback_path).exists() {
                log::info!(
                    "get_or_generate_tts: FakeYou cache miss, but Google fallback exists — reusing {}",
                    google_fallback_path
                );
                return Ok(TtsResult {
                    file_path: google_fallback_path,
                    actual_voice: "Google".to_string(),
                    fallback: true,
                });
            }
        }
    }

    log::info!("get_or_generate_tts: generating TTS for voice {}", voice);
    let (bytes, actual_voice, fallback) = if voice == "Google" {
        (get_tts_google(text).await?, "Google".to_string(), false)
    } else {
        match get_tts_fakeyou(text, voice).await {
            Ok(b) => (b, voice.to_string(), false),
            Err(e) => {
                log::warn!("get_or_generate_tts: falling back to Google for voice {}", voice);
                log::error!("FakeYou failed, falling back to Google: {}", e);
                (get_tts_google(text).await?, "Google".to_string(), true)
            }
        }
    };

    let save_path = if save_mp3 {
        // When disk saving is enabled, save permanently to audios/ with proper naming
        if fallback {
            get_file_path("Google", text)
        } else {
            get_file_path(voice, text)
        }
    } else {
        // When disk saving is disabled, save to a temp file for playback.
        // Include the voice token in the filename so that the same text
        // requested with different voices doesn't overwrite each other.
        // Use actual_voice (not the requested voice) so that a fallback
        // Google file is named with the Google token, not the FakeYou token.
        let hash = format!("{:x}", md5_compute(text));
        let voice_token = get_voice_token(&actual_voice);
        let temp_dir = std::env::var("TMP_DIR").unwrap_or_else(|_| "/tmp/discord-llm-bot".to_string());
        format!("{}/tts_{}_{}.mp3", temp_dir, voice_token, hash)
    };

    // Ensure temp directory exists when not saving to disk
    if !save_mp3 {
        let temp_dir = std::env::var("TMP_DIR").unwrap_or_else(|_| "/tmp/discord-llm-bot".to_string());
        std::fs::create_dir_all(&temp_dir)?;
    }

    compress_and_save_mp3(bytes, &save_path).await?;

    if save_mp3 {
        // Write ID3 tags (artist, title, lyrics) into the MP3 file (only for permanent files)
        let (artist, title) = if actual_voice == "Google" {
            ("Google", "Google")
        } else {
            (voice, get_voice_token(voice))
        };
        write_id3_tags(&save_path, artist, title, text);
    }

    // Schedule temp file cleanup when not saving to disk
    if !save_mp3 {
        let path_clone = save_path.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            let _ = tokio::fs::remove_file(&path_clone).await;
        });
    }

    Ok(TtsResult { file_path: save_path, actual_voice, fallback })
}
