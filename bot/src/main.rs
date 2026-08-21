mod database;
mod generator;
mod lang;
mod tts;
use poise::serenity_prelude as serenity;
use base64::{engine::general_purpose, Engine as _};
use rand::seq::SliceRandom;
use std::env;
use sysinfo::{System, SystemExt};

// Data stored in the bot's context
pub struct Data {
    pub db_pool: sqlx::SqlitePool,
    pub lang: lang::Lang,
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Context<'a> = poise::Context<'a, Data, Error>;

async fn change_presence_loop(ctx: serenity::Context) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(6 * 60 * 60));
    loop {
        interval.tick().await;
        let url = "https://steamspy.com/api.php?request=top100in2weeks";
        let client = reqwest::Client::new();
        if let Ok(resp) = client.get(url).send().await {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(obj) = json.as_object() {
                    let games: Vec<String> = obj.values().filter_map(|v| v["name"].as_str().map(|s| s.to_string())).collect();
                    if let Some(game) = games.choose(&mut rand::thread_rng()) {
                        let activity = serenity::Activity::playing(game);
                        ctx.set_activity(Some(activity));
                    }
                }
            }
        }
    }
}

fn get_current_guild_id(guild_id: serenity::GuildId) -> String {
    let parent_guild_id = env::var("GUILD_ID").unwrap_or_default();
    if guild_id.to_string() == parent_guild_id {
        "000000".to_string()
    } else {
        guild_id.to_string()
    }
}

async fn get_queue_message() -> String {
    let mut sys = System::new_all();
    sys.refresh_all();
    let cpu_usage = sys.global_cpu_info().cpu_usage();
    let total_memory = sys.total_memory();
    let used_memory = sys.used_memory();
    let ram_usage = (used_memory as f64 / total_memory as f64) * 100.0;
    format!("\n\nSe il server é sovraccarico, potrebbe volerci un po' di tempo\n*CPU: {:.1}% - RAM: {:.2}%*", cpu_usage, ram_usage)
}

async fn check_permissions(ctx: Context<'_>) -> Result<(), Error> {
    let guild = ctx.guild().ok_or("Guild not found")?;
    let channel_id = guild.voice_states.get(&ctx.author().id).and_then(|vs| vs.channel_id).ok_or("You must be in a voice channel")?;
    let perms = channel_id.to_channel(ctx.http()).await?.permissions_for_user(ctx.http(), ctx.cache().current_user_id())?;
    if !perms.speak() {
        return Err("I don't have permission to speak in this channel".into());
    }
    Ok(())
}

async fn connect_bot_by_voice_client(ctx: Context<'_>, channel_id: serenity::ChannelId) -> Result<(), Error> {
    let guild = ctx.guild().ok_or("Guild not found")?;
    let manager = songbird::get(ctx.serenity_context()).await.unwrap();
    let handler_lock = manager.join(guild.id, channel_id).await?;
    let mut handler = handler_lock.lock().await;
    // If the bot is already in a different channel, it will be moved to the new channel.
    Ok(())
}

async fn voice_autocomplete(
    _ctx: Context<'_>,
    current: str,
) -> Vec<poise::AutocompleteChoice<String>> {
    let voices = vec![
        "Google",
        "Goku (FakeYou.com)",
        "Gerry Scotti (FakeYou.com)",
        "Homer Simpson (FakeYou.com)",
        "Peter Griffin (FakeYou.com)",
        "Papa Francesco (FakeYou.com)",
        "Silvio Berlusconi (FakeYou.com)",
        "random",
    ];

    voices.into_iter()
        .filter(|v| v.to_lowercase().contains(&current.to_lowercase()))
        .map(|v| poise::AutocompleteChoice { name: v.to_string(), value: v.to_string() })
        .collect()
}

/// Join channel.
#[poise::command(slash_command, cooldown = 5)]
async fn join(ctx: Context<'_>) -> Result<(), Error> {
    check_permissions(ctx).await?;
    let guild = ctx.guild().ok_or("Guild not found")?;
    let channel_id = guild.voice_states.get(&ctx.author().id).and_then(|vs| vs.channel_id).ok_or("You must be in a voice channel")?;
    
    let manager = songbird::get(ctx.serenity_context()).await.unwrap();
    if let Some(_) = manager.get(guild.id) {
        manager.remove(guild.id).await;
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    let handler = manager.join(guild.id, channel_id).await;
    if handler.is_ok() {
        ctx.say(&ctx.data().lang.join_success).await?;
    } else {
        ctx.say(&ctx.data().lang.join_error).await?;
    }
    Ok(())
}

/// Leave channel
#[poise::command(slash_command, cooldown = 5)]
async fn leave(ctx: Context<'_>) -> Result<(), Error> {
    check_permissions(ctx).await?;
    let guild_id = ctx.guild_id().unwrap();
    let manager = songbird::get(ctx.serenity_context()).await.unwrap();
    if manager.get(guild_id).is_some() {
        manager.remove(guild_id).await;
        ctx.say(&ctx.data().lang.leave_success).await?;
    } else {
        ctx.say(&ctx.data().lang.not_connected).await?;
    }
    Ok(())
}

/// Stop playback.
#[poise::command(slash_command, cooldown = 5)]
async fn stop(ctx: Context<'_>) -> Result<(), Error> {
    check_permissions(ctx).await?;
    let guild_id = ctx.guild_id().unwrap();
    let manager = songbird::get(ctx.serenity_context()).await.unwrap();
    if let Some(handler) = manager.get(guild_id) {
        let handler = handler.lock().await;
        handler.stop();
        ctx.say(&ctx.data().lang.stop_success).await?;
    } else {
        ctx.say(&ctx.data().lang.not_connected).await?;
    }
    Ok(())
}

/// Repeat a sentence
#[poise::command(slash_command, cooldown = 5)]
async fn speak(
    ctx: Context<'_>,
    #[description = "La frase da ripetere"] text: String,
    #[description = "La voce da usare"]
    #[autocomplete = "voice_autocomplete"]
    voice: Option<String>,
) -> Result<(), Error> {
    let voice = voice.unwrap_or_else(|| "Google".to_string());
    let voices = [
        "Google",
        "Goku (FakeYou.com)",
        "Gerry Scotti (FakeYou.com)",
        "Homer Simpson (FakeYou.com)",
        "Peter Griffin (FakeYou.com)",
        "Papa Francesco (FakeYou.com)",
        "Silvio Berlusconi (FakeYou.com)",
    ];
    let actual_voice = if voice == "random" {
        let mut rng = rand::thread_rng();
        voices.choose(&mut rng).unwrap().to_string()
    } else {
        voice
    };

    check_permissions(ctx).await?;
    let guild = ctx.guild().ok_or("Guild not found")?;
    log::info!("[GUILDID : {}] speak - text: {}, voice: {}", get_current_guild_id(guild.id), text, actual_voice);
    let channel_id = guild.voice_states.get(&ctx.author().id).and_then(|vs| vs.channel_id).ok_or("You must be in a voice channel")?;
    connect_bot_by_voice_client(ctx, channel_id).await?;
    let manager = songbird::get(ctx.serenity_context()).await.unwrap();
    let handler_lock = manager.get(guild.id).unwrap();
    let mut handler = handler_lock.lock().await;

    ctx.defer_ephemeral().await?;
    let queue_msg = get_queue_message().await;
    let initial_msg = format!("Inizio a generare l'audio per la frase: **{}**{}", text, queue_msg);
    let reply = ctx.send(poise::CreateReply::default().content(initial_msg).ephemeral(true)).await?;
    let message_id = reply.message().await?.id;

    let file_path = match tts::get_or_generate_tts(&text, &actual_voice).await {
        Ok(path) => path,
        Err(e) => {
            log::error!("TTS generation failed: {}", e);
            ctx.say(&ctx.data().lang.tts_error).await?;
            return Ok(());
        }
    };
    
    database::insert_sentence(&ctx.data().db_pool, &text).await?;

    let source = songbird::input::File::new(&file_path);
    handler.play_only(source.into());

    let components = vec![
        serenity::CreateActionRow::Buttons(vec![
            serenity::CreateButton::new(format!("play:{}:{}", text, actual_voice))
                .label("Play")
                .style(serenity::ButtonStyle::Success),
            serenity::CreateButton::new("stop")
                .label("Stop")
                .style(serenity::ButtonStyle::Danger)
        ])
    ];

    ctx.http().edit_message(
        ctx.channel_id(),
        message_id,
        serenity::EditMessage::new()
            .content(format!(&ctx.data().lang.playing, text, actual_voice))
            .components(components)
    ).await?;

    Ok(())
}

/// Say a random sentence
#[poise::command(slash_command, cooldown = 5)]
async fn random(
    ctx: Context<'_>,
    #[description = "La voce da usare"]
    #[autocomplete = "voice_autocomplete"]
    voice: Option<String>,
    #[description = "Il testo da cercare"] text: Option<String>,
) -> Result<(), Error> {
    let voice = voice.unwrap_or_else(|| "Google".to_string());
    let voices = [
        "Google",
        "Goku (FakeYou.com)",
        "Gerry Scotti (FakeYou.com)",
        "Homer Simpson (FakeYou.com)",
        "Peter Griffin (FakeYou.com)",
        "Papa Francesco (FakeYou.com)",
        "Silvio Berlusconi (FakeYou.com)",
    ];
    let actual_voice = if voice == "random" {
        let mut rng = rand::thread_rng();
        voices.choose(&mut rng).unwrap().to_string()
    } else {
        voice
    };

    check_permissions(ctx).await?;
    let guild = ctx.guild().ok_or("Guild not found")?;
    log::info!("[GUILDID : {}] random - voice: {}, text: {:?}", get_current_guild_id(guild.id), actual_voice, text);
    let channel_id = guild.voice_states.get(&ctx.author().id).and_then(|vs| vs.channel_id).ok_or("You must be in a voice channel")?;
    connect_bot_by_voice_client(ctx, channel_id).await?;
    let manager = songbird::get(ctx.serenity_context()).await.unwrap();
    let handler_lock = manager.get(guild.id).unwrap();
    let mut handler = handler_lock.lock().await;

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
        ctx.say(&ctx.data().lang.no_sentence).await?;
        return Ok(());
    }

    let mut rng = rand::thread_rng();
    let random_sentence = sentences.choose(&mut rng).unwrap().to_string();

    ctx.defer_ephemeral().await?;
    let queue_msg = get_queue_message().await;
    let initial_msg = format!("Sto cercando una frase casuale{}", queue_msg);
    let reply = ctx.send(poise::CreateReply::default().content(initial_msg).ephemeral(true)).await?;
    let message_id = reply.message().await?.id;

    let file_path = match tts::get_or_generate_tts(&random_sentence, &actual_voice).await {
        Ok(path) => path,
        Err(e) => {
            log::error!("TTS generation failed: {}", e);
            ctx.say(&ctx.data().lang.tts_error).await?;
            return Ok(());
        }
    };

    let source = songbird::input::File::new(&file_path);
    handler.play_only(source.into());

    let components = vec![
        serenity::CreateActionRow::Buttons(vec![
            serenity::CreateButton::new(format!("play:{}:{}", random_sentence, actual_voice))
                .label("Play")
                .style(serenity::ButtonStyle::Success),
            serenity::CreateButton::new("stop")
                .label("Stop")
                .style(serenity::ButtonStyle::Danger)
        ])
    ];

    ctx.http().edit_message(
        ctx.channel_id(),
        message_id,
        serenity::EditMessage::new()
            .content(format!(&ctx.data().lang.playing, random_sentence, actual_voice))
            .components(components)
    ).await?;

    Ok(())
}

/// Audio playback from the input audio
#[poise::command(slash_command, cooldown = 5)]
async fn audio(
    ctx: Context<'_>,
    #[description = "Il file audio (mp3 or wav)"] audio: serenity::Attachment,
) -> Result<(), Error> {
    check_permissions(ctx).await?;
    let guild = ctx.guild().ok_or("Guild not found")?;
    let channel_id = guild.voice_states.get(&ctx.author().id).and_then(|vs| vs.channel_id).ok_or("You must be in a voice channel")?;
    
    ctx.defer_ephemeral().await?;

    connect_bot_by_voice_client(ctx, channel_id).await?;
    let manager = songbird::get(ctx.serenity_context()).await.unwrap();
    let handler_lock = manager.get(guild.id).unwrap();
    let mut handler = handler_lock.lock().await;

    let allowed_extensions = ["mp3", "wav", "ogg", "m4a"];
    let ext = audio.filename.split('.').last().unwrap_or("").to_lowercase();
    if !allowed_extensions.contains(&ext.as_str()) {
        ctx.say(&ctx.data().lang.invalid_extension).await?;
        return Ok(());
    }

    log::info!("[GUILDID : {}] audio - filename: {}", get_current_guild_id(guild.id), audio.filename);

    let temp_dir = std::env::var("TMP_DIR").unwrap_or_else(|_| "/tmp/discord-llm-bot".to_string());
    let file_path = format!("{}/{}", temp_dir, audio.filename);
    
    // Download the attachment
    let bytes = reqwest::get(&audio.url).await?.bytes().await?.to_vec();
    std::fs::write(&file_path, &bytes)?;

    let source = songbird::input::File::new(&file_path);
    handler.play_only(source.into());

    ctx.send(poise::CreateReply::default().content(&ctx.data().lang.audio_playback).ephemeral(true)).await?;
    Ok(())
}

/// Restart bot.
#[poise::command(slash_command, cooldown = 5)]
async fn restart(ctx: Context<'_>) -> Result<(), Error> {
    let admin_id = env::var("ADMIN_ID").expect("ADMIN_ID must be set");
    let guild_id = env::var("GUILD_ID").expect("GUILD_ID must be set");
    if ctx.guild_id().unwrap().to_string() != guild_id || ctx.author().id.to_string() != admin_id {
        ctx.say(&ctx.data().lang.admin_parent_server).await?;
        return Ok(());
    }
    let member = ctx.guild().unwrap().member(ctx.http(), ctx.author().id).await?;
    if !member.permissions(ctx.http()).await?.administrator() {
        ctx.say(&ctx.data().lang.admin_only).await?;
        return Ok(());
    }
    ctx.say(&ctx.data().lang.restarting).await?;
    std::process::exit(0);
}

/// Rename bot.
#[poise::command(slash_command, cooldown = 5)]
async fn rename(
    ctx: Context<'_>,
    #[description = "Nuovo nickname del bot (limite di 32 caratteri)"] name: String,
) -> Result<(), Error> {
    if name.chars().count() > 32 {
        ctx.say(&ctx.data().lang.nickname_too_long).await?;
        return Ok(());
    }
    let guild = ctx.guild().ok_or("Guild not found")?;
    guild.edit_nickname(ctx.http(), Some(&name), None).await?;
    ctx.say(format!(&ctx.data().lang.nickname_changed, name)).await?;
    Ok(())
}

/// Change bot avatar.
#[poise::command(slash_command, cooldown = 5)]
async fn avatar(
    ctx: Context<'_>,
    #[description = "Nuovo avatar del bot"] image: serenity::Attachment,
) -> Result<(), Error> {
    let admin_id = env::var("ADMIN_ID").expect("ADMIN_ID must be set");
    let guild_id = env::var("GUILD_ID").expect("GUILD_ID must be set");
    if ctx.guild_id().unwrap().to_string() != guild_id || ctx.author().id.to_string() != admin_id {
        ctx.say(&ctx.data().lang.admin_parent_server).await?;
        return Ok(());
    }

    if !image.content_type.as_deref().map_or(false, |ct| ct.starts_with("image/")) {
        ctx.say(&ctx.data().lang.unsupported_file).await?;
        return Ok(());
    }

    let bytes = reqwest::get(&image.url).await?.bytes().await?.to_vec();
    let b64 = general_purpose::STANDARD.encode(&bytes);
    let data_url = format!("data:image/png;base64,{}", b64);

    ctx.http().edit_user(serenity::EditUser::new().avatar(&data_url)).await?;
    ctx.say(&ctx.data().lang.avatar_changed).await?;
    Ok(())
}

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();
    env_logger::init();

    let token = env::var("BOT_TOKEN").expect("BOT_TOKEN must be set");
    let guild_id = env::var("GUILD_ID").expect("GUILD_ID must be set")
        .parse::<serenity::GuildId>().expect("Invalid GUILD_ID");

    // Initialize database pool (we will create the schema in the next step)
    let db_url = "sqlite:config/discord-bot.sqlite3";
    let db_pool = sqlx::SqlitePool::connect(db_url).await.expect("Failed to connect to DB");
    database::init_db(&db_pool).await.expect("Failed to initialize database");
    database::populate_db_if_empty(&db_pool).await.expect("Failed to populate database");

    let pool_clone = db_pool.clone();
    tokio::spawn(async move {
        generator::run_background_generator(pool_clone).await;
    });

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![join(), leave(), stop(), speak(), random(), audio(), restart(), rename(), avatar()],
            on_error: |error| {
                Box::pin(async move {
                    if let poise::FrameworkError::CooldownHit { remaining_cooldown, ctx, .. } = error {
                        let msg = format!(&ctx.data().lang.spam_detected, ctx.author().id, remaining_cooldown.as_secs());
                        let _ = ctx.send(poise::CreateReply::default().content(msg).ephemeral(true)).await;
                    } else {
                        log::error!("Error: {:?}", error);
                        if let poise::FrameworkError::Command { ctx, error, .. } = error {
                            log::error!("Command error: {}", error);
                            let _ = ctx.send(poise::CreateReply::default().content("Discord API Error, per favore riprova piú tardi").ephemeral(true)).await;
                        }
                    }
                })
            },
            event_handler: |ctx, event, _framework, data| {
                Box::pin(async move {
                    if let serenity::FullEvent::InteractionCreate { interaction } = event {
                        if let serenity::Interaction::Component(component) = interaction {
                            if component.data.custom_id == "stop" {
                                if let Some(guild_id) = component.guild_id {
                                    let manager = songbird::get(ctx).await.unwrap();
                                    if let Some(handler) = manager.get(guild_id) {
                                        let handler = handler.lock().await;
                                        handler.stop();
                                    }
                                }
                                component.create_response(ctx, serenity::CreateInteractionResponse::Message(
                                    serenity::CreateInteractionResponseMessage::new().content(&data.lang.stop_success).ephemeral(true)
                                )).await?;
                            } else if component.data.custom_id.starts_with("play:") {
                                let parts: Vec<&str> = component.data.custom_id.splitn(3, ':').collect();
                                if parts.len() == 3 {
                                    let text = parts[1];
                                    let voice = parts[2];
                                    if let Some(guild_id) = component.guild_id {
                                        let manager = songbird::get(ctx).await.unwrap();
                                        if let Some(handler) = manager.get(guild_id) {
                                            let mut handler = handler.lock().await;
                                            let file_path = tts::get_or_generate_tts(text, voice).await?;
                                            let source = songbird::input::File::new(&file_path);
                                            handler.play_only(source.into());
                                        }
                                    }
                                    component.create_response(ctx, serenity::CreateInteractionResponse::Message(
                                        serenity::CreateInteractionResponseMessage::new().content(format!(&data.lang.playing, text, voice)).ephemeral(true)
                                    )).await?;
                                }
                            }
                        }
                    }
                    Ok(())
                })
            },
            ..Default::default()
        })
        .setup(move |ctx, _ready, framework| {
            let db_pool = db_pool.clone();
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                let ctx_clone = ctx.clone();
                tokio::spawn(async move {
                    change_presence_loop(ctx_clone).await;
                });
                let lang = lang::Lang::new();
                Ok(Data { db_pool, lang })
            })
        })
        .build();

    let intents = serenity::GatewayIntents::non_privileged() | serenity::GatewayIntents::MESSAGE_CONTENT;
    
    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .register_songbird()
        .await
        .expect("Error creating client");

    client.start().await.unwrap();
}
