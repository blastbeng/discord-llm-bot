mod config;
mod database;

use config::Config;
use database::Database;
use poise::serenity::GatewayIntents;
use poise::FrameworkOptions;

pub struct Data {
    pub config: Config,
    pub db: Database,
    pub http_client: reqwest::Client,
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Context<'a> = poise::Context<'a, Data, Error>;

async fn on_error(error: poise::FrameworkError<'_, Data, Error>) {
    match error {
        poise::FrameworkError::Setup { error, .. } => panic!("Error in user data setup: {}", error),
        poise::FrameworkError::Command { error, ctx, .. } => {
            println!("Error in command `{}`: {:?}", ctx.command().name, error);
        }
        error => {
            if let Some(ctx) = error.ctx() {
                println!("Other error in command `{}`: {:?}", ctx.command().name, error);
            } else {
                println!("Other error: {:?}", error);
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    dotenv::dotenv().ok();
    tracing_subscriber::fmt::init();

    let config = Config::from_env()?;
    let token = config.discord_token.clone();
    let db = Database::new("sqlite:///app/config/discord-bot.sqlite3").await?;
    let http_client = reqwest::Client::new();

    let intents = GatewayIntents::all();

    let framework = poise::Framework::builder()
        .options(FrameworkOptions {
            commands: vec![],
            on_error: |error| {
                Box::pin(on_error(error))
            },
            ..Default::default()
        })
        .setup(move |ctx, _ready, framework| {
            let data = Data {
                config,
                db,
                http_client,
            };
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                Ok(data)
            })
        })
        .build();

    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await?;

    client.start().await?;

    Ok(())
}
