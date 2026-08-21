use md5::compute as md5_compute;
use std::path::Path;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

pub fn get_file_path(voice: &str, text: &str) -> String {
    let hash = format!("{:x}", md5_compute(text));
    let voice_token = match voice {
        "Papa Francesco (FakeYou.com)" => "weight_gc8gsr41974q5ax35gvttr85v",
        "Silvio Berlusconi (FakeYou.com)" => "weight_324nvat7xvaawe146na154gwh",
        "Goku (FakeYou.com)" => "weight_wn689844yyr08jny6jyyvkwcp",
        "Gerry Scotti (FakeYou.com)" => "weight_ms1kzt5m09cfw1yn666cxhy88",
        "Peter Griffin (FakeYou.com)" => "weight_t0y9rpba3qjnq02da44ynfs45",
        "Homer Simpson (FakeYou.com)" => "weight_zw97bw3hbtm07qwkd2exna15b",
        _ => "Google",
    };
    format!("audios/{}_{}.mp3", voice_token, hash)
}

pub async fn compress_and_save_mp3(input_bytes: Vec<u8>, file_path: &str) -> std::io::Result<()> {
    // Compress to 64k bitrate, mono channel to save disk space
    let mut cmd = Command::new("ffmpeg");
    cmd.args(&["-i", "pipe:0", "-b:a", "64k", "-ac", "1", "-y", file_path])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let mut child = cmd.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&input_bytes).await?;
    }
    child.wait().await?;
    Ok(())
}

pub async fn get_tts_google(text: &str) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let url = format!(
        "https://translate.google.com/translate_tts?ie=UTF-8&q={}&tl=it&client=tw-ob",
        urlencoding::encode(text)
    );
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0")
        .send()
        .await?;
    let bytes = resp.bytes().await?.to_vec();
    Ok(bytes)
}

#[derive(serde::Deserialize)]
struct FakeYouJobResponse {
    success: bool,
    job_token: Option<String>,
}

#[derive(serde::Deserialize)]
struct FakeYouStatusResponse {
    success: bool,
    status: Option<serde_json::Value>,
    media_url: Option<String>,
}

pub async fn get_tts_fakeyou(text: &str, voice: &str) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let voice_token = match voice {
        "Papa Francesco (FakeYou.com)" => "weight_gc8gsr41974q5ax35gvttr85v",
        "Silvio Berlusconi (FakeYou.com)" => "weight_324nvat7xvaawe146na154gwh",
        "Goku (FakeYou.com)" => "weight_wn689844yyr08jny6jyyvkwcp",
        "Gerry Scotti (FakeYou.com)" => "weight_ms1kzt5m09cfw1yn666cxhy88",
        "Peter Griffin (FakeYou.com)" => "weight_t0y9rpba3qjnq02da44ynfs45",
        "Homer Simpson (FakeYou.com)" => "weight_zw97bw3hbtm07qwkd2exna15b",
        _ => return Err("Invalid voice".into()),
    };

    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "tts_model_token": voice_token,
        "inference_text": text
    });

    let resp = client.post("https://api.fakeyou.com/tts")
        .json(&body)
        .send()
        .await?
        .json::<FakeYouJobResponse>()
        .await?;

    if !resp.success {
        return Err("FakeYou API failed to start job".into());
    }

    let job_token = resp.job_token.ok_or("No job token received")?;

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let status_resp = client.get(format!("https://api.fakeyou.com/tts/job/{}", job_token))
            .send()
            .await?
            .json::<FakeYouStatusResponse>()
            .await?;

        if !status_resp.success {
            return Err("FakeYou API job status failed".into());
        }

        if let Some(status) = status_resp.status {
            if let Some(status_str) = status.as_str() {
                if status_str == "complete" {
                    if let Some(media_url) = status_resp.media_url {
                        let media_resp = client.get(&media_url).send().await?;
                        let bytes = media_resp.bytes().await?.to_vec();
                        return Ok(bytes);
                    } else {
                        return Err("No media url received".into());
                    }
                } else if status_str == "failed" {
                    return Err("FakeYou job failed".into());
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
    let file_path = get_file_path(voice, text);
    if Path::new(&file_path).exists() {
        return Ok(TtsResult { file_path, actual_voice: voice.to_string(), fallback: false });
    }

    let (bytes, actual_voice, fallback) = if voice == "Google" {
        (get_tts_google(text).await?, "Google".to_string(), false)
    } else {
        match get_tts_fakeyou(text, voice).await {
            Ok(b) => (b, voice.to_string(), false),
            Err(e) => {
                log::error!("FakeYou failed, falling back to Google: {}", e);
                (get_tts_google(text).await?, "Google".to_string(), true)
            }
        }
    };

    // When fallback occurs, save with Google filename so we don't cache
    // Google audio under a FakeYou filename
    let save_path = if fallback {
        get_file_path("Google", text)
    } else {
        file_path
    };

    compress_and_save_mp3(bytes, &save_path).await?;
    Ok(TtsResult { file_path: save_path, actual_voice, fallback })
}
