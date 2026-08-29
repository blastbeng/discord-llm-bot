use std::sync::Arc;
use std::time::Duration;
use rand::Rng;
use sqlx::SqlitePool;
use serenity::all::{Context, GuildId};
use crate::{llm, tts, lang};
use crate::error::BotError;

/// Shared state for the voice eavesdrop feature.
#[derive(Default)]
pub struct VoiceEavesdropState {
    /// Random timeout in seconds for the next eavesdrop session.
    pub next_eavesdrop_secs: Option<u64>,
}

/// Validate an LLM response — reject anything that looks like a safety block,
/// JSON, or noise. If the response fails validation, we stay silent.
fn validate_response(text: &str) -> bool {
    if text.len() < 5 {
        return false;
    }
    let lower = text.to_lowercase();
    if lower.contains("user safety") || lower.contains("safe:") || lower.contains("safety:") ||
       lower.contains("policy:") || lower.contains("blocked") || lower.contains("explicit") ||
       lower.contains("content filter") || lower.contains("i'm sorry") ||
       lower.contains("cannot comply") || lower.contains("cannot generate") ||
       lower.contains("not appropriate") || lower.contains("against policy") ||
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
    let state = Arc::new(tokio::sync::RwLock::new(VoiceEavesdropState::default()));
    log::info!("voice_eavesdrop: loop starting (min={}s, max={}s)",
        lang::config_min_secs(), lang::config_max_secs());

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;

        if !lang::config_enabled() {
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
        let mut target: Option<(GuildId, String)> = None;

        for guild_id in guild_ids {
            // The CacheRef guard is !Send, so keep it alive only inside this
            // block and drop it before the await below.
            let found = {
                let Some(guild) = ctx.cache.guild(guild_id) else {
                    continue;
                };

                let Some(bot_vs) = guild.voice_states.get(&bot_user_id) else {
                    continue;
                };
                let Some(bot_channel) = bot_vs.channel_id else {
                    continue;
                };

                let mut human_count = 0;
                let mut target_username: Option<String> = None;

                for vs in guild.voice_states.values() {
                    if vs.channel_id != Some(bot_channel) {
                        continue;
                    }
                    if vs.user_id == bot_user_id {
                        continue;
                    }
                    if let Some(user) = ctx.cache.user(vs.user_id) {
                        if user.bot {
                            continue;
                        }
                    }
                    human_count += 1;
                    if target_username.is_none() {
                        target_username = Some(
                            ctx.cache.user(vs.user_id)
                                .map(|u| u.name.clone())
                                .unwrap_or_else(|| vs.user_id.to_string()),
                        );
                    }
                }

                (human_count, bot_channel, target_username)
            };

            let (human_count, bot_channel, target_username) = found;

            if human_count > 0 {
                log::info!("voice_eavesdrop: found channel {} in guild {} with {} human(s)",
                    bot_channel, guild_id, human_count);

                {
                    let mut state = state.write().await;
                    let mut rng = rand::thread_rng();
                    let secs = rng.gen_range(lang::config_min_secs()..=lang::config_max_secs());
                    state.next_eavesdrop_secs = Some(secs);
                }

                target = Some((guild_id, target_username.unwrap_or_default()));
                break;
            }
        }

        if let Some((guild_id, username)) = target {
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

            log::info!("voice_eavesdrop: LLM response ({} chars): {}", response.len(),
                &response[..response.len().min(100)]);

            // Generate TTS and play using songbird::get() pattern
            let tts_result = match tts::get_or_generate_tts_with_effect(&response, "Google", "none").await {
                Ok(r) => r,
                Err(e) => {
                    log::warn!("voice_eavesdrop: TTS generation error: {}", e);
                    continue;
                }
            };

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
    let track_handle = handler.play_only(source.into());

    // Apply the user-set volume
    let vol = *volume.lock().unwrap();
    let _ = track_handle.set_volume(vol);

    Ok(())
}
