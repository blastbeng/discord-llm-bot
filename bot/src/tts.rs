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
/// resolved through [`is_cloned_voice`] / [`owner_of_cloned_voice`].
/// NOTE: /random and every default-voice code path must always resolve to
/// "Google" — cloned voices are opt-in only (explicit --voice / slash choice).
pub const AVAILABLE_VOICES: &[&str] = &["Google"];

/// Build the cache-path token for a voice. Google voices use the voice name
/// itself; cloned voices use "clone|<name>" (normalized to '_' in file paths).
/// Accepts both the legacy "clone:<name>" syntax and plain clone names so old
/// cached files stay reachable.
pub fn get_voice_token(voice: &str) -> String {
    if voice.starts_with("clone|") {
        return voice.to_string();
    }
    if let Some(name) = voice.strip_prefix("clone:") {
        return format!("clone|{name}");
    }
    // Plain clone names ("Salvini") map to the clone token so temp/cache
    // paths can never collide with the Google token.
    if let Some(name) = clone_voice_name(voice) {
        return format!("clone|{name}");
    }
    "Google".to_string()
}

/// True when the name has the charset of a cloned-voice name and does not
/// collide with a built-in/reserved voice. Names are case-insensitively
/// reserved so nobody can shadow the main Google voice.
pub fn is_valid_clone_name(name: &str) -> bool {
    let reserved = ["google", "random", "none"];
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && !reserved.iter().any(|r| name.eq_ignore_ascii_case(r))
}

/// Resolve any accepted voice syntax to the plain cloned-voice name, or None
/// when the voice is not a clone request. Accepted forms: "Name" (plain,
/// current UX), legacy "clone:Name" / "clone|Name" (still honored), while
/// built-ins ("Google") and "random" never resolve to a clone.
pub fn clone_voice_name(voice: &str) -> Option<String> {
    if let Some(name) = voice.strip_prefix("clone:").or_else(|| voice.strip_prefix("clone|")) {
        return Some(name.to_string());
    }
    if is_valid_clone_name(voice) && !AVAILABLE_VOICES.contains(&voice) {
        return Some(voice.to_string());
    }
    None
}

/// Pick a random voice for the /random command: a random entry from the
/// built-in Google voices plus every registered cloned voice. Returns
/// "Google" when the registry is unavailable (degrades gracefully instead of
/// failing the command). Only /random resolves "random" through this — every
/// other command defaults to Google.
pub async fn pick_random_voice() -> String {
    use rand::seq::SliceRandom;
    let mut pool: Vec<String> = AVAILABLE_VOICES.iter().map(|v| v.to_string()).collect();
    if let Ok(voices) = list_cloned_voices().await {
        pool.extend(voices.into_iter().map(|v| v.name));
    }
    let mut rng = rand::thread_rng();
    pool.choose(&mut rng).cloned().unwrap_or_else(|| "Google".to_string())
}

/// Check if a voice name is valid (excluding "random"). Cloned voices are
/// always accepted at this level — actual existence is verified against the
/// fish.audio registry right before generating audio.
pub fn is_valid_voice(voice: &str) -> bool {
    if voice == "random" {
        return true;
    }
    if voice.starts_with("clone:") || voice.starts_with("clone|") {
        // Same charset fish clone names are restricted to.
        let name = voice
            .strip_prefix("clone:")
            .or_else(|| voice.strip_prefix("clone|"))
            .unwrap_or("");
        return is_valid_clone_name(name);
    }
    // Plain names: built-ins, plus any well-formed clone name (existence is
    // checked against the fish.audio registry at generation time).
    AVAILABLE_VOICES.contains(&voice) || is_valid_clone_name(voice)
}

// ─── fish.audio cloud voice cloning ─────────────────────────────────────
//
// Voice cloning runs on https://fish.audio (hosted voice-cloning + TTS
// service) instead of a local model — the RPi5 is far too slow for local
// inference. Locally we only keep the name → fish model-id mapping (plus the
// reference sample for re-registration) in the `voice_clones` table of the
// shared SQLite database.
//
// Endpoints used (Bearer auth via FISH_AUDIO_API_KEY):
//   • POST   /model       — clone: multipart (type=tts, title, train_mode=fast
//                           → model usable immediately, visibility=private,
//                           voices=<audio bytes>). 201 → {"_id": "..."}.
//   • DELETE /model/{id}  — remove a cloned model.
//   • POST   /v1/tts      — synthesize: JSON {text, reference_id}, header
//                           `model: <backend>`, response = raw MP3 bytes.
//   • POST   /v1/asr      — speech-to-text (multipart audio) used by the
//                           eavesdrop feature.

static FISH_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn fish_client() -> &'static reqwest::Client {
    // Generous timeout: clone creation uploads a full sample, fast training
    // happens server-side, and synthesizing long sentences can be slow.
    FISH_CLIENT.get_or_init(|| build_client(600))
}

fn fish_api_key() -> Option<String> {
    match std::env::var("FISH_AUDIO_API_KEY") {
        Ok(k) if !k.trim().is_empty() => Some(k.trim().to_string()),
        _ => None,
    }
}

/// TTS backend sent as the `model` header with /v1/tts.
/// "s2.1-pro-free" is the free tier of the current flagship model.
fn fish_tts_model() -> String {
    std::env::var("FISH_AUDIO_TTS_MODEL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "s2.1-pro-free".to_string())
}

fn fish_base_url() -> String {
    std::env::var("FISH_AUDIO_BASE_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "https://api.fish.audio".to_string())
}

/// The voice-clone feature is available when a fish.audio API key is set.
pub fn voiceclone_configured() -> bool {
    fish_api_key().is_some()
}

/// Lazily-opened connection to the shared SQLite DB that stores the
/// voice-clone registry (name, owner, fish model id, reference sample).
/// Same file and settings main.rs uses, so both pools safely coexist (WAL).
static VC_DB: tokio::sync::OnceCell<sqlx::SqlitePool> = tokio::sync::OnceCell::const_new();

async fn vc_db() -> Result<&'static sqlx::SqlitePool, String> {
    if let Some(pool) = VC_DB.get() {
        return Ok(pool);
    }
    let db_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "sqlite:config/discord-bot.sqlite3".to_string());
    let opts = db_url
        .parse::<sqlx::sqlite::SqliteConnectOptions>()
        .map_err(|e| e.to_string())?
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(5))
        .create_if_missing(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .connect_with(opts)
        .await
        .map_err(|e| e.to_string())?;
    // Same layout the old voiceclone sidecar used; `fish_model_id` is new.
    // Legacy rows (created before the fish.audio migration) carry a stored
    // `sample` but an empty `fish_model_id` and are re-registered on first
    // use (lazy migration, see get_tts_cloned).
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS voice_clones (
            name TEXT NOT NULL,
            owner TEXT NOT NULL DEFAULT '',
            origin TEXT NOT NULL DEFAULT 'sample',
            sample BLOB,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (name, owner)
        )",
    )
    .execute(&pool)
    .await
    .map_err(|e| e.to_string())?;
    let has_col: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('voice_clones') WHERE name = 'fish_model_id'",
    )
    .fetch_one(&pool)
    .await
    .map_err(|e| e.to_string())?;
    if has_col == 0 {
        sqlx::query("ALTER TABLE voice_clones ADD COLUMN fish_model_id TEXT NOT NULL DEFAULT ''")
            .execute(&pool)
            .await
            .map_err(|e| e.to_string())?;
    }
    let _ = VC_DB.set(pool);
    Ok(VC_DB.get().expect("voice-clone pool just set"))
}

pub struct ClonedVoice {
    pub name: String,
    pub owner: String,
}

/// List all cloned voices registered locally. fish.audio is the source of
/// truth for the actual models; this reads the local name → fish-id registry.
pub async fn list_cloned_voices() -> Result<Vec<ClonedVoice>, String> {
    let pool = vc_db().await?;
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT name, owner FROM voice_clones ORDER BY created_at",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows.into_iter().map(|(name, owner)| ClonedVoice { name, owner }).collect())
}

/// Upload `audio` to fish.audio as a new private TTS model titled `name`
/// (train_mode=fast → usable immediately). Returns the fish model id.
async fn fish_create_model(key: &str, name: &str, audio: Vec<u8>) -> Result<String, String> {
    let form = reqwest::multipart::Form::new()
        .text("type", "tts")
        .text("title", name.to_string())
        .text("train_mode", "fast")
        .text("visibility", "private")
        .part(
            "voices",
            reqwest::multipart::Part::bytes(audio)
                .file_name("sample.mp3")
                .mime_str("audio/mpeg")
                .map_err(|e| e.to_string())?,
        );
    let resp = fish_client()
        .post(format!("{}/model", fish_base_url()))
        .bearer_auth(key)
        .multipart(form)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "fish.audio model creation failed ({}): {}",
            status,
            extract_api_error(&body)
        ));
    }
    let model_id = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("_id").and_then(|i| i.as_str()).map(|s| s.to_string()))
        .ok_or_else(|| {
            format!("fish.audio did not return a model id: {}", extract_api_error(&body))
        })?;
    Ok(model_id)
}

/// Delete a model on fish.audio by id.
async fn fish_delete_model(key: &str, model_id: &str) -> Result<(), String> {
    let resp = fish_client()
        .delete(format!(
            "{}/model/{}",
            fish_base_url(),
            urlencoding::encode(model_id)
        ))
        .bearer_auth(key)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("{}: {}", status, extract_api_error(&body)));
    }
    Ok(())
}

/// Remove cached MP3s for a voice so overwritten/deleted voices don't keep
/// playing stale audio. Files are keyed by text hash only, with the voice
/// token prefix ("clone|<name>_" historically, "clone_<name>_" in paths).
async fn purge_voice_cache(name: &str) {
    let prefixes = [format!("clone|{name}_"), format!("clone_{name}_")];
    let mut removed = 0u32;
    if let Ok(mut entries) = tokio::fs::read_dir("audios").await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let fname = entry.file_name().to_string_lossy().to_string();
            if prefixes.iter().any(|p| fname.starts_with(p.as_str())) {
                if tokio::fs::remove_file(entry.path()).await.is_ok() {
                    removed += 1;
                }
            }
        }
    }
    if removed > 0 {
        log::info!("purge_voice_cache: removed {removed} cached files for voice '{name}'");
    }
}

/// Create (or overwrite) a cloned voice from a base64-encoded audio sample.
/// The sample is uploaded to fish.audio for cloning; the resulting fish model
/// id is stored in the local registry. Voice names are unique per owner — an
/// existing same-name voice is replaced (its old fish model is deleted and
/// stale cached MP3s are purged).
pub async fn create_cloned_voice(name: &str, owner: &str, audio_base64: &str) -> Result<(), String> {
    let key = fish_api_key().ok_or("fish.audio not configured")?;
    let name = name.trim();
    let audio = base64_decode(audio_base64).ok_or("could not decode audio sample")?;

    let pool = vc_db().await?;
    // Remember the previous fish model (if any) so the superseded cloud model
    // can be deleted after the new one is registered.
    let previous: Option<String> = sqlx::query_scalar::<_, String>(
        "SELECT fish_model_id FROM voice_clones WHERE name = ? AND owner = ?",
    )
    .bind(name)
    .bind(owner.trim())
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?
    .filter(|s| !s.is_empty());

    let model_id = fish_create_model(&key, name, audio.clone()).await?;

    // The sample is kept so the voice can be re-registered later if the fish
    // model disappears (e.g. deleted from the fish.audio dashboard).
    sqlx::query(
        "INSERT INTO voice_clones (name, owner, origin, sample, fish_model_id)
         VALUES (?, ?, 'sample', ?, ?)
         ON CONFLICT(name, owner) DO UPDATE SET
            sample = excluded.sample,
            fish_model_id = excluded.fish_model_id,
            created_at = CURRENT_TIMESTAMP",
    )
    .bind(name)
    .bind(owner.trim())
    .bind(&audio)
    .bind(&model_id)
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;

    if let Some(prev) = previous.filter(|p| p != &model_id) {
        if let Err(e) = fish_delete_model(&key, &prev).await {
            log::warn!("create_cloned_voice: could not delete superseded fish model {prev}: {e}");
        }
    }

    purge_voice_cache(name).await;
    log::info!("create_cloned_voice: voice '{name}' cloned on fish.audio (model {model_id})");
    Ok(())
}

/// Delete a previously created cloned voice (cloud model + local registry row
/// + cached MP3s).
pub async fn delete_cloned_voice(name: &str, owner: &str) -> Result<(), String> {
    let key = fish_api_key().ok_or("fish.audio not configured")?;
    let name = name.trim();
    let pool = vc_db().await?;

    // Owner-scoped first, then any owner (admins may delete shared voices —
    // the callers resolve permissions before reaching here).
    let row: Option<(Option<String>, String)> = sqlx::query_as(
        "SELECT fish_model_id, owner FROM voice_clones WHERE name = ? AND owner = ?",
    )
    .bind(name)
    .bind(owner.trim())
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?;
    let (fish_id, row_owner) = match row {
        Some(r) => r,
        None => sqlx::query_as(
            "SELECT fish_model_id, owner FROM voice_clones WHERE name = ? ORDER BY created_at LIMIT 1",
        )
        .bind(name)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("voice '{name}' not found"))?,
    };

    if let Some(id) = fish_id.filter(|s| !s.is_empty()) {
        fish_delete_model(&key, &id).await?;
    }
    sqlx::query("DELETE FROM voice_clones WHERE name = ? AND owner = ?")
        .bind(name)
        .bind(&row_owner)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    purge_voice_cache(name).await;
    log::info!("delete_cloned_voice: voice '{name}' deleted (owner '{row_owner}')");
    Ok(())
}

/// Transcribe recorded speech via fish.audio's ASR endpoint. Returns an empty
/// string when nothing intelligible was said — callers treat that as "no
/// transcript" and fall back to the name-only prompt.
pub async fn transcribe_audio(audio_base64: &str) -> Result<String, String> {
    let key = fish_api_key().ok_or("fish.audio not configured")?;
    let audio = base64_decode(audio_base64).ok_or("could not decode recorded audio")?;
    // Bot LANG codes ("ita"/"eng") → ISO codes ("it"/"en"). fish auto-detects
    // when the hint is missing, but the hint removes ambiguity for mixed chats.
    let lang = match std::env::var("LANG").unwrap_or_else(|_| "ita".to_string()).as_str() {
        "eng" => "en",
        _ => "it",
    };
    let form = reqwest::multipart::Form::new()
        .part(
            "audio",
            reqwest::multipart::Part::bytes(audio)
                .file_name("recording.mp3")
                .mime_str("audio/mpeg")
                .map_err(|e| e.to_string())?,
        )
        .text("language", lang)
        .text("ignore_timestamps", "true");
    let resp = fish_client()
        .post(format!("{}/v1/asr", fish_base_url()))
        .bearer_auth(key)
        .multipart(form)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "fish.audio asr failed ({}): {}",
            status,
            extract_api_error(&body)
        ));
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(body
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string())
}

/// Generate the MP3 for a cloned voice via fish.audio TTS. Returns raw MP3
/// bytes (the caller handles caching and effects).
pub async fn get_tts_cloned(
    voice: &str,
    owner: &str,
    text: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let key = fish_api_key().ok_or("fish.audio not configured")?;
    let pool = vc_db().await?;
    let voice = voice.trim();

    // Resolve the fish model: owner-scoped first, then any owner (lets
    // shared voices work for everyone unless a same-name voice exists —
    // same precedence the old sidecar used).
    let row: Option<(Option<String>, Option<Vec<u8>>)> = sqlx::query_as(
        "SELECT fish_model_id, sample FROM voice_clones WHERE name = ? AND owner = ?",
    )
    .bind(voice)
    .bind(owner.trim())
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?;
    let (fish_id, sample) = match row {
        Some(r) => r,
        None => sqlx::query_as(
            "SELECT fish_model_id, sample FROM voice_clones WHERE name = ? ORDER BY created_at LIMIT 1",
        )
        .bind(voice)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?
        .ok_or(format!("voice '{voice}' not found"))?,
    };

    // Legacy rows created before the fish.audio migration carry a stored
    // sample but no model id — re-register them on first use (fast training
    // makes the model immediately usable).
    let model_id = match fish_id.filter(|s| !s.is_empty()) {
        Some(id) => id,
        None => {
            let sample = sample
                .filter(|s| !s.is_empty())
                .ok_or(format!("voice '{voice}' has no fish model and no stored sample"))?;
            log::info!("get_tts_cloned: re-registering legacy voice '{voice}' on fish.audio");
            let id = fish_create_model(&key, voice, sample).await?;
            sqlx::query("UPDATE voice_clones SET fish_model_id = ? WHERE name = ?")
                .bind(&id)
                .bind(voice)
                .execute(pool)
                .await
                .map_err(|e| e.to_string())?;
            id
        }
    };

    let resp = fish_client()
        .post(format!("{}/v1/tts", fish_base_url()))
        .bearer_auth(key)
        .header("model", fish_tts_model())
        .json(&serde_json::json!({
            "text": text,
            "reference_id": model_id,
            "format": "mp3",
            "mp3_bitrate": 128,
        }))
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "fish.audio tts failed ({}): {}",
            status,
            extract_api_error(&body)
        )
        .into());
    }
    let bytes = resp.bytes().await?;
    if bytes.is_empty() {
        return Err("fish.audio returned empty audio".into());
    }
    Ok(bytes.to_vec())
}

/// Pull a short human-readable error message out of a fish.audio JSON body
/// ({"status": ..., "message": "..."}) or fall back to a truncated raw body.
fn extract_api_error(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        for field in ["message", "error", "detail", "reason"] {
            if let Some(e) = v.get(field).and_then(|e| e.as_str()) {
                return e.to_string();
            }
        }
    }
    body.chars().take(160).collect()
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    // Tiny standard-base64 decoder so we don't add a base64 dependency here.
    // (decode leniently: ignore whitespace)
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

/// Reverse-lookup a voice name from its token.
/// Used to display human-readable voice names for cached MP3 files
/// whose filenames contain the voice token (e.g. "Google_hash.mp3").
/// Cloned voices display as their plain name.
pub fn get_voice_name_from_token(token: &str) -> String {
    if let Some(name) = token.strip_prefix("clone|") {
        return name.to_string();
    }
    match token {
        "Google" => "Google".to_string(),
        _ => "Unknown".to_string(),
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
    // Cloned-voice tokens contain a pipe separator, which is not filename-safe;
    // normalize it to '_' for cache file paths (matches the historical sidecar
    // file layout when referenced from /random display logic). Plain clone names get the
    // clone| token here so "Salvini" and the legacy "clone:Salvini" resolve to
    // the same cache file — and can never collide with the Google token.
    let voice_token = if clone_voice_name(voice).is_some() {
        get_voice_token(&format!("clone|{}", clone_voice_name(voice).unwrap()))
            .replace('|', "_")
    } else {
        get_voice_token(voice).replace('|', "_")
    };
    let file_path = if effect != "none" && effect != "random" {
        format!("audios/{}_{}_{}.mp3", voice_token, effect, hash)
    } else {
        format!("audios/{}_{}.mp3", voice_token, hash)
    };
    log::debug!("get_file_path: voice={}, text={}, effect={}, path={}", voice, text, effect, file_path);
    file_path
}

// Effect-related constants and helpers (AVAILABLE_EFFECTS, is_valid_effect,
// compress_and_save_mp3_with_effect) are re-exported from `crate::audio_effects`.
// The ffmpeg-based `get_effect_filter` is kept here because it remains useful as
// a reference and for callers that still need the filter string (none in the
// production code paths use it directly anymore — see audio_effects.rs).

/// Get the ffmpeg filter string for a given effect name.
/// Returns None for "none" (no filtering applied).
#[allow(dead_code)]
pub fn get_effect_filter(effect: &str) -> Option<String> {
    match effect {
        // "bass", "telephone", "underwater", "echo", "reverb" effects were
        // removed entirely.
        "chipmunk" => Some("asetrate=44100*1.5,aresample=44100,atempo=0.6667".to_string()),
        // Demon voice: drops pitch to ~50% and keeps the audio slow so the
        // speech actually sounds deep and rumbling instead of just sped-up.
        // The previous filter did `asetrate=44100*0.6` (pitch down) then
        // `aresample=44100,atempo=1.6667` which raised the tempo back to
        // normal, so the net result was a bass-boosted speed-up rather than
        // a demonic voice.
        "demon" => Some("asetrate=44100*0.5,aresample=44100,bass=g=18".to_string()),
        _ => None,
    }
}

#[allow(dead_code)]
pub async fn compress_and_save_mp3(input_bytes: Vec<u8>, file_path: &str) -> std::io::Result<()> {
    compress_and_save_mp3_with_effect(input_bytes, file_path, "none")
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
    pub actual_voice: String,
    /// Set when a cloned-voice request ended up synthesized with another
    /// engine (e.g. the clone was unavailable and we fell back to
    /// Google). Commands surface this to the user instead of staying silent.
    pub fallback_used: Option<String>,
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
                return Ok(TtsResult { file_path: temp_path, actual_voice: voice.to_string(), fallback_used: None });
            }
            return Ok(TtsResult { file_path: plain_path, actual_voice: voice.to_string(), fallback_used: None });
        }
    }

    // 2) GENERATE the audio. Cloned voices are synthesized by the fish.audio
    //    cloud API (all other voices are Google only). Cloned audio is cached
    //    in the shared audios dir with the same token naming convention.
    // Cloned voices: plain names (current UX), legacy "clone:<name>" /
    // "clone|<name>" tokens still honored. If fish.audio can't produce the
    // voice (missing, unreachable, failed), fall back to Google but REPORT it
    // via fallback_used so the command can warn the user instead of silently
    // playing a different voice.
    let clone_request = clone_voice_name(voice);
    if let Some(clone_name) = clone_request {
        log::info!("get_or_generate_tts: generating CLONED TTS for voice {}", voice);
        let owner = std::env::var("VOICECLONE_SHARED_OWNER").unwrap_or_default();
        let bytes_result = get_tts_cloned(&clone_name, &owner, text).await;
        let (bytes, fallback_used) = match bytes_result {
            Ok(b) => (b, None),
            Err(e) => {
                log::warn!(
                    "get_or_generate_tts: clone '{}' unavailable ({}); falling back to Google TTS",
                    clone_name, e
                );
                (
                    get_tts_google(text).await.map_err(|ge| {
                        format!("clone '{clone_name}' failed ({e}) and Google TTS also failed ({ge})")
                    })?,
                    Some(format!(
                        "⚠️ Voce clonata '{}' non disponibile ({}), sto usando Google",
                        clone_name, e
                    )),
                )
            }
        };
        // On success keep the plain name for display AND token mapping
        // (get_voice_token now resolves plain clone names); on fallback the
        // audio is genuinely Google.
        let actual_voice = if fallback_used.is_some() {
            "Google".to_string()
        } else {
            clone_name
        };

        let temp_dir = std::env::var("TMP_DIR").unwrap_or_else(|_| "/tmp/discord-llm-bot".to_string());
        let hash = format!("{:x}", md5_compute(text));

        // Effects are applied locally on the cloned MP3, same as Google audio.
        if save_mp3 && !apply_effect && fallback_used.is_none() {
            // Cache the fish.audio MP3 in the shared audios dir — the API is
            // metered, so never re-synthesize text we already have on disk.
            let plain_path = get_file_path(&actual_voice, text);
            if !tokio::fs::try_exists(&plain_path).await.unwrap_or(false) {
                tokio::fs::write(&plain_path, &bytes).await?;
                // Same tags the sidecar wrote: artist "clone:<name>",
                // title/lyrics = text (so /random can recover the sentence).
                write_id3_tags(&plain_path, &format!("clone:{actual_voice}"), text, text);
            }
            return Ok(TtsResult { file_path: plain_path, actual_voice: voice.to_string(), fallback_used: None });
        }
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
        return Ok(TtsResult { file_path: temp_path, actual_voice, fallback_used });
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
                // …and cache the raw Google TTS bytes for future reuse (no MP3 round-trip).
                tokio::fs::write(&plain_path, &bytes).await?;
                // Title is the spoken text, artist is the voice — so the audio
                // shows e.g. "Google - <sentence>" instead of "Google - Google".
                write_id3_tags(&plain_path, &actual_voice, text, text);
                let path_clone = temp_path.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                    let _ = tokio::fs::remove_file(&path_clone).await;
                });
                return Ok(TtsResult { file_path: temp_path, actual_voice, fallback_used: None });
            } else {
                // No effect - write raw Google TTS bytes directly to cache
                tokio::fs::write(&plain_path, &bytes).await?;
                // The title is the spoken text, the artist is the voice.
                write_id3_tags(&plain_path, &actual_voice, text, text);
            }
        }
        // Plain audio now cached — return it (no effect path).
        return Ok(TtsResult { file_path: plain_path, actual_voice, fallback_used: None });
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
        // No effect requested - write the raw Google TTS bytes directly to avoid
        // unnecessary MP3 decode/encode round-trip which can cause audio corruption
        tokio::fs::write(&temp_path, &bytes).await?;
    }
    let path_clone = temp_path.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
        let _ = tokio::fs::remove_file(&path_clone).await;
    });
    Ok(TtsResult { file_path: temp_path, actual_voice, fallback_used: None })
}
