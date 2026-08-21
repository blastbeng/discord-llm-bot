mod database;
mod generator;
mod tts;
use poise::serenity_prelude as serenity;
use std::env;

// Data stored in the bot's context
pub struct Data {
    pub db_pool: sqlx::SqlitePool,
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Context<'a> = poise::Context<'a, Data, Error>;

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
            commands: vec![join(), leave(), stop()],
            ..Default::default()
        })
        .setup(move |ctx, _ready, framework| {
            let db_pool = db_pool.clone();
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
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
