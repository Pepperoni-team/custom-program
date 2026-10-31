use anchor_lang::prelude::*;

use crate::constants::*;
use crate::errors::FanexError;
use crate::state::*;

#[derive(Accounts)]
pub struct UpdateConfig<'info> {
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ FanexError::Unauthorized
    )]
    pub config: Account<'info, GlobalConfig>,
    pub admin: Signer<'info>,
}

/// Update mutable config fields. `None` leaves a field unchanged. An all-zero `new_admin` is refused
/// (`AdminUnset`, the internal audit, 2026-10-29): only the admin unpauses the SOL side, and no key
/// signs for all-zero. An off-curve key is taken, since a Squads vault is one.
pub fn update_config(
    ctx: Context<UpdateConfig>,
    new_admin: Option<Pubkey>,
    new_fanex_fee_wallet: Option<Pubkey>,
    new_graduation_authority: Option<Pubkey>,
) -> Result<()> {
    require!(new_admin != Some(Pubkey::default()), FanexError::AdminUnset);
    let config = &mut ctx.accounts.config;
    if let Some(a) = new_admin {
        config.admin = a;
    }
    if let Some(w) = new_fanex_fee_wallet {
        config.fanex_fee_wallet = w;
    }
    if let Some(g) = new_graduation_authority {
        config.graduation_authority = g;
    }
    Ok(())
}

/// On-chain kill-switch. Complements the off-chain B7 gate: when true, the
/// program itself refuses create/buy/sell/distribute.
pub fn set_paused(ctx: Context<UpdateConfig>, paused: bool) -> Result<()> {
    ctx.accounts.config.paused = paused;
    Ok(())
}

#[derive(Accounts)]
pub struct SetCurvePaused<'info> {
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ FanexError::Unauthorized
    )]
    pub config: Account<'info, GlobalConfig>,
    pub admin: Signer<'info>,
    #[account(mut)]
    pub curve: Account<'info, Curve>,
}

pub fn set_curve_paused(ctx: Context<SetCurvePaused>, paused: bool) -> Result<()> {
    ctx.accounts.curve.paused = paused;
    Ok(())
}
