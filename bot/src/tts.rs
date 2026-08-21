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

pub async fn get_tts_fakeyou(text: &str, voice: &str) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    // FakeYou API integration will be implemented in the next step
    Err("FakeYou not implemented yet".into())
}
