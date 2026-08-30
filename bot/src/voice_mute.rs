//! Self-healing for server-side voice mutes.
//!
//! If a server admin voice-mutes the bot, every track it plays is silent.
//! All playback paths call [`ensure_bot_not_muted`] right before playing:
//! it checks (from cache) whether the bot is muted and, if so, verifies the
//! bot has the right to unmute itself (ADMINISTRATOR or MUTE_MEMBERS), then
//! clears the mute via the Edit Guild Member endpoint
//! (PATCH /guilds/{id}/members/{bot} with `{"mute": false}`) — the REST
//! voice-state endpoints no longer accept `mute`/`deaf` (stage-channel only).

use serenity::builder::EditMember;
use serenity::model::guild::Member;
use serenity::model::id::RoleId;
use serenity::model::permissions::Permissions;
use serenity::prelude::Context;

/// Whether a permission set grants the right to unmute voice members.
fn grants_demute(p: Permissions) -> bool {
    p.contains(Permissions::ADMINISTRATOR) || p.contains(Permissions::MUTE_MEMBERS)
}

/// Determine whether `member` can self-demute in this guild.
///
/// Discord sometimes returns `permissions: null` in the member payload
/// (observed live), so when that field is absent we compute effective
/// permissions from the guild's role list, restricted to the member's roles.
/// This extra API call only happens while the bot is actually muted.
async fn member_has_demute_right(ctx: &Context, guild_id: serenity::model::id::GuildId, m: &Member) -> bool {
    if let Some(p) = m.permissions {
        return grants_demute(p);
    }

    match ctx.http.get_guild_roles(guild_id).await {
        Ok(roles) => {
            let member_roles: std::collections::HashSet<RoleId> = m.roles.iter().copied().collect();
            roles
                .iter()
                .filter(|r| member_roles.contains(&r.id))
                .any(|r| grants_demute(r.permissions))
        }
        Err(e) => {
            log::warn!(
                "voice_mute: bot is server-muted in guild {} but role lookup failed: {}",
                guild_id, e
            );
            false
        }
    }
}

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
    let (is_muted, cached_perm) = {
        let Some(guild) = ctx.cache.guild(guild_id) else {
            return;
        };

        let is_muted = guild
            .voice_states
            .get(&bot_user_id)
            .map(|vs| vs.mute)
            .unwrap_or(false);

        // None = inconclusive (member not cached, or permissions absent).
        let cached_perm = guild
            .members
            .get(&bot_user_id)
            .and_then(|m| m.permissions)
            .map(grants_demute);

        (is_muted, cached_perm)
    };

    if !is_muted {
        return;
    }

    // Only act when we have the right (ADMINISTRATOR or MUTE_MEMBERS).
    // The cache is sometimes inconclusive (member entry missing or
    // permissions field absent), so fall back to asking Discord — this
    // extra API call only happens while the bot is actually muted.
    let has_perm = match cached_perm {
        Some(p) => p,
        // NOTE: get_current_user_guild_member (GET /users/@me/guilds/{id}/member)
        // is forbidden for bots; use the regular member endpoint instead.
        None => match ctx.http.get_member(guild_id, bot_user_id).await {
            Ok(m) => member_has_demute_right(ctx, guild_id, &m).await,
            Err(e) => {
                log::warn!(
                    "voice_mute: bot is server-muted in guild {} but permission check failed: {}",
                    guild_id, e
                );
                return;
            }
        },
    };

    if !has_perm {
        log::warn!(
            "voice_mute: bot is server-muted in guild {} but lacks ADMINISTRATOR/MUTE_MEMBERS; cannot self-demute",
            guild_id
        );
        return;
    }

    // Server-side voice mute is set via the Edit Guild Member endpoint
    // (PATCH /guilds/{id}/members/{user.id} with `{"mute": false}`) — the same
    // mechanism discord.js uses for VoiceState#setMute. The REST voice-state
    // endpoints no longer accept `mute`/`deaf` (stage-channel only), and the
    // MUTE_MEMBERS/ADMINISTRATOR check above is exactly what this call requires.
    match ctx
        .http
        .edit_member(guild_id, bot_user_id, &EditMember::default().mute(false), None)
        .await {
        Ok(_) => {
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
