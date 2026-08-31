//! Self-healing for guild timeouts.
//!
//! If a server admin times out the bot (`communication_disabled_until` in the
//! future), the bot cannot send messages in the guild — slash-command replies
//! and followups fail. The bot has ADMINISTRATOR, so it can clear its own
//! timeout (removing a timeout requires MODERATE_MEMBERS, implied by
//! ADMINISTRATOR). Every command path calls [`ensure_bot_not_timed_out`] via
//! the centralized playback entry points (mirroring [`crate::voice_mute`]).
//!
//! Detection ALWAYS uses a REST member fetch (GET /guilds/{id}/members/{bot}),
//! never the cache: the bot does not subscribe to GUILD_MEMBER_UPDATE events
//! (privileged GUILD_MEMBERS intent is not enabled), so a freshly applied
//! timeout would not reach the cache and a stale entry could also read as
//! "not timed out". A lightweight conditional GET is not available here, so
//! the fetch happens once per invocation; commands are user-driven and
//! infrequent, so the extra call is acceptable.

use serenity::builder::EditMember;
use serenity::model::guild::Member;
use serenity::model::id::RoleId;
use serenity::model::permissions::Permissions;
use serenity::model::timestamp::Timestamp;
use serenity::prelude::Context;

/// Whether a permission set grants the right to remove timeouts.
fn grants_untimeout(p: Permissions) -> bool {
    p.contains(Permissions::ADMINISTRATOR) || p.contains(Permissions::MODERATE_MEMBERS)
}

/// Determine whether `member` can self-untimeout in this guild.
///
/// Discord sometimes returns `permissions: null` in the member payload
/// (observed live), so when that field is absent we compute effective
/// permissions from the guild's role list, restricted to the member's roles.
async fn member_has_untimeout_right(ctx: &Context, guild_id: serenity::model::id::GuildId, m: &Member) -> bool {
    if let Some(p) = m.permissions {
        return grants_untimeout(p);
    }

    match ctx.http.get_guild_roles(guild_id).await {
        Ok(roles) => {
            let member_roles: std::collections::HashSet<RoleId> = m.roles.iter().copied().collect();
            roles
                .iter()
                .filter(|r| member_roles.contains(&r.id))
                .any(|r| grants_untimeout(r.permissions))
        }
        Err(e) => {
            log::warn!(
                "voice_timeout: bot is timed out in guild {} but role lookup failed: {}",
                guild_id, e
            );
            false
        }
    }
}

/// True when `communication_disabled_until` is set and in the future.
/// The field is `None` (or a past/near-past timestamp) when not timed out.
fn is_timed_out(until: Option<Timestamp>) -> bool {
    until.is_some_and(|t| t > Timestamp::now())
}

/// Remove the bot's timeout in `guild_id` if it is currently timed out and has
/// the permission to do so (ADMINISTRATOR or MODERATE_MEMBERS).
///
/// Always fetches the bot's member over REST to detect the timeout (see the
/// module docs for why the cache is not trusted here). On failure (e.g.
/// missing permission) it logs a warning and returns; the calling command
/// proceeds and may fail with "missing permissions" as before.
pub async fn ensure_bot_not_timed_out(ctx: &Context, guild_id: serenity::model::id::GuildId) {
    let bot_user_id = ctx.cache.current_user().id;

    let member = match ctx.http.get_member(guild_id, bot_user_id).await {
        Ok(m) => m,
        Err(e) => {
            log::warn!(
                "voice_timeout: member lookup failed in guild {}: {}",
                guild_id, e
            );
            return;
        }
    };

    if !is_timed_out(member.communication_disabled_until) {
        return;
    }

    let has_perm = member_has_untimeout_right(ctx, guild_id, &member).await;

    if !has_perm {
        log::warn!(
            "voice_timeout: bot is timed out in guild {} but lacks ADMINISTRATOR/MODERATE_MEMBERS; cannot self-untimeout",
            guild_id
        );
        return;
    }

    // Removing a timeout is done via the Edit Guild Member endpoint
    // (PATCH /guilds/{id}/members/{user.id}) with
    // `"communication_disabled_until": null` — exactly what
    // EditMember::enable_communication() builds (it intentionally does NOT
    // skip-serialize the field when it is explicitly set, so the null is sent
    // and Discord clears the timeout). Requires MODERATE_MEMBERS/ADMINISTRATOR,
    // which is what the check above verifies.
    match ctx
        .http
        .edit_member(guild_id, bot_user_id, &EditMember::default().enable_communication(), None)
        .await {
        Ok(_) => {
            log::info!(
                "voice_timeout: bot was timed out in guild {}, self-untimeout applied",
                guild_id
            );
        }
        Err(e) => {
            log::warn!("voice_timeout: failed to self-untimeout in guild {}: {}", guild_id, e);
        }
    }
}