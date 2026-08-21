mod database;
use poise::serenity_prelude as serenity;
use std::env;

// Data stored in the bot's context
pub struct Data {
    pub db_pool: sqlx::SqlitePool,
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Context<'a> = poise::Context<'a, Data, Error>;

/// Ping command to test the bot
#[poise::command(slash_command)]
async fn ping(ctx: Context<'_>) -> Result<(), Error> {
    ctx.say("Pong!").await?;
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

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![ping()],
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
        .await
        .expect("Error creating client");

    client.start().await.unwrap();
}
