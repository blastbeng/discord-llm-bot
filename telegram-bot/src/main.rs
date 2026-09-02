//! Telegram mirror of the WhatsApp bot.
//!
//! Same commands and behaviour as the WhatsApp bot, sharing the same SQLite
//! database and TTS cache. Uses teloxide (long polling) for an efficient,
//! dependency-light setup. Gated by `TEL_ENABLED` — when false the process
//! stays alive but idle (no polling, no processing) so the container stays
//! healthy without actually running.

mod audio_effects;
mod database;
mod lang;
mod llm;
mod tts;

use std::env;
use std::sync::Arc;

use rand::seq::SliceRandom;
use teloxide::prelude::*;
use teloxide::net::Download as _;
use teloxide::types::{ChatId, ChatKind, InputFile, Message};

/// Shared application state, mirroring the WhatsApp bot's `AppState`.
struct AppState {
    db_pool: sqlx::SqlitePool,
    lang: lang::Lang,
    /// Per-chat conversation history for /ask, keyed by chat id.
    conversations: std::sync::Mutex<std::collections::HashMap<String, Vec<llm::ConversationMessage>>>,
    /// Whether Telegram processing is enabled (TEL_ENABLED=true).
    enabled: bool,
    /// Allowlist of chat/group IDs the bot is allowed to operate in.
    /// `None` means all chats are allowed (no restriction).
    allowed_chats: Option<std::collections::HashSet<i64>>,
}

/// Whether the Telegram bot is enabled. When not "true", the process starts
/// and stays idle (no polling, no processing) so the container stays healthy.
fn config_enabled() -> bool {
    env::var("TEL_ENABLED").unwrap_or_else(|_| "false".to_string()).to_lowercase() == "true"
}

/// Parse the `TEL_ALLOWED_CHATS` allowlist (comma-separated chat/group IDs).
/// Returns `None` when the variable is empty/unset, meaning all chats are
/// allowed. When set, only the listed chat/group IDs are processed.
fn config_allowed_chats() -> Option<std::collections::HashSet<i64>> {
    let raw = env::var("TEL_ALLOWED_CHATS").unwrap_or_default();
    let ids: std::collections::HashSet<i64> = raw
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<i64>().ok())
        .collect();
    if ids.is_empty() {
        None
    } else {
        Some(ids)
    }
}

#[tokio::main]
async fn main() {
    eprintln!("=== telegram-bot starting ===");
    dotenv::dotenv().ok();
    env_logger::Builder::from_env(env_logger::Env::default().filter_or("LOG_LEVEL", "info"))
        .filter_module("reqwest", log::LevelFilter::Warn)
        .filter_module("sqlx", log::LevelFilter::Warn)
        .init();

    let enabled = config_enabled();

    // Initialize the shared database (WAL mode + busy timeout so both bots can
    // safely share the SQLite file without "database is locked" errors).
    let db_url = env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:config/discord-bot.sqlite3".to_string());
    tokio::fs::create_dir_all("config").await.expect("Failed to create config directory");
    let db_path = db_url.strip_prefix("sqlite:").unwrap_or(&db_url);
    if !tokio::fs::try_exists(db_path).await.unwrap_or(false) {
        tokio::fs::File::create(db_path).await.expect("Failed to create database file");
    }
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
    database::init_db(&db_pool).await.expect("Database initialization failed");

    if enabled {
        database::populate_db_if_empty(&db_pool).await.expect("Database population failed");
    }

    let allowed_chats = config_allowed_chats();
    if let Some(ids) = &allowed_chats {
        log::info!("telegram-bot: restricting to allowed chats: {:?}", ids);
    } else {
        log::info!("telegram-bot: TEL_ALLOWED_CHATS not set — all chats allowed");
    }

    let state = Arc::new(AppState {
        db_pool,
        lang: lang::Lang::new(),
        conversations: std::sync::Mutex::new(std::collections::HashMap::new()),
        enabled,
        allowed_chats,
    });

    if !enabled {
        log::info!("telegram-bot: TEL_ENABLED is not 'true' — staying idle (no polling, no processing). Set TEL_ENABLED=true in .env.telegram to enable.");
        // Keep the process alive so the container stays healthy.
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    let token = env::var("TELOXIDE_TOKEN").expect("TELOXIDE_TOKEN must be set");
    let bot = Bot::new(token);

    log::info!("telegram-bot: starting long-polling (enabled)");

    let handler = {
        let state = state.clone();
        move |bot: Bot, msg: Message| {
            let state = state.clone();
            async move { handle_message(bot, msg, state).await }
        }
    };

    teloxide::repl(bot, handler).await;
}

/// Dispatch an incoming message to the appropriate command handler.
async fn handle_message(bot: Bot, msg: Message, state: Arc<AppState>) -> ResponseResult<()> {
    if !state.enabled {
        return Ok(());
    }
    let chat_id = msg.chat.id;

    // Enforce the chat/group allowlist. When TEL_ALLOWED_CHATS is set, only
    // messages from the listed chat/group IDs are processed; everything else
    // is ignored silently (mirrors the WhatsApp bot's allowed-groups filter).
    if let Some(allowed) = &state.allowed_chats {
        if !allowed.contains(&chat_id.0) {
            log::debug!("telegram-bot: ignoring message from non-allowed chat {}", chat_id);
            return Ok(());
        }
    }

    let Some(text) = msg.text() else { return Ok(()) };
    let text = text.trim();

    // Parse "/command" or "/command@BotName args".
    let (command, args) = match text.split_once(' ') {
        Some((cmd, rest)) => (cmd, rest.trim().to_string()),
        None => (text, String::new()),
    };
    // Strip any @botname suffix from the command.
    let command = command.split('@').next().unwrap_or(command).to_lowercase();

    match command.as_str() {
        "/speak" | "/s" => cmd_speak(&bot, chat_id, &state, &args).await,
        "/random" | "/r" => cmd_random(&bot, chat_id, &state, &args).await,
        "/createvoice" => cmd_createvoice(&bot, &msg, chat_id, &state, &args).await,
        "/myvoices" => cmd_myvoices(&bot, chat_id, &state).await,
        "/deletevoice" => cmd_deletevoice(&bot, chat_id, &state, &args).await,
        "/ask" | "/a" => {
            let r = cmd_ask(&state, &chat_id.to_string(), &args).await;
            let _ = bot.send_message(chat_id, r).await;
        }
        "/translate" | "/t" => {
            let r = cmd_translate(&state, &args).await;
            let _ = bot.send_message(chat_id, r).await;
        }
        "/joke" | "/j" => {
            let r = cmd_joke(&state).await;
            let _ = bot.send_message(chat_id, r).await;
        }
        "/stats" => {
            let r = cmd_stats(&state).await;
            let _ = bot.send_message(chat_id, r).await;
        }
        "/help" | "/h" | "/start" => {
            let r = cmd_help(&state).await;
            let _ = bot.send_message(chat_id, r).await;
        }
        _ => {
            // In a private (direct) chat, treat any non-command message as an
            // /ask query — the user is chatting with the bot directly, so a
            // plain message should get an LLM answer. Messages that start with
            // '/' (unrecognized slash commands) are still ignored, and unknown
            // messages in groups/other chats are ignored silently to avoid noise.
            if !text.starts_with('/') && matches!(msg.chat.kind, ChatKind::Private(_)) {
                let r = cmd_ask(&state, &chat_id.to_string(), text).await;
                let _ = bot.send_message(chat_id, r).await;
            }
        }
    }
    Ok(())
}

// ─── Helpers ──────────────────────────────────────────────────────

/// Pick a random cached MP3 file from the audios/ directory, if any exist.
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

/// Parse "text" / "text --voice X --effect Y" into (text, voice, effect).
fn parse_voice_effect(args: &str) -> (String, String, String) {
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

/// Send an audio file to the chat.
async fn send_audio(bot: &Bot, chat_id: ChatId, file_path: &str) {
    if let Err(e) = bot.send_audio(chat_id, InputFile::file(file_path.to_string())).await {
        log::error!("telegram-bot: failed to send audio {}: {}", file_path, e);
    }
}

// ─── Commands ─────────────────────────────────────────────────────

/// Owner identity for voice cloning, namespaced per Telegram user.
fn vc_owner(user_id: teloxide::types::UserId) -> String {
    format!("telegram:{}", user_id.0)
}

/// Fetch the base64 content of an audio attachment usable as a voice sample.
/// Accepts: audio, voice (voice note), or document (mp3/wav). Returns None if
/// the message carries none of these.
async fn sample_from_message(bot: &Bot, msg: &Message) -> Option<Result<Vec<u8>, String>> {
    let file_meta = if let Some(a) = &msg.audio() {
        Some((a.file.clone(), a.file_name.clone()))
    } else if let Some(v) = &msg.voice() {
        Some((v.file.clone(), None))
    } else if let Some(d) = &msg.document() {
        let ext_ok = d
            .file_name
            .as_deref()
            .map(|n| {
                let n = n.to_lowercase();
                n.ends_with(".mp3") || n.ends_with(".wav")
            })
            .unwrap_or(false);
        if ext_ok {
            Some((d.file.clone(), d.file_name.clone()))
        } else {
            None
        }
    } else {
        None
    }?;

    let (file, _name) = file_meta;
    match bot.get_file(file.id).await {
        Ok(f) => {
            let mut buf = Vec::new();
            match bot.download_file(&f.path, &mut buf).await {
                Ok(()) => Some(Ok(buf)),
                Err(e) => Some(Err(format!("download failed: {e}"))),
            }
        }
        Err(e) => Some(Err(format!("get_file failed: {e}"))),
    }
}

/// /createvoice <name> with an attached MP3/WAV sample in the same message.
/// (For voice-note flows: send the voice note, then /createvoice name as a
/// REPLY to it — the sample is then taken from the replied-to message.)
async fn cmd_createvoice(bot: &Bot, msg: &Message, chat_id: ChatId, state: &AppState, args: &str) {
    let name = args.trim().to_string();
    let lang = &state.lang;
    if !tts::voiceclone_configured() {
        let _ = bot.send_message(chat_id, &lang.vc_not_configured).await;
        return;
    }
    if name.is_empty() {
        let _ = bot.send_message(chat_id, &lang.vc_usage).await;
        return;
    }
    if !tts::is_valid_clone_name(&name) {
        let _ = bot.send_message(chat_id, &lang.vc_invalid_name).await;
        return;
    }

    // Sample can be attached to this message or, when replying, to the
    // replied-to message.
    let sample = match sample_from_message(bot, msg).await {
        Some(r) => Some(r),
        None => match msg.reply_to_message() {
            Some(replied) => sample_from_message(bot, replied).await,
            None => None,
        },
    };

    match sample {
        None => {
            let _ = bot.send_message(chat_id, &lang.vc_sample_invalid).await;
        }
        Some(Err(e)) => {
            log::error!("telegram-bot createvoice: {}", e);
            let _ = bot.send_message(chat_id, &lang.vc_sample_invalid).await;
        }
        Some(Ok(bytes)) => {
            use base64::Engine as _;
            let audio_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            let owner = msg
                .from
                .as_ref()
                .map(|u| vc_owner(u.id))
                .unwrap_or_default();
            match tts::create_cloned_voice(&name, &owner, &audio_b64).await {
                Ok(()) => {
                    let m = lang.vc_created.replacen("{}", &name, 1).replacen("{}", &name, 1);
                    let _ = bot.send_message(chat_id, m).await;
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
                    let _ = bot.send_message(chat_id, m).await;
                }
            }
        }
    }
}

/// /myvoices — list all cloned voices.
async fn cmd_myvoices(bot: &Bot, chat_id: ChatId, state: &AppState) {
    let lang = &state.lang;
    if !tts::voiceclone_configured() {
        let _ = bot.send_message(chat_id, &lang.vc_not_configured).await;
        return;
    }
    let voices = tts::list_cloned_voices().await.unwrap_or_default();
    if voices.is_empty() {
        let _ = bot.send_message(chat_id, &lang.vc_list_empty).await;
        return;
    }
    let lines: Vec<String> = voices
        .iter()
        .map(|v| format!("• **{}** — `/speak testo --voice {}`", v.name, v.name))
        .collect();
    let _ = bot.send_message(chat_id, lines.join("\n")).await;
}

/// /deletevoice <name> — delete a cloned voice.
async fn cmd_deletevoice(bot: &Bot, chat_id: ChatId, state: &AppState, args: &str) {
    let name = args.trim().to_string();
    let lang = &state.lang;
    if !tts::voiceclone_configured() {
        let _ = bot.send_message(chat_id, &lang.vc_not_configured).await;
        return;
    }
    if name.is_empty() {
        let _ = bot.send_message(chat_id, &lang.vc_delete_usage).await;
        return;
    }
    let voices = tts::list_cloned_voices().await.unwrap_or_default();
    match voices.iter().find(|v| v.name == name) {
        None => {
            let _ = bot.send_message(chat_id, lang.vc_not_found.replacen("{}", &name, 1)).await;
        }
        Some(v) => {
            match tts::delete_cloned_voice(&name, &v.owner).await {
                Ok(()) => {
                    let _ = bot.send_message(chat_id, lang.vc_deleted.replacen("{}", &name, 1)).await;
                }
                Err(e) => {
                    let _ = bot.send_message(chat_id, lang.vc_error.replacen("{}", &e, 1)).await;
                }
            }
        }
    }
}

async fn cmd_speak(bot: &Bot, chat_id: ChatId, state: &AppState, args: &str) {
    let (text, voice, effect) = parse_voice_effect(args);

    if text.is_empty() {
        let _ = bot.send_message(chat_id, &state.lang.speak_usage).await;
        return;
    }
    if text.chars().count() > 200 {
        let _ = bot.send_message(chat_id, &state.lang.text_too_long).await;
        return;
    }

    // "random" resolves against Google + registered cloned voices (clones
    // still fall back to Google if fish.audio fails). Non-random values are
    // used as-is.
    let voice_is_random = voice == "random";
    let effect_is_random = effect == "random";
    let actual_voice = if voice_is_random {
        tts::pick_random_voice().await
    } else {
        voice
    };
    // Effects apply ONLY to the built-in Google voice unless the user
    // explicitly names both voice and effect: a randomized voice pick
    // or a randomized effect pins the rendering voice to Google.
    let actual_voice = if (voice_is_random && effect != "none") || effect_is_random {
        "Google".to_string()
    } else {
        actual_voice
    };
    if !tts::is_valid_voice(&actual_voice) {
        let _ = bot.send_message(chat_id, &state.lang.invalid_voice).await;
        return;
    }

    let actual_effect = if effect_is_random {
        crate::audio_effects::random_effect().to_string()
    } else {
        effect
    };
    if !crate::audio_effects::is_valid_effect(&actual_effect) {
        let _ = bot.send_message(chat_id, &state.lang.invalid_effect).await;
        return;
    }

    match tts::get_or_generate_tts_with_effect(&text, &actual_voice, &actual_effect).await {
        Ok(tts_result) => {
            // Surface a Google-fallback (cloned voice unavailable) to the user.
            if let Some(warn) = &tts_result.fallback_used {
                let _ = bot.send_message(chat_id, warn).await;
            }
            if let Err(e) = database::insert_sentence(&state.db_pool, &text).await {
                log::error!("Failed to insert sentence: {}", e);
            }
            send_audio(bot, chat_id, &tts_result.file_path).await;
        }
        Err(e) => {
            log::error!("TTS generation failed: {}", e);
            let _ = bot.send_message(chat_id, &state.lang.error_generating_audio).await;
        }
    }
}

async fn cmd_random(bot: &Bot, chat_id: ChatId, state: &AppState, args: &str) {
    let (search_text, voice, effect) = parse_voice_effect(args);

    // No effect by default — /random varies voice and sentence only. Users can
    // still pick an effect explicitly (including "random").

    let voice_explicitly_set = args.contains("--voice");
    let save_mp3 = std::env::var("SAVE_MP3_ON_DISK")
        .unwrap_or_else(|_| "false".to_string())
        .to_lowercase() == "true";

    // /random picks a RANDOM voice when the user does not select one: a mix of
    // the built-in Google voice and every registered cloned voice (clones
    // still fall back to Google if fish.audio fails). Every other command
    // keeps Google as its default.
    let voice = if voice_explicitly_set {
        voice
    } else {
        tts::pick_random_voice().await
    };
    let voice_is_builtin = tts::AVAILABLE_VOICES.contains(&voice.as_str());

    // Fast path: pick a random cached MP3 when no voice was selected (and the
    // default random pick resolved to the built-in Google voice), no effect to
    // apply (explicitly "none"), no search text, and disk caching is enabled.
    // The cache only stores Google-voice files, so a cloned pick must fall
    // through to real TTS generation.
    if !voice_explicitly_set && voice_is_builtin && effect == "none" && search_text.is_empty() && save_mp3 {
        if let Some(chosen) = pick_cached_mp3().await {
            log::info!("telegram-bot random: picked cached MP3: {}", chosen);
            send_audio(bot, chat_id, &chosen).await;
            return;
        }
    }

    // Fetch sentences from database.
    let sentences = if !search_text.is_empty() {
        match database::select_like_sentence(&state.db_pool, &search_text).await {
            Ok(s) => s,
            Err(e) => {
                log::error!("Database error: {}", e);
                let _ = bot.send_message(chat_id, &state.lang.database_error).await;
                return;
            }
        }
    } else {
        match database::select_all_sentence(&state.db_pool).await {
            Ok(s) => s,
            Err(e) => {
                log::error!("Database error: {}", e);
                let _ = bot.send_message(chat_id, &state.lang.database_error).await;
                return;
            }
        }
    };

    if sentences.is_empty() {
        if search_text.is_empty() {
            let _ = bot.send_message(chat_id, &state.lang.no_sentences_found).await;
        } else {
            let _ = bot.send_message(chat_id, state.lang.no_sentence_with_text.replacen("{}", &search_text, 1)).await;
        }
        return;
    }

    let random_sentence = {
        let mut rng = rand::thread_rng();
        sentences.choose(&mut rng).unwrap().to_string()
    };

    // Record that this sentence was spoken (increments usage_count).
    if let Err(e) = database::insert_sentence(&state.db_pool, &random_sentence).await {
        log::error!("telegram-bot random: failed to record sentence usage: {}", e);
    }

    // Google TTS truncates on text longer than ~200 chars.
    let tts_text: String = if random_sentence.chars().count() > 200 {
        let truncated: String = random_sentence.chars().take(200).collect();
        format!("{truncated}...")
    } else {
        random_sentence.clone()
    };

    // "random" resolves against Google + registered cloned voices (clones
    // still fall back to Google if fish.audio fails). Non-random values are
    // used as-is.
    let voice_is_random = voice == "random";
    let effect_is_random = effect == "random";
    let actual_voice = if voice_is_random {
        tts::pick_random_voice().await
    } else {
        voice
    };
    // Effects apply ONLY to the built-in Google voice unless the user
    // explicitly names both voice and effect: a randomized voice pick
    // or a randomized effect pins the rendering voice to Google.
    let actual_voice = if (voice_is_random && effect != "none") || effect_is_random {
        "Google".to_string()
    } else {
        actual_voice
    };
    if !tts::is_valid_voice(&actual_voice) {
        let _ = bot.send_message(chat_id, &state.lang.invalid_voice).await;
        return;
    }

    let actual_effect = if effect_is_random {
        crate::audio_effects::random_effect().to_string()
    } else {
        effect
    };
    if !crate::audio_effects::is_valid_effect(&actual_effect) {
        let _ = bot.send_message(chat_id, &state.lang.invalid_effect).await;
        return;
    }

    match tts::get_or_generate_tts_with_effect(&tts_text, &actual_voice, &actual_effect).await {
        Ok(tts_result) => {
            // Surface a Google-fallback (cloned voice unavailable) to the user.
            if let Some(warn) = &tts_result.fallback_used {
                let _ = bot.send_message(chat_id, warn).await;
            }
            send_audio(bot, chat_id, &tts_result.file_path).await;
        }
        Err(e) => {
            log::error!("TTS generation failed: {}", e);
            let _ = bot.send_message(chat_id, &state.lang.error_generating_audio).await;
        }
    }
}

async fn cmd_ask(state: &AppState, chat_id: &str, args: &str) -> String {
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

    let db_sentences = database::select_all_sentence(&state.db_pool).await.unwrap_or_default();

    let history = {
        let conversations = state.conversations.lock().unwrap();
        conversations.get(chat_id).cloned().unwrap_or_default()
    };

    match llm::ask(&text, &db_sentences, "Telegram Bot", &history).await {
        Ok(response) if llm::is_refusal_error(&response) => {
            // The LLM refused — never answer with the refusal boilerplate and
            // never persist it (it would poison the shared sentence database
            // and resurface via other bots' TTS).
            log::warn!("telegram-bot: LLM refused the request, not answering with it");
            state.lang.ai_refused.clone()
        }
        Ok(response) => {
            log::info!("telegram-bot: LLM response: {:?}", response);

            if let Err(e) = database::insert_sentence(&state.db_pool, &response).await {
                log::error!("Failed to insert LLM response: {}", e);
            }

            {
                let mut conversations = state.conversations.lock().unwrap();
                let history = conversations.entry(chat_id.to_string()).or_insert_with(Vec::new);
                history.push(llm::ConversationMessage { role: "user".to_string(), content: text.clone() });
                history.push(llm::ConversationMessage { role: "assistant".to_string(), content: response.clone() });
                if history.len() > 20 {
                    let start = history.len() - 20;
                    history.drain(0..start);
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

async fn cmd_translate(state: &AppState, args: &str) -> String {
    if !llm::is_configured() {
        return state.lang.ask_not_configured.clone();
    }

    let parts: Vec<&str> = args.rsplitn(2, ' ').collect();
    if parts.len() < 2 {
        return state.lang.translate_usage.clone();
    }
    let target_lang = parts[0].trim().to_string();
    let text = parts[1].trim().to_string();

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

async fn cmd_joke(state: &AppState) -> String {
    // JokeAPI has no Italian jokes, so fetch English ones and translate to the
    // configured language via the LLM when needed. This keeps the joke in the
    // language defined by LANG (see also the Discord/WhatsApp bots).
    let joke_url = "https://v2.jokeapi.dev/joke/Any?lang=en&safe-mode&type=twopart&format=json";
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            log::error!("telegram-bot joke: failed to build client: {}", e);
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
            log::error!("telegram-bot joke: failed to translate joke: {}", e);
            joke.to_string()
        }
    }
}

async fn cmd_stats(state: &AppState) -> String {
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

async fn cmd_help(state: &AppState) -> String {
    let voices = tts::AVAILABLE_VOICES.iter().map(|v| format!("`{}`", v)).collect::<Vec<_>>().join(", ");
    let cloned_hint = if tts::voiceclone_configured() { " (cloned voices by name, see /myvoices; /random picks a random voice when none is given)" } else { "" };
    let effects = crate::audio_effects::AVAILABLE_EFFECTS.iter().map(|e| format!("`{}`", e)).collect::<Vec<_>>().join(", ");
    format!(
        "{}{}",
        state.lang.help_title,
        state.lang.help_text.replacen("{}", &format!("{voices}{cloned_hint}"), 1).replacen("{}", &effects, 1),
    )
}
