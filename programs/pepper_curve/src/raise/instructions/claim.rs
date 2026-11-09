//! Claims (spec §4.6). Anyone may trigger a claim; the money can only reach the
//! associated USDC account of the position's owner, the one its wallet shows
//! (the internal audit of 2026-10-29: before, any account the owner held, which
//! a caller could open for it). The pool's own payouts never go through here:
//! `sweep_pool_earnings` moves them into the pool.

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::raise::constants::*;
use crate::errors::FanexError;
use crate::raise::events::Claimed;
use crate::raise::state::*;
use crate::raise::transfer::{usdc_transfer, RaiseSeeds};
use crate::raise::validation::is_associated_token_account;

#[derive(Accounts)]
pub struct Claim<'info> {
    pub caller: Signer<'info>,

    #[account(seeds = [RAISE_CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, RaiseConfig>>,

    #[account(address = config.usdc_mint @ FanexError::RaiseAccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    pub raise: Box<Account<'info, Raise>>,

    #[account(mut, has_one = raise @ FanexError::RaiseAccountMismatch)]
    pub position: Box<Account<'info, Position>>,

    #[account(
        mut,
        token::mint = usdc_mint,
        constraint = owner_usdc.owner == position.owner @ FanexError::RaiseAccountMismatch,
        constraint = is_associated_token_account(&owner_usdc.key(), &position.owner, &usdc_mint.key()) @ FanexError::RaiseAccountMismatch
    )]
    pub owner_usdc: Box<Account<'info, TokenAccount>>,

    #[account(mut, seeds = [PAYOUTS_SEED, raise.key().as_ref()], bump = raise.payouts_bump)]
    pub payouts_vault: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

pub fn claim(ctx: Context<Claim>) -> Result<()> {
    let raise_key = ctx.accounts.raise.key();
    // The raise PDA owns the pool's position and the pool vault: a claim on that
    // position could land in the vault, outside the pool's counter.
    require!(ctx.accounts.position.owner != raise_key, FanexError::RaiseAccountMismatch);
    let acc = ctx.accounts.raise.acc_per_unit;
    let closed = ctx.accounts.raise.state == RaiseState::Closed;
    let amount = ctx.accounts.position.take_pending(acc)?;
    require!(amount > 0, FanexError::NothingOwed);
    require!(amount >= MIN_CLAIM || closed, FanexError::ClaimTooSmall);

    let raise_seeds = RaiseSeeds::of(&ctx.accounts.raise);
    let seeds = raise_seeds.seeds();
    let signer: &[&[&[u8]]] = &[&seeds];
    usdc_transfer(
        &ctx.accounts.token_program.to_account_info(),
        &ctx.accounts.usdc_mint.to_account_info(),
        &ctx.accounts.payouts_vault.to_account_info(),
        &ctx.accounts.owner_usdc.to_account_info(),
        &ctx.accounts.raise.to_account_info(),
        amount,
        signer,
    )?;

    emit!(Claimed { raise: raise_key, owner: ctx.accounts.position.owner, amount });
    Ok(())
}
