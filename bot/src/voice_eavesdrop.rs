use std::sync::Arc;
use rand::Rng;
use sqlx::SqlitePool;
use serenity::all::{ChannelId, ChannelType, Context, GuildId};
use crate::{auto_join, llm, tts, lang};
use crate::error::BotError;

/// Shared state for the voice eavesdrop feature.
#[derive(Default)]
pub struct VoiceEavesdropState {
    /// Random timeout in seconds for the next eavesdrop session.
    pub next_eavesdrop_secs: Option<u64>,
    /// Runtime enable flag (toggled by /enable and /disable). The loop only
    /// schedules new eavesdrop sessions while this is true. Initialized from
    /// VOICE_EAVESDROP_ENABLED so the env var still controls the default.
    pub enabled: bool,
}

/// Global handle to the eavesdrop state so the /enable and /disable slash
/// commands can flip the runtime flag the background loop reads. Populated
/// once by start_eavesdrop_loop; commands are a no-op before that.
static SHARED_STATE: std::sync::OnceLock<Arc<tokio::sync::RwLock<VoiceEavesdropState>>> =
    std::sync::OnceLock::new();

fn set_shared_state(state: Arc<tokio::sync::RwLock<VoiceEavesdropState>>) {
    let _ = SHARED_STATE.set(state);
}

/// Enable/disable new eavesdrop sessions at runtime. Returns the new state,
/// or None if the eavesdrop loop has not started yet.
pub async fn set_enabled(enable: bool) -> Option<bool> {
    match SHARED_STATE.get() {
        Some(state) => {
            let mut s = state.write().await;
            s.enabled = enable;
            // Reset any pending timer so re-enabling schedules a fresh
            // randomized session instead of firing immediately.
            if !enable {
                s.next_eavesdrop_secs = None;
            }
            Some(s.enabled)
        }
        None => None,
    }
}

/// Current runtime eavesdrop state, if the loop has started.
#[allow(dead_code)]
pub async fn is_enabled() -> Option<bool> {
    match SHARED_STATE.get() {
        Some(state) => Some(state.read().await.enabled),
        None => None,
    }
}

/// Validate an LLM response — reject anything that looks like a safety block,
/// JSON, or noise. Refusal wording is delegated to the shared heuristics in
/// [`llm::looks_like_refusal`] so eavesdrop, welcome, goodbye, here-i-am and
/// ask all stay in sync. If the response fails validation, we stay silent.
fn validate_response(text: &str) -> bool {
    if text.len() < 5 {
        return false;
    }
    if llm::looks_like_refusal(text) {
        return false;
    }
    let lower = text.to_lowercase();
    if lower.contains("user safety") || lower.contains("safe:") || lower.contains("safety:") ||
       lower.contains("policy:") || lower.contains("blocked") || lower.contains("explicit") ||
       lower.contains("content filter") ||
       lower.contains("{") || lower.contains("```") || lower.contains("label:") ||
       lower.contains("classification:") || lower.contains("category:") ||
       lower.starts_with("safe") || lower.starts_with("blocked") ||
       lower.starts_with("error") {
        return false;
    }
    true
}

/// Start the voice eavesdrop loop. Runs in a background task, scanning for
/// active voice channels and periodically "eavesdropping" on random users.
///
/// Takes the pieces it needs directly (db pool + volume) instead of the whole
/// poise `Data`, because it is spawned from the framework setup closure before
/// poise's user data exists — same pattern as the auto-join scanner loop.
pub async fn start_eavesdrop_loop(ctx: Context, db_pool: SqlitePool, volume: Arc<std::sync::Mutex<f32>>) {
    // The runtime flag starts as the configured default (VOICE_EAVESDROP_ENABLED)
    // and can be flipped at runtime by the /enable and /disable commands.
    let initial = lang::config_enabled();
    let state = Arc::new(tokio::sync::RwLock::new(VoiceEavesdropState {
        enabled: initial,
        ..Default::default()
    }));
    // Share the state with the slash commands via OnceLock so /enable and
    // /disable can flip the flag the loop reads.
    set_shared_state(state.clone());
    log::info!("voice_eavesdrop: loop starting (min={}s, max={}s, initially {})",
        lang::config_min_secs(), lang::config_max_secs(),
        if initial { "enabled" } else { "disabled" });

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;

        if !state.read().await.enabled {
            continue;
        }

        // Check if we have an active timer, or schedule a new one
        let should_eavesdrop = {
            let mut state = state.write().await;
            if state.next_eavesdrop_secs.is_none() {
                let mut rng = rand::thread_rng();
                let secs = rng.gen_range(lang::config_min_secs()..=lang::config_max_secs());
                state.next_eavesdrop_secs = Some(secs);
                log::debug!("voice_eavesdrop: scheduled in {} seconds", secs);
            }
            if let Some(remaining) = state.next_eavesdrop_secs {
                if remaining <= 10 {
                    state.next_eavesdrop_secs = None;
                    true
                } else {
                    state.next_eavesdrop_secs = Some(remaining - 10);
                    false
                }
            } else {
                false
            }
        };

        if !should_eavesdrop {
            continue;
        }

        log::info!("voice_eavesdrop: triggered, looking for a voice channel with humans");

        let bot_user_id = ctx.cache.current_user().id;
        // Materialize guild IDs up front so no cache borrow is held across
        // the awaits below (the future must be Send for tokio::spawn).
        let guild_ids: Vec<GuildId> = ctx.cache.guilds();
        let mut target: Option<(GuildId, ChannelId, String)> = None;

        for guild_id in guild_ids {
            // The CacheRef guard is !Send, so keep it alive only inside this
            // block and drop it before the await below.
            let found: Option<(ChannelId, usize, String)> = {
                let Some(guild) = ctx.cache.guild(guild_id) else {
                    continue;
                };

                // Find the most-populated voice channel (same logic as the
                // scanner), not just the bot's own channel — the bot may be
                // stuck in an empty one after a failed leave.
                let mut best: Option<(ChannelId, usize, String)> = None;
                for ch in guild.channels.values() {
                    if !matches!(ch.kind, ChannelType::Voice) {
                        continue;
                    }
                    let mut human_count = 0;
                    let mut username: Option<String> = None;
                    for vs in guild.voice_states.values() {
                        if vs.channel_id != Some(ch.id) || vs.user_id == bot_user_id {
                            continue;
                        }
                        if let Some(user) = ctx.cache.user(vs.user_id) {
                            if user.bot {
                                continue;
                            }
                        }
                        human_count += 1;
                        if username.is_none() {
                            username = Some(
                                ctx.cache.user(vs.user_id)
                                    .map(|u| u.name.clone())
                                    .unwrap_or_else(|| vs.user_id.to_string()),
                            );
                        }
                    }
                    if human_count > 0 {
                        let better = match &best {
                            None => true,
                            Some((_, count, _)) => human_count > *count,
                        };
                        if better {
                            best = Some((ch.id, human_count, username.unwrap_or_default()));
                        }
                    }
                }
                best
            };

            let Some((channel_id, human_count, username)) = found else {
                continue;
            };

            log::info!("voice_eavesdrop: found channel {} in guild {} with {} human(s)",
                channel_id, guild_id, human_count);

            {
                let mut state = state.write().await;
                let mut rng = rand::thread_rng();
                let secs = rng.gen_range(lang::config_min_secs()..=lang::config_max_secs());
                state.next_eavesdrop_secs = Some(secs);
            }

            target = Some((guild_id, channel_id, username));
            break;
        }

        if let Some((guild_id, channel_id, username)) = target {
            log::info!("voice_eavesdrop: eavesdropping on user {} in guild {}", username, guild_id);

            let pool = db_pool.clone();
            let lang_code = std::env::var("LANG").unwrap_or_else(|_| "ita".to_string());

            // Fetch random sentences from DB for style context
            let db_sentences = match llm::fetch_random_sentences(&pool, 30).await {
                Ok(s) => s,
                Err(e) => {
                    log::warn!("voice_eavesdrop: failed to fetch DB sentences: {}", e);
                    continue;
                }
            };

            // Generate eavesdrop response via LLM
            let response = match llm::eavesdrop_response(&username, &db_sentences, &lang_code).await {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("voice_eavesdrop: LLM error: {}", e);
                    continue;
                }
            };

            if !validate_response(&response) {
                log::info!("voice_eavesdrop: LLM response rejected by validation, staying silent");
                continue;
            }

            // Char-based truncation — a byte slice could land mid-multibyte
            // character (Italian accents) and panic.
            let preview: String = response.chars().take(100).collect();
            log::info!("voice_eavesdrop: LLM response ({} chars): {}", response.len(), preview);

            // Generate TTS and play using songbird::get() pattern
            // Random effect per eavesdrop comment: the bot never comments the
            // same way twice. Applied on-the-fly from the plain cache.
            let effect = crate::audio_effects::random_effect();
            let tts_result = match tts::get_or_generate_tts_with_effect(&response, "Google", effect).await {
                Ok(r) => r,
                Err(e) => {
                    log::warn!("voice_eavesdrop: TTS generation error: {}", e);
                    continue;
                }
            };

            // Make sure the bot is actually in the target channel — it may be
            // stuck in an empty one after a failed idle-leave, and audio only
            // reaches listeners in the channel the bot is connected to.
            if let Some(current) = auto_join::current_bot_channel(&ctx, guild_id).await {
                if current != channel_id {
                    log::info!("voice_eavesdrop: moving bot from channel {} to {}", current, channel_id);
                    if let Err(e) = auto_join::switch_to(&ctx, guild_id, current, channel_id).await {
                        log::warn!("voice_eavesdrop: failed to move to channel {}: {}", channel_id, e);
                    }
                }
            }

            // Play audio using songbird's get() pattern like auto_join does
            if let Err(e) = play_eavesdrop_audio(&ctx, &volume, guild_id, tts_result.file_path).await {
                log::warn!("voice_eavesdrop: playback failed: {}", e);
            } else {
                log::info!("voice_eavesdrop: playback started in guild {}", guild_id);
            }
        }
    }
}

async fn play_eavesdrop_audio(
    ctx: &Context,
    volume: &Arc<std::sync::Mutex<f32>>,
    guild_id: GuildId,
    file_path: String,
) -> Result<(), BotError> {
    let manager = match songbird::get(ctx).await {
        Some(m) => m,
        None => return Err(BotError::VoiceConnection("Songbird not initialized".to_string())),
    };

    let handler_lock = match manager.get(guild_id) {
        Some(h) => h,
        None => return Err(BotError::VoiceConnection("Bot not in voice channel".to_string())),
    };

    let mut handler = handler_lock.lock().await;
    let source = songbird::input::File::new(file_path);
    // Centralized playback: self-demutes the bot if server-muted (and
    // self-untimeouts if server-timed-out), then applies
    // the user-set volume.
    crate::play_with_volume(ctx, &mut handler, source.into(), volume, guild_id).await;

    Ok(())
}
