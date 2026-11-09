//! Buying in a raise (spec §4.4, §4.5): the current tier first, split across
//! tiers; Fanex co-signs (kill switch, per-person limit off-chain); the program
//! holds the per-wallet limit in quota units.

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::raise::checked::add;
use crate::raise::constants::*;
use crate::errors::FanexError;
use crate::raise::events::QuotasBought;
use crate::raise::pricing::fill_purchase;
use crate::raise::state::*;
use crate::raise::transfer::usdc_transfer;
use crate::raise::validation::within_wallet_limit;

#[derive(Accounts)]
pub struct BuyQuotas<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    pub buyer: Signer<'info>,

    #[account(address = config.attester @ FanexError::RaiseUnauthorized)]
    pub attester: Signer<'info>,

    #[account(seeds = [RAISE_CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, RaiseConfig>>,

    #[account(address = config.usdc_mint @ FanexError::RaiseAccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    #[account(mut)]
    pub raise: Box<Account<'info, Raise>>,

    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + Position::INIT_SPACE,
        seeds = [POSITION_SEED, raise.key().as_ref(), buyer.key().as_ref()],
        bump
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(
        mut,
        token::mint = usdc_mint,
        constraint = buyer_usdc.owner == buyer.key() @ FanexError::RaiseAccountMismatch
    )]
    pub buyer_usdc: Box<Account<'info, TokenAccount>>,

    #[account(mut, seeds = [ESCROW_SEED, raise.key().as_ref()], bump = raise.escrow_bump)]
    pub escrow: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn buy_quotas(ctx: Context<BuyQuotas>, max_cost: u64, min_units: u64) -> Result<()> {
    require!(!ctx.accounts.config.paused, FanexError::RaiseGloballyPaused);
    require!(max_cost > 0, FanexError::RaiseZeroAmount);
    let now = Clock::get()?.unix_timestamp;

    let (fill, held) = {
        let raise = &ctx.accounts.raise;
        require!(raise.state == RaiseState::Raising, FanexError::NotRaising);
        require!(now < raise.deadline, FanexError::RaiseExpired);
        let fill = fill_purchase(&raise.tiers, raise.tier_count, max_cost)?;
        require!(fill.units > 0, FanexError::NothingToBuy);
        require!(fill.units >= min_units, FanexError::RaiseSlippageExceeded);
        let sells_out = add(raise.sold_units, fill.units)? == raise.max_units;
        require!(fill.cost >= MIN_PURCHASE || sells_out, FanexError::BelowMinimumPurchase);
        let held = add(ctx.accounts.position.units, fill.units)?;
        require!(within_wallet_limit(held, raise.max_units), FanexError::WalletLimitExceeded);
        (fill, held)
    };

    usdc_transfer(
        &ctx.accounts.token_program.to_account_info(),
        &ctx.accounts.usdc_mint.to_account_info(),
        &ctx.accounts.buyer_usdc.to_account_info(),
        &ctx.accounts.escrow.to_account_info(),
        &ctx.accounts.buyer.to_account_info(),
        fill.cost,
        &[],
    )?;

    let raise_key = ctx.accounts.raise.key();
    let position = &mut ctx.accounts.position;
    let created = position.init_if_new(raise_key, ctx.accounts.buyer.key(), ctx.accounts.payer.key(), ctx.bumps.position);
    // While raising the accumulator is zero, so the debt stays zero.
    position.units = held;
    position.paid = add(position.paid, fill.cost)?;

    let raise = &mut ctx.accounts.raise;
    for (tier, units) in raise.tiers.iter_mut().zip(fill.per_tier.iter()) {
        tier.sold = add(tier.sold, *units)?;
    }
    raise.sold_units = add(raise.sold_units, fill.units)?;
    raise.fan_paid = add(raise.fan_paid, fill.cost)?;
    if created {
        raise.count_position_opened()?;
    }

    emit!(QuotasBought { raise: raise_key, buyer: ctx.accounts.buyer.key(), units: fill.units, cost: fill.cost });
    Ok(())
}
