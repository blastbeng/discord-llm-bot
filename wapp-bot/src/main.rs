// WhatsApp bot — receives messages from the Baileys bridge via HTTP webhook,
// processes commands, and sends responses back via the bridge HTTP API.
// Shares the same SQLite database and TTS cache with the Discord bot.

mod audio_effects;
mod database;
mod llm;
mod tts;
mod lang;

use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use base64::Engine as _;
use rand::seq::SliceRandom;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;

/// Shared base64 engine for sample upload/download.
static BASE64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;
use std::env;

struct AppState {
    db_pool: sqlx::SqlitePool,
    lang: lang::Lang,
    bridge_url: String,
    /// Per-group conversation history for /ask.
    conversations: std::sync::Mutex<std::collections::HashMap<String, Vec<llm::ConversationMessage>>>,
    /// Whether WhatsApp processing is enabled (WAPP_ENABLED=true).
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WebhookPayload {
    from: String,
    is_group: bool,
    #[allow(dead_code)] // sender is sent by the bridge but not currently used
    sender: String,
    #[allow(dead_code)]
    message_id: String,
    text: String,
    /// Present when the command message QUOTES a voice note / audio document
    /// (used by /createvoice to fetch the sample to clone from).
    #[serde(default)]
    quoted_media: Option<QuotedMedia>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuotedMedia {
    #[allow(dead_code)]
    #[serde(default)]
    r#type: String,
    /// The full quoted (replied-to) message — forwarded back to the bridge's
    /// /fetchMedia endpoint to download the audio bytes.
    message: serde_json::Value,
}

#[tokio::main]
async fn main() {
    eprintln!("=== wapp-bot starting ===");
    dotenv::dotenv().ok();

    // Whether WhatsApp processing is enabled. When WAPP_ENABLED is not "true",
    // the service still starts and keeps its webhook server up (so the container
    // stays healthy and does NOT enter a restart loop), but all incoming messages
    // are ignored. This keeps the service in docker-compose without it actively
    // running WhatsApp, and without affecting the shared Discord bot.
    let enabled = std::env::var("WAPP_ENABLED").unwrap_or_else(|_| "false".to_string()).to_lowercase() == "true";

    let mut builder = env_logger::Builder::from_env(env_logger::Env::default().filter_or("LOG_LEVEL", "info"));
    builder.init();

    let bridge_url = env::var("WAPP_BRIDGE_URL").unwrap_or_else(|_| "http://whatsapp-bridge:3001".to_string());
    let webhook_port: u16 = env::var("WAPP_WEBHOOK_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3002);

    // Initialize database (shared with Discord bot)
    let db_url = env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:config/discord-bot.sqlite3".to_string());
    tokio::fs::create_dir_all("config").await.expect("Failed to create config directory");
    let db_path = db_url.strip_prefix("sqlite:").unwrap_or(&db_url);
    if !tokio::fs::try_exists(db_path).await.unwrap_or(false) {
        tokio::fs::File::create(db_path).await.expect("Failed to create database file");
    }

    // Enable WAL mode + busy timeout so both bots can safely share this
    // SQLite file without "database is locked" errors on concurrent writes.
    let db_pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(
            db_url
                .parse::<sqlx::sqlite::SqliteConnectOptions>()
                .expect("Invalid DATABASE_URL for SQLite")
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .busy_timeout(std::time::Duration::from_secs(5))
                .create_if_missing(true),
        )
        .await
        .expect("Database connection failed");
    eprintln!("Connecting to database at: {}", db_url);
    database::init_db(&db_pool).await.expect("Database initialization failed");
    log::info!("✓ Database initialized");

    if enabled {
        database::populate_db_if_empty(&db_pool).await.expect("Database population failed");
        log::info!("✓ Database population check completed");
    }

    let state = Arc::new(AppState {
        db_pool,
        lang: lang::Lang::new(),
        bridge_url: bridge_url.clone(),
        conversations: std::sync::Mutex::new(std::collections::HashMap::new()),
        enabled,
    });

    if !enabled {
        log::info!("wapp-bot: WAPP_ENABLED is not 'true' — running in disabled (idle) mode. The webhook server stays up but no messages are processed. Set WAPP_ENABLED=true in .env.wapp to enable.");
    }

    let app = Router::new()
        .route("/webhook", post(handle_webhook))
        .with_state(state);

    let addr = format!("0.0.0.0:{}", webhook_port);
    log::info!("wapp-bot listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn handle_webhook(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<WebhookPayload>,
) -> (StatusCode, Json<Value>) {
    // When WhatsApp is disabled (WAPP_ENABLED != "true"), the server stays up
    // to keep the container healthy, but incoming messages are ignored.
    if !state.enabled {
        return (StatusCode::OK, Json(serde_json::json!({"status": "disabled"})));
    }

    log::info!("wapp-bot: received message from {} in group={}: {:?}", payload.from, payload.is_group, payload.text);

    // Parse the command
    let text = payload.text.trim();
    let (command, args) = match text.split_once(' ') {
        Some((cmd, rest)) => (cmd.to_lowercase(), rest.trim().to_string()),
        None => (text.to_lowercase(), String::new()),
    };

    let response = match command.as_str() {
        // /speak and /random send audio directly via the bridge — no text reply needed
        "/speak" | "/s" => {
            cmd_speak(&state, &payload, &args).await;
            String::new()
        }
        "/random" | "/r" => {
            cmd_random(&state, &payload, &args).await;
            String::new()
        }
        "/ask" | "/a" => cmd_ask(&state, &payload, &args).await,
        "/translate" | "/t" => cmd_translate(&state, &payload, &args).await,
        "/joke" | "/j" => cmd_joke(&state, &payload).await,
        "/createvoice" => {
            cmd_createvoice(&state, &payload, &args).await;
            String::new()
        }
        "/myvoices" => {
            cmd_myvoices(&state, &payload).await;
            String::new()
        }
        "/deletevoice" => {
            cmd_deletevoice(&state, &payload, &args).await;
            String::new()
        }
        "/stats" => cmd_stats(&state, &payload).await,
        "/help" | "/h" => cmd_help(&state, &payload).await,
        _ => return (StatusCode::OK, Json(serde_json::json!({"status": "ignored"}))),
    };

    (StatusCode::OK, Json(serde_json::json!({"status": "processed", "response": response})))
}

// ─── Helper: send text via bridge ──────────────────────────────────

async fn send_text(state: &AppState, chat_id: &str, text: &str) {
    let url = format!("{}/sendText", state.bridge_url);
    let client = tts::http_client();
    match client.post(&url).json(&serde_json::json!({"chatId": chat_id, "text": text})).send().await {
        Ok(r) => {
            if !r.status().is_success() {
                log::error!("wapp-bot: sendText failed with status {}", r.status());
            }
        }
        Err(e) => log::error!("wapp-bot: sendText error: {}", e),
    }
}

// ─── Helper: send audio via bridge ─────────────────────────────────

async fn send_audio(state: &AppState, chat_id: &str, file_path: &str) {
    // Read the audio file and send it as base64-encoded bytes to the bridge.
    // The bridge and the bot run in separate containers, so a file path
    // would not be accessible across them — sending the bytes avoids
    // sharing volumes and works regardless of where the file is stored.
    let bytes = match tokio::fs::read(file_path).await {
        Ok(b) => b,
        Err(e) => {
            log::error!("wapp-bot: failed to read audio file {}: {}", file_path, e);
            return;
        }
    };
    let audio_base64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);

    let url = format!("{}/sendAudio", state.bridge_url);
    let client = tts::http_client();
    match client.post(&url).json(&serde_json::json!({"chatId": chat_id, "audioBase64": audio_base64})).send().await {
        Ok(r) => {
            if !r.status().is_success() {
                log::error!("wapp-bot: sendAudio failed with status {}", r.status());
            }
        }
        Err(e) => log::error!("wapp-bot: sendAudio error: {}", e),
    }
}

// ─── Helper: parse voice and effect from args ──────────────────────

/// Pick a random cached MP3 file from the audios/ directory, if any exist.
/// Returns the file path, or None if there are no cached MP3s (or the
/// directory can't be read). Used by /random to replay an already-generated
/// TTS file instead of calling the TTS API again.
async fn pick_cached_mp3() -> Option<String> {
    let mut entries = tokio::fs::read_dir("audios").await.ok()?;
    let mut mp3_files = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "mp3") {
            if let Some(s) = path.to_str() {
                // Exclude cloned-voice files so /random never plays a clone.
                // Clone cache files are "clone|Name_hash.mp3" (legacy sidecar-written)
                        // or "clone_Name_hash.mp3" (bot-written with plain
                        // names); exclude both forms.
                        if !s.contains("clone|") && !s.contains("clone_") {
                    mp3_files.push(s.to_string());
                }
            }
        }
    }
    if mp3_files.is_empty() {
        return None;
    }
    let mut rng = rand::thread_rng();
    mp3_files.choose(&mut rng).map(|s| s.clone())
}

fn parse_voice_effect(args: &str) -> (String, String, String) {
    // Format: "text" or "text --voice Google" or "text --voice Google --effect demon".
    // Flags may appear anywhere; each flag consumes its following token as its
    // value. A flag with no value (e.g. "hello --voice") is simply dropped so
    // the flag token is never spoken as part of the text, and the default
    // voice/effect is kept.
    let mut voice = "Google".to_string();
    let mut effect = "none".to_string();
    let parts: Vec<&str> = args.split_whitespace().collect();
    let mut text_parts: Vec<String> = Vec::new();

    let mut i = 0;
    while i < parts.len() {
        match parts[i] {
            "--voice" => {
                if i + 1 < parts.len() && !parts[i + 1].starts_with("--") {
                    voice = parts[i + 1].to_string();
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--effect" => {
                if i + 1 < parts.len() && !parts[i + 1].starts_with("--") {
                    effect = parts[i + 1].to_string();
                    i += 2;
                } else {
                    i += 1;
                }
            }
            token => {
                text_parts.push(token.to_string());
                i += 1;
            }
        }
    }

    (text_parts.join(" "), voice, effect)
}

// ─── Commands ──────────────────────────────────────────────────────

async fn cmd_speak(state: &AppState, payload: &WebhookPayload, args: &str) {
    let (text, voice, effect) = parse_voice_effect(args);

    if text.is_empty() {
        send_text(state, &payload.from, &state.lang.speak_usage).await;
        return;
    }

    if text.chars().count() > 200 {
        send_text(state, &payload.from, &state.lang.text_too_long).await;
        return;
    }

    let actual_voice = if voice == "random" {
        let mut rng = rand::thread_rng();
        tts::AVAILABLE_VOICES.choose(&mut rng).unwrap().to_string()
    } else {
        voice
    };

    if !tts::is_valid_voice(&actual_voice) {
        send_text(state, &payload.from, &state.lang.invalid_voice).await;
        return;
    }

    let actual_effect = if effect == "random" {
        crate::audio_effects::random_effect().to_string()
    } else {
        effect
    };

    if !crate::audio_effects::is_valid_effect(&actual_effect) {
        send_text(state, &payload.from, &state.lang.invalid_effect).await;
        return;
    }

    // Generate TTS
    match tts::get_or_generate_tts_with_effect(&text, &actual_voice, &actual_effect).await {
        Ok(tts_result) => {
            // Surface a Google-fallback (cloned voice unavailable) to the user.
            if let Some(warn) = &tts_result.fallback_used {
                send_text(state, &payload.from, warn).await;
            }
            // Save sentence to database
            if let Err(e) = database::insert_sentence(&state.db_pool, &text).await {
                log::error!("Failed to insert sentence: {}", e);
            }

            // Send audio via bridge
            send_audio(state, &payload.from, &tts_result.file_path).await;
        }
        Err(e) => {
            log::error!("TTS generation failed: {}", e);
            send_text(state, &payload.from, &state.lang.error_generating_audio).await;
        }
    }
}

async fn cmd_random(state: &AppState, payload: &WebhookPayload, args: &str) {
    let (search_text, voice, mut effect) = parse_voice_effect(args);

    if effect == "none" && !args.contains("--effect") {
        effect = "random".to_string();
    }

    // When no voice, no effect to apply (explicitly "none"), no search text,
    // and disk caching is enabled, pick a random already-cached MP3 from
    // audios/ and send it directly — much faster and avoids unnecessary TTS
    // API calls (mirrors the Discord bot's /random). A cached file has no
    // effect filter applied, so if an effect will be applied we must fall
    // through to real TTS generation.
    let voice_explicitly_set = args.contains("--voice");
    let save_mp3 = std::env::var("SAVE_MP3_ON_DISK")
        .unwrap_or_else(|_| "false".to_string())
        .to_lowercase() == "true";

    if !voice_explicitly_set && effect == "none" && search_text.is_empty() && save_mp3 {
        if let Some(chosen) = pick_cached_mp3().await {
            log::info!("wapp-bot random: picked cached MP3: {}", chosen);
            send_audio(state, &payload.from, &chosen).await;
            return;
        }
    }

    // Fetch sentences from database
    let sentences = if !search_text.is_empty() {
        match database::select_like_sentence(&state.db_pool, &search_text).await {
            Ok(s) => s,
            Err(e) => {
                log::error!("Database error: {}", e);
                send_text(state, &payload.from, &state.lang.database_error).await;
                return;
            }
        }
    } else {
        match database::select_all_sentence(&state.db_pool).await {
            Ok(s) => s,
            Err(e) => {
                log::error!("Database error: {}", e);
                send_text(state, &payload.from, &state.lang.database_error).await;
                return;
            }
        }
    };

    if sentences.is_empty() {
        if search_text.is_empty() {
            send_text(state, &payload.from, &state.lang.no_sentences_found).await;
        } else {
            send_text(state, &payload.from, &state.lang.no_sentence_with_text.replacen("{}", &search_text, 1)).await;
        }
        return;
    }

    let random_sentence = {
        let mut rng = rand::thread_rng();
        sentences.choose(&mut rng).unwrap().to_string()
    };

    // Record that this sentence was spoken (increments its usage_count). This
    // keeps the least-used-first ordering meaningful so the background
    // generator and /random don't keep landing on the same sentences.
    if let Err(e) = database::insert_sentence(&state.db_pool, &random_sentence).await {
        log::error!("wapp-bot random: failed to record sentence usage: {}", e);
    }

    // Google TTS silently fails or truncates on text longer than ~200 chars.
    // Database sentences are normally bounded, but /ask and /translate
    // responses can exceed this. Truncate only the spoken text.
    let tts_text: String = if random_sentence.chars().count() > 200 {
        let truncated: String = random_sentence.chars().take(200).collect();
        format!("{truncated}...")
    } else {
        random_sentence.clone()
    };

    let actual_voice = if voice == "random" {
        let mut rng = rand::thread_rng();
        tts::AVAILABLE_VOICES.choose(&mut rng).unwrap().to_string()
    } else {
        voice
    };

    if !tts::is_valid_voice(&actual_voice) {
        send_text(state, &payload.from, &state.lang.invalid_voice).await;
        return;
    }

    let actual_effect = if effect == "random" {
        crate::audio_effects::random_effect().to_string()
    } else {
        effect
    };

    if !crate::audio_effects::is_valid_effect(&actual_effect) {
        send_text(state, &payload.from, &state.lang.invalid_effect).await;
        return;
    }

    match tts::get_or_generate_tts_with_effect(&tts_text, &actual_voice, &actual_effect).await {
        Ok(tts_result) => {
            // Surface a Google-fallback (cloned voice unavailable) to the user.
            if let Some(warn) = &tts_result.fallback_used {
                send_text(state, &payload.from, warn).await;
            }
            send_audio(state, &payload.from, &tts_result.file_path).await;
        }
        Err(e) => {
            log::error!("TTS generation failed: {}", e);
            send_text(state, &payload.from, &state.lang.error_generating_audio).await;
        }
    }
}

async fn cmd_ask(state: &AppState, payload: &WebhookPayload, args: &str) -> String {
    if !llm::is_configured() {
        return state.lang.ask_not_configured.clone();
    }

    let (text, _voice, _effect) = parse_voice_effect(args);

    if text.is_empty() {
        return state.lang.ask_usage.clone();
    }

    if text.chars().count() > 500 {
        return state.lang.ask_text_too_long.clone();
    }

    // Fetch database sentences for personality context
    let db_sentences = database::select_all_sentence(&state.db_pool).await.unwrap_or_default();

    // Fetch conversation history for this group
    let history = {
        let conversations = state.conversations.lock().unwrap();
        conversations.get(&payload.from).cloned().unwrap_or_default()
    };

    // Query the LLM
    match llm::ask(&text, &db_sentences, "WhatsApp Bot", &history).await {
        Ok(response) if llm::is_refusal_error(&response) => {
            // The LLM refused — never answer with the refusal boilerplate and
            // never persist it (it would poison the shared sentence database
            // and resurface via other bots' TTS).
            log::warn!("wapp-bot: LLM refused the request, not answering with it");
            state.lang.ai_refused.clone()
        }
        Ok(response) => {
            log::info!("wapp-bot: LLM response: {:?}", response);

            // Save response to database
            if let Err(e) = database::insert_sentence(&state.db_pool, &response).await {
                log::error!("Failed to insert LLM response: {}", e);
            }

            // Store in conversation history
            {
                let mut conversations = state.conversations.lock().unwrap();
                let group_history = conversations.entry(payload.from.clone()).or_insert_with(Vec::new);
                group_history.push(llm::ConversationMessage { role: "user".to_string(), content: text.clone() });
                group_history.push(llm::ConversationMessage { role: "assistant".to_string(), content: response.clone() });
                if group_history.len() > 20 {
                    let start = group_history.len() - 20;
                    group_history.drain(0..start);
                }
            }

            response
        }
        Err(e) => {
            log::error!("LLM failed: {}", e);
            state.lang.ai_unavailable.clone()
        }
    }
}

async fn cmd_translate(state: &AppState, _payload: &WebhookPayload, args: &str) -> String {
    if !llm::is_configured() {
        return state.lang.ask_not_configured.clone();
    }

    // Format: /translate <text> <target_lang>
    // We need to split the last word as the target language
    let parts: Vec<&str> = args.rsplitn(2, ' ').collect();
    if parts.len() < 2 {
        return state.lang.translate_usage.clone();
    }
    let target_lang = parts[0].trim().to_string();
    let text = parts[1].trim().to_string();

    // Guard against a trailing space producing an empty language (e.g. a
    // message like "/translate hello " with no language) so we don't pass an
    // empty language to the LLM.
    if text.is_empty() || target_lang.is_empty() {
        return state.lang.translate_usage.clone();
    }

    match llm::translate(&text, &target_lang).await {
        Ok(response) => {
            if let Err(e) = database::insert_sentence(&state.db_pool, &response).await {
                log::error!("Failed to insert translation: {}", e);
            }
            response
        }
        Err(e) => {
            log::error!("Translation failed: {}", e);
            state.lang.translation_failed.clone()
        }
    }
}

async fn cmd_joke(state: &AppState, _payload: &WebhookPayload) -> String {
    // JokeAPI has no Italian jokes, so fetch English ones and translate to the
    // configured language via the LLM when needed. This keeps the joke in the
    // language defined by LANG (see also the Discord/Telegram bots).
    let joke_url = "https://v2.jokeapi.dev/joke/Any?lang=en&safe-mode&type=twopart&format=json";
    // Bounded timeout so a slow JokeAPI doesn't stall the response indefinitely.
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            log::error!("wapp-bot joke: failed to build client: {}", e);
            return state.lang.joke_error.clone();
        }
    };

    let joke_text = match client.get(joke_url).send().await {
        Ok(resp) => {
            if !resp.status().is_success() {
                return state.lang.joke_error.clone();
            }
            match resp.json::<serde_json::Value>().await {
                Ok(json) => {
                    if json.get("error").is_some_and(|e| e.as_bool().unwrap_or(false)) {
                        return state.lang.joke_error.clone();
                    }
                    let setup = json.get("setup").and_then(|s| s.as_str()).unwrap_or("");
                    let delivery = json.get("delivery").and_then(|d| d.as_str()).unwrap_or("");
                    let single = json.get("joke").and_then(|j| j.as_str()).unwrap_or("");
                    if !setup.is_empty() && !delivery.is_empty() {
                        format!("{}. {}", setup, delivery)
                    } else if !single.is_empty() {
                        single.to_string()
                    } else {
                        return state.lang.joke_error.clone();
                    }
                }
                Err(_) => return state.lang.joke_error.clone(),
            }
        }
        Err(e) => {
            log::error!("JokeAPI request failed: {}", e);
            return state.lang.joke_error.clone();
        }
    };

    // Translate the joke to the configured language when it isn't English and
    // an LLM is available (JokeAPI only serves English + a few non-Italian
    // languages). Without an LLM we fall back to the English joke as-is.
    let joke_text = translate_joke_to_lang(&joke_text).await;

    // Save joke to database
    if let Err(e) = database::insert_sentence(&state.db_pool, &joke_text).await {
        log::error!("Failed to insert joke: {}", e);
    }

    joke_text
}

/// Translate a joke to the bot's configured language via the LLM, if needed.
/// JokeAPI only serves a fixed set of languages (no Italian), so for any
/// configured language other than English we ask the LLM to translate. When no
/// LLM is configured, the original (English) joke is returned unchanged.
async fn translate_joke_to_lang(joke: &str) -> String {
    let lang = std::env::var("LANG").unwrap_or_else(|_| "ita".to_string());
    let target = match lang.as_str() {
        "eng" => return joke.to_string(), // English — JokeAPI already returned it
        _ => "it",                        // e.g. ita → translate to Italian
    };
    if !llm::is_configured() {
        return joke.to_string();
    }
    match llm::translate(joke, target).await {
        Ok(translated) => translated,
        Err(e) => {
            log::error!("wapp-bot joke: failed to translate joke: {}", e);
            joke.to_string()
        }
    }
}

/// Owner identity for voice cloning, namespaced per WhatsApp chat.
fn vc_owner(payload: &WebhookPayload) -> String {
    format!("whatsapp:{}", payload.from)
}

/// Download the audio bytes of the quoted media message via the bridge.
async fn fetch_quoted_media(state: &AppState, payload: &WebhookPayload) -> Option<Result<Vec<u8>, String>> {
    let media = payload.quoted_media.as_ref()?;
    let url = format!("{}/fetchMedia", state.bridge_url);
    let resp = tts::http_client()
        .post(&url)
        .json(&serde_json::json!({ "message": media.message }))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return Some(Err(format!("bridge returned {}", resp.status())));
    }
    let body: Value = resp.json().await.ok()?;
    let b64 = body.get("base64").and_then(|b| b.as_str())?;
    match BASE64.decode(b64) {
        Ok(bytes) => Some(Ok(bytes)),
        Err(e) => Some(Err(format!("base64 decode: {e}"))),
    }
}

/// /createvoice <name> — quotes a voice note / audio document to clone from.
async fn cmd_createvoice(state: &AppState, payload: &WebhookPayload, args: &str) {
    let name = args.trim().to_string();
    let lang = &state.lang;
    if !tts::voiceclone_configured() {
        send_text(state, &payload.from, &lang.vc_not_configured).await;
        return;
    }
    if name.is_empty() {
        send_text(state, &payload.from, &lang.vc_usage).await;
        return;
    }
    if !tts::is_valid_clone_name(&name) {
        send_text(state, &payload.from, &lang.vc_invalid_name).await;
        return;
    }
    let sample = match fetch_quoted_media(state, payload).await {
        None => {
            send_text(state, &payload.from, &lang.vc_sample_invalid).await;
            return;
        }
        Some(Err(e)) => {
            log::error!("wapp-bot createvoice: fetch media failed: {}", e);
            send_text(state, &payload.from, &lang.vc_sample_invalid).await;
            return;
        }
        Some(Ok(bytes)) => bytes,
    };
    use base64::Engine as _;
    let audio_b64 = BASE64.encode(&sample);
    match tts::create_cloned_voice(&name, &vc_owner(payload), &audio_b64).await {
        Ok(()) => {
            let m = lang.vc_created.replacen("{}", &name, 1).replacen("{}", &name, 1);
            send_text(state, &payload.from, &m).await;
        }
        Err(e) => {
            let m = if e.contains("already exists") {
                lang.vc_exists.replacen("{}", &name, 1)
            } else if e.contains("could not decode") || e.contains("too short") || e.contains("between 4KB") {
                lang.vc_sample_invalid.clone()
            } else if e.contains("invalid voice name") {
                lang.vc_invalid_name.clone()
            } else {
                lang.vc_error.replacen("{}", &e, 1)
            };
            send_text(state, &payload.from, &m).await;
        }
    }
}

/// /myvoices — list all cloned voices.
async fn cmd_myvoices(state: &AppState, payload: &WebhookPayload) {
    let lang = &state.lang;
    if !tts::voiceclone_configured() {
        send_text(state, &payload.from, &lang.vc_not_configured).await;
        return;
    }
    let voices = tts::list_cloned_voices().await.unwrap_or_default();
    if voices.is_empty() {
        send_text(state, &payload.from, &lang.vc_list_empty).await;
        return;
    }
    let lines: Vec<String> = voices
        .iter()
        .map(|v| format!("• *{}* — `/speak testo --voice {}`", v.name, v.name))
        .collect();
    send_text(state, &payload.from, &lines.join("\n")).await;
}

/// /deletevoice <name> — delete a cloned voice.
async fn cmd_deletevoice(state: &AppState, payload: &WebhookPayload, args: &str) {
    let name = args.trim().to_string();
    let lang = &state.lang;
    if !tts::voiceclone_configured() {
        send_text(state, &payload.from, &lang.vc_not_configured).await;
        return;
    }
    if name.is_empty() {
        send_text(state, &payload.from, &lang.vc_delete_usage).await;
        return;
    }
    let voices = tts::list_cloned_voices().await.unwrap_or_default();
    match voices.iter().find(|v| v.name == name) {
        None => {
            { let m = lang.vc_not_found.replacen("{}", &name, 1); send_text(state, &payload.from, &m).await; }
        }
        Some(v) => {
            match tts::delete_cloned_voice(&name, &v.owner).await {
                Ok(()) => { let m = lang.vc_deleted.replacen("{}", &name, 1); send_text(state, &payload.from, &m).await; }
                Err(e) => { let m = lang.vc_error.replacen("{}", &e, 1); send_text(state, &payload.from, &m).await; }
            }
        }
    }
}

async fn cmd_stats(state: &AppState, _payload: &WebhookPayload) -> String {
    let db_stats = database::get_db_statistics(&state.db_pool).await.unwrap_or_else(|e| format!("Error: {}", e));

    let cache_info = match tokio::fs::read_dir("audios").await {
        Ok(mut entries) => {
            let mut count = 0u64;
            let mut size_bytes: u64 = 0;
            while let Ok(Some(entry)) = entries.next_entry().await {
                if entry.path().extension().is_some_and(|ext| ext == "mp3") {
                    count += 1;
                    if let Ok(meta) = entry.metadata().await {
                        size_bytes += meta.len();
                    }
                }
            }
            let size_mb = size_bytes as f64 / (1024.0 * 1024.0);
            format!("{} files ({:.1} MB)", count, size_mb)
        }
        Err(_) => "N/A".to_string(),
    };

    let llm_status = if llm::is_configured() {
        let endpoints = env::var("LLM_ENDPOINTS").unwrap_or_default();
        let count = endpoints.split(',').filter(|s| !s.trim().is_empty()).count();
        format!("{} {}", count, state.lang.endpoint_label)
    } else {
        state.lang.not_configured.clone()
    };

    format!(
        "{}\n\n🗄️ {}: {}\n🎵 {}: {}\n🤖 {}: {}",
        state.lang.stats_title,
        state.lang.stats_database, db_stats,
        state.lang.stats_cache, cache_info,
        state.lang.stats_llm, llm_status,
    )
}

async fn cmd_help(state: &AppState, _payload: &WebhookPayload) -> String {
    let voices = tts::AVAILABLE_VOICES.iter().map(|v| format!("`{}`", v)).collect::<Vec<_>>().join(", ");
    let cloned_hint = if tts::voiceclone_configured() { " (cloned voices by name, see /myvoices)" } else { "" };
    let effects = crate::audio_effects::AVAILABLE_EFFECTS.iter().map(|e| format!("`{}`", e)).collect::<Vec<_>>().join(", ");
    format!(
        "{}{}",
        state.lang.help_title,
        state.lang.help_text.replacen("{}", &format!("{voices}{cloned_hint}"), 1).replacen("{}", &effects, 1),
    )
}