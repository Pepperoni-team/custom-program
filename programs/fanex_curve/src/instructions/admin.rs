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

pub fn update_config(
    ctx: Context<UpdateConfig>,
    new_admin: Option<Pubkey>,
    new_fanex_fee_wallet: Option<Pubkey>,
    new_graduation_authority: Option<Pubkey>,
) -> Result<()> {
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
