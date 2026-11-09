//! The rent of a finished raise goes back to the sponsor (the 2026-10-21 design). Both
//! instructions are permissionless and run under the pause: every lamport can only reach
//! `Raise.rent_payer`, the payer of `create_raise`, so a caller gains nothing and needs no key.
//! A raise written before the upgrade records no payer (`Raise::tracks_rent`) and is refused.
//! That refusal is a constraint on `raise`, not a check in the handler: Anchor reads the accounts'
//! constraints in field order, and the zero payer is the system program's key, which a cluster
//! demotes to read-only, so `rent_payer`'s `mut` would otherwise refuse first (`ConstraintMut`).

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::raise::checked::add;
use crate::raise::constants::*;
use crate::errors::FanexError;
use crate::raise::events::{MonthClosed, RaiseAccountsClosed};
use crate::raise::ledger::{month_closable, raise_finished};
use crate::raise::state::*;
use crate::raise::transfer::{usdc_transfer, vault_close, RaiseSeeds};

#[derive(Accounts)]
pub struct CloseMonth<'info> {
    pub caller: Signer<'info>,

    #[account(mut, constraint = raise.tracks_rent() @ FanexError::RentPayerUnknown)]
    pub raise: Box<Account<'info, Raise>>,

    #[account(mut, has_one = raise @ FanexError::RaiseAccountMismatch, close = rent_payer)]
    pub month_account: Box<Account<'info, Month>>,

    /// CHECK: receives the month's rent; bound to the payer the raise recorded at its creation.
    #[account(mut, address = raise.rent_payer @ FanexError::RaiseAccountMismatch)]
    pub rent_payer: UncheckedAccount<'info>,
}

/// Closes one `Month` that is finished (`ledger::month_closable`: settled, behind the payment pointer,
/// at or behind the last settled month). Nothing reads such a month again: `fund_month` takes the
/// oldest open month, `settle_month` the next unsettled one, every other instruction the raise's counters.
pub fn close_month(ctx: Context<CloseMonth>) -> Result<()> {
    let raise_key = ctx.accounts.raise.key();
    let lamports = ctx.accounts.month_account.to_account_info().lamports();
    let m = &ctx.accounts.month_account;
    let raise = &mut ctx.accounts.raise;
    require!(matches!(raise.state, RaiseState::Active | RaiseState::Closed), FanexError::NotActive);
    require!(
        month_closable(m.finalized, m.month, raise.oldest_open_month, raise.last_finalized_month),
        FanexError::MonthNotClosable
    );
    raise.count_month_closed()?;
    emit!(MonthClosed { raise: raise_key, month: m.month, lamports });
    Ok(())
}

#[derive(Accounts)]
pub struct CloseRaiseAccounts<'info> {
    pub caller: Signer<'info>,

    #[account(seeds = [RAISE_CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, RaiseConfig>>,

    #[account(address = config.usdc_mint @ FanexError::RaiseAccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    #[account(mut, close = rent_payer, constraint = raise.tracks_rent() @ FanexError::RentPayerUnknown)]
    pub raise: Box<Account<'info, Raise>>,

    /// CHECK: receives every account's rent; bound to the payer the raise recorded at its creation.
    #[account(mut, address = raise.rent_payer @ FanexError::RaiseAccountMismatch)]
    pub rent_payer: UncheckedAccount<'info>,

    #[account(mut, seeds = [ESCROW_SEED, raise.key().as_ref()], bump = raise.escrow_bump)]
    pub escrow: Box<Account<'info, TokenAccount>>,

    #[account(mut, seeds = [GUARANTEE_SEED, raise.key().as_ref()], bump = raise.guarantee_bump)]
    pub guarantee_vault: Box<Account<'info, TokenAccount>>,

    #[account(mut, seeds = [POOL_SEED, raise.key().as_ref()], bump = raise.pool_bump)]
    pub pool_vault: Box<Account<'info, TokenAccount>>,

    #[account(mut, seeds = [PAYOUTS_SEED, raise.key().as_ref()], bump = raise.payouts_bump)]
    pub payouts_vault: Box<Account<'info, TokenAccount>>,

    /// Whatever is left in a vault can only land in the fee wallet's account (the binding `fund_month`
    /// uses for `fee_usdc`).
    #[account(
        mut,
        token::mint = usdc_mint,
        constraint = fee_usdc.owner == config.fee_wallet @ FanexError::RaiseAccountMismatch
    )]
    pub fee_usdc: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

/// Closes the `Raise` and its four vaults once nothing is owed from them (`ledger::raise_finished`).
/// A vault must be empty to close: its whole balance (rounding dust, USDC sent to it straight) goes to
/// the fee wallet first, in this same instruction, so no transfer can slip in between the sweep and the
/// close. The vaults close through the Token program, the `Raise` through Anchor's `close` at exit.
pub fn close_raise_accounts(ctx: Context<CloseRaiseAccounts>) -> Result<()> {
    {
        let raise = &ctx.accounts.raise;
        require!(
            raise_finished(raise.state, raise.oldest_open_month, raise.last_declared_month, raise.open_positions, raise.live_months),
            FanexError::RaiseNotFinished
        );
    }
    let raise_key = ctx.accounts.raise.key();
    let token_program = ctx.accounts.token_program.to_account_info();
    let mint = ctx.accounts.usdc_mint.to_account_info();
    let authority = ctx.accounts.raise.to_account_info();
    let rent_payer = ctx.accounts.rent_payer.to_account_info();
    let fee_usdc = ctx.accounts.fee_usdc.to_account_info();
    let raise_seeds = RaiseSeeds::of(&ctx.accounts.raise);
    let seeds = raise_seeds.seeds();
    let signer: &[&[&[u8]]] = &[&seeds];

    let vaults = [
        (ctx.accounts.escrow.to_account_info(), ctx.accounts.escrow.amount),
        (ctx.accounts.guarantee_vault.to_account_info(), ctx.accounts.guarantee_vault.amount),
        (ctx.accounts.pool_vault.to_account_info(), ctx.accounts.pool_vault.amount),
        (ctx.accounts.payouts_vault.to_account_info(), ctx.accounts.payouts_vault.amount),
    ];
    let mut leftover_usdc = 0u64;
    let mut lamports = authority.lamports();
    for (vault, amount) in &vaults {
        leftover_usdc = add(leftover_usdc, *amount)?;
        lamports = add(lamports, vault.lamports())?;
        usdc_transfer(&token_program, &mint, vault, &fee_usdc, &authority, *amount, signer)?;
    }
    for (vault, _) in &vaults {
        vault_close(&token_program, vault, &rent_payer, &authority, signer)?;
    }
    emit!(RaiseAccountsClosed { raise: raise_key, leftover_usdc, lamports });
    Ok(())
}
