//! Self-healing for server-side voice mutes.
//!
//! If a server admin voice-mutes the bot, every track it plays is silent.
//! All playback paths call [`ensure_bot_not_muted`] right before playing:
//! it checks (from cache) whether the bot is muted and, if so, verifies the
//! bot has the right to unmute itself (ADMINISTRATOR or MUTE_MEMBERS), then
//! clears the mute via a PATCH on its own voice state.

use serenity::model::permissions::Permissions;
use serenity::prelude::Context;

/// Unmute the bot in `guild_id` if it is server-side muted and has the
/// permission to do so.
///
/// Fast path: reads the cached voice state, so no API call is made unless
/// the bot is actually muted. On failure (e.g. missing permission) it logs a
/// warning and returns; playback proceeds as-is (silent, same as before).
pub async fn ensure_bot_not_muted(ctx: &Context, guild_id: serenity::model::id::GuildId) {
    let bot_user_id = ctx.cache.current_user().id;

    // Read the cache inside a block: the returned CacheRef holds a
    // (non-Send) shard guard and must not be held across an await.
    let (is_muted, has_perm) = {
        let Some(guild) = ctx.cache.guild(guild_id) else {
            return;
        };

        let is_muted = guild
            .voice_states
            .get(&bot_user_id)
            .map(|vs| vs.mute)
            .unwrap_or(false);

        // Only act when we have the right (ADMINISTRATOR or MUTE_MEMBERS).
        let has_perm = guild
            .members
            .get(&bot_user_id)
            .and_then(|m| m.permissions)
            .map(|p| {
                p.contains(Permissions::ADMINISTRATOR) || p.contains(Permissions::MUTE_MEMBERS)
            })
            .unwrap_or(false);

        (is_muted, has_perm)
    };

    if !is_muted {
        return;
    }

    if !has_perm {
        log::warn!(
            "voice_mute: bot is server-muted in guild {} but lacks ADMINISTRATOR/MUTE_MEMBERS; cannot self-demute",
            guild_id
        );
        return;
    }

    match ctx.http.edit_voice_state_me(guild_id, &serde_json::json!({ "mute": false })).await {
        Ok(()) => {
            log::info!(
                "voice_mute: bot was server-muted in guild {}, self-demuted before playback",
                guild_id
            );
        }
        Err(e) => {
            log::warn!("voice_mute: failed to self-demute in guild {}: {}", guild_id, e);
        }
    }
}
