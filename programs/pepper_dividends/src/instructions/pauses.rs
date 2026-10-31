//! The pauses (spec §4.3): the funding's and the claims'. The pauser, a hot key, and the admin
//! pause at once; only the admin, the upgrade Squad's vault, ends a pause. A claims pause only
//! starts or extends, at most 7 days from each call, and the resume takes no argument, so a vault
//! transaction executed 48 hours or more after it was written leaves the time it runs.

use anchor_lang::prelude::*;

use crate::constants::CONFIG_SEED;
use crate::errors::DividendsError;
use crate::events::{ClaimsPaused, FundingPauseSet};
use crate::state::DividendsConfig;
use crate::validation::{check_claims_pause, known_version, resumed_claims_paused_until};

/// The accounts of the three pauses: who signs, and the config.
#[derive(Accounts)]
pub struct Pause<'info> {
    /// The pauser or the admin; each instruction checks which may act.
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = known_version(config.version) @ DividendsError::UnsupportedVersion,
    )]
    pub config: Box<Account<'info, DividendsConfig>>,
}

/// Whether `by` may start a pause: the pauser or the admin.
fn starts_a_pause(by: &Pubkey, config: &DividendsConfig) -> bool {
    *by == config.pauser || *by == config.admin
}

/// The funding pause: set by the pauser or the admin, cleared by the admin alone
/// (`Unauthorized` otherwise). Emits `FundingPauseSet`.
pub fn set_paused(ctx: Context<Pause>, paused: bool) -> Result<()> {
    let by = ctx.accounts.authority.key();
    let config = &mut ctx.accounts.config;
    let allowed = if paused { starts_a_pause(&by, config) } else { by == config.admin };
    require!(allowed, DividendsError::Unauthorized);
    config.paused = paused;
    emit!(FundingPauseSet { paused, by });
    Ok(())
}

/// Claims paused until `until`, by the pauser or the admin (`Unauthorized` otherwise), which only
/// starts or extends a pause, for an hour at least:
/// `max(now + 1 hour, claims_paused_until) ≤ until ≤ now + 7 days` (`PauseEndTooEarly`,
/// `PauseTooShort`, `PauseTooLong`). Emits `ClaimsPaused` with the value it leaves.
pub fn pause_claims(ctx: Context<Pause>, until: i64) -> Result<()> {
    let by = ctx.accounts.authority.key();
    let config = &mut ctx.accounts.config;
    require!(starts_a_pause(&by, config), DividendsError::Unauthorized);
    check_claims_pause(until, Clock::get()?.unix_timestamp, config.claims_paused_until)?;
    config.claims_paused_until = until;
    emit!(ClaimsPaused { until, by });
    Ok(())
}

/// Claims open again, by the admin alone (`Unauthorized` otherwise): `claims_paused_until`
/// becomes `min(claims_paused_until, now)`, the time it runs while a pause runs, and the value as
/// it was once that pause has ended, so a resume moves no close's 30-day guard for nothing and
/// fails nothing. Emits `ClaimsPaused` with the value it leaves.
pub fn resume_claims(ctx: Context<Pause>) -> Result<()> {
    let by = ctx.accounts.authority.key();
    let config = &mut ctx.accounts.config;
    require!(by == config.admin, DividendsError::Unauthorized);
    let resumed = resumed_claims_paused_until(config.claims_paused_until, Clock::get()?.unix_timestamp);
    config.claims_paused_until = resumed;
    emit!(ClaimsPaused { until: resumed, by });
    Ok(())
}
