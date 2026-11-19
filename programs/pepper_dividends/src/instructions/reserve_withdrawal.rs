//! A reserve's withdrawal, in two steps (the reserve's spec §4.3 to §4.5, 2026-11-07): its owner asks
//! (`request_reserve_withdrawal`), from then on the reserve pays only the months accrued before the
//! request, and 45 days on the owner takes the whole balance back (`withdraw_reserve`), or cancels the
//! request before (`cancel_reserve_withdrawal`). No Pepper key signs any of the three, and no pause
//! stops them: a pause never holds a creator's money.

use anchor_lang::prelude::*;
use anchor_spl::associated_token::get_associated_token_address;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::constants::{CONFIG_SEED, RESERVE_SEED};
use crate::errors::DividendsError;
use crate::events::{ReserveWithdrawalCancelled, ReserveWithdrawalRequested, ReserveWithdrawn};
use crate::state::{DividendReserve, DividendsConfig, ReserveSeeds};
use crate::transfer::usdc_transfer;
use crate::validation::{check_withdrawal_ready, known_version, withdrawable_at};

/// The owner's request or its cancel: the reserve alone.
#[derive(Accounts)]
pub struct ReserveRequest<'info> {
    /// The reserve's owner (`Unauthorized` for anyone else).
    pub owner: Signer<'info>,

    #[account(
        mut,
        seeds = [RESERVE_SEED, reserve.mint.as_ref()],
        bump = reserve.bump,
        has_one = owner @ DividendsError::Unauthorized,
        constraint = known_version(reserve.version) @ DividendsError::UnsupportedVersion,
    )]
    pub reserve: Box<Account<'info, DividendReserve>>,
}

/// No request already pending (`WithdrawalPending`). Writes the time it runs, which the funding
/// dates every month against from then on, and emits `ReserveWithdrawalRequested`.
pub fn request_reserve_withdrawal(ctx: Context<ReserveRequest>) -> Result<()> {
    let reserve = &mut ctx.accounts.reserve;
    require!(reserve.withdrawal_requested_at == 0, DividendsError::WithdrawalPending);
    let now = Clock::get()?.unix_timestamp;
    reserve.withdrawal_requested_at = now;
    emit!(ReserveWithdrawalRequested {
        mint: reserve.mint,
        owner: reserve.owner,
        requested_at: now,
        withdrawable_at: withdrawable_at(now),
    });
    Ok(())
}

/// A request pending (`NoWithdrawalPending`). Resets it, so the reserve pays every month again, and
/// emits `ReserveWithdrawalCancelled`.
pub fn cancel_reserve_withdrawal(ctx: Context<ReserveRequest>) -> Result<()> {
    let reserve = &mut ctx.accounts.reserve;
    require!(reserve.withdrawal_requested_at != 0, DividendsError::NoWithdrawalPending);
    reserve.withdrawal_requested_at = 0;
    emit!(ReserveWithdrawalCancelled { mint: reserve.mint, owner: reserve.owner });
    Ok(())
}

#[derive(Accounts)]
pub struct WithdrawReserve<'info> {
    /// The reserve's owner (`Unauthorized` for anyone else).
    pub owner: Signer<'info>,

    /// Binds the USDC, so the vault is the reserve's USDC account and no other token's.
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = known_version(config.version) @ DividendsError::UnsupportedVersion,
    )]
    pub config: Box<Account<'info, DividendsConfig>>,

    #[account(
        mut,
        seeds = [RESERVE_SEED, reserve.mint.as_ref()],
        bump = reserve.bump,
        has_one = owner @ DividendsError::Unauthorized,
        constraint = known_version(reserve.version) @ DividendsError::UnsupportedVersion,
    )]
    pub reserve: Box<Account<'info, DividendReserve>>,

    /// The reserve's vault, its associated USDC account.
    #[account(
        mut,
        associated_token::mint = usdc_mint,
        associated_token::authority = reserve,
        associated_token::token_program = token_program,
    )]
    pub reserve_vault: Box<Account<'info, TokenAccount>>,

    /// CHECK: the owner's own USDC ATA (`AccountMismatch` for any other account), read by the transfer
    /// alone: an empty reserve withdraws while it is closed, and one holding USDC needs it opened
    /// again first, `create_idempotent` in the same transaction.
    #[account(
        mut,
        address = get_associated_token_address(&owner.key(), &usdc_mint.key()) @ DividendsError::AccountMismatch,
    )]
    pub owner_usdc: UncheckedAccount<'info>,

    /// The config's USDC (`AccountMismatch` for another mint).
    #[account(address = config.usdc_mint @ DividendsError::AccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    pub token_program: Program<'info, Token>,
}

/// A request pending for its whole delay (`check_withdrawal_ready`: `NoWithdrawalPending`,
/// `WithdrawalNotReady`). Then the vault's whole balance, stray transfers included, moves to the
/// owner's USDC account with `transfer_checked` (an empty vault sends nothing), the request is reset,
/// and `ReserveWithdrawn` is emitted. The reserve and its vault stay, for a deposit to come.
pub fn withdraw_reserve(ctx: Context<WithdrawReserve>) -> Result<()> {
    check_withdrawal_ready(ctx.accounts.reserve.withdrawal_requested_at, Clock::get()?.unix_timestamp)?;
    let amount = ctx.accounts.reserve_vault.amount;
    let seeds = ReserveSeeds::of(&ctx.accounts.reserve);
    let seeds = seeds.seeds();
    usdc_transfer(
        &ctx.accounts.token_program.to_account_info(),
        &ctx.accounts.usdc_mint.to_account_info(),
        &ctx.accounts.reserve_vault.to_account_info(),
        &ctx.accounts.owner_usdc.to_account_info(),
        &ctx.accounts.reserve.to_account_info(),
        amount,
        &[&seeds[..]],
    )?;
    let reserve = &mut ctx.accounts.reserve;
    reserve.withdrawal_requested_at = 0;
    emit!(ReserveWithdrawn { mint: reserve.mint, owner: reserve.owner, amount });
    Ok(())
}
