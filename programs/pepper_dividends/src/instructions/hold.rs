//! A month's claims hold (Rick, 2026-10-26: «Con il blocco per mese: si ferma solo il mese
//! sbagliato, e tutti gli altri riscatti continuano normalmente»). The pauser, a hot key, or the
//! admin holds one month's claims at once, outside the time lock, by the global claims pause's
//! rule: at most 7 days from each call, renewed, never shortened. Only the admin, the upgrade
//! Squad's vault, releases it early; the void ends it with the distributor it closes. Every other
//! month and token keeps claiming.

use anchor_lang::prelude::*;

use crate::constants::CONFIG_SEED;
use crate::errors::DividendsError;
use crate::events::ClaimsHeld;
use crate::state::{DividendsConfig, UsdcDistributor};
use crate::validation::{check_claims_pause, known_version, resumed_claims_paused_until};

/// The accounts of a month's hold and its release: who signs, the config, and the month.
#[derive(Accounts)]
pub struct Hold<'info> {
    /// The pauser or the admin; each instruction checks which may act.
    pub authority: Signer<'info>,

    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = known_version(config.version) @ DividendsError::UnsupportedVersion,
    )]
    pub config: Box<Account<'info, DividendsConfig>>,

    /// The month. Only this program creates one, at its own address: its owner and discriminator
    /// bind it.
    #[account(mut, constraint = known_version(distributor.version) @ DividendsError::UnsupportedVersion)]
    pub distributor: Box<Account<'info, UsdcDistributor>>,
}

/// This month's claims held until `until`, by the pauser or the admin (`Unauthorized` otherwise),
/// a hold only started or extended, for an hour at least:
/// `max(now + 1 hour, claims_held_until) ≤ until ≤ now + 7 days` (`PauseEndTooEarly`,
/// `PauseTooShort`, `PauseTooLong`). A month in its review window is held already: a hold extends
/// it. Emits `ClaimsHeld` with the value it leaves.
pub fn hold_claims(ctx: Context<Hold>, until: i64) -> Result<()> {
    let by = ctx.accounts.authority.key();
    let config = &ctx.accounts.config;
    require!(by == config.pauser || by == config.admin, DividendsError::Unauthorized);
    let distributor = &mut ctx.accounts.distributor;
    check_claims_pause(until, Clock::get()?.unix_timestamp, distributor.claims_held_until)?;
    distributor.claims_held_until = until;
    emit!(ClaimsHeld { mint: distributor.mint, month: distributor.month, until, by });
    Ok(())
}

/// This month's claims open again, by the admin alone (`Unauthorized` otherwise):
/// `claims_held_until` becomes `min(claims_held_until, now)`, the time it runs while the hold
/// runs, the value as it was once the hold has ended, so a release after the hold ran out moves
/// this month's close guard nowhere and fails nothing. Emits `ClaimsHeld` with the value it leaves.
pub fn release_claims(ctx: Context<Hold>) -> Result<()> {
    let by = ctx.accounts.authority.key();
    require!(by == ctx.accounts.config.admin, DividendsError::Unauthorized);
    let distributor = &mut ctx.accounts.distributor;
    let released = resumed_claims_paused_until(distributor.claims_held_until, Clock::get()?.unix_timestamp);
    distributor.claims_held_until = released;
    emit!(ClaimsHeld { mint: distributor.mint, month: distributor.month, until: released, by });
    Ok(())
}
