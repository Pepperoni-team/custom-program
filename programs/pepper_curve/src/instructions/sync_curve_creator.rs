use anchor_lang::prelude::*;
use anchor_spl::token::Mint;

use crate::constants::*;
use crate::errors::FanexError;
use crate::events::CurveCreatorSynced;
use crate::instructions::set_vesting_beneficiary::fan_led_placeholder;
use crate::state::*;
use crate::validation::validate_curve_creator_syncable;

#[derive(Accounts)]
pub struct SyncCurveCreator<'info> {
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ FanexError::Unauthorized
    )]
    pub config: Account<'info, GlobalConfig>,

    pub admin: Signer<'info>,

    pub mint: Account<'info, Mint>,

    /// Read-only: the SOURCE of the value written. Never modified here.
    #[account(
        seeds = [VESTING_SEED, mint.key().as_ref()],
        bump = vesting.bump,
        has_one = mint @ FanexError::AccountMismatch,
    )]
    pub vesting: Account<'info, Vesting>,

    #[account(
        mut,
        seeds = [CURVE_SEED, mint.key().as_ref()],
        bump = curve.bump,
        has_one = mint @ FanexError::AccountMismatch,
    )]
    pub curve: Account<'info, Curve>,
}

/// Point `Curve.creator` at the creator who claimed a fan-led token.
///
/// `create_curve` stamps the keyless placeholder into BOTH `Curve.creator` and
/// `Vesting.beneficiary`, and `set_vesting_beneficiary` only moves the second.
/// Left alone, a claimed curve keeps the placeholder as its creator forever:
/// the creator cannot fund a dividend (`init_distributor` authorises the funder
/// against `Curve.creator`) and the off-chain LP-fee harvest, which refuses to
/// pay when the database's copy disagrees with this field, never pays.
///
/// It takes NO address on purpose. The only value it can write is the vesting
/// beneficiary, which only the placeholder-only `set_vesting_beneficiary` can
/// set, so a compromised admin key gains nothing here it did not already have,
/// and a curve launched by a real creator is unreachable (see
/// `validate_curve_creator_syncable`). One-shot: once synced, the guard refuses.
pub fn handler(ctx: Context<SyncCurveCreator>) -> Result<()> {
    let new_creator = ctx.accounts.vesting.beneficiary;
    let curve = &mut ctx.accounts.curve;
    let previous = curve.creator;

    validate_curve_creator_syncable(previous, new_creator, fan_led_placeholder())?;

    curve.creator = new_creator;

    emit!(CurveCreatorSynced {
        mint: ctx.accounts.mint.key(),
        previous,
        new_creator,
    });
    Ok(())
}
