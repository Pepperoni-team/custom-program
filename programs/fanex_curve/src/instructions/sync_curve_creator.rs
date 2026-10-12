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
