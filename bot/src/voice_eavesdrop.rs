use std::sync::Arc;
use std::time::Duration;
use rand::Rng;
use serenity::all::{Context, GuildId};
use crate::{Data, llm, tts, lang};
use crate::error::BotError;

/// Shared state for the voice eavesdrop feature.
#[derive(Default)]
pub struct VoiceEavesdropState {
    /// Random timeout in seconds for the next eavesdrop session.
    /// None = no active timer (eavesdrop not running).
    pub next_eavesdrop_secs: Option<u64>,
}

/// Validate an LLM response — reject anything that looks like a safety block,
/// JSON, or noise. If the response fails validation, we stay silent.
fn validate_response(text: &str) -> bool {
    if text.len() < 5 {
        return false;
    }
    // Block common safety/filter artifacts
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

/// Pick a random human user currently in a voice channel with the bot.
async fn pick_random_human<'a>(
    ctx: &Context,
    guild_id: GuildId,
    bot_user_id: serenity::UserId,
) -> Option<(serenity::UserId, String)> {
    let guild = ctx.cache.guild(guild_id)?;
    let voice_states = guild.voice_states.values();
    let mut candidates = Vec::new();

    for vs in voice_states {
        if vs.channel_id.is_none() {
            continue;
        }
        if vs.user_id == bot_user_id {
            continue; // skip the bot itself
        }
        // Check if the user is a bot
        if let Some(user) = ctx.cache.user(vs.user_id) {
            if user.bot {
                continue;
            }
        }
        candidates.push(vs.user_id);
    }

    if candidates.is_empty() {
        return None;
    }

    let mut rng = rand::thread_rng();
    let user_id = candidates[rng.gen_range(0..candidates.len())];

    // Get username
    let username = if let Some(user) = ctx.cache.user(user_id) {
        user.name.clone()
    } else {
        // Fallback: fetch from API
        if let Ok(user) = user_id.to_user(ctx).await {
            user.name.clone()
        } else {
            return None;
        }
    };

    Some((user_id, username))
}

/// Start the voice eavesdrop loop. Runs in a background task, scanning for
/// active voice channels and periodically "eavesdropping" on random users.
pub async fn start_eavesdrop_loop(ctx: Context, data: Arc<Data>) {
    log::info!("voice_eavesdrop: loop starting (min={}s, max={}s)",
        lang::config_min_secs(), lang::config_max_secs());

    // Ensure the shared state is initialized
    let mut state = data.voice_eavesdrop.write().await;
    if state.next_eavesdrop_secs.is_none() {
        let mut rng = rand::thread_rng();
        let secs = rng.gen_range(lang::config_min_secs()..=lang::config_max_secs());
        state.next_eavesdrop_secs = Some(secs);
        log::debug!("voice_eavesdrop: scheduled in {} seconds", secs);
    }

    loop {
        tokio::time::sleep(Duration::from_secs(10)).await;

        if !lang::config_enabled() {
            continue;
        }

        // Check if we have an active timer, or schedule a new one
        let should_eavesdrop = {
            let mut state = data.voice_eavesdrop.write().await;
            if state.next_eavesdrop_secs.is_none() {
                // Schedule a new random timeout
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

        // Find a guild where the bot is in a voice channel with at least one human
        let bot_user_id = ctx.cache.current_user_id();
        let mut target: Option<(GuildId, String)> = None;

        // Iterate over voice state guilds
        for (guild_id, _channel_id) in ctx.cache.voice_state_guilds() {
            let guild = match ctx.cache.guild(guild_id) {
                Some(g) => g,
                None => continue,
            };

            // Find the bot's voice state
            let bot_vs = match guild.voice_states.get(&bot_user_id) {
                Some(vs) => vs,
                None => continue,
            };
            let bot_channel = match bot_vs.channel_id {
                Some(c) => c,
                None => continue,
            };

            // Count humans in this channel (excluding bots and the local bot)
            let mut human_count = 0;
            let mut target_user: Option<(serenity::UserId, String)> = None;

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
                if target_user.is_none() {
                    // Pick the first human as the target for now
                    let username = ctx.cache.user(vs.user_id)
                        .map(|u| u.name.clone())
                        .unwrap_or_else(|| vs.user_id.to_string());
                    target_user = Some((vs.user_id, username));
                }
            }

            if human_count > 0 {
                log::info!("voice_eavesdrop: found channel {} in guild {} with {} human(s), targeting user: {}",
                    bot_channel, guild_id, human_count,
                    target_user.as_ref().map(|(_, n)| n.as_str()).unwrap_or("?"));

                // Schedule the next eavesdrop AFTER this one
                {
                    let mut state = data.voice_eavesdrop.write().await;
                    let mut rng = rand::thread_rng();
                    let secs = rng.gen_range(lang::config_min_secs()..=lang::config_max_secs());
                    state.next_eavesdrop_secs = Some(secs);
                }

                target = Some((guild_id, target_user.unwrap_or_default().1));
                break;
            }
        }

        if let Some((guild_id, username)) = target {
            log::info!("voice_eavesdrop: eavesdropping on user {} in guild {}", username, guild_id);

            // Call LLM to generate a response based on what the user might say
            let pool = data.db_pool.clone();
            let lang = std::env::var("LANG").unwrap_or_else(|_| "ita".to_string());

            let prompt = format!(
                "The user '{}' just said something funny or interesting in a voice channel. \
                 Based on their typical speech patterns and personality, generate a funny, \
                 sarcastic, idiotic response that the bot would say to roast or mock them. \
                 Keep it short (2-3 sentences max). Be creative, vulgar, and insulting if appropriate. \
                 The response should be in the same language the user is speaking.",
                username
            );

            let response = llm::call_llm(&pool, &prompt, &lang)
                .await
                .map_err(|e| log::warn!("voice_eavesdrop: LLM call failed: {}", e))
                .ok();

            let content = match response {
                Some(c) if validate_response(&c) => c,
                _ => {
                    log::info!("voice_eavesdrop: LLM response rejected by validation, staying silent");
                    continue;
                }
            };

            log::info!("voice_eavesdrop: LLM response ({} chars): {}", content.len(), &content[..content.len().min(100)]);

            // Generate TTS and play
            match tts::get_or_generate_tts_with_effect(&pool, &content, "Google", "none", &data).await {
                Ok(result) => {
                    log::info!("voice_eavesdrop: TTS generated: {}", result.file_path);
                    if let Err(e) = play_audio(&ctx, &data, guild_id, &result.file_path).await {
                        log::warn!("voice_eavesdrop: playback failed: {}", e);
                    } else {
                        log::info!("voice_eavesdrop: playback complete");
                    }
                }
                Err(e) => {
                    log::warn!("voice_eavesdrop: TTS generation failed: {}", e);
                }
            }
        }
    }
}

async fn play_audio(
    ctx: &Context,
    data: &Data,
    guild_id: GuildId,
    file_path: &str,
) -> Result<(), BotError> {
    let shard_manager = data.shard_manager.lock().await;
    let handler = shard_manager.get(guild_id.shard_id()).ok_or_else(|| {
        BotError::Audio("Failed to get shard handler".to_string())
    })?;

    let track_handle = {
        let mut h = handler.lock().await;
        let source = songbird::input::File::new(file_path);
        h.play_only(source.into())
    };

    // Apply volume
    let vol = *data.volume.lock().unwrap();
    let _ = track_handle.set_volume(vol);

    Ok(())
}
