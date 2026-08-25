mod database;
mod lang;
mod llm;
mod tts;

use std::env;
use std::sync::Arc;

use teloxide::prelude::*;
use teloxide::types::Message;

/// Shared application state, mirroring the WhatsApp bot's `AppState`.
/// `enabled` gates whether incoming messages are processed — when false the
/// bot stays idle (see `TEL_ENABLED`).
struct AppState {
    db_pool: sqlx::SqlitePool,
    lang: lang::Lang,
    /// Per-chat conversation history for /ask, keyed by chat id.
    conversations: std::sync::Mutex<std::collections::HashMap<String, Vec<llm::ConversationMessage>>>,
    /// Whether Telegram processing is enabled (TEL_ENABLED=true).
    enabled: bool,
}

/// Whether the Telegram bot is enabled. When not "true", the process starts
/// and stays idle (so the container remains healthy) but does not poll
/// Telegram or process any message — mirroring the WhatsApp bot's behaviour.
fn config_enabled() -> bool {
    env::var("TEL_ENABLED").unwrap_or_else(|_| "false".to_string()).to_lowercase() == "true"
}

/// Placeholder handler for Step 1: it just logs incoming messages so we can
/// confirm the long-polling loop and bot connection work. The actual command
/// dispatch is added in a later step.
async fn handle_message(bot: Bot, msg: Message, state: Arc<AppState>) -> ResponseResult<()> {
    if !state.enabled {
        return Ok(());
    }
    if let Some(text) = msg.text() {
        log::info!("telegram-bot: received from {}: {:?}", msg.chat.id, text);
    } else {
        log::info!("telegram-bot: received non-text message from {}", msg.chat.id);
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    eprintln!("=== telegram-bot starting ===");
    dotenv::dotenv().ok();
    env_logger::Builder::from_env(env_logger::Env::default().filter_or("LOG_LEVEL", "info")).init();

    let enabled = config_enabled();

    if !enabled {
        // Stay alive but idle (no polling, no processing) so the container
        // remains healthy without actually running Telegram processing.
        log::info!("telegram-bot: disabled (TEL_ENABLED != true), staying idle");
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    // Initialize the shared database and language.
    let db_url = env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:config/discord-bot.sqlite3".to_string());
    let db_pool = match sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            log::error!("telegram-bot: failed to connect to database: {}", e);
            std::process::exit(1);
        }
    };
    if let Err(e) = database::init_db(&db_pool).await {
        log::error!("telegram-bot: failed to initialize database: {}", e);
        std::process::exit(1);
    }
    if let Err(e) = database::populate_db_if_empty(&db_pool).await {
        log::warn!("telegram-bot: populate_db_if_empty error: {}", e);
    }

    let state = Arc::new(AppState {
        db_pool,
        lang: lang::Lang::new(),
        conversations: std::sync::Mutex::new(std::collections::HashMap::new()),
        enabled,
    });

    // Initialise FakeYou login (best-effort) before starting to poll.
    tts::init_fakeyou().await;

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
