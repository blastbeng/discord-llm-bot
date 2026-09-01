// Voice cloning TTS service (voiceclone sidecar).
//
// A standalone HTTP service that performs zero-shot voice cloning on the CPU
// of the host (Raspberry Pi 5) using k2-fsa sherpa-onnx and the PocketTTS
// int8 zero-shot cloning model. The three bots (Discord, Telegram, WhatsApp)
// call this service over HTTP to create, list, delete and use cloned voices.
//
// Why a sidecar: sherpa-onnx's native static library and the int8 model files
// would each multiply the size of the three bot images. One service also means
// the model is loaded once for all bots, and concurrent clone generations are
// serialized here so the Pi's 4 CPUs are never oversubscribed.
//
// Endpoints (all JSON):
//   GET  /health                → liveness probe { status, ready }
//   GET  /voices                → { voices: [{name, owner, origin, created_at}] }
//   POST /voices                → create: {name, owner, audioBase64} (MP3 or WAV)
//   DELETE /voices/:name        → delete: ?owner=<user id>
//   POST /synthesize            → {voice, owner, text, speed} → {audioBase64, cached, path}
//   POST /transcribe            → {audioBase64} → {text} (whisper-tiny STT; optional)
//
// Synthesized audio is returned as base64 AND written to the shared audios
// directory using the same "{token}_{md5(text)}.mp3" naming the bots use for
// Google TTS, so bots can treat cloned voices exactly like any other voice.
// Files get ID3 tags (artist = "clone:<name>", title/lyrics = text) matching
// what the bots write for their own cache entries.
//
// Voice definitions (reference samples) are stored in the shared SQLite DB
// (voice_clones table) — the same file all three bots already share.

mod audio;

use axum::{
    extract::{DefaultBodyLimit, Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use base64::Engine as _;
use md5::compute as md5_compute;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

// ─── Shared state ──────────────────────────────────────────────────

struct AppState {
    db_pool: sqlx::SqlitePool,
    /// PocketTTS engine. Created lazily on first use (model load takes several
    /// seconds and is pointless if cloning is never used). Wrapped in a tokio
    /// Mutex so only one generation runs at a time — generation is CPU-bound
    /// and RTF is ~2.3 on a Pi 5, so a queue is the correct backpressure.
    engine: tokio::sync::Mutex<Option<sherpa_onnx::OfflineTts>>,
    /// STT engines (whisper-tiny int8) for the eavesdrop transcribe endpoint,
    /// one per language hint ("it", "en", "" = auto-detect). Created lazily
    /// and always serialized like the TTS engine — the Pi has 4 CPUs, so
    /// running clone generation and STT concurrently would just slow both.
    stt_engines: tokio::sync::Mutex<std::collections::HashMap<String, sherpa_onnx::OfflineRecognizer>>,
    /// Directory shared with the bots where cached MP3s live.
    audios_dir: String,
}

type SharedState = Arc<AppState>;

// ─── Requests / responses ──────────────────────────────────────────

#[derive(Deserialize)]
struct CreateVoiceReq {
    /// User-chosen voice name (1-64 chars, restricted charset).
    name: String,
    /// Owner identity (platform-prefixed; empty means global/shared voice).
    #[serde(default)]
    owner: String,
    /// Base64-encoded audio sample (MP3 or 16-bit PCM WAV).
    /// Accepts both snake_case and camelCase (bots send camelCase).
    #[serde(alias = "audioBase64")]
    audio_base64: String,
    /// Voice names are unique per owner: re-creating an existing name
    /// OVERWRITES the sample (no delete-first required). When overwriting,
    /// callers set this so cached MP3s for the stale voice are purged.
    #[serde(default, alias = "overwriteCached")]
    overwrite_cached: bool,
}

#[derive(Deserialize)]
struct SynthesizeReq {
    voice: String,
    #[serde(default)]
    owner: String,
    text: String,
    #[serde(default = "default_speed")]
    speed: f32,
}

fn default_speed() -> f32 {
    1.15
}

#[derive(Deserialize)]
struct DeleteQuery {
    #[serde(default)]
    owner: String,
}

#[derive(Deserialize)]
struct TranscribeReq {
    /// Base64-encoded audio (MP3 or 16-bit PCM WAV).
    #[serde(alias = "audioBase64")]
    audio_base64: String,
}

#[derive(Deserialize)]
struct SttQuery {
    /// Optional language hint override (e.g. "it", "en"); defaults to the
    /// VOICECLONE_STT_LANG env (or auto-detect when unset/"auto").
    #[serde(default)]
    lang: Option<String>,
}

#[derive(Serialize)]
struct VoiceInfo {
    name: String,
    owner: String,
    origin: String,
    created_at: Option<String>,
}

// ─── Voice name handling ───────────────────────────────────────────

fn valid_voice_name(name: &str) -> bool {
    let n = name.trim();
    if n.is_empty() || n.len() > 64 {
        return false;
    }
    // Restrict to characters that are safe in URLs, file paths, and the
    // slash/command parsers of all three bots.
    let ok = n
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok && n != "Google" && n != "random" && n != "none"
}

/// The token used in filenames for a cloned voice (same "Token_text.mp3"
/// convention as the Google voice token).
fn voice_token(name: &str) -> String {
    format!("clone|{}", name.trim())
}

// ─── DB helpers ────────────────────────────────────────────────────

async fn init_db(pool: &sqlx::SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS voice_clones (
            name TEXT NOT NULL,
            owner TEXT NOT NULL DEFAULT '',
            origin TEXT NOT NULL DEFAULT 'sample',
            sample BLOB NOT NULL,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (name, owner)
        )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

// ─── Handlers ──────────────────────────────────────────────────────

async fn health(State(state): State<SharedState>) -> Json<Value> {
    let ready = state.engine.try_lock().map(|g| g.is_some()).unwrap_or(false);
    Json(serde_json::json!({ "status": "ok", "ready": ready }))
}

async fn list_voices(
    State(state): State<SharedState>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT name, owner, origin, CAST(created_at AS TEXT) FROM voice_clones ORDER BY created_at",
    )
    .fetch_all(&state.db_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let voices: Vec<VoiceInfo> = rows
        .into_iter()
        .map(|(name, owner, origin, created_at)| VoiceInfo {
            name,
            owner,
            origin,
            created_at,
        })
        .collect();
    Ok(Json(serde_json::json!({ "voices": voices })))
}

async fn create_voice(
    State(state): State<SharedState>,
    Json(req): Json<CreateVoiceReq>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let name = req.name.trim().to_string();
    let owner = req.owner.trim().to_string();
    if !valid_voice_name(&name) {
        return Err((
            StatusCode::BAD_REQUEST,
            "invalid voice name (allowed: A-Z a-z 0-9 _ -, length 1-64, not Google/random)".into(),
        ));
    }
    // A hard limit keeps the Pi from drowning in 25MB BLOBs.
    let max_voices: i64 = std::env::var("VOICECLONE_MAX_VOICES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(24);
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM voice_clones")
        .fetch_one(&state.db_pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if total >= max_voices {
        return Err((
            StatusCode::CONFLICT,
            format!("voice limit reached ({max_voices}); delete one first"),
        ));
    }

    let audio = base64::engine::general_purpose::STANDARD
        .decode(&req.audio_base64)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid base64: {e}")))?;
    // 12MB decoded cap — generous for a 20-30s 128kbps MP3.
    if audio.len() < 4_000 || audio.len() > 12_000_000 {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "audio sample must be between 4KB and 12MB (got {} bytes)",
                audio.len()
            ),
        ));
    }

    // Validate the sample can be decoded and is long enough to characterize
    // a voice. PocketTTS produces degenerate output (endless rambling) from
    // very short references, so enforce a real duration, not a byte count.
    let (samples, rate) = audio::decode_audio(&audio).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            "could not decode audio (send MP3 or WAV, 10-30 seconds of speech)".to_string(),
        )
    })?;
    let duration_secs = samples.len() as f32 / rate.max(1) as f32;
    if duration_secs < 5.0 {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "audio sample too short ({duration_secs:.1}s) — need at least 5 seconds of speech, ideally 10-30s"
            ),
        ));
    }

    // Overwrite-if-exists: re-creating a voice with an existing name replaces
    // the previous sample (all previous cached MP3s stay valid — they are
    // content-hash keyed, and the same text with the new voice is regenerated
    // only if the caller passes overwrite_cached; the bots do when overwriting).
    let result = sqlx::query(
        "INSERT INTO voice_clones (name, owner, origin, sample) VALUES (?, ?, 'sample', ?)
         ON CONFLICT(name, owner) DO UPDATE SET sample = excluded.sample, created_at = CURRENT_TIMESTAMP",
    )
    .bind(&name)
    .bind(&owner)
    .bind(&audio)
    .execute(&state.db_pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // When overwriting an existing voice, stale cached MP3s for the old voice
    // would be served forever (they are keyed by text hash only). The caller
    // marks overwrites by sending `overwriteCached: true`; purge cached files
    // for this voice token.
    if req.overwrite_cached {
        let token = format!("clone|{}", name);
        let mut removed = 0u32;
        if let Ok(mut entries) = tokio::fs::read_dir(&state.audios_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let fname = entry.file_name().to_string_lossy().to_string();
                if fname.starts_with(&format!("{token}_")) {
                    let _ = tokio::fs::remove_file(entry.path()).await;
                    removed += 1;
                }
            }
        }
        if removed > 0 {
            log::info!("voiceclone: removed {removed} cached files for overwritten voice '{name}'");
        }
    }

    log::info!(
        "voiceclone: created voice '{}' for owner '{}' ({} rows affected)",
        name,
        owner,
        result.rows_affected()
    );
    Ok(Json(serde_json::json!({ "status": "created", "name": name })))
}

async fn delete_voice(
    State(state): State<SharedState>,
    Path(name): Path<String>,
    Query(q): Query<DeleteQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let name = name.trim().to_string();
    let owner = q.owner.trim().to_string();
    let result = if owner.is_empty() {
        // No owner given: delete the voice regardless of owner (bots resolve
        // the owner server-side when they want a scoped delete).
        sqlx::query("DELETE FROM voice_clones WHERE name = ?")
            .bind(&name)
            .execute(&state.db_pool)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    } else {
        sqlx::query("DELETE FROM voice_clones WHERE name = ? AND owner = ?")
            .bind(&name)
            .bind(&owner)
            .execute(&state.db_pool)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    };
    if result.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, format!("voice '{name}' not found")));
    }
    log::info!("voiceclone: deleted voice '{}'", name);
    Ok(Json(serde_json::json!({ "status": "deleted" })))
}

/// Generate speech with a cloned voice.
///
/// The audio is persisted to the shared audios dir with the standard
/// "{token}_{md5(text)}.mp3" naming (so bots can use their normal cache paths)
/// and returned as base64 so bots don't need filesystem access beyond the
/// shared volume they already have.
async fn synthesize(
    State(state): State<SharedState>,
    Json(req): Json<SynthesizeReq>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let text = req.text.trim().to_string();
    if text.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "text is required".into()));
    }
    if text.chars().count() > 600 {
        return Err((StatusCode::BAD_REQUEST, "text too long".into()));
    }
    let voice_name = req.voice.trim().to_string();
    if voice_name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "voice is required".into()));
    }

    // Engine created lazily (model load takes ~5s the first time).
    {
        let mut guard = state.engine.lock().await;
        if guard.is_none() {
            log::info!("voiceclone: loading PocketTTS model (first use)...");
            match audio::create_engine() {
                Some(e) => {
                    log::info!("voiceclone: PocketTTS model ready");
                    *guard = Some(e);
                }
                None => {
                    return Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "model not configured, check VOICECLONE_MODEL_DIR".into(),
                    ));
                }
            }
        }
    }
    let engine_guard = state.engine.lock().await;
    let engine = engine_guard.as_ref().unwrap();

    // Cache path mirrors the bots' naming convention (md5 of the text).
    let hash = format!("{:x}", md5_compute(&text));
    let path = format!("{}/{}_{}.mp3", state.audios_dir, voice_token(&voice_name), hash);
    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        return Ok(Json(
            serde_json::json!({ "cached": true, "path": path, "audioBase64": b64 }),
        ));
    }

    // Load the reference sample.
    let sample: Option<Vec<u8>> =
        sqlx::query_scalar(
            "SELECT sample FROM voice_clones WHERE name = ? AND owner = ?",
        )
        .bind(&voice_name)
        .bind(req.owner.trim())
        .fetch_optional(&state.db_pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let sample = match sample {
        Some(s) => s,
        None => {
            // Owner-scoped miss → try any owner (lets /speak --voice X work
            // for voices shared by admins unless a same-name private one wins).
            sqlx::query_scalar(
                "SELECT sample FROM voice_clones WHERE name = ? ORDER BY created_at LIMIT 1",
            )
            .bind(&voice_name)
            .fetch_optional(&state.db_pool)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
                .ok_or((StatusCode::NOT_FOUND, format!("voice '{voice_name}' not found")))?
        }
    };

    // Decode the reference and synthesize. Engine generation is fully
    // synchronous (onnxruntime) — block_in_place keeps the tokio worker free
    // for other tasks while the Mutex guarantees one generation at a time.
    let (samples_in, ref_rate) = audio::decode_audio(&sample).ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "stored sample is corrupt; delete and re-create the voice".to_string(),
        )
    })?;
    // PocketTTS expects 24kHz reference audio; uploaded samples keep their
    // original rate (usually 44.1/48kHz) and must be resampled, otherwise
    // the model hears pitched-down garbage and the output degenerates.
    let samples_in = audio::resample_linear(&samples_in, ref_rate, 24000);
    let samples = tokio::task::block_in_place(|| {
        audio::generate(engine, text.clone(), samples_in, req.speed)
    })
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mp3 = audio::encode_mono_mp3(&samples, 24000)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("mp3 encode: {e}")))?;

    // Persist to the shared cache dir with ID3 tags (artist = clone name like
    // the bots set artist = voice, title/lyrics = text).
    tokio::fs::create_dir_all(&state.audios_dir)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("audios dir: {e}")))?;
    {
        // tag writing is synchronous file IO; tiny, so just block briefly.
        let mp3 = mp3.clone();
        let path = path.clone();
        let voice_display = voice_name.clone();
        let text_tag = text.clone();
        tokio::task::block_in_place(|| {
            write_id3_tags_bytes(&path, &mp3, &voice_display, &text_tag)
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("id3: {e}")))?;
    }
    log::info!(
        "voiceclone: synthesized {} chars -> {} ({} bytes)",
        text.chars().count(),
        path,
        mp3.len()
    );

    let b64 = base64::engine::general_purpose::STANDARD.encode(&mp3);
    Ok(Json(
        serde_json::json!({ "cached": false, "path": path, "audioBase64": b64 }),
    ))
}

/// Transcribe recorded speech to text (used by the Discord eavesdrop feature
/// to learn what a user actually said before roasting them).
///
/// The whisper-tiny int8 model runs at RTF ≈ 0.26 on the Pi 5 (a 12s clip
/// transcribes in ~3s), so this is fast enough to run inline in the
/// eavesdrop flow. One engine per language hint is cached in the shared
/// state (whisper bakes the language into its decoder, so switching
/// languages means creating a new engine around the same ONNX files — cheap,
/// the model bytes are shared by the runtime).
async fn transcribe(
    State(state): State<SharedState>,
    Query(q): Query<SttQuery>,
    Json(req): Json<TranscribeReq>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let audio = base64::engine::general_purpose::STANDARD
        .decode(&req.audio_base64)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid base64: {e}")))?;
    if audio.len() > 12_000_000 {
        return Err((StatusCode::BAD_REQUEST, "audio too large".into()));
    }
    let samples = audio::decode_to_mono(&audio).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            "could not decode audio (send MP3 or WAV)".to_string(),
        )
    })?;
    // Cap input at 60s — eavesdrop clips are ~12s, anything longer is a bug.
    let rate = decode_sample_rate(&audio).unwrap_or(24000);
    if samples.len() as u64 > rate as u64 * 60 {
        return Err((StatusCode::BAD_REQUEST, "audio longer than 60s".into()));
    }

    // Resolve the language hint: query param wins, then the env default.
    let lang = q
        .lang
        .as_deref()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto"))
        .or_else(|| {
            std::env::var("VOICECLONE_STT_LANG")
                .ok()
                .map(|v| v.trim().to_lowercase())
                .filter(|v| !v.is_empty() && !v.eq_ignore_ascii_case("auto"))
        })
        .unwrap_or_default();

    // Engine created lazily per language (first call loads the model, ~2-3s).
    {
        let mut engines = state.stt_engines.lock().await;
        if !engines.contains_key(&lang) {
            log::info!("voiceclone: loading whisper-tiny STT model (lang='{lang}', first use)...");
            match audio::create_stt_engine(&lang) {
                Some(e) => {
                    log::info!("voiceclone: STT model ready (lang='{lang}')");
                    engines.insert(lang.clone(), e);
                }
                None => {
                    return Err((
                        StatusCode::NOT_IMPLEMENTED,
                        "STT not configured, set VOICECLONE_STT_MODEL_DIR".into(),
                    ));
                }
            }
        }
    }
    let engines = state.stt_engines.lock().await;
    let engine = engines.get(&lang).unwrap();

    let text = tokio::task::block_in_place(|| audio::transcribe(engine, &samples, rate));

    match text {
        Some(t) => {
            log::info!("voiceclone: transcribed {} samples → {:?} ({} chars)",
                samples.len(), t.chars().take(80).collect::<String>(), t.len());
            Ok(Json(serde_json::json!({ "text": t })))
        }
        None => Ok(Json(serde_json::json!({ "text": "" }))),
    }
}

/// Sample rate of the decoded audio, used only for the 60s duration cap.
/// Cheap header peek: reuses decode via a rate probe on the raw bytes.
fn decode_sample_rate(bytes: &[u8]) -> Option<u32> {
    // 16-bit PCM WAV: rate is at offset 24 in a canonical header.
    if bytes.len() > 28 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        return Some(u32::from_le_bytes(bytes[24..28].try_into().ok()?));
    }
    // MP3: the bots always send 24kHz mono, so that is the sane default.
    Some(24000)
}

/// Write the MP3 file with ID3 tags in one pass (temp + rename for safety).
fn write_id3_tags_bytes(path: &str, mp3: &[u8], artist: &str, title: &str) -> Result<(), String> {
    use id3::TagLike;
    let mut tag = id3::Tag::new();
    tag.set_artist(artist);
    tag.set_title(title);
    tag.add_frame(id3::frame::Lyrics {
        lang: "und".to_string(),
        description: String::new(),
        text: title.to_string(),
    });
    // Prefix the raw MP3 with a freshly-encoded ID3v2 tag (same approach the
    // bots use via write_to_path, but on in-memory bytes so we can temp+rename).
    let mut tagged = Vec::with_capacity(mp3.len() + 1024);
    {
        let mut cursor = std::io::Cursor::new(&mut tagged);
        tag.write_to(&mut cursor, id3::Version::Id3v24)
            .map_err(|e| e.to_string())?;
    }
    tagged.extend_from_slice(mp3);
    let tmp = format!("{path}.part");
    std::fs::write(&tmp, &tagged).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

// ─── Main ──────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();
    let mut builder =
        env_logger::Builder::from_env(env_logger::Env::default().filter_or("LOG_LEVEL", "info"));
    builder.init();

    let db_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "sqlite:config/discord-bot.sqlite3".to_string());
    std::fs::create_dir_all("config").expect("config dir");
    let db_path = db_url.strip_prefix("sqlite:").unwrap_or(&db_url);
    if !std::path::Path::new(db_path).exists() {
        std::fs::File::create(db_path).expect("create db file");
    }
    let db_pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(3)
        .connect_with(
            db_url
                .parse::<sqlx::sqlite::SqliteConnectOptions>()
                .expect("bad DATABASE_URL")
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .busy_timeout(std::time::Duration::from_secs(5))
                .create_if_missing(true),
        )
        .await
        .expect("db connect");
    init_db(&db_pool).await.expect("init db");

    let audios_dir = std::env::var("AUDIOS_DIR").unwrap_or_else(|_| "audios".to_string());
    std::fs::create_dir_all(&audios_dir).expect("audios dir");

    // Optionally preload the model at startup (default: lazy load on first use).
    let engine = if std::env::var("VOICECLONE_PRELOAD")
        .unwrap_or_default()
        .to_lowercase()
        == "true"
    {
        log::info!("voiceclone: preloading PocketTTS model...");
        let e = audio::create_engine();
        if e.is_some() {
            log::info!("voiceclone: model preloaded");
        } else {
            log::error!("voiceclone: model preload FAILED (will retry on first request)");
        }
        e
    } else {
        None
    };

    let state = Arc::new(AppState {
        db_pool,
        engine: tokio::sync::Mutex::new(engine),
        stt_engines: tokio::sync::Mutex::new(Default::default()),
        audios_dir,
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/voices", get(list_voices).post(create_voice))
        .route("/voices/:name", axum::routing::delete(delete_voice))
        .route("/synthesize", post(synthesize))
        .route("/transcribe", post(transcribe))
        // axum defaults to 2MB, which silently rejects ~10s+ WAV samples
        // (base64-inflated JSON) that the create API otherwise allows.
        .layer(DefaultBodyLimit::max(24 * 1024 * 1024))
        .with_state(state);

    let port: u16 = std::env::var("VOICECLONE_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3010);
    let addr = format!("0.0.0.0:{port}");
    log::info!("voiceclone listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
    axum::serve(listener, app).await.expect("serve");
}