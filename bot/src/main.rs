mod database;
mod error;
mod generator;
mod lang;
mod tts;

use error::{ErrorTracker, Logger};
use poise::serenity_prelude as serenity;
use poise::serenity_prelude::Mentionable;
use rand::seq::SliceRandom;
use std::env;
use sysinfo::System;
use songbird::SerenityInit;
use image::GenericImageView;

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
    handler.play_only(source.into());
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
pub struct Data {
    pub db_pool: sqlx::SqlitePool,
    pub lang: lang::Lang,
    pub error_tracker: ErrorTracker,
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Context<'a> = poise::Context<'a, Data, Error>;

async fn change_presence_loop(ctx: serenity::Context) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(6 * 60 * 60));
    loop {
        interval.tick().await;
        let url = "https://steamspy.com/api.php?request=top100in2weeks";
        let client = tts::http_client();
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
                            }
                        }
                    }
                    Err(e) => log::error!("change_presence_loop - failed to parse JSON: {}", e),
                }
            }
            Err(e) => log::error!("change_presence_loop - failed to fetch from steamspy: {}", e),
        }
    }
}

async fn get_queue_message(lang: &lang::Lang) -> String {
    let mut sys = System::new_all();
    sys.refresh_cpu();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    sys.refresh_cpu();
    let cpu_usage = sys.global_cpu_info().cpu_usage();
    let total_memory = sys.total_memory();
    let used_memory = sys.used_memory();
    let ram_usage = (used_memory as f64 / total_memory as f64) * 100.0;
    
    log::debug!("get_queue_message: CPU {:.1}%, RAM {:.2}%", 
        cpu_usage, ram_usage);
    
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
    let mut voices: Vec<&str> = tts::AVAILABLE_VOICES.to_vec();
    voices.push("random");

    voices.into_iter()
        .filter(|v| {
            // Substring match (case-insensitive). When the current input is empty,
            // every voice matches because every string contains the empty string.
            v.to_lowercase().contains(&current.to_lowercase())
        })
        .map(|v| {
            let name = v.to_string();
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
) -> Result<(), Error> {
    log::info!("[GUILDID : {}] speak command invoked by user {} with text: {:?}, voice: {:?}", ctx.guild_id().unwrap(), ctx.author().id, text, voice);
    let voice = voice.unwrap_or_else(|| "Google".to_string());
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
        log::info!("[GUILDID : {}] speak - text: {}, voice: {}", guild.id, text, actual_voice);
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

    let tts_result = match tts::get_or_generate_tts(&text, &actual_voice).await {
        Ok(result) => result,
        Err(e) => {
            log::error!("TTS generation failed: {}", e);
            let error_msg = if actual_voice == "Google" {
                &ctx.data().lang.tts_error_google
            } else {
                &ctx.data().lang.tts_error_fakeyou
            };
            reply.edit(ctx, poise::CreateReply::default().content(error_msg).ephemeral(true)).await?;
            return Ok(());
        }
    };

    if let Err(e) = database::insert_sentence(&ctx.data().db_pool, &text).await {
        log::error!("Failed to insert sentence into database: {}", e);
    }

    let mut handler = handler_lock.lock().await;
    if handler.current_channel().is_none() {
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
        return Ok(());
    }
    
    log::info!("TTS file path: {}", tts_result.file_path);
    if !std::path::Path::new(&tts_result.file_path).exists() {
        log::error!("TTS file does not exist: {}", tts_result.file_path);
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.tts_error).ephemeral(true)).await?;
        return Ok(());
    }
    log::info!("Playing audio file: {}", tts_result.file_path);
    let source = songbird::input::File::new(tts_result.file_path.clone());
    handler.play_only(source.into());
    log::info!("Audio playback started in guild {}", guild_id);

    let warning = if tts_result.fallback {
        &ctx.data().lang.fakeyou_warning
    } else {
        ""
    };

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
        .content(ctx.data().lang.playing.replacen("{}", &text, 1).replacen("{}", &tts_result.actual_voice, 1) + warning)
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
) -> Result<(), Error> {
    log::info!("[GUILDID : {}] random command invoked by user {} with voice: {:?}, text: {:?}", ctx.guild_id().unwrap(), ctx.author().id, voice, text);

    // Track whether the user explicitly specified a voice
    let voice_explicitly_set = voice.is_some();
    let voice = voice.unwrap_or_else(|| "Google".to_string());
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
    if !voice_explicitly_set && save_mp3 {
        log::info!("random: no voice specified and SAVE_MP3_ON_DISK=true, scanning audios/ folder");
        if let Ok(entries) = std::fs::read_dir("audios") {
            let mp3_files: Vec<String> = entries
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let path = e.path();
                    if path.extension().is_some_and(|ext| ext == "mp3") {
                        path.to_str().map(|s| s.to_string())
                    } else {
                        None
                    }
                })
                .collect();
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
        handler.play_only(source.into());
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
        // Filenames follow the pattern {voice_token}_{hash}.mp3 — extract
        // the token and reverse-lookup the voice name for display.
        // The original sentence text isn't recoverable from the filename,
        // so show "Cached audio" as the sentence label instead of the
        // opaque hash.
        let voice_name = std::path::Path::new(audio_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("cached")
            .split('_')
            .next()
            .map(tts::get_voice_name_from_token)
            .unwrap_or("Unknown");
        // Use match instead of ? so expired interaction tokens don't propagate
        // to on_error — the audio already started playing on the line above.
        match reply.edit(ctx, poise::CreateReply::default()
            .content(ctx.data().lang.playing.replacen("{}", "Cached audio", 1).replacen("{}", voice_name, 1))
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

    let tts_result = match tts::get_or_generate_tts(&random_sentence, &actual_voice).await {
        Ok(result) => result,
        Err(e) => {
            log::error!("TTS generation failed: {}", e);
            let error_msg = if actual_voice == "Google" {
                &ctx.data().lang.tts_error_google
            } else {
                &ctx.data().lang.tts_error_fakeyou
            };
            reply.edit(ctx, poise::CreateReply::default().content(error_msg).ephemeral(true)).await?;
            return Ok(());
        }
    };

    let mut handler = handler_lock.lock().await;
    if handler.current_channel().is_none() {
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
        return Ok(());
    }
    
    log::info!("TTS file path: {}", tts_result.file_path);
    if !std::path::Path::new(&tts_result.file_path).exists() {
        log::error!("TTS file does not exist: {}", tts_result.file_path);
        reply.edit(ctx, poise::CreateReply::default().content(&ctx.data().lang.tts_error).ephemeral(true)).await?;
        return Ok(());
    }
    log::info!("Playing audio file: {}", tts_result.file_path);
    let source = songbird::input::File::new(tts_result.file_path.clone());
    handler.play_only(source.into());
    log::info!("Audio playback started in guild {}", guild_id);

    let warning = if tts_result.fallback {
        &ctx.data().lang.fakeyou_warning
    } else {
        ""
    };

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
        .content(ctx.data().lang.playing.replacen("{}", &random_sentence, 1).replacen("{}", &tts_result.actual_voice, 1) + warning)
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

/// Audio playback from the input audio
#[poise::command(slash_command, user_cooldown = 5)]
async fn audio(
    ctx: Context<'_>,
    #[description = "Il file audio (mp3, wav, ogg, m4a, flac)"] audio: serenity::Attachment,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    log::info!("[GUILDID : {}] audio command invoked by user {} with filename: {}", ctx.guild_id().unwrap(), ctx.author().id, audio.filename);
    check_permissions(ctx).await?;

    let allowed_extensions = ["mp3", "wav", "ogg", "m4a", "flac"];
    let ext = audio.filename.split('.').next_back().unwrap_or("").to_lowercase();
    if !allowed_extensions.contains(&ext.as_str()) {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.invalid_extension).ephemeral(true)).await?;
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
        ctx.send(poise::CreateReply::default().content(&msg).ephemeral(true)).await?;
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
            ctx.send(poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
            return Ok(());
        }
    };
    let handler_lock = match manager.get(guild_id) {
        Some(h) => h,
        None => {
            ctx.send(poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
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
            ctx.send(poise::CreateReply::default().content(&ctx.data().lang.bot_not_ready).ephemeral(true)).await?;
            return Ok(());
        }
    }

    log::info!("[GUILDID : {}] audio - filename: {}", guild_id, audio.filename);

    // Create the reply early so we can edit it with the final result, matching
    // the speak/random pattern. This gives a cleaner UX: the deferred "thinking..."
    // indicator transitions into the final message instead of a new followup.
    // Compute queue metrics once and reuse for both the initial and final message
    // so the user sees consistent values (sysinfo CPU/RAM can fluctuate between calls).
    let queue_status = get_queue_message(&ctx.data().lang).await;
    let initial_msg = format!("{}{}", ctx.data().lang.audio_playback, queue_status);
    let reply = ctx.send(poise::CreateReply::default().content(initial_msg).ephemeral(true)).await?;

    let temp_dir = std::env::var("TMP_DIR").unwrap_or_else(|_| "/tmp/discord-llm-bot".to_string());
    let safe_filename = std::path::Path::new(&audio.filename)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("audio.mp3");
    // Prefix with a UUID to prevent concurrent uploads with the same filename
    // from overwriting each other's temp file while playback is in progress.
    let file_path = format!("{}/{}_{}", temp_dir, uuid::Uuid::new_v4(), safe_filename);
    
    // Download the attachment with proper error handling
    let bytes = match tts::http_client().get(&audio.url).send().await {
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

/// Restart bot.
#[poise::command(slash_command, user_cooldown = 5)]
async fn restart(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    log::info!("[GUILDID : {}] restart command invoked by user {}", ctx.guild_id().unwrap(), ctx.author().id);
    let admin_id = env::var("ADMIN_ID").unwrap_or_default();
    let env_guild_id = env::var("GUILD_ID").unwrap_or_default();
    if ctx.guild_id().unwrap().to_string() != env_guild_id || ctx.author().id.to_string() != admin_id {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.admin_parent_server).ephemeral(true)).await?;
        return Ok(());
    }
    let guild_id = ctx.guild_id().unwrap();
    let member = guild_id.member(ctx.http(), ctx.author().id).await?;
    #[allow(deprecated)]
    if !member.permissions(ctx)?.administrator() {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.admin_only).ephemeral(true)).await?;
        return Ok(());
    }
    ctx.send(poise::CreateReply::default().content(&ctx.data().lang.restarting).ephemeral(true)).await?;
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

    // Verify admin permissions (same check as /restart and /avatar)
    let admin_id = env::var("ADMIN_ID").unwrap_or_default();
    let env_guild_id = env::var("GUILD_ID").unwrap_or_default();
    if ctx.guild_id().unwrap().to_string() != env_guild_id || ctx.author().id.to_string() != admin_id {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.admin_parent_server).ephemeral(true)).await?;
        return Ok(());
    }
    let guild_id = ctx.guild_id().unwrap();
    let member = guild_id.member(ctx.http(), ctx.author().id).await?;
    #[allow(deprecated)]
    if !member.permissions(ctx)?.administrator() {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.admin_only).ephemeral(true)).await?;
        return Ok(());
    }

    if name.chars().count() > 32 {
        ctx.send(poise::CreateReply::default().content(&ctx.data().lang.nickname_too_long).ephemeral(true)).await?;
        return Ok(());
    }
    match guild_id.edit_nickname(ctx.http(), Some(&name)).await {
        Ok(_) => {
            ctx.send(poise::CreateReply::default().content(ctx.data().lang.nickname_changed.replacen("{}", &name, 1)).ephemeral(true)).await?;
        }
        Err(e) => {
            log::error!("[GUILDID : {}] rename - failed to set nickname: {}", guild_id, e);
            ctx.send(poise::CreateReply::default().content(&ctx.data().lang.discord_api_error).ephemeral(true)).await?;
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
    let reply = ctx.send(poise::CreateReply::default().content("...").ephemeral(true)).await?;

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
    dotenv::dotenv().ok();
    let mut builder = env_logger::Builder::from_env(env_logger::Env::default().filter_or("LOG_LEVEL", "info"));
    builder.filter_module("tracing", log::LevelFilter::Warn);
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
    std::fs::create_dir_all("config").expect("Failed to create config directory");

    // Also create the database file if it doesn't exist
    let db_path = db_url.strip_prefix("sqlite:").unwrap_or(&db_url);
    if !std::path::Path::new(db_path).exists() {
        std::fs::File::create(db_path).expect("Failed to create database file");
    }

    // Connect to SQLite database with proper error handling
    let db_pool = sqlx::SqlitePool::connect(&db_url).await
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
            commands: vec![join(), leave(), stop(), speak(), random(), audio(), restart(), rename(), avatar()],
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
                    // Clear ALL global slash commands on startup. If any were left from
                    // a previous build that used register_globally(), they would
                    // appear as duplicates alongside the per-guild commands we set
                    // below. Setting an empty list removes them.
                    if let serenity::FullEvent::CacheReady { .. } = event {
                        log::info!("Cache ready, clearing global commands and syncing per-guild");
                        let http = ctx.http.clone();
                        // Remove all global commands (sets to empty list).
                        // If any were left from a previous build that used
                        // register_globally(), they would appear as duplicates
                        // alongside the per-guild commands we set below.
                        if let Err(e) = serenity::Command::set_global_commands(&http, Vec::new()).await {
                            log::warn!("Failed to clear global commands: {}", e);
                        } else {
                            log::info!("Global commands cleared successfully");
                        }
                    }
                    // Sync commands per-guild when the cache is ready (all guilds at once).
                    // This makes slash commands available instantly in each guild,
                    // instead of waiting up to 1 hour for global registration to roll out.
                    if let serenity::FullEvent::CacheReady { guilds, .. } = event {
                        log::info!("Syncing commands to {} guild(s)", guilds.len());
                        let commands = poise::builtins::create_application_commands(&framework.options().commands);
                        let http = ctx.http.clone();
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
                    if let serenity::FullEvent::InteractionCreate { interaction: serenity::Interaction::Component(component) } = event {
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
                                    let playback_path = if std::path::Path::new(file_path).exists() {
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
                                        if std::path::Path::new(&permanent).exists() {
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
                                                handler.play_only(source.into());
                                                log::info!("Audio playback started in guild {}", guild_id);
                                            }
                                        } else {
                                            log::info!("Playing audio file: {}", playback_path);
                                            let source = songbird::input::File::new(playback_path.clone());
                                            handler.play_only(source.into());
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
                                                handler.play_only(source.into());
                                                log::info!("Audio playback started in guild {}", guild_id);
                                            }
                                        }
                                    }
                                }
                            // Use let _ to ignore edit_response errors — the interaction
                            // token may have expired if the user clicked after a long delay,
                            // and playback already succeeded at this point.
                            let _ = component.edit_response(ctx, serenity::EditInteractionResponse::new().content(&data.lang.replaying_audio)).await;
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
                
                // Initialize error tracker and language settings
                let lang = lang::Lang::new();
                let error_tracker = ErrorTracker::new();
                
                log::info!("Framework setup complete with enhanced error tracking");
                Logger::info("INIT", "Error tracking system initialized successfully");
                
                Ok(Data { 
                    db_pool, 
                    lang,
                    error_tracker,
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
