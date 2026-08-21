use serenity::model::id::GuildId;

pub struct Config {
    pub discord_token: String,
    pub guild_id: GuildId,
    pub admin_id: String,
    pub anything_llm_endpoint: String,
    pub anything_llm_api_key: String,
    pub anything_llm_workspace: String,
}

impl Config {
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let discord_token = std::env::var("BOT_TOKEN")?;
        let guild_id_str = std::env::var("GUILD_ID")?;
        let guild_id = GuildId::new(guild_id_str.parse::<u64>()?);
        let admin_id = std::env::var("ADMIN_ID")?;
        let anything_llm_endpoint = std::env::var("ANYTHING_LLM_ENDPOINT")?;
        let anything_llm_api_key = std::env::var("ANYTHING_LLM_API_KEY")?;
        let anything_llm_workspace = std::env::var("ANYTHING_LLM_WORKSPACE")?;

        Ok(Self {
            discord_token,
            guild_id,
            admin_id,
            anything_llm_endpoint,
            anything_llm_api_key,
            anything_llm_workspace,
        })
    }
}
