mod database;
mod auto_join;
mod audio_effects;
mod error;
mod generator;
mod lang;
mod llm;
mod soundboard;
mod tts;

mod voice_capture;
mod voice_eavesdrop;
mod voice_mute;
mod voice_timeout;
use error::{ErrorTracker, Logger};
use poise::serenity_prelude as serenity;
use poise::serenity_prelude::Mentionable;
use rand::seq::SliceRandom;
use std::env;
use sysinfo::System;
use songbird::SerenityInit;
use image::GenericImageView;

/// Records the moment the bot process started, used to report uptime in
/// /stats. Initialized once at startup so it reflects real boot time rather
/// than the first time /stats happens to be called.
static BOOT_TIME: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Cached system resource stats (CPU%, RAM%) refreshed periodically in the
/// background so command handlers read the last value instantly instead of
/// blocking ~200ms on CPU sampling. Stored as `(cpu_percent, ram_percent)`.
static SYSTEM_STATS: std::sync::Mutex<Option<(f32, f32)>> = std::sync::Mutex::new(None);

/// Play an audio file through Songbird's built-in FFmpeg decoder.
/// Returns an error string if the bot is not connected or no handler is found,
/// so callers can inform the user instead of silently failing.
async fn play_audio_with_ffmpeg_pipe(
    ctx: &Context<'_>,
    file_path: &str,
    _voice: &str,
) -> Result<(), Error> {
    log::info!("play_audio_with_ffmpeg_pipe: playing {}", file_path);

    let guild_id = ctx.guild_id().unwrap();
    let manager = songbird::get(ctx.serenity_context()).await
        .ok_or("Songbird not registered")?;

    let handler_lock = manager.get(guild_id)
        .ok_or("No voice handler found for guild")?;
    let mut handler = handler_lock.lock().await;

    if handler.current_channel().is_none() {
        log::warn!("play_audio_with_ffmpeg_pipe: bot not connected to any channel");
        return Err("Bot not connected to any channel".into());
    }

    // Create the audio source only after confirming the bot is connected,
    // so we don't open a file handle that's immediately dropped on error.
    let source = songbird::input::File::new(file_path.to_string());
    play_with_volume(ctx.serenity_context(), &mut handler, source.into(), &ctx.data().volume, guild_id).await;
    log::info!("Audio playback started for guild {}", guild_id);
    Ok(())
}

// ============================================================================
// Enhanced File Validation (like Python's check_image_with_pil)
// ============================================================================

/// Validates image bytes using the image crate (mimics Python's check_image_with_pil).
/// Returns `Ok(dimensions)` on success, or `Err(reason)` if the image is invalid
/// or too small for a Discord avatar (minimum 128x128 pixels).
fn validate_image_bytes(bytes: &[u8]) -> Result<(u32, u32), &'static str> {
    match image::load_from_memory(bytes) {
        Ok(image) => {
            let (w, h) = image.dimensions();
            log::info!("Image validated: {}x{} pixels", w, h);
            if w < 128 || h < 128 {
                log::warn!("Image too small for avatar: {}x{} (minimum 128x128)", w, h);
                return Err("too_small");
            }
            Ok((w, h))
        }
        Err(e) => {
            log::warn!("Image validation failed: could not decode image: {}", e);
            Err("invalid")
        }
    }
}

// ============================================================================
// Smart Voice Client Management (ports Python's connect_bot_by_voice_client)
// ============================================================================

#[derive(Debug)]
// Data stored in the bot's context
/// A single message in the conversation history.
#[derive(Clone)]
pub struct ConversationMessage {
    role: String,      // "user" or "assistant"
    content: String,
}

pub struct Data {
    pub db_pool: sqlx::SqlitePool,
    pub lang: lang::Lang,
    pub error_tracker: ErrorTracker,
    pub volume: std::sync::Arc<std::sync::Mutex<f32>>,
    /// Per-guild conversation history for /ask. Stores the last N messages
    /// so the LLM can have a back-and-forth conversation instead of one-off
    /// questions. Keyed by guild ID.
    pub conversations: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<u64, Vec<ConversationMessage>>>>,
    /// Whether the bot automatically joins voice channels when a user joins
    /// (and switches/leaves according to the auto-join rules). Config: AUTO_JOIN_VOICE.
    pub auto_join_enabled: bool,
    /// Whether to speak a (humorous) welcome phrase when auto-joining/welcoming.
    /// Config: AUTO_JOIN_WELCOME.
    pub auto_join_welcome: bool,
    /// Whether to speak an insulting goodbye phrase when a user leaves the
    /// bot's voice channel. Config: AUTO_JOIN_GOODBYE.
    pub auto_join_goodbye: bool,
    /// Shared auto-join state for the "here I am" announcement (enable flag +
    /// per-channel throttle), shared with the background scanner loop so both
    /// owners stay in sync. Config: AUTO_JOIN_HERE_I_AM.
    pub auto_join_shared: std::sync::Arc<auto_join::AutoJoinShared>,
    /// Per-channel timestamp of the last spoken welcome phrase, used to throttle
    /// so multiple rapid joins don't trigger an LLM call each time. Keyed by
    /// channel id so different channels get independent throttling.
    pub last_welcome: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<u64, std::time::Instant>>>,
    /// Per-channel timestamp of the last spoken goodbye phrase. Same throttling
    /// rationale as `last_welcome`.
    pub last_goodbye: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<u64, std::time::Instant>>>,
    /// Active /soundboard sessions keyed by a short session id, so the
    /// pagination/play component buttons can resolve the stored search results.
    pub soundboard_sessions: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, soundboard::SoundboardSession>>>,
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Context<'a> = poise::Context<'a, Data, Error>;

/// Fetch the current top Steam games and set the bot's "Playing" activity to a
/// random one. Returns true if a presence was set successfully.
///
/// Uses a dedicated client with a short timeout so an unreachable SteamSpy
/// fails fast and lets the caller fall back to a default presence, instead of
/// hanging on the shared no-timeout TTS client.
async fn update_presence_from_steam(ctx: &serenity::Context) -> bool {
    let url = "https://steamspy.com/api.php?request=top100in2weeks";
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("Failed to build presence HTTP client");
    match client.get(url).send().await {
        Ok(resp) => {
            match resp.json::<serde_json::Value>().await {
                Ok(json) => {
                    if let Some(obj) = json.as_object() {
                        let games: Vec<String> = obj.values().filter_map(|v| v["name"].as_str().map(|s| s.to_string())).collect();
                        if let Some(game) = games.choose(&mut rand::thread_rng()) {
                            log::info!("change_presence_loop - setting game: {}", game);
                            let activity = serenity::ActivityData::playing(game.clone());
                            ctx.set_activity(Some(activity));
                            return true;
                        }
                        log::warn!("change_presence_loop - SteamSpy returned no games");
                    }
                }
                Err(e) => log::error!("change_presence_loop - failed to parse JSON: {}", e),
            }
        }
        Err(e) => log::error!("change_presence_loop - failed to fetch from steamspy: {}", e),
    }
    false
}

/// Periodically update the bot's presence (a random top Steam game). Runs the
/// first update immediately at startup so the bot never sits without a
/// presence waiting for the first 6-hour tick. If SteamSpy is unreachable or
/// returns nothing, fall back to a stable, meaningful default presence so the
/// bot still shows something instead of appearing offline.
async fn change_presence_loop(ctx: serenity::Context) {
    // Run once immediately, then keep the same 6-hour cadence.
    if !update_presence_from_steam(&ctx).await {
        log::warn!("change_presence_loop - SteamSpy unavailable, using fallback presence");
        let activity = serenity::ActivityData::playing("with /help");
        ctx.set_activity(Some(activity));
    }

    let mut interval = tokio::time::interval(std::time::Duration::from_secs(6 * 60 * 60));
    loop {
        interval.tick().await;
        if !update_presence_from_steam(&ctx).await {
            log::warn!("change_presence_loop - SteamSpy unavailable, keeping current presence");
        }
    }
}

/// Periodically sample CPU/RAM usage and cache it in SYSTEM_STATS, so command
/// handlers don't block ~200ms on CPU sampling for every reply.
async fn sample_system_stats_loop() {
    let mut sys = System::new_all();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
    loop {
        interval.tick().await;
        // Two refreshes with a short delay are needed for a meaningful CPU delta.
        sys.refresh_cpu();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        sys.refresh_cpu();
        let cpu = sys.global_cpu_info().cpu_usage();
        let total = sys.total_memory();
        let used = sys.used_memory();
        let ram = if total > 0 { (used as f64 / total as f64) * 100.0 } else { 0.0 };
        *SYSTEM_STATS.lock().unwrap() = Some((cpu, ram as f32));
    }
}

async fn get_queue_message(lang: &lang::Lang) -> String {
    let (cpu_usage, ram_usage) = SYSTEM_STATS.lock().unwrap().unwrap_or((0.0, 0.0));
    log::debug!("get_queue_message: CPU {:.1}%, RAM {:.2}%", cpu_usage, ram_usage);

    // Format comprehensive queue message with all metrics for user visibility (like Python's get_queue_message)
    lang.queue_overload
        .replacen("{:.1}", &format!("{:.1}", cpu_usage), 1)
        .replacen("{:.2}", &format!("{:.2}", ram_usage), 1)
}

async fn check_permissions(ctx: Context<'_>) -> Result<(), Error> {
    let lang = &ctx.data().lang;
    let _guild_id = ctx.guild_id().unwrap();
    let channel_id = {
        let guild = ctx.guild().ok_or(lang.guild_not_found.as_str())?;
        guild.voice_states.get(&ctx.author().id).and_then(|vs| vs.channel_id).ok_or(lang.must_be_in_voice.as_str())?
    };
    log::debug!("check_permissions: user {} in channel {}", ctx.author().id, channel_id);
    
    let channel = channel_id.to_channel(ctx.http()).await?;
    if let serenity::Channel::Guild(guild_channel) = channel {
        #[allow(deprecated)]
        let perms = guild_channel.permissions_for_user(ctx.cache(), ctx.cache().current_user().id)?;
        if !perms.speak() || !perms.connect() {
            log::warn!("check_permissions: bot lacks speak/connect permission in channel {}", channel_id);
            return Err(lang.user_no_permission.as_str().into());
        }
    }
    Ok(())
}

/// Check only speak permission (for stop/leave commands that don't need to connect)
async fn check_speak_permission(ctx: Context<'_>) -> Result<(), Error> {
    let lang = &ctx.data().lang;
    let _guild_id = ctx.guild_id().unwrap();
    let channel_id = {
        let guild = ctx.guild().ok_or(lang.guild_not_found.as_str())?;
        guild.voice_states.get(&ctx.author().id).and_then(|vs| vs.channel_id).ok_or(lang.must_be_in_voice.as_str())?
    };
    log::debug!("check_speak_permission: user {} in channel {}", ctx.author().id, channel_id);

    let channel = channel_id.to_channel(ctx.http()).await?;
    if let serenity::Channel::Guild(guild_channel) = channel {
        #[allow(deprecated)]
        let perms = guild_channel.permissions_for_user(ctx.cache(), ctx.cache().current_user().id)?;
        if !perms.speak() {
            log::warn!("check_speak_permission: bot lacks speak permission in channel {}", channel_id);
            return Err(lang.user_no_permission.as_str().into());
        }
    }
    Ok(())
}

async fn connect_bot_by_voice_client(
    ctx: Context<'_>,
    channel_id: serenity::ChannelId,
    member_id: serenity::UserId,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let manager = songbird::get(ctx.serenity_context()).await
        .ok_or("Songbird not registered")?;

    // Check if the bot has speak permission in the target channel (like Python's connect_bot_by_voice_client)
    let channel = channel_id.to_channel(ctx.http()).await?;
    if let serenity::Channel::Guild(guild_channel) = channel {
        #[allow(deprecated)]
        let perms = guild_channel.permissions_for_user(ctx.cache(), ctx.cache().current_user().id)?;
        if !perms.speak() {
            log::warn!("connect_bot_by_voice_client: bot lacks speak permission in channel {}", channel_id);
            return Err(ctx.data().lang.disagio.as_str().into());
        }
    }

    // Smart channel switching (ports Python's connect_bot_by_voice_client logic):
    // 1. If bot is already in the target channel, stay put
    // 2. If bot is in a different channel and is NOT playing, check if the invoking
    //    member is already in the bot's current channel — if so, keep the bot there
    //    instead of switching (so the bot doesn't abandon users mid-conversation)
    // 3. Only switch channels when the member is NOT in the bot's current channel
    if let Some(handler_lock) = manager.get(guild_id) {
        let handler = handler_lock.lock().await;
        if let Some(current_channel) = handler.current_channel() {
            // Bot is already in the target channel — nothing to do
            if current_channel.0.get() == channel_id.get() {
                log::info!("connect_bot_by_voice_client: bot already in channel {}", channel_id);
                return Ok(());
            }

            // Bot is in a different channel — check if it's currently playing
            let is_playing = handler.queue().current().is_some();
            if is_playing {
                // Bot is playing audio — don't interrupt, keep the bot in its current channel
                log::info!("connect_bot_by_voice_client: bot is playing, keeping current channel {:?}", current_channel);
                return Ok(());
            }

            // Bot is not playing — check if the invoking member is in the bot's current channel
            // If so, keep the bot there instead of switching (Python's smart behavior)
            let bot_channel_id = serenity::ChannelId::new(current_channel.0.get());
            if let Ok(serenity::Channel::Guild(bot_guild_channel)) = bot_channel_id.to_channel(ctx.http()).await {
                let members = bot_guild_channel.members(ctx.cache()).unwrap_or_default();
                if members.iter().any(|m| m.user.id == member_id) {
                    log::info!("connect_bot_by_voice_client: member {} is in bot's current channel {}, keeping bot there", member_id, bot_channel_id);
                    return Ok(());
                }
            }
        }
        drop(handler);

        // Bot is in a different channel and member is not there — switch
        log::info!("connect_bot_by_voice_client: leaving current channel to join {}", channel_id);
        let mut handler = handler_lock.lock().await;
        let _ = handler.leave().await;
        drop(handler);
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    log::info!("connect_bot_by_voice_client: joining channel {}", channel_id);
    let _handler_lock = manager.join(guild_id, channel_id).await?;
    // Wait for connection to establish
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    Ok(())
}

async fn voice_autocomplete(
    _ctx: Context<'_>,
    current: &str,
) -> Vec<serenity::AutocompleteChoice> {
    // Built-ins first, then cloned voices. Clones are ALWAYS offered in the
    // picker under their PLAIN name (filtered by what the user typed):
    // selecting one is an explicit opt-in, so /random and every
    // default/automatic path still stay Google-only.
    // Discord hard limit: 25 autocomplete choices.
    const MAX_CHOICES: usize = 25;
    let mut choices: Vec<serenity::AutocompleteChoice> = Vec::new();
    let cur = current.to_lowercase();

    let builtin: Vec<&str> = tts::AVAILABLE_VOICES
        .iter()
        .chain(std::iter::once(&"random"))
        .copied()
        .collect();
    for v in builtin {
        if v.to_lowercase().contains(&cur) {
            choices.push(serenity::AutocompleteChoice::new(v.to_string(), v.to_string()));
        }
    }
    if let Ok(voices) = tts::list_cloned_voices().await {
        for v in voices.iter() {
            if choices.len() >= MAX_CHOICES {
                break;
            }
            if cur.is_empty() || v.name.to_lowercase().contains(&cur) {
                choices.push(serenity::AutocompleteChoice::new(v.name.clone(), v.name.clone()));
            }
        }
    }
    choices.truncate(MAX_CHOICES);
    choices
}

/// Centralized playback entry point. Every audio playback path must go
/// through this instead of calling `handler.play_only(source)` directly:
/// it first self-demutes the bot if a server admin voice-muted it (see
/// [`crate::voice_mute`]), then self-removes the bot's timeout if a server
/// admin timed it out (see [`crate::voice_timeout`]), then plays the source
/// and applies the stored volume so the bot respects the level set by
/// /volume across all tracks.
pub async fn play_with_volume(
    ctx: &serenity::Context,
    handler: &mut songbird::Call,
    source: songbird::input::Input,
    volume: &std::sync::Arc<std::sync::Mutex<f32>>,
    guild_id: serenity::GuildId,
) {
    crate::voice_mute::ensure_bot_not_muted(ctx, guild_id).await;
    crate::voice_timeout::ensure_bot_not_timed_out(ctx, guild_id).await;
    let track_handle = handler.play_only(source.into());
    // Apply the stored volume level to the new track
    let vol = *volume.lock().unwrap();
    let _ = track_handle.set_volume(vol);
}

async fn effect_autocomplete(
    _ctx: Context<'_>,
    current: &str,
) -> Vec<serenity::AutocompleteChoice> {
    // AVAILABLE_EFFECTS already includes "random"; no need to append it again.
    crate::audio_effects::AVAILABLE_EFFECTS
        .iter()
        .filter(|e| e.to_lowercase().contains(&current.to_lowercase()))
        .map(|e| {
            let name = e.to_string();
            serenity::AutocompleteChoice::new(name.clone(), name)
        })
        .collect()
}

/// Join channel.
#[poise::command(slash_command, user_cooldown = 5)]
async fn join(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    log::info!("[GUILDID : {}] join command invoked by user {}", ctx.guild_id().unwrap(), ctx.author().id);
    check_permissions(ctx).await?;
    let channel_id = {
        let guild = ctx.guild().ok_or(ctx.data().lang.guild_not_found.as_str())?;
        guild.voice_states.get(&ctx.author().id).and_then(|vs| vs.channel_id).ok_or(ctx.data().lang.must_be_in_voice.as_str())?
    };
    
    match connect_bot_by_voice_client(ctx, channel_id, ctx.author().id).await {
        Ok(_) => {
            // Check if the bot actually ended up in the user's channel.
            // connect_bot_by_voice_client may keep the bot in a different
            // channel (e.g. it was playing, or the member was already in
            // the bot's channel), so we must verify the actual channel.
            let manager = songbird::get(ctx.serenity_context()).await
                .ok_or("Songbird not registered")?;
            let bot_in_user_channel = manager.get(ctx.guild_id().unwrap())
                .is_some_and(|h| {
                    h.try_lock()
                        .ok()
                        .is_some_and(|guard| {
                            guard.current_channel()
                                .is_some_and(|c| c.0.get() == channel_id.get())
                        })
                });

            let message = if bot_in_user_channel {
                ctx.data().lang.join_success_to_self.replacen("{}", &ctx.author().id.mention().to_string(), 1)
            } else {
                ctx.data().lang.join_success.clone()
            };
            ctx.send(poise::CreateReply::default().content(&message).ephemeral(true)).await?;

            // The bot just entered the channel — announce its arrival with an
            // arrogant/insulting phrase (skipped when disabled, throttled, or
            // when the LLM misbehaves; and never announced twice when the bot
            // was already in the user's channel).
            if bot_in_user_channel {
                auto_join::speak_here_i_am(ctx.serenity_context(), ctx.data(), ctx.guild_id().unwrap(), channel_id).await;
            }
        }
        Err(e) => {
            log::error!("Failed to join voice channel: {:?}", e);
            ctx.send(poise::CreateReply::default().content(&ctx.data().lang.join_error).ephemeral(true)).await?;
        }
    }
    Ok(())
}

/// Leave channel
#[poise::command(slash_command, user_cooldown = 5)]
async fn leave(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    log::info!("[GUILDID : {}] leave command invoked by user {}", ctx.guild_id().unwrap(), ctx.author().id);
    check_speak_permission(ctx).await?;
    let guild_id = ctx.guild_id().unwrap();
    let manager = songbird::get(ctx.serenity_context()).await
        .ok_or("Songbird not registered")?;
    if manager.get(guild_id).is_some() {
        let _ = manager.remove(guild_id).await;
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.leave_success).ephemeral(true)).await?;
    } else {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.not_connected).ephemeral(true)).await?;
    }
    Ok(())
}

/// Stop playback.
#[poise::command(slash_command, user_cooldown = 5)]
async fn stop(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    log::info!("[GUILDID : {}] stop command invoked by user {}", ctx.guild_id().unwrap(), ctx.author().id);
    check_speak_permission(ctx).await?;
    let guild_id = ctx.guild_id().unwrap();
    let manager = songbird::get(ctx.serenity_context()).await
        .ok_or("Songbird not registered")?;
    if let Some(handler) = manager.get(guild_id) {
        let mut handler = handler.lock().await;
        handler.stop();
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.stop_success).ephemeral(true)).await?;
    } else {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.not_connected).ephemeral(true)).await?;
    }
    Ok(())
}

/// Repeat a sentence
#[poise::command(slash_command, user_cooldown = 1)]
async fn speak(
    ctx: Context<'_>,
    #[description = "La frase da ripetere"] text: String,
    #[description = "La voce da usare (default: Google)"]
    #[autocomplete = "voice_autocomplete"]
    voice: Option<String>,
    #[description = "Effetto audio (default: none)"]
    #[autocomplete = "effect_autocomplete"]
    effect: Option<String>,
) -> Result<(), Error> {
    log::info!("[GUILDID : {}] speak command invoked by user {} with text: {:?}, voice: {:?}, effect: {:?}", ctx.guild_id().unwrap(), ctx.author().id, text, voice, effect);
    let voice = voice.unwrap_or_else(|| "Google".to_string());
    let effect = effect.unwrap_or_else(|| "none".to_string());
    let actual_voice = if voice == "random" {
        let mut rng = rand::thread_rng();
        tts::AVAILABLE_VOICES.choose(&mut rng).unwrap().to_string()
    } else {
        voice
    };

    if !tts::is_valid_voice(&actual_voice) {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.invalid_voice).ephemeral(true)).await?;
        return Ok(());
    }

    // Resolve "random" effect to a random choice
    let actual_effect = if effect == "random" {
        crate::audio_effects::random_effect().to_string()
    } else {
        effect
    };

    if !crate::audio_effects::is_valid_effect(&actual_effect) {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.invalid_effect).ephemeral(true)).await?;
        return Ok(());
    }

    // Google TTS has a ~200 character limit; longer text silently fails or
    // returns truncated audio. Reject early with a clear user-facing message.
    if text.chars().count() > 200 {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.text_too_long).ephemeral(true)).await?;
        return Ok(());
    }

    ctx.defer_ephemeral().await?;
    check_permissions(ctx).await?;

    let guild_id = ctx.guild_id().unwrap();
    let channel_id = {
        let guild = ctx.guild().ok_or(ctx.data().lang.guild_not_found.as_str())?;
        log::info!("[GUILDID : {}] speak - text: {}, voice: {}, effect: {}", guild.id, text, actual_voice, actual_effect);
        guild.voice_states.get(&ctx.author().id).and_then(|vs| vs.channel_id).ok_or(ctx.data().lang.must_be_in_voice.as_str())?
    };
    connect_bot_by_voice_client(ctx, channel_id, ctx.author().id).await?;
    let queue_msg = get_queue_message(&ctx.data().lang).await;
    let initial_msg = ctx.data().lang.generating_audio.replacen("{}", &text, 1).replacen("{}", &queue_msg, 1);
    let reply = ctx.send(poise::CreateReply::default().content(initial_msg).ephemeral(true)).await?;

    let manager = match songbird::get(ctx.serenity_context()).await {
        Some(m) => m,
        None => {
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
            return Ok(());
        }
    };
    let handler_lock = match manager.get(guild_id) {
        Some(h) => h,
        None => {
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
            return Ok(());
        }
    };
    {
        // Wait for connection to establish with retry
        let mut connected = false;
        for _ in 0..5 {
            let handler = handler_lock.lock().await;
            if handler.current_channel().is_some() {
                connected = true;
                break;
            }
            drop(handler);
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        if !connected {
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
            return Ok(());
        }
    }

    let tts_result = match tts::get_or_generate_tts_with_effect(&text, &actual_voice, &actual_effect).await {
        Ok(result) => result,
        Err(e) => {
            log::error!("TTS generation failed: {}", e);
            let error_msg = &ctx.data().lang.tts_error_google;
            reply.edit(ctx, poise::CreateReply::default().content(error_msg).ephemeral(true)).await?;
            return Ok(());
        }
    };
    // Surface a Google-fallback (cloned voice unavailable) instead of
    // silently playing a different voice than the user picked.
    if let Some(warn) = &tts_result.fallback_used {
        let _ = reply.edit(ctx, poise::CreateReply::default().content(warn.clone()).ephemeral(true)).await;
    }

    if let Err(e) = database::insert_sentence(&ctx.data().db_pool, &text).await {
        log::error!("Failed to insert sentence into database: {}", e);
    }

    let mut handler = handler_lock.lock().await;
    if handler.current_channel().is_none() {
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
        return Ok(());
    }
    
    log::info!("TTS file path: {}", tts_result.file_path);
    if !tokio::fs::try_exists(&tts_result.file_path).await.unwrap_or(false) {
        log::error!("TTS file does not exist: {}", tts_result.file_path);
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.tts_error).ephemeral(true)).await?;
        return Ok(());
    }
    log::info!("Playing audio file: {}", tts_result.file_path);
    let source = songbird::input::File::new(tts_result.file_path.clone());
    play_with_volume(ctx.serenity_context(), &mut handler, source.into(), &ctx.data().volume, guild_id).await;
    log::info!("Audio playback started in guild {}", guild_id);

    let components = vec![
        serenity::CreateActionRow::Buttons(vec![
            serenity::CreateButton::new(format!("play:{}", tts_result.file_path))
                .label("Play")
                .style(serenity::ButtonStyle::Success),
            serenity::CreateButton::new("stop")
                .label("Stop")
                .style(serenity::ButtonStyle::Danger)
        ])
    ];

    // Update the message with Play/Stop buttons. Use match instead of ? so
    // that an expired/invalid interaction token doesn't propagate to on_error
    // as "Unknown interaction" — the audio already played successfully, so
    // a failed message update is cosmetic, not a real failure.
    match reply.edit(ctx, poise::CreateReply::default()
        .content(ctx.data().lang.playing.replacen("{}", &text, 1).replacen("{}", &tts_result.actual_voice, 1))
        .components(components)
        .ephemeral(true)
    ).await {
        Ok(_) => {}
        Err(e) => {
            let e_str = e.to_string();
            if e_str.contains("Unknown interaction") || e_str.contains("already been acknowledged") {
                log::debug!("speak: reply.edit failed (interaction expired, audio already played): {}", e_str);
            } else {
                log::warn!("speak: reply.edit failed: {}", e_str);
            }
        }
    }

    Ok(())
}

/// Say a random sentence
#[poise::command(slash_command, user_cooldown = 1)]
async fn random(
    ctx: Context<'_>,
    #[description = "La voce da usare (default: Google)"]
    #[autocomplete = "voice_autocomplete"]
    voice: Option<String>,
    #[description = "Il testo da cercare"] text: Option<String>,
    #[description = "Effetto audio (default: random)"]
    #[autocomplete = "effect_autocomplete"]
    effect: Option<String>,
) -> Result<(), Error> {
    log::info!("[GUILDID : {}] random command invoked by user {} with voice: {:?}, text: {:?}, effect: {:?}", ctx.guild_id().unwrap(), ctx.author().id, voice, text, effect);

    // Track whether the user explicitly specified a voice or effect.
    // The cached-MP3 shortcut below must only trigger when no effect will be
    // applied, because a cached file was generated without any effect filter.
    // When the user does not pick an effect, /random defaults to "random", so
    // the shortcut only fires when the effect resolves to "none".
    let voice_explicitly_set = voice.is_some();
    let voice = voice.unwrap_or_else(|| "Google".to_string());
    // Default to a random effect (which may itself resolve to "none") — the
    // /random command is about variety, including plain speech.
    let effect = effect.unwrap_or_else(|| "random".to_string());
    // /random resolves to Google only — cloned voices are never chosen here
    // ("random" always means a Google voice; cloned voices need explicit
    // --voice clone:<name>).
    let actual_voice = if voice == "random" {
        let mut rng = rand::thread_rng();
        tts::AVAILABLE_VOICES.choose(&mut rng).unwrap().to_string()
    } else {
        voice
    };

    if !tts::is_valid_voice(&actual_voice) {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.invalid_voice).ephemeral(true)).await?;
        return Ok(());
    }

    // Resolve "random" effect to a random choice
    let actual_effect = if effect == "random" {
        crate::audio_effects::random_effect().to_string()
    } else {
        effect
    };

    if !crate::audio_effects::is_valid_effect(&actual_effect) {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.invalid_effect).ephemeral(true)).await?;
        return Ok(());
    }

    // Validate search text length (if provided) — the resulting sentence from
    // the database is already bounded, but an excessively long search query
    // is still wasteful and likely a user mistake.
    if let Some(t) = &text {
        if t.chars().count() > 200 {
            ctx.send(poise::CreateReply::default().content(&ctx.data().lang.text_too_long).ephemeral(true)).await?;
            return Ok(());
        }
    }

    ctx.defer_ephemeral().await?;
    check_permissions(ctx).await?;

    let guild_id = ctx.guild_id().unwrap();
    let channel_id = {
        let guild = ctx.guild().ok_or(ctx.data().lang.guild_not_found.as_str())?;
        log::info!("[GUILDID : {}] random - voice: {}, text: {:?}", guild.id, actual_voice, text);
        guild.voice_states.get(&ctx.author().id).and_then(|vs| vs.channel_id).ok_or(ctx.data().lang.must_be_in_voice.as_str())?
    };
    connect_bot_by_voice_client(ctx, channel_id, ctx.author().id).await?;

    // When no voice is explicitly specified and SAVE_MP3_ON_DISK is true,
    // try to pick a random MP3 directly from the audios/ folder
    let save_mp3 = std::env::var("SAVE_MP3_ON_DISK")
        .unwrap_or_else(|_| "false".to_string())
        .to_lowercase() == "true";

    let mut cached_audio_path: Option<String> = None;
    // The cached-MP3 shortcut only makes sense when there is NO search text.
    // When the user passes a text to search, they expect a matching sentence
    // from the database — picking an unrelated random cached file would be
    // logically wrong (the audio wouldn't match their query).
    let has_search = text.as_ref().map_or(false, |t| !t.trim().is_empty());
    if !voice_explicitly_set && actual_effect == "none" && save_mp3 && !has_search {
        log::info!("random: no voice/effect/search and SAVE_MP3_ON_DISK=true, scanning audios/ folder");
        if let Ok(mut entries) = tokio::fs::read_dir("audios").await {
            let mut mp3_files = Vec::new();
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "mp3") {
                    if let Some(s) = path.to_str() {
                        // Voice cloning must never leak into /random — cached
                        // cloned-voice files (clone|*_*.mp3) are excluded so
                        // /random always plays a Google voice.
                        if !s.contains("clone|") {
                            mp3_files.push(s.to_string());
                        }
                    }
                }
            }
            if !mp3_files.is_empty() {
                let mut rng = rand::thread_rng();
                let chosen = mp3_files.choose(&mut rng).unwrap().clone();
                log::info!("random: picked cached MP3: {}", chosen);
                cached_audio_path = Some(chosen);
            } else {
                log::info!("random: no MP3 files found in audios/, falling back to Google TTS");
            }
        }
    }

    let queue_msg = get_queue_message(&ctx.data().lang).await;
    let initial_msg = ctx.data().lang.searching_random.replacen("{}", &queue_msg, 1);
    let reply = ctx.send(poise::CreateReply::default().content(initial_msg).ephemeral(true)).await?;

    let manager = match songbird::get(ctx.serenity_context()).await {
        Some(m) => m,
        None => {
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
            return Ok(());
        }
    };
    let handler_lock = match manager.get(guild_id) {
        Some(h) => h,
        None => {
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
            return Ok(());
        }
    };
    {
        // Wait for connection to establish with retry
        let mut connected = false;
        for _ in 0..5 {
            let handler = handler_lock.lock().await;
            if handler.current_channel().is_some() {
                connected = true;
                break;
            }
            drop(handler);
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        if !connected {
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
            return Ok(());
        }
    }

    // If we found a cached MP3, play it directly without TTS generation
    if let Some(audio_path) = &cached_audio_path {
        let mut handler = handler_lock.lock().await;
        if handler.current_channel().is_none() {
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
            return Ok(());
        }

        log::info!("Playing cached audio file: {}", audio_path);
        let source = songbird::input::File::new(audio_path.clone());
        play_with_volume(ctx.serenity_context(), &mut handler, source.into(), &ctx.data().volume, guild_id).await;
        log::info!("Audio playback started in guild {}", guild_id);

        let components = vec![
            serenity::CreateActionRow::Buttons(vec![
                serenity::CreateButton::new(format!("play:{}", audio_path))
                    .label("Play")
                    .style(serenity::ButtonStyle::Success),
                serenity::CreateButton::new("stop")
                    .label("Stop")
                    .style(serenity::ButtonStyle::Danger)
            ])
        ];

        // Derive a human-readable voice name from the filename token.
        // Filenames follow the pattern {voice_token}_[effect_]_{hash}.mp3
        // — extract the token and reverse-lookup the voice name for display.
        let voice_name = std::path::Path::new(audio_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("cached")
            .split('_')
            .next()
            .map(tts::get_voice_name_from_token)
            .unwrap_or_else(|| "Unknown".to_string());
        // Try to recover the original sentence text from ID3 tags.
        // Falls back to "Cached audio" if the file has no ID3 lyrics
        // (e.g., generated before ID3 tagging was added, or corrupted).
        let sentence_label = tts::read_id3_lyrics(audio_path).unwrap_or_else(|| "Cached audio".to_string());
        // Use match instead of ? so expired interaction tokens don't propagate
        // to on_error — the audio already started playing on the line above.
        match reply.edit(ctx, poise::CreateReply::default()
            .content(ctx.data().lang.playing.replacen("{}", &sentence_label, 1).replacen("{}", &voice_name, 1))
            .components(components)
            .ephemeral(true)
        ).await {
            Ok(_) => {}
            Err(e) => {
                let e_str = e.to_string();
                if e_str.contains("Unknown interaction") || e_str.contains("already been acknowledged") {
                    log::debug!("random (cached): reply.edit failed (interaction expired, audio already played): {}", e_str);
                } else {
                    log::warn!("random (cached): reply.edit failed: {}", e_str);
                }
            }
        }

        return Ok(());
    }

    // No cached audio found, fall back to normal TTS generation from database sentences
    let sentences = if let Some(t) = &text {
        if !t.trim().is_empty() {
            database::select_like_sentence(&ctx.data().db_pool, t).await?
        } else {
            database::select_all_sentence(&ctx.data().db_pool).await?
        }
    } else {
        database::select_all_sentence(&ctx.data().db_pool).await?
    };

    if sentences.is_empty() {
        let msg = if let Some(t) = &text {
            if !t.trim().is_empty() {
                ctx.data().lang.no_sentence_with_text.replacen("{}", t, 1)
            } else {
                ctx.data().lang.no_sentence.clone()
            }
        } else {
            ctx.data().lang.no_sentence.clone()
        };
        reply.edit(ctx, poise::CreateReply::default().content(msg).ephemeral(true)).await?;
        return Ok(());
    }

    let random_sentence = {
        let mut rng = rand::thread_rng();
        sentences.choose(&mut rng).unwrap().to_string()
    };

    // Record that this sentence was spoken (increments its usage_count). This
    // keeps the least-used-first ordering meaningful so the background
    // generator and /random don't keep landing on the same sentences.
    if let Err(e) = database::insert_sentence(&ctx.data().db_pool, &random_sentence).await {
        log::error!("random: failed to record sentence usage: {}", e);
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

    let tts_result = match tts::get_or_generate_tts_with_effect(&tts_text, &actual_voice, &actual_effect).await {
        Ok(result) => result,
        Err(e) => {
            log::error!("TTS generation failed: {}", e);
            let error_msg = &ctx.data().lang.tts_error_google;
            reply.edit(ctx, poise::CreateReply::default().content(error_msg).ephemeral(true)).await?;
            return Ok(());
        }
    };
    // Surface a Google-fallback (cloned voice unavailable) instead of
    // silently playing a different voice than the user picked.
    if let Some(warn) = &tts_result.fallback_used {
        let _ = reply.edit(ctx, poise::CreateReply::default().content(warn.clone()).ephemeral(true)).await;
    }

    let mut handler = handler_lock.lock().await;
    if handler.current_channel().is_none() {
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
        return Ok(());
    }
    
    log::info!("TTS file path: {}", tts_result.file_path);
    if !tokio::fs::try_exists(&tts_result.file_path).await.unwrap_or(false) {
        log::error!("TTS file does not exist: {}", tts_result.file_path);
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.tts_error).ephemeral(true)).await?;
        return Ok(());
    }
    log::info!("Playing audio file: {}", tts_result.file_path);
    let source = songbird::input::File::new(tts_result.file_path.clone());
    play_with_volume(ctx.serenity_context(), &mut handler, source.into(), &ctx.data().volume, guild_id).await;
    log::info!("Audio playback started in guild {}", guild_id);

    let components = vec![
        serenity::CreateActionRow::Buttons(vec![
            serenity::CreateButton::new(format!("play:{}", tts_result.file_path))
                .label("Play")
                .style(serenity::ButtonStyle::Success),
            serenity::CreateButton::new("stop")
                .label("Stop")
                .style(serenity::ButtonStyle::Danger)
        ])
    ];

    // Same pattern as speak: don't propagate reply.edit errors to on_error
    // since the audio already played successfully.
    match reply.edit(ctx, poise::CreateReply::default()
        .content(ctx.data().lang.playing.replacen("{}", &tts_text, 1).replacen("{}", &tts_result.actual_voice, 1))
        .components(components)
        .ephemeral(true)
    ).await {
        Ok(_) => {}
        Err(e) => {
            let e_str = e.to_string();
            if e_str.contains("Unknown interaction") || e_str.contains("already been acknowledged") {
                log::debug!("random: reply.edit failed (interaction expired, audio already played): {}", e_str);
            } else {
                log::warn!("random: reply.edit failed: {}", e_str);
            }
        }
    }

    Ok(())
}

/// Ask the AI a question
#[poise::command(slash_command, user_cooldown = 10)]
async fn ask(
    ctx: Context<'_>,
    #[description = "La domanda da fare"] text: String,
    #[description = "La voce da usare (default: Google)"]
    #[autocomplete = "voice_autocomplete"]
    voice: Option<String>,
    #[description = "Effetto audio (default: none)"]
    #[autocomplete = "effect_autocomplete"]
    effect: Option<String>,
) -> Result<(), Error> {
    log::info!("[GUILDID : {}] ask command invoked by user {} with text: {:?}", ctx.guild_id().unwrap(), ctx.author().id, text);

    // Check if LLM is configured before doing anything else
    if !llm::is_configured() {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.ask_not_configured).ephemeral(true)).await?;
        return Ok(());
    }

    let voice = voice.unwrap_or_else(|| "Google".to_string());
    let effect = effect.unwrap_or_else(|| "none".to_string());
    let actual_voice = if voice == "random" {
        let mut rng = rand::thread_rng();
        tts::AVAILABLE_VOICES.choose(&mut rng).unwrap().to_string()
    } else {
        voice
    };

    if !tts::is_valid_voice(&actual_voice) {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.invalid_voice).ephemeral(true)).await?;
        return Ok(());
    }

    // Resolve "random" effect to a random choice
    let actual_effect = if effect == "random" {
        crate::audio_effects::random_effect().to_string()
    } else {
        effect
    };

    if !crate::audio_effects::is_valid_effect(&actual_effect) {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.invalid_effect).ephemeral(true)).await?;
        return Ok(());
    }

    if text.chars().count() > 500 {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.ask_text_too_long).ephemeral(true)).await?;
        return Ok(());
    }

    ctx.defer_ephemeral().await?;
    check_permissions(ctx).await?;

    let guild_id = ctx.guild_id().unwrap();
    let channel_id = {
        let guild = ctx.guild().ok_or(ctx.data().lang.guild_not_found.as_str())?;
        guild.voice_states.get(&ctx.author().id).and_then(|vs| vs.channel_id).ok_or(ctx.data().lang.must_be_in_voice.as_str())?
    };
    connect_bot_by_voice_client(ctx, channel_id, ctx.author().id).await?;

    // Get the bot's current nickname for the LLM system prompt
    let bot_nickname = ctx.guild()
        .and_then(|g| {
            g.members.get(&ctx.cache().current_user().id)
                .and_then(|m| m.nick.clone())
        })
        .unwrap_or_else(|| "Bot".to_string());

    // Fetch database sentences to use as personality context for the LLM
    let db_sentences = database::select_all_sentence(&ctx.data().db_pool).await.unwrap_or_default();

    // Fetch conversation history for this guild so the LLM has context
    let history: Vec<ConversationMessage> = {
        let conversations = ctx.data().conversations.lock().unwrap();
        conversations.get(&guild_id.get()).cloned().unwrap_or_default()
    };

    let queue_msg = get_queue_message(&ctx.data().lang).await;
    let initial_msg = ctx.data().lang.ask_generating
        .replacen("{}", &text, 1)
        .replacen("{}", &queue_msg, 1);
    let reply = ctx.send(poise::CreateReply::default().content(initial_msg).ephemeral(true)).await?;

    // Query the LLM with database context and conversation history
    let llm_history: Vec<llm::ConversationMessage> = history
        .iter()
        .map(|m| llm::ConversationMessage { role: m.role.clone(), content: m.content.clone() })
        .collect();
    let llm_response = match llm::ask(&text, &db_sentences, &bot_nickname, &llm_history).await {
        Ok(response) => response,
        Err(e) => {
            log::error!("[GUILDID : {}] ask - LLM failed: {}", guild_id, e);
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.ask_error).ephemeral(true)).await?;
            return Ok(());
        }
    };

    // The LLM refused the request (JSON "refused" flag or refusal boilerplate).
    // Never speak the refusal boilerplate — tell the user in chat instead and
    // stop before TTS-generation, playback, or persistence.
    if llm::is_refusal_error(&llm_response) {
        log::warn!("[GUILDID : {}] ask - LLM refused the request, not speaking it", guild_id);
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.ask_refused).ephemeral(true)).await?;
        return Ok(());
    }

    log::info!("[GUILDID : {}] ask - LLM response: {:?}", guild_id, llm_response);

    // Save the LLM response as a sentence in the database (like /speak does)
    if let Err(e) = database::insert_sentence(&ctx.data().db_pool, &llm_response).await {
        log::error!("Failed to insert LLM response into database: {}", e);
    }

    // Store the user question and LLM response in conversation history
    // so the LLM can have context of previous questions in this guild.
    // Keep only the last 20 messages to prevent unbounded memory growth.
    {
        let mut conversations = ctx.data().conversations.lock().unwrap();
        let guild_history = conversations.entry(guild_id.get()).or_insert_with(Vec::new);
        guild_history.push(ConversationMessage { role: "user".to_string(), content: text.clone() });
        guild_history.push(ConversationMessage { role: "assistant".to_string(), content: llm_response.clone() });
        // Trim to last 20 messages (10 user + 10 assistant exchanges)
        if guild_history.len() > 20 {
            let start = guild_history.len() - 20;
            guild_history.drain(0..start);
        }
    }

    // Generate TTS for the LLM response (same flow as /speak)
    let manager = match songbird::get(ctx.serenity_context()).await {
        Some(m) => m,
        None => {
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
            return Ok(());
        }
    };
    let handler_lock = match manager.get(guild_id) {
        Some(h) => h,
        None => {
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
            return Ok(());
        }
    };
    {
        let mut connected = false;
        for _ in 0..5 {
            let handler = handler_lock.lock().await;
            if handler.current_channel().is_some() {
                connected = true;
                break;
            }
            drop(handler);
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        if !connected {
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
            return Ok(());
        }
    }

    // Google TTS silently fails or truncates on text longer than ~200 chars.
    // The LLM response can be up to 500 chars, so truncate only the spoken
    // text while keeping the full response in the displayed message.
    let tts_text: String = if llm_response.chars().count() > 200 {
        let truncated: String = llm_response.chars().take(200).collect();
        format!("{truncated}...")
    } else {
        llm_response.clone()
    };

    let tts_result = match tts::get_or_generate_tts_with_effect(&tts_text, &actual_voice, &actual_effect).await {
        Ok(result) => result,
        Err(e) => {
            log::error!("ask: TTS generation failed: {}", e);
            let error_msg = &ctx.data().lang.tts_error_google;
            reply.edit(ctx, poise::CreateReply::default().content(error_msg).ephemeral(true)).await?;
            return Ok(());
        }
    };
    // Surface a Google-fallback (cloned voice unavailable) instead of
    // silently playing a different voice than the user picked.
    if let Some(warn) = &tts_result.fallback_used {
        let _ = reply.edit(ctx, poise::CreateReply::default().content(warn.clone()).ephemeral(true)).await;
    }

    let mut handler = handler_lock.lock().await;
    if handler.current_channel().is_none() {
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
        return Ok(());
    }

    log::info!("ask: TTS file path: {}", tts_result.file_path);
    if !tokio::fs::try_exists(&tts_result.file_path).await.unwrap_or(false) {
        log::error!("ask: TTS file does not exist: {}", tts_result.file_path);
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.tts_error).ephemeral(true)).await?;
        return Ok(());
    }
    log::info!("ask: Playing audio file: {}", tts_result.file_path);
    let source = songbird::input::File::new(tts_result.file_path.clone());
    play_with_volume(ctx.serenity_context(), &mut handler, source.into(), &ctx.data().volume, guild_id).await;
    log::info!("ask: Audio playback started in guild {}", guild_id);

    let components = vec![
        serenity::CreateActionRow::Buttons(vec![
            serenity::CreateButton::new(format!("play:{}", tts_result.file_path))
                .label("Play")
                .style(serenity::ButtonStyle::Success),
            serenity::CreateButton::new("stop")
                .label("Stop")
                .style(serenity::ButtonStyle::Danger)
        ])
    ];

    // Use match instead of ? so expired interaction tokens don't propagate
    // to on_error — the audio already played successfully.
    match reply.edit(ctx, poise::CreateReply::default()
        .content(ctx.data().lang.playing.replacen("{}", &llm_response, 1).replacen("{}", &tts_result.actual_voice, 1))
        .components(components)
        .ephemeral(true)
    ).await {
        Ok(_) => {}
        Err(e) => {
            let e_str = e.to_string();
            if e_str.contains("Unknown interaction") || e_str.contains("already been acknowledged") {
                log::debug!("ask: reply.edit failed (interaction expired, audio already played): {}", e_str);
            } else {
                log::warn!("ask: reply.edit failed: {}", e_str);
            }
        }
    }

    Ok(())
}

/// Translate text and speak it via TTS
#[poise::command(slash_command, user_cooldown = 10)]
async fn translate(
    ctx: Context<'_>,
    #[description = "Il testo da tradurre"] text: String,
    #[description = "La lingua di destinazione (e.g. en, it, fr, de, es)"] target_lang: String,
    #[description = "La voce da usare (default: Google)"]
    #[autocomplete = "voice_autocomplete"]
    voice: Option<String>,
    #[description = "Effetto audio (default: none)"]
    #[autocomplete = "effect_autocomplete"]
    effect: Option<String>,
) -> Result<(), Error> {
    log::info!("[GUILDID : {}] translate command invoked by user {} with text: {:?}, target_lang: {}", ctx.guild_id().unwrap(), ctx.author().id, text, target_lang);

    // Check if LLM is configured before doing anything else
    if !llm::is_configured() {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.ask_not_configured).ephemeral(true)).await?;
        return Ok(());
    }

    let voice = voice.unwrap_or_else(|| "Google".to_string());
    let effect = effect.unwrap_or_else(|| "none".to_string());
    let actual_voice = if voice == "random" {
        let mut rng = rand::thread_rng();
        tts::AVAILABLE_VOICES.choose(&mut rng).unwrap().to_string()
    } else {
        voice
    };

    if !tts::is_valid_voice(&actual_voice) {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.invalid_voice).ephemeral(true)).await?;
        return Ok(());
    }

    let actual_effect = if effect == "random" {
        crate::audio_effects::random_effect().to_string()
    } else {
        effect
    };

    if !crate::audio_effects::is_valid_effect(&actual_effect) {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.invalid_effect).ephemeral(true)).await?;
        return Ok(());
    }

    if text.chars().count() > 500 {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.ask_text_too_long).ephemeral(true)).await?;
        return Ok(());
    }

    ctx.defer_ephemeral().await?;
    check_permissions(ctx).await?;

    let guild_id = ctx.guild_id().unwrap();
    let channel_id = {
        let guild = ctx.guild().ok_or(ctx.data().lang.guild_not_found.as_str())?;
        guild.voice_states.get(&ctx.author().id).and_then(|vs| vs.channel_id).ok_or(ctx.data().lang.must_be_in_voice.as_str())?
    };
    connect_bot_by_voice_client(ctx, channel_id, ctx.author().id).await?;

    let queue_msg = get_queue_message(&ctx.data().lang).await;
    let initial_msg = ctx.data().lang.translating
        .replacen("{}", &text, 1)
        .replacen("{}", &target_lang, 1)
        .replacen("{}", &queue_msg, 1);
    let reply = ctx.send(poise::CreateReply::default().content(initial_msg).ephemeral(true)).await?;

    // Use the LLM to translate the text
    let translated = match llm::translate(&text, &target_lang).await {
        Ok(response) => response,
        Err(e) => {
            log::error!("[GUILDID : {}] translate - LLM failed: {}", guild_id, e);
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.ask_error).ephemeral(true)).await?;
            return Ok(());
        }
    };

    log::info!("[GUILDID : {}] translate - result: {:?}", guild_id, translated);

    // Save the translated text as a sentence in the database
    if let Err(e) = database::insert_sentence(&ctx.data().db_pool, &translated).await {
        log::error!("Failed to insert translated text into database: {}", e);
    }

    // Generate TTS for the translated text
    let manager = match songbird::get(ctx.serenity_context()).await {
        Some(m) => m,
        None => {
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
            return Ok(());
        }
    };
    let handler_lock = match manager.get(guild_id) {
        Some(h) => h,
        None => {
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
            return Ok(());
        }
    };
    {
        let mut connected = false;
        for _ in 0..5 {
            let handler = handler_lock.lock().await;
            if handler.current_channel().is_some() {
                connected = true;
                break;
            }
            drop(handler);
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        if !connected {
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
            return Ok(());
        }
    }

    // Google TTS silently fails or truncates on text longer than ~200 chars.
    // Truncate only the spoken text, keeping the full translation in the
    // displayed message (consistent with /random and /joke).
    let tts_text: String = if translated.chars().count() > 200 {
        let truncated: String = translated.chars().take(200).collect();
        format!("{truncated}...")
    } else {
        translated.clone()
    };

    let tts_result = match tts::get_or_generate_tts_with_effect(&tts_text, &actual_voice, &actual_effect).await {
        Ok(result) => result,
        Err(e) => {
            log::error!("translate: TTS generation failed: {}", e);
            let error_msg = &ctx.data().lang.tts_error_google;
            reply.edit(ctx, poise::CreateReply::default().content(error_msg).ephemeral(true)).await?;
            return Ok(());
        }
    };
    // Surface a Google-fallback (cloned voice unavailable) instead of
    // silently playing a different voice than the user picked.
    if let Some(warn) = &tts_result.fallback_used {
        let _ = reply.edit(ctx, poise::CreateReply::default().content(warn.clone()).ephemeral(true)).await;
    }

    let mut handler = handler_lock.lock().await;
    if handler.current_channel().is_none() {
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
        return Ok(());
    }

    if !tokio::fs::try_exists(&tts_result.file_path).await.unwrap_or(false) {
        log::error!("translate: TTS file does not exist: {}", tts_result.file_path);
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.tts_error).ephemeral(true)).await?;
        return Ok(());
    }
    log::info!("translate: Playing audio file: {}", tts_result.file_path);
    let source = songbird::input::File::new(tts_result.file_path.clone());
    play_with_volume(ctx.serenity_context(), &mut handler, source.into(), &ctx.data().volume, ctx.guild_id().unwrap()).await;

    let components = vec![
        serenity::CreateActionRow::Buttons(vec![
            serenity::CreateButton::new(format!("play:{}", tts_result.file_path))
                .label("Play")
                .style(serenity::ButtonStyle::Success),
            serenity::CreateButton::new("stop")
                .label("Stop")
                .style(serenity::ButtonStyle::Danger)
        ])
    ];

    match reply.edit(ctx, poise::CreateReply::default()
        .content(ctx.data().lang.playing.replacen("{}", &translated, 1).replacen("{}", &tts_result.actual_voice, 1))
        .components(components)
        .ephemeral(true)
    ).await {
        Ok(_) => {}
        Err(e) => {
            let e_str = e.to_string();
            if e_str.contains("Unknown interaction") || e_str.contains("already been acknowledged") {
                log::debug!("translate: reply.edit failed (interaction expired): {}", e_str);
            } else {
                log::warn!("translate: reply.edit failed: {}", e_str);
            }
        }
    }

    Ok(())
}

/// Tell a random joke fetched from JokeAPI (free, no API key needed).
#[poise::command(slash_command, user_cooldown = 10)]
async fn joke(
    ctx: Context<'_>,
    #[description = "La voce da usare (default: Google)"]
    #[autocomplete = "voice_autocomplete"]
    voice: Option<String>,
    #[description = "Effetto audio (default: none)"]
    #[autocomplete = "effect_autocomplete"]
    effect: Option<String>,
) -> Result<(), Error> {
    log::info!("[GUILDID : {}] joke command invoked by user {}", ctx.guild_id().unwrap(), ctx.author().id);

    let voice = voice.unwrap_or_else(|| "Google".to_string());
    let effect = effect.unwrap_or_else(|| "none".to_string());
    let actual_voice = if voice == "random" {
        let mut rng = rand::thread_rng();
        tts::AVAILABLE_VOICES.choose(&mut rng).unwrap().to_string()
    } else {
        voice
    };

    if !tts::is_valid_voice(&actual_voice) {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.invalid_voice).ephemeral(true)).await?;
        return Ok(());
    }

    let actual_effect = if effect == "random" {
        crate::audio_effects::random_effect().to_string()
    } else {
        effect
    };

    if !crate::audio_effects::is_valid_effect(&actual_effect) {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.invalid_effect).ephemeral(true)).await?;
        return Ok(());
    }

    ctx.defer_ephemeral().await?;
    check_permissions(ctx).await?;

    let guild_id = ctx.guild_id().unwrap();
    let channel_id = {
        let guild = ctx.guild().ok_or(ctx.data().lang.guild_not_found.as_str())?;
        guild.voice_states.get(&ctx.author().id).and_then(|vs| vs.channel_id).ok_or(ctx.data().lang.must_be_in_voice.as_str())?
    };
    connect_bot_by_voice_client(ctx, channel_id, ctx.author().id).await?;

    let reply = ctx.send(poise::CreateReply::default().content(&ctx.data().lang.processing).ephemeral(true)).await?;

    // Fetch a joke from JokeAPI (free, no API key needed). JokeAPI has no
    // Italian jokes, so fetch English ones and translate to the configured
    // language via the LLM when needed (see below).
    // Filter out nsfw, religious, political, racist, sexist, explicit categories
    let joke_url = "https://v2.jokeapi.dev/joke/Any?lang=en&safe-mode&type=twopart&format=json";
    let joke_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("Failed to build JokeAPI client: {}", e))?;
    let mut joke_text = match joke_client.get(joke_url).send().await {
        Ok(resp) => {
            if !resp.status().is_success() {
                log::error!("joke: JokeAPI returned status {}", resp.status());
                reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.joke_error).ephemeral(true)).await?;
                return Ok(());
            }
            match resp.json::<serde_json::Value>().await {
                Ok(json) => {
                    // JokeAPI returns either:
                    // {"type":"twopart","setup":"...","delivery":"..."}
                    // {"type":"single","joke":"..."}
                    if json.get("error").is_some_and(|e| e.as_bool().unwrap_or(false)) {
                        log::error!("joke: JokeAPI returned error: {:?}", json);
                        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.joke_error).ephemeral(true)).await?;
                        return Ok(());
                    }
                    let setup = json.get("setup").and_then(|s| s.as_str()).unwrap_or("");
                    let delivery = json.get("delivery").and_then(|d| d.as_str()).unwrap_or("");
                    let single = json.get("joke").and_then(|j| j.as_str()).unwrap_or("");
                    if !setup.is_empty() && !delivery.is_empty() {
                        format!("{}. {}", setup, delivery)
                    } else if !single.is_empty() {
                        single.to_string()
                    } else {
                        log::error!("joke: JokeAPI returned unexpected format: {:?}", json);
                        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.joke_error).ephemeral(true)).await?;
                        return Ok(());
                    }
                }
                Err(e) => {
                    log::error!("joke: failed to parse JokeAPI response: {}", e);
                    reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.joke_error).ephemeral(true)).await?;
                    return Ok(());
                }
            }
        }
        Err(e) => {
            log::error!("joke: failed to fetch from JokeAPI: {}", e);
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.joke_error).ephemeral(true)).await?;
            return Ok(());
        }
    };

    log::info!("[GUILDID : {}] joke: fetched joke ({} chars)", guild_id, joke_text.len());

    // JokeAPI only serves English + a few non-Italian languages. When the
    // configured language isn't English, translate the joke via the LLM so it
    // matches LANG (falls back to the English joke if no LLM is configured).
    let lang_code = std::env::var("LANG").unwrap_or_else(|_| "ita".to_string());
    if lang_code != "eng" && llm::is_configured() {
        match llm::translate(&joke_text, "it").await {
            Ok(translated) => {
                log::info!("[GUILDID : {}] joke: translated to {} ({} chars)", guild_id, lang_code, translated.len());
                joke_text = translated;
            }
            Err(e) => log::warn!("[GUILDID : {}] joke: failed to translate joke: {}", guild_id, e),
        }
    }

    // Save the joke as a sentence in the database
    if let Err(e) = database::insert_sentence(&ctx.data().db_pool, &joke_text).await {
        log::error!("Failed to insert joke into database: {}", e);
    }

    // Google TTS silently fails or truncates on text longer than ~200 chars.
    // Jokes (especially setup + delivery) can exceed this, so truncate only
    // the spoken text to a safe length while keeping the full joke in the DB.
    let tts_text: String = if joke_text.chars().count() > 200 {
        let truncated: String = joke_text.chars().take(200).collect();
        format!("{truncated}...")
    } else {
        joke_text.clone()
    };

    // Generate TTS and play it
    let manager = match songbird::get(ctx.serenity_context()).await {
        Some(m) => m,
        None => {
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
            return Ok(());
        }
    };
    let handler_lock = match manager.get(guild_id) {
        Some(h) => h,
        None => {
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
            return Ok(());
        }
    };
    {
        let mut connected = false;
        for _ in 0..5 {
            let handler = handler_lock.lock().await;
            if handler.current_channel().is_some() {
                connected = true;
                break;
            }
            drop(handler);
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        if !connected {
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
            return Ok(());
        }
    }

    let tts_result = match tts::get_or_generate_tts_with_effect(&tts_text, &actual_voice, &actual_effect).await {
        Ok(result) => result,
        Err(e) => {
            log::error!("joke: TTS generation failed: {}", e);
            let error_msg = &ctx.data().lang.tts_error_google;
            reply.edit(ctx, poise::CreateReply::default().content(error_msg).ephemeral(true)).await?;
            return Ok(());
        }
    };
    // Surface a Google-fallback (cloned voice unavailable) instead of
    // silently playing a different voice than the user picked.
    if let Some(warn) = &tts_result.fallback_used {
        let _ = reply.edit(ctx, poise::CreateReply::default().content(warn.clone()).ephemeral(true)).await;
    }

    let mut handler = handler_lock.lock().await;
    if handler.current_channel().is_none() {
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
        return Ok(());
    }

    if !tokio::fs::try_exists(&tts_result.file_path).await.unwrap_or(false) {
        log::error!("joke: TTS file does not exist: {}", tts_result.file_path);
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.tts_error).ephemeral(true)).await?;
        return Ok(());
    }
    log::info!("joke: Playing audio file: {}", tts_result.file_path);
    let source = songbird::input::File::new(tts_result.file_path.clone());
    play_with_volume(ctx.serenity_context(), &mut handler, source.into(), &ctx.data().volume, ctx.guild_id().unwrap()).await;

    let components = vec![
        serenity::CreateActionRow::Buttons(vec![
            serenity::CreateButton::new(format!("play:{}", tts_result.file_path))
                .label("Play")
                .style(serenity::ButtonStyle::Success),
            serenity::CreateButton::new("stop")
                .label("Stop")
                .style(serenity::ButtonStyle::Danger)
        ])
    ];

    match reply.edit(ctx, poise::CreateReply::default()
        .content(ctx.data().lang.playing.replacen("{}", &tts_text, 1).replacen("{}", &tts_result.actual_voice, 1))
        .components(components)
        .ephemeral(true)
    ).await {
        Ok(_) => {}
        Err(e) => {
            let e_str = e.to_string();
            if e_str.contains("Unknown interaction") || e_str.contains("already been acknowledged") {
                log::debug!("joke: reply.edit failed (interaction expired): {}", e_str);
            } else {
                log::warn!("joke: reply.edit failed: {}", e_str);
            }
        }
    }

    Ok(())
}

/// Show bot statistics: database, cache, system resources, and uptime.
#[poise::command(slash_command, user_cooldown = 10)]
async fn stats(ctx: Context<'_>) -> Result<(), Error> {
    log::info!("[GUILDID : {}] stats command invoked by user {}", ctx.guild_id().unwrap(), ctx.author().id);
    ctx.defer_ephemeral().await?;

    // Database statistics
    let db_stats = database::get_db_statistics(&ctx.data().db_pool).await.unwrap_or_else(|e| format!("Error: {}", e));

    // TTS cache size (count MP3 files in audios/ directory)
    let cache_info: String = match tokio::fs::read_dir("audios").await {
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

    // System resources — read the cached values from the background sampler
    // (sample_system_stats_loop) instead of sampling inline, so /stats doesn't
    // block ~200ms on CPU sampling for every invocation.
    let (cpu_usage, ram_usage) = SYSTEM_STATS.lock().unwrap().unwrap_or((0.0, 0.0));

    // Uptime — based on the module-level BOOT_TIME set once at startup.
    let uptime: String = {
        let boot = BOOT_TIME.get_or_init(std::time::Instant::now);
        let elapsed = boot.elapsed();
        let hours = elapsed.as_secs() / 3600;
        let mins = (elapsed.as_secs() % 3600) / 60;
        format!("{}h {}m", hours, mins)
    };

    // LLM status
    let llm_status = if llm::is_configured() {
        let endpoints = std::env::var("LLM_ENDPOINTS").unwrap_or_default();
        let count = endpoints.split(',').filter(|s| !s.trim().is_empty()).count();
        format!("{} {}", count, ctx.data().lang.endpoint_label)
    } else {
        ctx.data().lang.not_configured.clone()
    };

    let embed = serenity::CreateEmbed::new()
        .title(&ctx.data().lang.stats_title)
        .color(0x57F287)
        .field(&ctx.data().lang.stats_database, db_stats, false)
        .field(&ctx.data().lang.stats_cache, cache_info, true)
        .field(&ctx.data().lang.stats_uptime, uptime, true)
        .field(&ctx.data().lang.stats_cpu, format!("{:.1}%", cpu_usage), true)
        .field(&ctx.data().lang.stats_ram, format!("{:.1}%", ram_usage), true)
        .field(&ctx.data().lang.stats_llm, llm_status, true)
        .field(&ctx.data().lang.stats_errors, ctx.data().error_tracker.total_count().to_string(), true);

    ctx.send(poise::CreateReply::default().embed(embed).ephemeral(true)).await?;
    Ok(())
}

/// Show help for all bot commands with interactive category buttons.
#[poise::command(slash_command, user_cooldown = 5)]
async fn help(ctx: Context<'_>) -> Result<(), Error> {
    let lang = &ctx.data().lang;

    let embed = serenity::CreateEmbed::new()
        .title(&lang.help_title)
        .description(&lang.help_description)
        .color(0x5865F2);

    let components = vec![
        serenity::CreateActionRow::Buttons(vec![
            serenity::CreateButton::new("help:voice")
                .label(&lang.help_button_voice)
                .style(serenity::ButtonStyle::Primary),
            serenity::CreateButton::new("help:ai")
                .label(&lang.help_button_ai)
                .style(serenity::ButtonStyle::Primary),
            serenity::CreateButton::new("help:admin")
                .label(&lang.help_button_admin)
                .style(serenity::ButtonStyle::Secondary),
            serenity::CreateButton::new("help:all")
                .label(&lang.help_button_all)
                .style(serenity::ButtonStyle::Success),
        ])
    ];

    ctx.send(poise::CreateReply::default()
        .embed(embed)
        .components(components)
        .ephemeral(true)
    ).await?;

    Ok(())
}

/// Owner identity for voice cloning interactions. Discord users are namespaced
/// so the same voice name can exist per-user without collisions.
fn vc_owner(user_id: serenity::UserId) -> String {
    format!("discord:{}", user_id)
}

/// Create a cloned voice from an audio sample (MP3/WAV, 10-30s of speech).
#[poise::command(slash_command, user_cooldown = 10)]
async fn createvoice(
    ctx: Context<'_>,
    #[description = "Nome della voce da creare (A-Z, 0-9, _ -)"] name: String,
    #[description = "File audio con 10-30 secondi di voce pulita (MP3 o WAV)"]
    sample: serenity::Attachment,
) -> Result<(), Error> {
    log::info!("[GUILDID : {}] createvoice invoked by user {} name={}", ctx.guild_id().unwrap(), ctx.author().id, name);
    let lang = &ctx.data().lang;

    if !tts::voiceclone_configured() {
        ctx.send(poise::CreateReply::default().content(&lang.vc_not_configured).ephemeral(true)).await?;
        return Ok(());
    }
    if !tts::is_valid_clone_name(name.trim()) {
        ctx.send(poise::CreateReply::default().content(&lang.vc_invalid_name).ephemeral(true)).await?;
        return Ok(());
    }

    ctx.defer_ephemeral().await?;
    let size = sample.size;
    if size < 4_000 || size > 12_000_000 {
        ctx.send(poise::CreateReply::default().content(&lang.vc_sample_invalid).ephemeral(true)).await?;
        return Ok(());
    }
    match sample.download().await {
        Ok(bytes) => {
            use base64::Engine as _;
            let audio_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            match tts::create_cloned_voice(name.trim(), &vc_owner(ctx.author().id), &audio_b64).await {
                Ok(()) => {
                    let msg = lang.vc_created.replacen("{}", name.trim(), 1).replacen("{}", name.trim(), 1);
                    ctx.send(poise::CreateReply::default().content(msg).ephemeral(true)).await?;
                }
                Err(e) => {
                    let msg = if e.contains("already exists") {
                        lang.vc_exists.replacen("{}", name.trim(), 1)
                    } else if e.contains("could not decode") || e.contains("too short") || e.contains("between 4KB") {
                        lang.vc_sample_invalid.clone()
                    } else if e.contains("invalid voice name") {
                        lang.vc_invalid_name.clone()
                    } else {
                        lang.vc_error.replacen("{}", &e, 1)
                    };
                    ctx.send(poise::CreateReply::default().content(msg).ephemeral(true)).await?;
                }
            }
        }
        Err(e) => {
            log::error!("createvoice: sample download failed: {}", e);
            ctx.send(poise::CreateReply::default().content(&lang.vc_sample_invalid).ephemeral(true)).await?;
        }
    }
    Ok(())
}

/// List all cloned voices registered on the voiceclone sidecar.
#[poise::command(slash_command, user_cooldown = 5)]
async fn myvoices(ctx: Context<'_>) -> Result<(), Error> {
    log::info!("[GUILDID : {}] myvoices invoked by user {}", ctx.guild_id().unwrap(), ctx.author().id);
    let lang = &ctx.data().lang;
    if !tts::voiceclone_configured() {
        ctx.send(poise::CreateReply::default().content(&lang.vc_not_configured).ephemeral(true)).await?;
        return Ok(());
    }
    let voices = tts::list_cloned_voices().await.unwrap_or_default();
    if voices.is_empty() {
        ctx.send(poise::CreateReply::default().content(&lang.vc_list_empty).ephemeral(true)).await?;
        return Ok(());
    }
    let me = vc_owner(ctx.author().id);
    let lines: Vec<String> = voices
        .iter()
        .map(|v| {
            let badge = if v.owner == me { "🟢" } else { "⚪" };
            format!("{} **{}** — `--voice {}`", badge, v.name, v.name)
        })
        .collect();
    let body = format!(
        "{}\n🟢 = tua (your own) / ⚪ = other users\n{}",
        lines.join("\n"),
        if lines.len() > 25 { "" } else { "" }
    );
    ctx.send(poise::CreateReply::default().content(body).ephemeral(true)).await?;
    Ok(())
}

/// Delete a previously cloned voice.
#[poise::command(slash_command, user_cooldown = 5)]
async fn deletevoice(
    ctx: Context<'_>,
    #[description = "Nome della voce da eliminare"] name: String,
) -> Result<(), Error> {
    log::info!("[GUILDID : {}] deletevoice invoked by user {} name={}", ctx.guild_id().unwrap(), ctx.author().id, name);
    let lang = &ctx.data().lang;
    if !tts::voiceclone_configured() {
        ctx.send(poise::CreateReply::default().content(&lang.vc_not_configured).ephemeral(true)).await?;
        return Ok(());
    }
    let name = name.trim();
    // Verify the voice exists and belongs to the requester (admins may delete
    // anything; regular users only their own voices).
    let admin_id = env::var("ADMIN_ID").unwrap_or_default();
    let is_admin = ctx.author().id.to_string() == admin_id;
    let voices = tts::list_cloned_voices().await.unwrap_or_default();
    match voices.iter().find(|v| v.name == name) {
        None => {
            let msg = lang.vc_not_found.replacen("{}", name, 1);
            ctx.send(poise::CreateReply::default().content(msg).ephemeral(true)).await?;
        }
        Some(v) => {
            if !is_admin && v.owner != vc_owner(ctx.author().id) {
                ctx.send(poise::CreateReply::default().content(&lang.vc_owner_mismatch).ephemeral(true)).await?;
                return Ok(());
            }
            match tts::delete_cloned_voice(name, &v.owner).await {
                Ok(()) => {
                    let msg = lang.vc_deleted.replacen("{}", name, 1);
                    ctx.send(poise::CreateReply::default().content(msg).ephemeral(true)).await?;
                }
                Err(e) => {
                    let msg = lang.vc_error.replacen("{}", &e, 1);
                    ctx.send(poise::CreateReply::default().content(msg).ephemeral(true)).await?;
                }
            }
        }
    }
    Ok(())
}

/// Hidden live voice-clone command, visible only to the server owner.
///
/// Records the target user (who must be in the bot's voice channel) via
/// songbird's voice-receive until enough speech is captured, then forwards the
/// sample to the voiceclone sidecar. Overwrites an existing voice of the same
/// name. Hidden from command listing (`hide_in_help`) but still a real slash
/// command.
#[poise::command(slash_command, owners_only, user_cooldown = 10, hide_in_help)]
async fn clone(
    ctx: Context<'_>,
    #[description = "L'utente da registrare"] user: serenity::UserId,
    #[description = "Nome della voce da salvare (sovrascrive se esiste)"] name: String,
) -> Result<(), Error> {
    log::info!("[GUILDID : {}] clone (live) invoked by user {} target={} name={}", ctx.guild_id().unwrap(), ctx.author().id, user, name);
    let lang = &ctx.data().lang;

    if !tts::voiceclone_configured() {
        ctx.send(poise::CreateReply::default().content(&lang.vc_not_configured).ephemeral(true)).await?;
        return Ok(());
    }

    // Gate hard to the server owner: the bot's ADMIN_ID (the conventional
    // "server owner" for this bot's admin commands) or the actual Discord
    // guild owner. This is a consent-sensitive feature — never allow anyone
    // else. poise's owners_only attr additionally restricts to the
    // application owner.
    let admin_id = env::var("ADMIN_ID").unwrap_or_default();
    // The CacheRef from ctx.guild() is !Send — extract what we need in this
    // scope and drop the guard before further awaits.
    let (is_owner, target_channel): (bool, Option<u64>) = {
        let guild = ctx.guild().ok_or(ctx.data().lang.guild_not_found.as_str())?;
        let tc = guild.voice_states.get(&user).and_then(|vs| vs.channel_id).map(|c| c.get());
        let owner = ctx.author().id.to_string() == admin_id || ctx.author().id == guild.owner_id;
        (owner, tc)
    };
    if !is_owner {
        ctx.send(poise::CreateReply::default().content(&lang.vc_clone_not_owner).ephemeral(true)).await?;
        return Ok(());
    }

    let name = name.trim().to_string();
    if !tts::is_valid_clone_name(&name) {
        ctx.send(poise::CreateReply::default().content(&lang.vc_invalid_name).ephemeral(true)).await?;
        return Ok(());
    }

    // The bot must be connected to a voice channel.
    let guild_id = ctx.guild_id().unwrap();
    let manager = songbird::get(ctx.serenity_context()).await.ok_or("Songbird not registered")?;
    let Some(handler_lock) = manager.get(guild_id) else {
        ctx.send(poise::CreateReply::default().content(&lang.vc_clone_no_voice).ephemeral(true)).await?;
        return Ok(());
    };

    // The target must be in the SAME channel the bot is connected to.
    let bot_channel: Option<u64> = handler_lock
        .try_lock()
        .ok()
        .and_then(|h| h.current_channel())
        .map(|c| c.0.get());
    match (bot_channel, target_channel) {
        (Some(bc), Some(tc)) if bc == tc => {}
        _ => {
            ctx.send(poise::CreateReply::default().content(&lang.vc_clone_user_not_in_channel).ephemeral(true)).await?;
            return Ok(());
        }
    }

    ctx.defer_ephemeral().await?;

    // Register capture handlers on the driver. Voice receive requires the
    // decode config so ticks deliver PCM (not just packets).
    let reply_message: Option<serenity::Message>;
    let sink: std::sync::Arc<voice_capture::RecordSink>;
    {
        let mut handler = handler_lock.lock().await;
        let cfg = {
            let mut c = handler.config().clone();
            c.decode_mode = songbird::driver::DecodeMode::Decode(songbird::driver::DecodeConfig::default());
            c
        };
        handler.set_config(cfg);

        let ssrc_map: voice_capture::SsrcMap = Default::default();
        sink = std::sync::Arc::new(voice_capture::RecordSink {
            target_user: user.to_string(),
            samples: std::sync::Mutex::new(Vec::new()),
            done: std::sync::atomic::AtomicBool::new(false),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(voice_capture::MAX_RECORD_SECS),
        });

        handler.add_global_event(
            songbird::events::Event::Core(songbird::events::CoreEvent::SpeakingStateUpdate),
            voice_capture::SsrcTracker { map: ssrc_map.clone() },
        );
        handler.add_global_event(
            songbird::events::Event::Core(songbird::events::CoreEvent::VoiceTick),
            voice_capture::CaptureHandler { target_user: user.get(), sink: sink.clone(), ssrc_map },
        );

        // Notify the invoker and let the background recorder finish.
        let display_name = user.mention().to_string();
        let msg = lang.vc_clone_recording
            .replacen("{}", &display_name, 1)
            .replacen("{}", &format!("{}", voice_capture::TARGET_SPEECH_SECS as u32), 1);
        let reply = ctx.send(poise::CreateReply::default().content(msg).ephemeral(true)).await?;
        // Grab the underlying message so the detached recorder task can edit
        // it with plain serenity (poise's ReplyHandle borrows the interaction).
        reply_message = match reply.message().await {
            Ok(m) => Some(m.into_owned()),
            Err(_) => None,
        };

    }
    // Spawn the recorder AFTER the Call guard is dropped (handler moved into
    // the task while nothing borrows it).
    let owner = vc_owner(ctx.author().id);
    let ctx_handle = ctx.serenity_context().clone();
    let name_clone = name.clone();
    let target_display = user.mention().to_string();
    tokio::spawn(async move {
        voice_capture_finish(
            ctx_handle,
            reply_message,
            handler_lock,
            sink,
            name_clone,
            owner,
            guild_id,
            target_display,
        )
        .await;
    });
    Ok(())
}

/// Background recorder loop: waits until the sink has enough speech (or the
/// deadline hits), then encodes + uploads the sample to the voiceclone
/// sidecar, reporting progress to the invoker's ephemeral reply.
async fn voice_capture_finish(
    ctx: serenity::Context,
    reply_msg: Option<serenity::Message>,
    handler_lock: std::sync::Arc<tokio::sync::Mutex<songbird::Call>>,
    sink: std::sync::Arc<voice_capture::RecordSink>,
    voice_name: String,
    owner: String,
    guild_id: serenity::GuildId,
    target_display: String,
) {
    let target = voice_capture::TARGET_SPEECH_SECS;
    // The finish task only needs the vc_clone_* strings; rebuild Lang from the
    // process-wide LANG env (same as Lang::new()).
    let lang = lang::Lang::new();
    log::info!("clone: recorder started for '{}' (target {:.0}s speech, max {}s wall)", voice_name, target, voice_capture::MAX_RECORD_SECS);

    let mut last_reported = 0.0f32;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;

        if sink.done.load(std::sync::atomic::Ordering::Relaxed)
            || std::time::Instant::now() >= sink.deadline
        {
            break;
        }
        let captured = sink.speech_secs();
        // Log the first sign of capture so "nothing happens" failures are
        // diagnosable: if this never fires, no decoded PCM is reaching the
        // sink (SSRC mapping or decode-mode problem).
        if captured > 0.0 && last_reported == 0.0 {
            log::info!("clone: first audio captured for '{}' ({:.1}s so far)", voice_name, captured);
        }
        if captured >= target {
            log::info!("clone: speech target reached for '{}' ({:.1}s), stopping capture", voice_name, captured);
            break;
        }
        if captured - last_reported >= 10.0 {
            last_reported = captured;
            log::info!("clone: progress for '{}': {:.0}s / {:.0}s", voice_name, captured, target);
            // vc_clone_progress placeholders, in order: {:.0} captured secs,
            // {} target secs, {} display name.
            let msg = lang.vc_clone_progress
                .replacen("{:.0}", &format!("{:.0}", captured), 1)
                .replacen("{}", &format!("{}", target as u32), 1)
                .replacen("{}", &target_display, 1);
            // Ephemeral interaction responses can't be re-edited after the
            // token window; edit the original reply message instead (visible
            // as a normal message — acceptable for recorder progress).
            if let Some(mut m) = reply_msg.clone() {
                let _ = m
                    .edit(&ctx, serenity::EditMessage::new().content(msg))
                    .await;
            }
        }
    }

    // Stop capturing whatever happened.
    sink.done.store(true, std::sync::atomic::Ordering::Relaxed);
    {
        let mut handler = handler_lock.lock().await;
        handler.remove_all_global_events();
        // Restore the cheaper default decode mode.
        let mut cfg = handler.config().clone();
        cfg.decode_mode = songbird::driver::DecodeMode::Decrypt;
        handler.set_config(cfg);
    }

    let samples = sink.samples.lock().unwrap().clone();
    log::info!(
        "clone: capture finished for '{}': {:.1}s of audio ({} samples)",
        voice_name,
        samples.len() as f32 / 48000.0,
        samples.len()
    );
    if !voice_capture::has_speech(&samples) {
        log::warn!("clone: has_speech gate REJECTED the sample for '{}' (<5s loud audio captured — was the target actually speaking?)", voice_name);
        let msg = lang.vc_clone_no_speech.clone();
        edit_or_post(&ctx, &reply_msg, guild_id, msg).await;
        return;
    }

    let captured = samples.len() as f32 / 48000.0;
    let msg = lang
        .vc_clone_done
        .replacen("{:.1}", &format!("{:.1}", captured), 1)
        .replacen("{}", &voice_name, 1);
    edit_or_post(&ctx, &reply_msg, guild_id, msg).await;

    // 48kHz ticks → 24kHz MP3 for the sidecar.
    let mp3 = match voice_capture::encode_samples_to_mp3(&samples, 48000) {
        Ok(m) => m,
        Err(e) => {
            log::error!("clone: mp3 encode failed: {}", e);
            let msg = lang.vc_clone_failed.replacen("{}", &e, 1);
            edit_or_post(&ctx, &reply_msg, guild_id, msg).await;
            return;
        }
    };
    log::info!("clone: sample encoded for '{}' ({} bytes mp3), uploading to voiceclone sidecar", voice_name, mp3.len());

    use base64::Engine as _;
    let audio_b64 = base64::engine::general_purpose::STANDARD.encode(&mp3);
    // Overwrite semantics: the create API replaces an existing same-name voice.
    match tts::create_cloned_voice(&voice_name, &owner, &audio_b64).await {
        Ok(()) => {
            log::info!("clone: live recording cloned into voice '{}' (guild {})", voice_name, guild_id);
            let msg = lang.vc_created
                .replacen("{}", &voice_name, 1)
                .replacen("{}", &voice_name, 1);
            edit_or_post(&ctx, &reply_msg, guild_id, msg).await;
        }
        Err(e) => {
            log::error!("clone: sidecar create failed: {}", e);
            let msg = lang.vc_clone_failed.replacen("{}", &e, 1);
            edit_or_post(&ctx, &reply_msg, guild_id, msg).await;
        }
    }
}

/// Edit the original reply message if available; otherwise post to the
/// command channel. Used from the detached recorder task (poise's
/// ReplyHandle borrows the interaction and can't be moved into 'static).
/// The channel fallback matters: the ephemeral reply's message fetch can fail
/// (interaction token expiry), and silently dropping progress/results made
/// /clone look like it "did nothing".
async fn edit_or_post(
    ctx: &serenity::Context,
    reply_msg: &Option<serenity::Message>,
    guild_id: serenity::GuildId,
    msg: String,
) {
    if let Some(mut m) = reply_msg.clone() {
        if m.edit(ctx, serenity::EditMessage::new().content(&msg)).await.is_ok() {
            return;
        }
        log::warn!("clone: failed to edit the original reply message; falling back to a channel message");
    }
    // Resolve the channel id synchronously and drop the cache guard before
    // any await: CacheRef borrows cache internals and is !Send, which would
    // poison the whole recorder future for tokio::spawn.
    let text_channel_id = ctx.cache.guild(guild_id).and_then(|guild| {
        guild
            .channels
            .iter()
            .find(|(_, c)| matches!(c.kind, serenity::ChannelType::Text))
            .map(|(id, _)| *id)
    });
    if let Some(id) = text_channel_id {
        let _ = id.say(ctx, &msg).await;
        return;
    }
    log::error!("clone: could not deliver message anywhere (no reply message and no text channel found): {}", msg);
}

/// Set the bot's playback volume (0-100)
#[poise::command(slash_command, user_cooldown = 5)]
async fn volume(
    ctx: Context<'_>,
    #[description = "Volume level (0-100, default: 100)"] level: Option<i64>,
) -> Result<(), Error> {
    log::info!("[GUILDID : {}] volume command invoked by user {}", ctx.guild_id().unwrap(), ctx.author().id);
    check_speak_permission(ctx).await?;
    let guild_id = ctx.guild_id().unwrap();
    let level = level.unwrap_or(100).clamp(0, 100);

    let manager = songbird::get(ctx.serenity_context()).await
        .ok_or("Songbird not registered")?;
    let handler_lock = match manager.get(guild_id) {
        Some(h) => h,
        None => {
            ctx.send(poise::CreateReply::default().content(&ctx.data().lang.not_connected).ephemeral(true)).await?;
            return Ok(());
        }
    };
    let handler = handler_lock.lock().await;

    // Songbird 0.6: set volume on the current playing track, and store it
    // in Data so future tracks inherit the same volume level.
    // Volume is 0.0-1.0, Discord users think in 0-100.
    let volume = level as f32 / 100.0;
    if let Some(track) = handler.queue().current() {
        let _ = track.set_volume(volume);
    }
    // Persist volume for future tracks
    {
        let mut vol_lock = ctx.data().volume.lock().unwrap();
        *vol_lock = volume;
    }
    log::info!("[GUILDID : {}] volume set to {}% ({})", guild_id, level, volume);

    let msg = ctx.data().lang.volume_set.replacen("{}", &level.to_string(), 1);
    ctx.send(poise::CreateReply::default().content(msg).ephemeral(true)).await?;
    Ok(())
}

/// Audio playback from the input audio
#[poise::command(slash_command, user_cooldown = 5)]
async fn audio(
    ctx: Context<'_>,
    #[description = "Il file audio (mp3, wav, ogg, m4a, flac)"] audio: serenity::Attachment,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    log::info!("[GUILDID : {}] audio command invoked by user {} with filename: {}", ctx.guild_id().unwrap(), ctx.author().id, audio.filename);

    // Create the reply early so all subsequent messages edit the deferred response
    // instead of creating followup messages (cleaner UX, avoids "already
    // acknowledged" errors if the on_error handler also tries to send).
    let reply = ctx.send(poise::CreateReply::default().content(&ctx.data().lang.processing).ephemeral(true)).await?;

    check_permissions(ctx).await?;

    let allowed_extensions = ["mp3", "wav", "ogg", "m4a", "flac"];
    let ext = audio.filename.split('.').next_back().unwrap_or("").to_lowercase();
    if !allowed_extensions.contains(&ext.as_str()) {
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.invalid_extension).ephemeral(true)).await?;
        return Ok(());
    }

    // Reject oversized attachments before downloading to prevent memory/disk
    // exhaustion. The limit is configurable via MAX_AUDIO_FILE_SIZE_MB (default 25).
    let max_size_mb: u64 = env::var("MAX_AUDIO_FILE_SIZE_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(25);
    let max_size_bytes = max_size_mb * 1024 * 1024;
    if u64::from(audio.size) > max_size_bytes {
        log::warn!("[GUILDID : {}] audio - file too large: {} bytes (limit {})", ctx.guild_id().unwrap(), audio.size, max_size_bytes);
        let msg = ctx.data().lang.file_too_large.replacen("{}", &max_size_mb.to_string(), 1);
        reply.edit(ctx, poise::CreateReply::default().content(&msg).ephemeral(true)).await?;
        return Ok(());
    }

    let guild_id = ctx.guild_id().unwrap();
    let channel_id = {
        let guild = ctx.guild().ok_or(ctx.data().lang.guild_not_found.as_str())?;
        guild.voice_states.get(&ctx.author().id).and_then(|vs| vs.channel_id).ok_or(ctx.data().lang.must_be_in_voice.as_str())?
    };

    // Smart voice connection: don't interrupt playback if bot is already playing
    connect_bot_by_voice_client(ctx, channel_id, ctx.author().id).await?;

    let manager = match songbird::get(ctx.serenity_context()).await {
        Some(m) => m,
        None => {
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
            return Ok(());
        }
    };
    let handler_lock = match manager.get(guild_id) {
        Some(h) => h,
        None => {
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
            return Ok(());
        }
    };
    {
        // Wait for connection to establish with retry (same as speak/random)
        let mut connected = false;
        for _ in 0..5 {
            let handler = handler_lock.lock().await;
            if handler.current_channel().is_some() {
                connected = true;
                break;
            }
            drop(handler);
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        if !connected {
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
            return Ok(());
        }
    }

    log::info!("[GUILDID : {}] audio - filename: {}", guild_id, audio.filename);

    // Compute queue metrics once and reuse for both the initial and final message
    // so the user sees consistent values (sysinfo CPU/RAM can fluctuate between calls).
    let queue_status = get_queue_message(&ctx.data().lang).await;

    let temp_dir = std::env::var("TMP_DIR").unwrap_or_else(|_| "/tmp/discord-llm-bot".to_string());
    let safe_filename = std::path::Path::new(&audio.filename)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("audio.mp3");
    // Prefix with a UUID to prevent concurrent uploads with the same filename
    // from overwriting each other's temp file while playback is in progress.
    let file_path = format!("{}/{}_{}", temp_dir, uuid::Uuid::new_v4(), safe_filename);
    
    // Download the attachment with proper error handling. Use a bounded
    // timeout so a slow Discord CDN doesn't stall the command indefinitely.
    let download_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("Failed to build download client: {}", e))?;
    let bytes = match download_client.get(&audio.url).send().await {
        Ok(resp) => match resp.bytes().await {
            Ok(b) => b.to_vec(),
            Err(e) => {
                log::error!("[GUILDID : {}] audio - failed to read attachment bytes: {}", guild_id, e);
                reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.audio_download_failed).ephemeral(true)).await?;
                return Ok(());
            }
        },
        Err(e) => {
            log::error!("[GUILDID : {}] audio - failed to download attachment: {}", guild_id, e);
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.audio_download_failed).ephemeral(true)).await?;
            return Ok(());
        }
    };

    if bytes.is_empty() {
        log::warn!("[GUILDID : {}] audio - downloaded attachment is empty", guild_id);
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.audio_download_failed).ephemeral(true)).await?;
        return Ok(());
    }

    tokio::fs::create_dir_all(&temp_dir).await?;
    tokio::fs::write(&file_path, &bytes).await?;

    // Play audio using Songbird's built-in FFmpeg decoder
    if let Err(e) = play_audio_with_ffmpeg_pipe(&ctx, &file_path, "Custom Audio").await {
        log::error!("[GUILDID : {}] audio - playback failed: {}", guild_id, e);
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
        return Ok(());
    }

    log::info!("Audio playback started in guild {}", guild_id);

    let components = vec![
        serenity::CreateActionRow::Buttons(vec![
            serenity::CreateButton::new(format!("play:{}", file_path))
                .label("Play")
                .style(serenity::ButtonStyle::Success),
            serenity::CreateButton::new("stop")
                .label("Stop")
                .style(serenity::ButtonStyle::Danger)
        ])
    ];

    // Edit the reply with Play/Stop buttons. Use match instead of ? so
    // that an expired interaction token doesn't propagate to on_error
    // as "Unknown interaction" — the audio already played successfully.
    let response_content = format!("{}{}", ctx.data().lang.audio_playback, queue_status);

    match reply.edit(ctx, poise::CreateReply::default()
        .content(&response_content)
        .components(components)
        .ephemeral(true)
    ).await {
        Ok(_) => {}
        Err(e) => {
            let e_str = e.to_string();
            if e_str.contains("Unknown interaction") || e_str.contains("already been acknowledged") {
                log::debug!("audio: reply.edit failed (interaction expired, audio already played): {}", e_str);
            } else {
                log::warn!("audio: reply.edit failed: {}", e_str);
            }
        }
    }

    // Clean up temp file after 10 minutes to ensure playback completes.
    // Audio uploads can be up to 25 MB (potentially 30+ minutes of audio),
    // so the 5-minute cleanup used for TTS temp files is too short here.
    let file_path_clone = file_path.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(600)).await;
        let _ = tokio::fs::remove_file(&file_path_clone).await;
    });

    Ok(())
}

/// Build the embed + button row for the current page of a soundboard session.
/// Shared by the /soundboard command and the pagination component handler.
fn soundboard_view(
    session: &soundboard::SoundboardSession,
    session_id: &str,
) -> (serenity::CreateEmbed, Vec<serenity::CreateActionRow>) {
    let total = session.items.len();
    let total_pages = total.div_ceil(soundboard::PAGE_SIZE).max(1);
    let page = session.page.min(total_pages - 1);
    let start = page * soundboard::PAGE_SIZE;
    let end = (start + soundboard::PAGE_SIZE).min(total);

    let mut desc = String::new();
    for (i, item) in session.items[start..end].iter().enumerate() {
        desc.push_str(&format!("**{}.** {}\n", i + 1, item.title));
    }
    desc.push_str(&format!(
        "\n*Risultati {}–{} di {} — pagina {}/{}*",
        start + 1,
        end,
        total,
        page + 1,
        total_pages
    ));

    let effect_label = if session.effect == "none" {
        "nessuno".to_string()
    } else {
        session.effect.clone()
    };
    desc.push_str(&format!("\n🎛️ Effetto: **{}**", effect_label));

    let embed = serenity::CreateEmbed::new()
        .title(format!("🔊 Soundboard: {}", session.query))
        .description(desc)
        .color(0x5865F2);

    // Sound buttons (one per result on this page).
    let mut sound_buttons = Vec::new();
    for i in start..end {
        let label = if session.items[i].title.chars().count() > 80 {
            let s: String = session.items[i].title.chars().take(80).collect();
            format!("{}…", s)
        } else {
            session.items[i].title.clone()
        };
        sound_buttons.push(
            serenity::CreateButton::new(format!("sb:play:{}:{}", session_id, i))
                .label(label)
                .style(serenity::ButtonStyle::Primary),
        );
    }

    // Navigation row (prev / page indicator / next / close).
    let prev = serenity::CreateButton::new(format!("sb:prev:{}", session_id))
        .label("◀ Prev")
        .style(serenity::ButtonStyle::Secondary)
        .disabled(page == 0);
    let next = serenity::CreateButton::new(format!("sb:next:{}", session_id))
        .label("Next ▶")
        .style(serenity::ButtonStyle::Secondary)
        .disabled(page + 1 >= total_pages);
    let close = serenity::CreateButton::new(format!("sb:close:{}", session_id))
        .label("✖ Chiudi")
        .style(serenity::ButtonStyle::Danger);

    let rows = vec![
        serenity::CreateActionRow::Buttons(sound_buttons),
        serenity::CreateActionRow::Buttons(vec![prev, next, close]),
    ];

    (embed, rows)
}

/// Soundboard: search MyInstants sounds and play one in voice.
#[poise::command(slash_command, user_cooldown = 5)]
async fn soundboard(
    ctx: Context<'_>,
    #[description = "Cosa cercare su MyInstants (obbligatorio)"] search: String,
    #[description = "Effetto audio (opzionale, default: nessuno)"]
    #[autocomplete = "effect_autocomplete"]
    effect: Option<String>,
) -> Result<(), Error> {
    log::info!(
        "[GUILDID : {}] soundboard command invoked by user {} with search: {:?}, effect: {:?}",
        ctx.guild_id().unwrap(),
        ctx.author().id,
        search,
        effect
    );

    if search.trim().is_empty() {
        ctx.send(poise::CreateReply::default().content("Devi inserire una ricerca per usare la soundboard.").ephemeral(true)).await?;
        return Ok(());
    }

    let effect = effect.unwrap_or_else(|| "none".to_string());
    let actual_effect = if effect == "random" {
        crate::audio_effects::random_effect().to_string()
    } else {
        effect
    };
    if !crate::audio_effects::is_valid_effect(&actual_effect) {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.invalid_effect).ephemeral(true)).await?;
        return Ok(());
    }

    ctx.defer_ephemeral().await?;

    let guild_id = ctx.guild_id().unwrap().get();

    // Search MyInstants and build a session.
    let items = match soundboard::search(search.trim()).await {
        Ok(items) => items,
        Err(e) => {
            ctx.send(poise::CreateReply::default().content(&format!("⚠️ {}", e)).ephemeral(true)).await?;
            return Ok(());
        }
    };

    let session_id = format!("{}", uuid::Uuid::new_v4().simple());
    let session = soundboard::SoundboardSession {
        query: search.trim().to_string(),
        items,
        page: 0,
        effect: actual_effect,
        guild_id,
        created_at: std::time::Instant::now(),
    };
    {
        let mut sessions = ctx.data().soundboard_sessions.lock().unwrap();
        let now = std::time::Instant::now();
        // Evict stale sessions (user never interacted), then if we're still at
        // capacity evict the oldest so memory stays bounded.
        sessions.retain(|_, s| now.duration_since(s.created_at) < soundboard::SESSION_TTL);
        if sessions.len() >= soundboard::MAX_SESSIONS {
            if let Some(oldest) = sessions.iter().min_by_key(|(_, s)| s.created_at).map(|(k, _)| k.clone()) {
                sessions.remove(&oldest);
            }
        }
        sessions.insert(session_id.clone(), session);
    }

    let (embed, rows) = {
        let sessions = ctx.data().soundboard_sessions.lock().unwrap();
        let session = sessions.get(&session_id).unwrap();
        soundboard_view(session, &session_id)
    };

    ctx.send(poise::CreateReply::default().embed(embed).components(rows).ephemeral(true)).await?;
    Ok(())
}

/// Download a MyInstants sound and play it in the user's voice channel, with
/// an optional ffmpeg effect applied. Returns a user-facing message on success
/// or an error string on failure.
async fn play_soundboard_item(
    ctx: &serenity::Context,
    data: &Data,
    guild_id: serenity::GuildId,
    user_id: serenity::UserId,
    url: &str,
    effect: &str,
) -> Result<String, String> {
    // Resolve the user's current voice channel.
    let channel_id = ctx
        .cache
        .guild(guild_id)
        .and_then(|g| g.voice_states.get(&user_id).and_then(|vs| vs.channel_id))
        .ok_or("Devi essere in un canale vocale".to_string())?;

    // Verify the bot has connect + speak permission in the target channel so a
    // permission failure gives a clear message instead of a silent failed join.
    let has_perms = match channel_id.to_channel(ctx).await {
        Ok(serenity::Channel::Guild(gc)) => {
            #[allow(deprecated)]
            gc.permissions_for_user(&ctx.cache, ctx.cache.current_user().id)
                .map(|p| p.connect() && p.speak())
                .unwrap_or(false)
        }
        _ => false,
    };
    if !has_perms {
        return Err("Il bot non ha il permesso di connettersi/parlare in quel canale.".to_string());
    }

    // Connect the bot: join if disconnected, or switch if alone (never abandon
    // other members). Mirror the smart behaviour of connect_bot_by_voice_client.
    let manager = songbird::get(ctx)
        .await
        .ok_or("Songbird not registered".to_string())?;
    let bot_channel = {
        let mut current = None;
        if let Some(handler) = manager.get(guild_id) {
            let h = handler.lock().await;
            current = h.current_channel().map(|c| serenity::ChannelId::new(c.0.get()));
        }
        current
    };
    match bot_channel {
        Some(c) if c == channel_id => {}
        Some(c) => {
            let humans = ctx
                .cache
                .guild(guild_id)
                .map(|g| {
                    g.voice_states
                        .values()
                        .filter(|vs| vs.channel_id == Some(c) && vs.user_id != ctx.cache.current_user().id)
                        .count()
                })
                .unwrap_or(0);
            if humans > 0 {
                return Err("The bot is busy with other people in a voice channel. Use /join or wait.".to_string());
            }
            // Switch channels.
            if let Some(handler) = manager.get(guild_id) {
                let mut h = handler.lock().await;
                let _ = h.leave().await;
                drop(h);
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            let _ = manager.join(guild_id, channel_id).await;
        }
        None => {
            let _ = manager.join(guild_id, channel_id).await;
        }
    }

    // Wait for the voice connection to establish.
    let mut connected = false;
    if let Some(handler) = manager.get(guild_id) {
        for _ in 0..5 {
            let h = handler.lock().await;
            if h.current_channel().is_some() {
                connected = true;
                break;
            }
            drop(h);
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
    if !connected {
        return Err("Could not connect to the voice channel. Try again.".to_string());
    }

    // The plain (no-effect) audio is cached in a dedicated TMP_DIR/soundboard
    // folder so frequently-played sounds replay instantly instead of being
    // re-downloaded from MyInstants each time. Effects are applied on-the-fly
    // to a temp file and never cached.
    let temp_dir = std::env::var("TMP_DIR").unwrap_or_else(|_| "/tmp/discord-llm-bot".to_string());
    let cache_dir = format!("{}/soundboard", temp_dir);
    tokio::fs::create_dir_all(&cache_dir)
        .await
        .map_err(|e| format!("Failed to create soundboard cache dir: {}", e))?;
    let cache_path = format!("{}/{}.mp3", cache_dir, format!("{:x}", md5::compute(url.as_bytes())));

    // Ensure the plain audio is cached (download only on a cache miss).
    if !tokio::fs::try_exists(&cache_path).await.unwrap_or(false) {
        let bytes = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|e| format!("Failed to build download client: {}", e))?
            .get(url)
            .send()
            .await
            .map_err(|e| format!("Failed to download sound: {}", e))?
            .bytes()
            .await
            .map_err(|e| format!("Failed to read sound: {}", e))?
            .to_vec();
        if bytes.is_empty() {
            return Err("The sound file was empty.".to_string());
        }
        tokio::fs::write(&cache_path, &bytes)
            .await
            .map_err(|e| format!("Failed to write sound: {}", e))?;
        // Keep the cache bounded by removing the oldest files if it grows too large.
        cap_soundboard_cache(&cache_dir, 150).await;
    }

    // Determine the file to play: the cached plain file directly when no effect
    // is requested, otherwise an effect-processed temp file.
    let (play_path, is_temp) = if effect == "none" {
        (cache_path.clone(), false)
    } else {
        let temp_path = format!(
            "{}/sb_eff_{}_{}.mp3",
            temp_dir,
            uuid::Uuid::new_v4().simple(),
            format!("{:x}", md5::compute(url.as_bytes()))
        );
        let bytes = tokio::fs::read(&cache_path)
            .await
            .map_err(|e| format!("Failed to read cached sound: {}", e))?;
        crate::audio_effects::compress_and_save_mp3_with_effect(bytes, &temp_path, effect)
            .await
            .map_err(|e| format!("Failed to apply effect: {}", e))?;
        (temp_path, true)
    };

    // Play with the stored volume.
    if let Some(handler) = manager.get(guild_id) {
        let mut h = handler.lock().await;
        if h.current_channel().is_none() {
            return Err("Bot disconnected while playing.".to_string());
        }
        let source = songbird::input::File::new(play_path.clone());
        play_with_volume(ctx, &mut h, source.into(), &data.volume, guild_id).await;
    }

    // Clean up effect temp files after a delay (the cached plain file is kept).
    if is_temp {
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            let _ = tokio::fs::remove_file(&play_path).await;
        });
    }

    Ok("Playing soundboard sound.".to_string())
}

/// Keep the soundboard download cache bounded: if it exceeds `max_files`,
/// remove the oldest files until it's back within the cap.
async fn cap_soundboard_cache(cache_dir: &str, max_files: usize) {
    let mut entries = match tokio::fs::read_dir(cache_dir).await {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut files = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "mp3") {
            let modified = entry
                .metadata()
                .await
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            files.push((path, modified));
        }
    }
    if files.len() <= max_files {
        return;
    }
    // Oldest first, then remove the excess.
    files.sort_by_key(|(_, t)| *t);
    let to_remove = files.len() - max_files;
    for (path, _) in files.into_iter().take(to_remove) {
        let _ = tokio::fs::remove_file(path).await;
    }
}

/// Toggle voice eavesdropping runtime flag and reply with a lang message.
async fn toggle_eavesdrop(ctx: Context<'_>, enable: bool) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    let action = if enable { "enable" } else { "disable" };
    log::info!("[GUILDID : {}] {} command invoked by user {}", ctx.guild_id().unwrap(), action, ctx.author().id);
    let lang = &ctx.data().lang;

    // Available to every user who can use slash commands — toggling
    // eavesdropping is a community moderation action, not admin-gated.

    match voice_eavesdrop::set_enabled(enable).await {
        Some(_) => {
            let msg = if enable { &lang.eavesdrop_enabled } else { &lang.eavesdrop_disabled };
            ctx.send(poise::CreateReply::default().content(msg).ephemeral(true)).await?;
        }
        None => {
            ctx.send(poise::CreateReply::default().content(&lang.eavesdrop_not_ready).ephemeral(true)).await?;
        }
    }
    Ok(())
}

/// Disable voice eavesdropping at runtime.
#[poise::command(slash_command, user_cooldown = 5)]
async fn disable(ctx: Context<'_>) -> Result<(), Error> {
    toggle_eavesdrop(ctx, false).await
}

/// Enable voice eavesdropping at runtime.
#[poise::command(slash_command, user_cooldown = 5)]
async fn enable(ctx: Context<'_>) -> Result<(), Error> {
    toggle_eavesdrop(ctx, true).await
}

/// Restart bot.
#[poise::command(slash_command, user_cooldown = 5)]
async fn restart(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    log::info!("[GUILDID : {}] restart command invoked by user {}", ctx.guild_id().unwrap(), ctx.author().id);
    let admin_id = env::var("ADMIN_ID").unwrap_or_default();
    let env_guild_id = env::var("GUILD_ID").unwrap_or_default();

    // Create reply early so all subsequent messages edit the deferred response
    let reply = ctx.send(poise::CreateReply::default().content(&ctx.data().lang.processing).ephemeral(true)).await?;

    if ctx.guild_id().unwrap().to_string() != env_guild_id || ctx.author().id.to_string() != admin_id {
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.admin_parent_server).ephemeral(true)).await?;
        return Ok(());
    }
    let guild_id = ctx.guild_id().unwrap();
    let member = guild_id.member(ctx.http(), ctx.author().id).await?;
    #[allow(deprecated)]
    if !member.permissions(ctx)?.administrator() {
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.admin_only).ephemeral(true)).await?;
        return Ok(());
    }
    reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.restarting).ephemeral(true)).await?;
    // Give Discord time to deliver the interaction response before exiting.
    // Without this delay, std::process::exit(0) may kill the process before
    // the HTTP response reaches Discord, and the user never sees the message.
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    std::process::exit(0);
}

/// Rename bot.
#[poise::command(slash_command, user_cooldown = 5)]
async fn rename(
    ctx: Context<'_>,
    #[description = "Nuovo nickname del bot (limite di 32 caratteri)"] name: String,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    log::info!("[GUILDID : {}] rename command invoked by user {} with name: {}", ctx.guild_id().unwrap(), ctx.author().id, name);

    // Create reply early so all subsequent messages edit the deferred response
    let reply = ctx.send(poise::CreateReply::default().content(&ctx.data().lang.processing).ephemeral(true)).await?;

    // Verify admin permissions (same check as /restart and /avatar)
    let admin_id = env::var("ADMIN_ID").unwrap_or_default();
    let env_guild_id = env::var("GUILD_ID").unwrap_or_default();
    if ctx.guild_id().unwrap().to_string() != env_guild_id || ctx.author().id.to_string() != admin_id {
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.admin_parent_server).ephemeral(true)).await?;
        return Ok(());
    }
    let guild_id = ctx.guild_id().unwrap();
    let member = guild_id.member(ctx.http(), ctx.author().id).await?;
    #[allow(deprecated)]
    if !member.permissions(ctx)?.administrator() {
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.admin_only).ephemeral(true)).await?;
        return Ok(());
    }

    if name.chars().count() > 32 {
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.nickname_too_long).ephemeral(true)).await?;
        return Ok(());
    }
    match guild_id.edit_nickname(ctx.http(), Some(&name)).await {
        Ok(_) => {
            reply.edit(ctx, poise::CreateReply::default().content(ctx.data().lang.nickname_changed.replacen("{}", &name, 1)).ephemeral(true)).await?;
        }
        Err(e) => {
            log::error!("[GUILDID : {}] rename - failed to set nickname: {}", guild_id, e);
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.discord_api_error).ephemeral(true)).await?;
        }
    }
    Ok(())
}

/// Change bot avatar.
#[poise::command(slash_command, user_cooldown = 5)]
async fn avatar(
    ctx: Context<'_>,
    #[description = "Nuovo avatar del bot"] image: serenity::Attachment,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;

    let admin_id = env::var("ADMIN_ID").unwrap_or_default();
    let env_guild_id = env::var("GUILD_ID").unwrap_or_default();

    // Create reply early so all subsequent messages edit the deferred response
    // instead of creating followup messages (cleaner UX, avoids "already
    // acknowledged" errors if the on_error handler also tries to send).
    let reply = ctx.send(poise::CreateReply::default().content(&ctx.data().lang.processing).ephemeral(true)).await?;

    // Verify admin permissions and guild restrictions first (like Python's flow)
    if ctx.guild_id().unwrap().to_string() != env_guild_id || ctx.author().id.to_string() != admin_id {
        log::info!("Avatar update restricted to parent server administrators only");
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.admin_parent_server).ephemeral(true)).await?;
        return Ok(());
    }

    log::info!("[GUILDID : {}] avatar command invoked by user {} with image: {}",
        ctx.guild_id().unwrap(), ctx.author().id, image.filename);

    // Verify the user is a guild administrator (same check as /restart)
    let guild_id = ctx.guild_id().unwrap();
    let member = guild_id.member(ctx.http(), ctx.author().id).await?;
    #[allow(deprecated)]
    if !member.permissions(ctx)?.administrator() {
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.admin_only).ephemeral(true)).await?;
        return Ok(());
    }

    // Check content type (like Python's check_image_with_pil)
    if !image.content_type.as_deref().is_some_and(|ct| ct.starts_with("image/")) {
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.unsupported_file).ephemeral(true)).await?;
        return Ok(());
    }

    // Download image bytes with proper error handling
    let bytes = match tts::http_client().get(&image.url).send().await {
        Ok(resp) => match resp.bytes().await {
            Ok(b) => b.to_vec(),
            Err(e) => {
                log::error!("[GUILDID : {}] avatar - failed to read image bytes: {}", ctx.guild_id().unwrap(), e);
                reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.discord_api_error).ephemeral(true)).await?;
                return Ok(());
            }
        },
        Err(e) => {
            log::error!("[GUILDID : {}] avatar - failed to download image: {}", ctx.guild_id().unwrap(), e);
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.discord_api_error).ephemeral(true)).await?;
            return Ok(());
        }
    };

    if bytes.is_empty() {
        log::warn!("[GUILDID : {}] avatar - downloaded image is empty", ctx.guild_id().unwrap());
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.unsupported_file).ephemeral(true)).await?;
        return Ok(());
    }

    // Validate image using the image crate (like Python's check_image_with_pil)
    match validate_image_bytes(&bytes) {
        Ok((w, h)) => {
            log::info!("[GUILDID : {}] avatar - image validated: {}x{} pixels", ctx.guild_id().unwrap(), w, h);
        }
        Err("too_small") => {
            log::warn!("[GUILDID : {}] avatar - image too small (minimum 128x128)", ctx.guild_id().unwrap());
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.image_too_small).ephemeral(true)).await?;
            return Ok(());
        }
        Err(_) => {
            log::warn!("[GUILDID : {}] avatar - invalid image file", ctx.guild_id().unwrap());
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.unsupported_file).ephemeral(true)).await?;
            return Ok(());
        }
    }

    // Update bot avatar (like Python's client.user.edit(avatar=image))
    let avatar = serenity::builder::CreateAttachment::bytes(bytes, image.filename.clone());
    let mut current_user = ctx.cache().current_user().clone();

    match current_user.edit(ctx.http(), serenity::builder::EditProfile::new().avatar(&avatar)).await {
        Ok(_) => {
            log::info!("Bot avatar updated successfully");
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.avatar_changed).ephemeral(true)).await?;
        }
        Err(e) => {
            log::error!("Failed to update bot avatar: {}", e);
            reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.discord_api_error).ephemeral(true)).await?;
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    eprintln!("=== discord-llm-bot starting (binary loaded correctly) ===");
    // Record boot time so /stats can report real uptime.
    let _ = BOOT_TIME.get_or_init(std::time::Instant::now);
    dotenv::dotenv().ok();
    let mut builder = env_logger::Builder::from_env(env_logger::Env::default().filter_or("LOG_LEVEL", "info"));
    // Keep the bot's own logs at the configured level, but silence the very
    // verbose per-request/connection logging from third-party libraries so the
    // output stays readable (serenity logs every HTTP request at INFO).
    builder.filter_module("tracing", log::LevelFilter::Warn);
    builder.filter_module("serenity", log::LevelFilter::Warn);
    builder.filter_module("songbird", log::LevelFilter::Warn);
    builder.filter_module("reqwest", log::LevelFilter::Warn);
    builder.filter_module("sqlx", log::LevelFilter::Warn);
    builder.init();

    let token = env::var("BOT_TOKEN").expect("BOT_TOKEN must be set");
    let admin_id: u64 = env::var("ADMIN_ID")
        .ok()
        .and_then(|id| id.parse().ok())
        .unwrap_or(0);
    let guild_id: u64 = env::var("GUILD_ID")
        .ok()
        .and_then(|id| id.parse().ok())
        .unwrap_or(0);
    
    eprintln!("Admin ID: {}, Guild ID: {}", admin_id, guild_id);

    // Initialize database pool
    let db_url = env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:config/discord-bot.sqlite3".to_string());
    
    // Create config directory if it doesn't exist
    tokio::fs::create_dir_all("config").await.expect("Failed to create config directory");

    // Also create the database file if it doesn't exist
    let db_path = db_url.strip_prefix("sqlite:").unwrap_or(&db_url);
    if !tokio::fs::try_exists(db_path).await.unwrap_or(false) {
        tokio::fs::File::create(db_path).await.expect("Failed to create database file");
    }

    // Connect to SQLite database with proper error handling.
    // Enable WAL mode + a busy timeout so the two bots (Discord and WhatsApp)
    // can safely share the same SQLite file without "database is locked" errors
    // when they write concurrently. WAL allows concurrent readers with one writer.
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
    let db_pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(
            db_url
                .parse::<SqliteConnectOptions>()
                .expect("Invalid DATABASE_URL for SQLite")
                .journal_mode(SqliteJournalMode::Wal)
                .busy_timeout(std::time::Duration::from_secs(5))
                .create_if_missing(true),
        )
        .await
        .map_err(|e| format!("Failed to connect to DB: {}", e))
        .expect("Database connection failed");
    
    eprintln!("Connecting to database at: {}", db_url);
    database::init_db(&db_pool).await
        .map_err(|e| format!("Failed to initialize database: {}", e))
        .expect("Database initialization failed");
    
    log::info!("✓ Database initialized successfully");
    database::populate_db_if_empty(&db_pool).await
        .map_err(|e| format!("Failed to populate database: {}", e))
        .expect("Database population check completed");
    
    log::info!("✓ Database population check completed - ready for operations");

    let pool_clone = db_pool.clone();
    tokio::spawn(async move {
        generator::run_background_generator(pool_clone).await;
    });

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            // NOTE: clone() is intentionally NOT in this list — it is registered
            // per-guild only (see guild command sync below) so it never shows
            // up in the global command list.
            commands: vec![join(), leave(), stop(), speak(), random(), ask(), translate(), volume(), audio(), soundboard(), restart(), rename(), avatar(), help(), stats(), joke(), disable(), enable(), createvoice(), myvoices(), deletevoice(), clone()],
            pre_command: |ctx| {
                Box::pin(async move {
                    let command_name = ctx.command().name.as_str();
                    // Only check guild restriction for admin commands (restart, avatar, rename).
                    // Each of these commands also has its own guild+admin check inside the
                    // command body, so this hook only provides an early log warning.
                    if command_name != "restart" && command_name != "avatar" && command_name != "rename" {
                        return;
                    }
                    let allowed_guild_id = env::var("GUILD_ID").unwrap_or_default();
                    if ctx.guild_id().map(|g| g.to_string()) != Some(allowed_guild_id) {
                        log::warn!("Command {} invoked in wrong guild by user {}", command_name, ctx.author().id);
                    }
                })
            },
            on_error: |error| {
                Box::pin(async move {
                    if let poise::FrameworkError::CooldownHit { remaining_cooldown, ctx, .. } = error {
                        let spam_msgs = &ctx.data().lang.spam_messages;
                        let random_msg = {
                            let mut rng = rand::thread_rng();
                            spam_msgs.choose(&mut rng).unwrap()
                        };
                        let user_id = ctx.author().id.to_string();
                        let random_msg_filled = random_msg.replace("{}", &user_id);
                        let msg = format!("{}\nCooldown: {:.2}s", random_msg_filled, remaining_cooldown.as_secs_f32());
                        let _ = ctx.send(poise::CreateReply::default().content(msg).ephemeral(true)).await;
                    } else {
                        if let poise::FrameworkError::Command { ctx, error, .. } = error {
                            let error_str = error.to_string();
                            // "Unknown interaction" and "already been acknowledged" happen
                            // when a command takes long enough that Discord expires the
                            // interaction token. The command already sent its reply, so
                            // these are expected and not real errors — log at debug level.
                            if error_str.contains("Unknown interaction") || error_str.contains("already been acknowledged") {
                                log::debug!("Command completed but interaction expired (expected for long-running commands): {}", error_str);
                            } else {
                                log::error!("Error in command: {}", error_str);
                                // Track real command errors so /stats can report the bot's health.
                                ctx.data().error_tracker.record_incident();
                            }
                            let msg = if error_str.is_empty() {
                                ctx.data().lang.discord_api_error.clone()
                            } else {
                                error_str
                            };
                            // If the command already sent a reply (e.g. via ctx.send after
                            // defer_ephemeral), attempting ctx.send again causes "Interaction
                            // has already been acknowledged". Wrap in match and silently
                            // ignore that specific error instead of crashing.
                            match ctx.send(poise::CreateReply::default().content(msg).ephemeral(true)).await {
                                Ok(_) => {}
                                Err(e) => {
                                    let e_str = e.to_string();
                                    if e_str.contains("already been acknowledged") || e_str.contains("Unknown interaction") {
                                        log::debug!("on_error: could not send error message (interaction already acknowledged): {}", e_str);
                                    } else {
                                        log::error!("on_error: failed to send error message: {}", e_str);
                                    }
                                }
                            }
                        }
                    }
                })
            },
            event_handler: |ctx, event, framework, data| {
                Box::pin(async move {
                    // On CacheReady: clear all global slash commands and sync per-guild.
                    // Clearing global commands is critical — if any were left from a
                    // previous build that used register_globally(), they would appear as
                    // duplicates alongside the per-guild commands we set below.
                    if let serenity::FullEvent::CacheReady { guilds, .. } = event {
                        log::info!("Cache ready, clearing global commands and syncing per-guild");
                        let http = ctx.http.clone();
                        // Remove all global commands (sets to empty list).
                        if let Err(e) = serenity::Command::set_global_commands(&http, Vec::new()).await {
                            log::warn!("Failed to clear global commands: {}", e);
                        } else {
                            log::info!("Global commands cleared successfully");
                        }
                        // Sync commands to each guild — makes slash commands available
                        // instantly instead of waiting up to 1 hour for global registration.
                        log::info!("Syncing commands to {} guild(s)", guilds.len());
                        let commands = poise::builtins::create_application_commands(&framework.options().commands);
                        for guild_id in guilds {
                            if let Err(e) = guild_id.set_commands(&http, commands.clone()).await {
                                log::error!("Failed to sync commands for guild {}: {}", guild_id, e);
                            }
                        }
                    }
                    // Sync commands when the bot joins a new guild
                    if let serenity::FullEvent::GuildCreate { guild, .. } = event {
                        log::info!("Guild joined (ID: {}, NAME: {}), syncing commands", guild.id, guild.name);
                        let commands = poise::builtins::create_application_commands(&framework.options().commands);
                        let http = ctx.http.clone();
                        if let Err(e) = guild.id.set_commands(&http, commands).await {
                            log::error!("Failed to sync commands for guild {}: {}", guild.id, e);
                        }
                    }
                    // Auto-join / auto-welcome / auto-switch behaviour driven by
                    // voice state changes. Only acts when AUTO_JOIN_VOICE is enabled.
                    if data.auto_join_enabled {
                        if let serenity::FullEvent::VoiceStateUpdate { old, new } = event {
                            auto_join::handle_voice_state_update(&ctx, data, old.as_ref(), &new).await;
                        }
                    }
                    if let serenity::FullEvent::InteractionCreate { interaction: serenity::Interaction::Component(component) } = event {
                            // Help button handler — shows command lists by category
                            if let Some(category) = component.data.custom_id.strip_prefix("help:") {
                                let lang = &data.lang;
                                let embed = match category {
                                    "voice" => serenity::CreateEmbed::new()
                                        .title(&lang.help_title)
                                        .color(0x5865F2)
                                        .field("🎤 /join", &lang.help_join_desc, false)
                                        .field("👋 /leave", &lang.help_leave_desc, false)
                                        .field("⏹️ /stop", &lang.help_stop_desc, false)
                                        .field("🗣️ /speak", &lang.help_speak_desc, false)
                                        .field("🎲 /random", &lang.help_random_desc, false)
                                        .field("😂 /joke", &lang.help_joke_desc, false)
                                        .field("🔊 /volume", &lang.help_volume_desc, false)
                                        .field("🎵 /audio", &lang.help_audio_desc, false)
                                        .field("🎛️ /soundboard", &lang.help_soundboard_desc, false)
                                        .field("🎙️ /createvoice", &lang.help_createvoice_desc, false)
                                        .field("📋 /myvoices", &lang.help_myvoices_desc, false)
                                        .field("🗑️ /deletevoice", &lang.help_deletevoice_desc, false),
                                    "ai" => serenity::CreateEmbed::new()
                                        .title(&lang.help_title)
                                        .color(0x99AAB5)
                                        .field("🤔 /ask", &lang.help_ask_desc, false)
                                        .field("🌐 /translate", &lang.help_translate_desc, false),
                                    "admin" => serenity::CreateEmbed::new()
                                        .title(&lang.help_title)
                                        .color(0xED4245)
                                        .field("📊 /stats", &lang.help_stats_desc, false)
                                        .field("🔄 /restart", &lang.help_restart_desc, false)
                                        .field("✏️ /rename", &lang.help_rename_desc, false)
                                        .field("🖼️ /avatar", &lang.help_avatar_desc, false)
                                        .field("🔇 /disable", &lang.eavesdrop_disabled_desc, false)
                                        .field("🔊 /enable", &lang.eavesdrop_enabled_desc, false),
                                    "all" => serenity::CreateEmbed::new()
                                        .title(&lang.help_title)
                                        .color(0x57F287)
                                        .field("🎤 /join", &lang.help_join_desc, false)
                                        .field("👋 /leave", &lang.help_leave_desc, false)
                                        .field("⏹️ /stop", &lang.help_stop_desc, false)
                                        .field("🗣️ /speak", &lang.help_speak_desc, false)
                                        .field("🎲 /random", &lang.help_random_desc, false)
                                        .field("😂 /joke", &lang.help_joke_desc, false)
                                        .field("🔊 /volume", &lang.help_volume_desc, false)
                                        .field("🎵 /audio", &lang.help_audio_desc, false)
                                        .field("🎛️ /soundboard", &lang.help_soundboard_desc, false)
                                        .field("🎙️ /createvoice", &lang.help_createvoice_desc, false)
                                        .field("📋 /myvoices", &lang.help_myvoices_desc, false)
                                        .field("🗑️ /deletevoice", &lang.help_deletevoice_desc, false)
                                        .field("🤔 /ask", &lang.help_ask_desc, false)
                                        .field("🌐 /translate", &lang.help_translate_desc, false)
                                        .field("📊 /stats", &lang.help_stats_desc, false)
                                        .field("❓ /help", &lang.help_help_desc, false)
                                        .field("🔄 /restart", &lang.help_restart_desc, false)
                                        .field("✏️ /rename", &lang.help_rename_desc, false)
                                        .field("🖼️ /avatar", &lang.help_avatar_desc, false)
                                        .field("🔇 /disable", &lang.eavesdrop_disabled_desc, false)
                                        .field("🔊 /enable", &lang.eavesdrop_enabled_desc, false),
                                    _ => return Ok(()),
                                };
                                component.edit_response(ctx, serenity::EditInteractionResponse::new().embed(embed)).await?;
                                return Ok(());
                            }

                            if component.data.custom_id == "stop" {
                                // Check if user is in a voice channel
                                if let Some(guild_id) = component.guild_id {
                                    log::info!("Component interaction: stop button pressed by user {} in guild {}", component.user.id, guild_id);
                                    let user_in_voice = ctx.cache.guild(guild_id)
                                        .and_then(|g| g.voice_states.get(&component.user.id).and_then(|vs| vs.channel_id))
                                        .is_some();
                                    if !user_in_voice {
                                        component.create_response(ctx, serenity::CreateInteractionResponse::Message(
                                            serenity::CreateInteractionResponseMessage::new().content(&data.lang.must_be_in_voice).ephemeral(true)
                                        )).await?;
                                        return Ok(());
                                    }
                                    // Check bot permissions in the user's channel
                                    let has_perms = {
                                        if let Some(guild) = ctx.cache.guild(guild_id) {
                                            if let Some(channel_id) = guild.voice_states.get(&component.user.id).and_then(|vs| vs.channel_id) {
                                                if let Some(guild_channel) = guild.channels.get(&channel_id) {
                                                    #[allow(deprecated)]
                                                    let perms = guild_channel.permissions_for_user(&ctx.cache, ctx.cache.current_user().id);
                                                    if let Ok(p) = perms {
                                                        p.speak() && p.connect()
                                                    } else {
                                                        false
                                                    }
                                                } else { false }
                                            } else { false }
                                        } else { false }
                                    };
                                    if !has_perms {
                                        component.create_response(ctx, serenity::CreateInteractionResponse::Message(
                                            serenity::CreateInteractionResponseMessage::new().content(&data.lang.user_no_permission).ephemeral(true)
                                        )).await?;
                                        return Ok(());
                                    }
                                    let manager = match songbird::get(ctx).await {
                                        Some(m) => m,
                                        None => {
                                            component.create_response(ctx, serenity::CreateInteractionResponse::Message(
                                                serenity::CreateInteractionResponseMessage::new().content(&data.lang.bot_not_ready).ephemeral(true)
                                            )).await?;
                                            return Ok(());
                                        }
                                    };
                                    if let Some(handler) = manager.get(guild_id) {
                                        let mut handler = handler.lock().await;
                                        handler.stop();
                                        component.create_response(ctx, serenity::CreateInteractionResponse::Message(
                                            serenity::CreateInteractionResponseMessage::new().content(&data.lang.stop_success).ephemeral(true)
                                        )).await?;
                                    } else {
                                        // Bot is not connected to any voice channel in this guild
                                        component.create_response(ctx, serenity::CreateInteractionResponse::Message(
                                            serenity::CreateInteractionResponseMessage::new().content(&data.lang.not_connected).ephemeral(true)
                                        )).await?;
                                    }
                                }
                            } else if let Some(file_path) = component.data.custom_id.strip_prefix("play:") {
                                if let Some(guild_id) = component.guild_id {
                                    log::info!("Component interaction: play button pressed by user {} for file {}", component.user.id, file_path);
                                    // Defer first to avoid timeout during rejoin
                                    let _ = component.create_response(ctx, serenity::CreateInteractionResponse::Defer(
                                        serenity::CreateInteractionResponseMessage::new().ephemeral(true)
                                    )).await;
                                    // Check if user is in a voice channel
                                    let user_in_voice = ctx.cache.guild(guild_id)
                                        .and_then(|g| g.voice_states.get(&component.user.id).and_then(|vs| vs.channel_id))
                                        .is_some();
                                    if !user_in_voice {
                                        component.edit_response(ctx, serenity::EditInteractionResponse::new().content(&data.lang.must_be_in_voice)).await?;
                                        return Ok(());
                                    }
                                    // Resolve the file path: if the original file doesn't exist,
                                    // try to find a permanent copy in audios/ (handles the case
                                    // where SAVE_MP3_ON_DISK was toggled after the file was
                                    // created as a temp file in /tmp).
                                    let playback_path = if tokio::fs::try_exists(file_path).await.unwrap_or(false) {
                                        file_path.to_string()
                                    } else if file_path.contains("/tts_") {
                                        // Temp file pattern: /tmp/.../tts_voice_hash.mp3
                                        // Permanent pattern: audios/voice_hash.mp3
                                        let filename = std::path::Path::new(file_path)
                                            .file_name()
                                            .and_then(|n| n.to_str())
                                            .unwrap_or("");
                                        let stripped = filename.strip_prefix("tts_").unwrap_or(filename);
                                        let permanent = format!("audios/{}", stripped);
                                        if tokio::fs::try_exists(&permanent).await.unwrap_or(false) {
                                            log::info!("Play button: temp file gone, found permanent: {}", permanent);
                                            permanent
                                        } else {
                                            log::warn!("Play button: file no longer exists: {} (no permanent fallback)", file_path);
                                            component.edit_response(ctx, serenity::EditInteractionResponse::new().content(&data.lang.file_expired)).await?;
                                            return Ok(());
                                        }
                                    } else {
                                        log::warn!("Play button: file no longer exists: {}", file_path);
                                        component.edit_response(ctx, serenity::EditInteractionResponse::new().content(&data.lang.file_expired)).await?;
                                        return Ok(());
                                    };
                                    let manager = match songbird::get(ctx).await {
                                        Some(m) => m,
                                        None => {
                                            component.edit_response(ctx, serenity::EditInteractionResponse::new().content(&data.lang.bot_not_ready)).await?;
                                            return Ok(());
                                        }
                                    };
                                    if let Some(handler_lock) = manager.get(guild_id) {
                                        let mut handler = handler_lock.lock().await;
                                        // Check if bot is still connected
                                        if handler.current_channel().is_none() {
                                            // Bot disconnected, try to rejoin user's channel
                                            if let Some(user_channel) = ctx.cache.guild(guild_id)
                                                .and_then(|g| g.voice_states.get(&component.user.id).and_then(|vs| vs.channel_id)) {
                                                drop(handler);
                                                let _ = manager.join(guild_id, user_channel).await;
                                                let handler_lock = manager.get(guild_id).unwrap();
                                                let mut handler = handler_lock.lock().await;
                                                log::info!("Playing audio file: {}", playback_path);
                                                let source = songbird::input::File::new(playback_path.clone());
                                                play_with_volume(&ctx, &mut handler, source.into(), &data.volume, guild_id).await;
                                                log::info!("Audio playback started in guild {}", guild_id);
                                            }
                                        } else {
                                            log::info!("Playing audio file: {}", playback_path);
                                            let source = songbird::input::File::new(playback_path.clone());
                                            play_with_volume(&ctx, &mut handler, source.into(), &data.volume, guild_id).await;
                                            log::info!("Audio playback started in guild {}", guild_id);
                                        }
                                    } else {
                                        // Bot not in manager, try to join and play
                                        if let Some(user_channel) = ctx.cache.guild(guild_id)
                                            .and_then(|g| g.voice_states.get(&component.user.id).and_then(|vs| vs.channel_id)) {
                                            if let Ok(handler_lock) = manager.join(guild_id, user_channel).await {
                                                let mut handler = handler_lock.lock().await;
                                                log::info!("Playing audio file: {}", playback_path);
                                                let source = songbird::input::File::new(playback_path.clone());
                                                play_with_volume(&ctx, &mut handler, source.into(), &data.volume, guild_id).await;
                                                log::info!("Audio playback started in guild {}", guild_id);
                                            }
                                        }
                                    }
                                }
                            // Use let _ to ignore edit_response errors — the interaction
                            // token may have expired if the user clicked after a long delay,
                            // and playback already succeeded at this point.
                            let _ = component.edit_response(ctx, serenity::EditInteractionResponse::new().content(&data.lang.replaying_audio)).await;
                            } else if let Some(sb) = component.data.custom_id.strip_prefix("sb:") {
                                // Soundboard component buttons: play a sound, or
                                // navigate/close the paginated result list.
                                let parts: Vec<&str> = sb.split(':').collect();
                                if parts.is_empty() {
                                    return Ok(());
                                }
                                let action = parts[0];
                                let session_id = parts.get(1).copied().unwrap_or("").to_string();
                                let Some(guild_id) = component.guild_id else { return Ok(()) };

                                match action {
                                    "close" => {
                                        data.soundboard_sessions.lock().unwrap().remove(&session_id);
                                        let _ = component
                                            .edit_response(
                                                ctx,
                                                serenity::EditInteractionResponse::new().content("Soundboard chiuso.").components(Vec::new()),
                                            )
                                            .await;
                                    }
                                    "prev" | "next" => {
                                        // Compute the new page and view while holding the lock,
                                        // then drop it before awaiting the network call.
                                        let view = {
                                            let mut sessions = data.soundboard_sessions.lock().unwrap();
                                            if let Some(session) = sessions.get_mut(&session_id) {
                                                let total_pages = session.items.len().div_ceil(soundboard::PAGE_SIZE).max(1);
                                                if action == "prev" && session.page > 0 {
                                                    session.page -= 1;
                                                } else if action == "next" && session.page + 1 < total_pages {
                                                    session.page += 1;
                                                }
                                                Some(soundboard_view(session, &session_id))
                                            } else {
                                                None
                                            }
                                        };
                                        if let Some((embed, rows)) = view {
                                            let _ = component
                                                .edit_response(ctx, serenity::EditInteractionResponse::new().embed(embed).components(rows))
                                                .await;
                                        }
                                    }
                                    "play" => {
                                        let index: usize = parts.get(2).and_then(|p| p.parse().ok()).unwrap_or(0);
                                        let session = {
                                            let sessions = data.soundboard_sessions.lock().unwrap();
                                            sessions.get(&session_id).cloned()
                                        };
                                        let Some(session) = session else { return Ok(()) };
                                        if index >= session.items.len() {
                                            return Ok(());
                                        }
                                        let item = &session.items[index];
                                        let _ = component
                                            .create_response(
                                                ctx,
                                                serenity::CreateInteractionResponse::Defer(serenity::CreateInteractionResponseMessage::new().ephemeral(true)),
                                            )
                                            .await;
                                        match play_soundboard_item(&ctx, data, guild_id, component.user.id, &item.url, &session.effect).await {
                                            Ok(msg) => {
                                                // Show a Stop button (like /speak and /random) so
                                                // playback can be stopped without typing /stop.
                                                let stop = serenity::CreateButton::new("stop")
                                                    .label("Stop")
                                                    .style(serenity::ButtonStyle::Danger);
                                                let rows = vec![serenity::CreateActionRow::Buttons(vec![stop])];
                                                let _ = component
                                                    .edit_response(
                                                        &ctx,
                                                        serenity::EditInteractionResponse::new()
                                                            .content(format!("🔊 {}: {}", item.title, msg))
                                                            .components(rows),
                                                    )
                                                    .await;
                                            }
                                            Err(e) => {
                                                let _ = component
                                                    .edit_response(&ctx, serenity::EditInteractionResponse::new().content(format!("⚠️ {}", e)))
                                                    .await;
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                    }
                    Ok(())
                })
            },
            ..Default::default()
        })
        .setup(move |ctx, _ready, _framework| {
            let db_pool = db_pool.clone();
            Box::pin(async move {
                log::info!("Logged in as {} (ID: {})", _ready.user.name, _ready.user.id);
                // Per-guild command registration is handled in the CacheReady and
                // GuildCreate event handlers, which call set_commands() for each guild.
                // We intentionally do NOT call register_globally() here — registering both
                // globally and per-guild causes duplicate slash commands to appear in Discord.
                let ctx_clone = ctx.clone();
                tokio::spawn(async move {
                    change_presence_loop(ctx_clone).await;
                });
                // Background CPU/RAM sampler so commands read cached stats
                // instead of blocking ~200ms sampling CPU on every reply.
                tokio::spawn(async move {
                    sample_system_stats_loop().await;
                });
                
                // Initialize error tracker and language settings
                let lang = lang::Lang::new();
                let error_tracker = ErrorTracker::new();

                let auto_join_enabled = auto_join::config_enabled();
                let auto_join_welcome = auto_join::config_welcome();
                let auto_join_goodbye = auto_join::config_goodbye();
                let auto_join_here_i_am = auto_join::config_here_i_am();

                // Shared playback volume: one Arc shared by poise's Data
                // (/volume command) and the background scanner loop, so the
                // scanner's "here I am" announcements honor /volume too.
                let volume_arc = std::sync::Arc::new(std::sync::Mutex::new(1.0));

                // Shared auto-join state: cloned into both poise's Data
                // (commands) and the background scanner loop, so throttling
                // stays in sync across owners.
                let auto_join_shared = std::sync::Arc::new(auto_join::AutoJoinShared::new(auto_join_here_i_am));

                // If auto-join is enabled, spawn a background loop that watches
                // each connected voice channel and disconnects the bot after it
                // has been alone for the configured timeout.
                if auto_join_enabled {
                    let idle_ctx = ctx.clone();
                    tokio::spawn(async move {
                        auto_join::idle_disconnect_loop(idle_ctx).await;
                    });
                    let scanner_ctx = ctx.clone();
                    let scanner_shared = (*auto_join_shared).clone();
                    let scanner_pool = db_pool.clone();
                    let scanner_volume = volume_arc.clone();
                    tokio::spawn(async move {
                        auto_join::channel_scanner_loop(
                            scanner_ctx,
                            scanner_shared,
                            scanner_pool,
                            scanner_volume,
                        )
                        .await;
                    });
                }

                // Spawn the voice eavesdrop loop: periodically "eavesdrops" on a
                // random user in the bot's voice channel and comments via LLM+TTS.
                // The loop itself checks VOICE_EAVESDROP_ENABLED every tick, so it
                // can be toggled at runtime without restarting the bot.
                {
                    let eavesdrop_ctx = ctx.clone();
                    let eavesdrop_pool = db_pool.clone();
                    let eavesdrop_volume = volume_arc.clone();
                    tokio::spawn(async move {
                        voice_eavesdrop::start_eavesdrop_loop(
                            eavesdrop_ctx,
                            eavesdrop_pool,
                            eavesdrop_volume,
                        )
                        .await;
                    });
                }

                log::info!("Framework setup complete with enhanced error tracking");
                Logger::info("INIT", "Error tracking system initialized successfully");

                Ok(Data {
                    db_pool,
                    lang,
                    error_tracker,
                    volume: volume_arc,
                    conversations: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
                    auto_join_enabled,
                    auto_join_welcome,
                    auto_join_goodbye,
                    auto_join_shared,
                    last_welcome: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
                    last_goodbye: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
                    soundboard_sessions: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
                })
            })
        })
        .build();

    let intents = serenity::GatewayIntents::non_privileged() 
        | serenity::GatewayIntents::MESSAGE_CONTENT
        | serenity::GatewayIntents::GUILD_VOICE_STATES;
    
    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .register_songbird()
        .await
        .expect("Error creating client");

    log::info!("Starting Discord client...");
    match client.start().await {
        Ok(()) => {
            log::error!("Client exited normally (Ok) - gateway disconnected immediately!");
            std::process::exit(1);
        }
        Err(e) => {
            log::error!("Client error: {}", e);
            std::process::exit(1);
        }
    }
}
