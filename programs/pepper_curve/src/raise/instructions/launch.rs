//! Launch (spec §4.5): the creator opens a raise, Fanex co-signs that its
//! parameters fit the limits computed from YouTube revenue (the realism limit of
//! amended spec §5), and the launch fee moves. Four USDC vaults are opened, all
//! owned by the raise PDA: escrow, guarantee, pool, payouts. The creator may
//! cancel while raising (before the deadline and before a sell-out); everyone
//! is then refunded in full.

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::raise::constants::*;
use crate::errors::FanexError;
use crate::raise::events::{RaiseCancelled, RaiseCreated};
use crate::raise::state::*;
use crate::raise::transfer::usdc_transfer;
use crate::raise::validation::{validate_params, RaiseParams};

#[derive(Accounts)]
#[instruction(params: RaiseParams)]
pub struct CreateRaise<'info> {
    /// Pays every account's rent: the Fanex raise sponsor wallet (spec §6.6).
    #[account(mut)]
    pub payer: Signer<'info>,

    pub creator: Signer<'info>,

    #[account(address = config.attester @ FanexError::RaiseUnauthorized)]
    pub attester: Signer<'info>,

    #[account(seeds = [RAISE_CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, RaiseConfig>>,

    #[account(address = config.usdc_mint @ FanexError::RaiseAccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    #[account(
        init,
        payer = payer,
        space = 8 + Raise::INIT_SPACE,
        seeds = [RAISE_SEED, creator.key().as_ref(), &params.raise_id.to_le_bytes()],
        bump
    )]
    pub raise: Box<Account<'info, Raise>>,

    #[account(
        init,
        payer = payer,
        seeds = [ESCROW_SEED, raise.key().as_ref()],
        bump,
        token::mint = usdc_mint,
        token::authority = raise
    )]
    pub escrow: Box<Account<'info, TokenAccount>>,

    #[account(
        init,
        payer = payer,
        seeds = [GUARANTEE_SEED, raise.key().as_ref()],
        bump,
        token::mint = usdc_mint,
        token::authority = raise
    )]
    pub guarantee_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        init,
        payer = payer,
        seeds = [POOL_SEED, raise.key().as_ref()],
        bump,
        token::mint = usdc_mint,
        token::authority = raise
    )]
    pub pool_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        init,
        payer = payer,
        seeds = [PAYOUTS_SEED, raise.key().as_ref()],
        bump,
        token::mint = usdc_mint,
        token::authority = raise
    )]
    pub payouts_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        token::mint = usdc_mint,
        constraint = creator_usdc.owner == creator.key() @ FanexError::RaiseAccountMismatch
    )]
    pub creator_usdc: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        token::mint = usdc_mint,
        constraint = fee_usdc.owner == config.fee_wallet @ FanexError::RaiseAccountMismatch
    )]
    pub fee_usdc: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

/// `max_launch_fee` is the fee the creator's transaction was built on: a config changed between the
/// build and the landing never takes more than that (`PriceOutsideLimit`; internal audit of 2026-10-29).
pub fn create_raise(ctx: Context<CreateRaise>, params: RaiseParams, max_launch_fee: u64) -> Result<()> {
    require!(!ctx.accounts.config.paused, FanexError::RaiseGloballyPaused);
    require!(ctx.accounts.config.launch_fee <= max_launch_fee, FanexError::PriceOutsideLimit);
    let now = Clock::get()?.unix_timestamp;
    let validated = validate_params(&params, now)?;

    usdc_transfer(
        &ctx.accounts.token_program.to_account_info(),
        &ctx.accounts.usdc_mint.to_account_info(),
        &ctx.accounts.creator_usdc.to_account_info(),
        &ctx.accounts.fee_usdc.to_account_info(),
        &ctx.accounts.creator.to_account_info(),
        ctx.accounts.config.launch_fee,
        &[],
    )?;

    let raise_key = ctx.accounts.raise.key();
    let raise = &mut ctx.accounts.raise;
    raise.creator = ctx.accounts.creator.key();
    raise.raise_id = params.raise_id;
    raise.state = RaiseState::Raising;
    raise.share_bps = params.share_bps;
    raise.term_months = params.term_months;
    raise.deadline = params.deadline;
    raise.tier_count = params.tier_count;
    for (tier, input) in raise.tiers.iter_mut().zip(params.tiers.iter()) {
        *tier = Tier { units: input.units, price: input.price, sold: 0 };
    }
    raise.max_units = validated.max_units;
    raise.min_creator_part = params.min_creator_part;
    raise.fan_max = validated.fan_max;
    raise.bump = ctx.bumps.raise;
    raise.escrow_bump = ctx.bumps.escrow;
    raise.guarantee_bump = ctx.bumps.guarantee_vault;
    raise.pool_bump = ctx.bumps.pool_vault;
    raise.payouts_bump = ctx.bumps.payouts_vault;
    // Where every account's rent goes back (the 2026-10-21 design); the counters start at zero.
    raise.rent_payer = ctx.accounts.payer.key();

    emit!(RaiseCreated {
        raise: raise_key,
        creator: raise.creator,
        raise_id: raise.raise_id,
        fan_max: validated.fan_max,
        creator_max: validated.creator_max,
        deadline: raise.deadline,
    });
    Ok(())
}

#[derive(Accounts)]
pub struct CancelRaise<'info> {
    pub creator: Signer<'info>,
    #[account(mut, has_one = creator @ FanexError::RaiseUnauthorized)]
    pub raise: Box<Account<'info, Raise>>,
}

/// Only while the raise is still open: once it sold out or reached its deadline
/// it has closed (spec §4.5), even before anyone calls `finalize`, so a creator
/// can never back out of a raise that succeeded.
pub fn cancel_raise(ctx: Context<CancelRaise>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let raise_key = ctx.accounts.raise.key();
    let raise = &mut ctx.accounts.raise;
    require!(raise.state == RaiseState::Raising, FanexError::NotRaising);
    require!(now < raise.deadline && raise.sold_units < raise.max_units, FanexError::NotRaising);
    raise.state = RaiseState::Refunding;
    emit!(RaiseCancelled { raise: raise_key });
    Ok(())
}
