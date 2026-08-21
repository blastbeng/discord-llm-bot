mod database;
mod generator;
mod tts;
use poise::serenity_prelude as serenity;
use base64::{engine::general_purpose, Engine as _};
use rand::seq::SliceRandom;
use std::env;

// Data stored in the bot's context
pub struct Data {
    pub db_pool: sqlx::SqlitePool,
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
#[poise::command(slash_command)]
async fn join(ctx: Context<'_>) -> Result<(), Error> {
    let guild = ctx.guild().ok_or("Guild not found")?;
    let channel_id = guild.voice_states.get(&ctx.author().id).and_then(|vs| vs.channel_id).ok_or("You must be in a voice channel")?;
    
    let manager = songbird::get(ctx.serenity_context()).await.unwrap();
    let handler = manager.join(guild.id, channel_id).await;
    if handler.is_ok() {
        ctx.say("Sto entrando nel canale").await?;
    } else {
        ctx.say("Errore nell'entrare nel canale").await?;
    }
    Ok(())
}

/// Leave channel
#[poise::command(slash_command)]
async fn leave(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let manager = songbird::get(ctx.serenity_context()).await.unwrap();
    if manager.get(guild_id).is_some() {
        manager.remove(guild_id).await;
        ctx.say("Sto lasciando il canale").await?;
    } else {
        ctx.say("Non sono connesso a nessun canale").await?;
    }
    Ok(())
}

/// Stop playback.
#[poise::command(slash_command)]
async fn stop(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let manager = songbird::get(ctx.serenity_context()).await.unwrap();
    if let Some(handler) = manager.get(guild_id) {
        let handler = handler.lock().await;
        handler.stop();
        ctx.say("Interrompo il bot").await?;
    } else {
        ctx.say("Non sono connesso a nessun canale").await?;
    }
    Ok(())
}

/// Repeat a sentence
#[poise::command(slash_command)]
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

    ctx.defer_ephemeral().await?;

    let guild = ctx.guild().ok_or("Guild not found")?;
    let channel_id = guild.voice_states.get(&ctx.author().id).and_then(|vs| vs.channel_id).ok_or("You must be in a voice channel")?;

    let manager = songbird::get(ctx.serenity_context()).await.unwrap();
    let handler_lock = manager.join(guild.id, channel_id).await?;
    let mut handler = handler_lock.lock().await;

    let file_path = tts::get_or_generate_tts(&text, &actual_voice).await?;
    
    database::insert_sentence(&ctx.data().db_pool, &text).await?;

    let source = songbird::input::File::new(&file_path);
    handler.play_only(source.into());

    ctx.say(format!("Sto riproducendo: **{}** con voce: {}", text, actual_voice)).await?;
    Ok(())
}

/// Say a random sentence
#[poise::command(slash_command)]
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

    ctx.defer_ephemeral().await?;

    let guild = ctx.guild().ok_or("Guild not found")?;
    let channel_id = guild.voice_states.get(&ctx.author().id).and_then(|vs| vs.channel_id).ok_or("You must be in a voice channel")?;

    let manager = songbird::get(ctx.serenity_context()).await.unwrap();
    let handler_lock = manager.join(guild.id, channel_id).await?;
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
        ctx.say("Nessuna frase trovata").await?;
        return Ok(());
    }

    let mut rng = rand::thread_rng();
    let random_sentence = sentences.choose(&mut rng).unwrap();

    let file_path = tts::get_or_generate_tts(random_sentence, &actual_voice).await?;

    let source = songbird::input::File::new(&file_path);
    handler.play_only(source.into());

    ctx.say(format!("Sto riproducendo: **{}** con voce: {}", random_sentence, actual_voice)).await?;
    Ok(())
}

/// Audio playback from the input audio
#[poise::command(slash_command)]
async fn audio(
    ctx: Context<'_>,
    #[description = "Il file audio (mp3 or wav)"] audio: serenity::Attachment,
) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;

    let guild = ctx.guild().ok_or("Guild not found")?;
    let channel_id = guild.voice_states.get(&ctx.author().id).and_then(|vs| vs.channel_id).ok_or("You must be in a voice channel")?;

    let manager = songbird::get(ctx.serenity_context()).await.unwrap();
    let handler_lock = manager.join(guild.id, channel_id).await?;
    let mut handler = handler_lock.lock().await;

    let temp_dir = std::env::var("TMP_DIR").unwrap_or_else(|_| "/tmp/discord-llm-bot".to_string());
    let file_path = format!("{}/{}", temp_dir, audio.filename);
    
    // Download the attachment
    let bytes = reqwest::get(&audio.url).await?.bytes().await?.to_vec();
    std::fs::write(&file_path, &bytes)?;

    let source = songbird::input::File::new(&file_path);
    handler.play_only(source.into());

    ctx.say("Done! I'm starting the audio playback!").await?;
    Ok(())
}

/// Restart bot.
#[poise::command(slash_command)]
async fn restart(ctx: Context<'_>) -> Result<(), Error> {
    let admin_id = env::var("ADMIN_ID").expect("ADMIN_ID must be set");
    let guild_id = env::var("GUILD_ID").expect("GUILD_ID must be set");
    if ctx.guild_id().unwrap().to_string() != guild_id || ctx.author().id.to_string() != admin_id {
        ctx.say("Solo gli amministratori possono utilizzare questo comando nel server padre").await?;
        return Ok(());
    }
    ctx.say("Sto riavviando il bot.").await?;
    std::process::exit(0);
}

/// Rename bot.
#[poise::command(slash_command)]
async fn rename(
    ctx: Context<'_>,
    #[description = "Nuovo nickname del bot (limite di 32 caratteri)"] name: String,
) -> Result<(), Error> {
    if name.len() > 32 {
        ctx.say("Il mio nickname non puó essere piú lungo di 32 caratteri").await?;
        return Ok(());
    }
    let guild = ctx.guild().ok_or("Guild not found")?;
    guild.edit_nickname(ctx.http(), Some(&name), None).await?;
    ctx.say(format!("Mi hai rinominato in \"{}\"", name)).await?;
    Ok(())
}

/// Change bot avatar.
#[poise::command(slash_command)]
async fn avatar(
    ctx: Context<'_>,
    #[description = "Nuovo avatar del bot"] image: serenity::Attachment,
) -> Result<(), Error> {
    let admin_id = env::var("ADMIN_ID").expect("ADMIN_ID must be set");
    let guild_id = env::var("GUILD_ID").expect("GUILD_ID must be set");
    if ctx.guild_id().unwrap().to_string() != guild_id || ctx.author().id.to_string() != admin_id {
        ctx.say("Solo gli amministratori possono utilizzare questo comando nel server padre").await?;
        return Ok(());
    }

    let bytes = reqwest::get(&image.url).await?.bytes().await?.to_vec();
    let b64 = general_purpose::STANDARD.encode(&bytes);
    let data_url = format!("data:image/png;base64,{}", b64);

    ctx.http().edit_user(serenity::EditUser::new().avatar(&data_url)).await?;
    ctx.say("L'immagine é stata modificata").await?;
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

    let pool_clone = db_pool.clone();
    tokio::spawn(async move {
        generator::run_background_generator(pool_clone).await;
    });

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![join(), leave(), stop(), speak(), random(), audio(), restart(), rename(), avatar()],
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
                Ok(Data { db_pool })
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
