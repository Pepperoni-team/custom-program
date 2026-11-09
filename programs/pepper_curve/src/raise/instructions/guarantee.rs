//! The guarantee (spec §4.7): every creator payment rebuilds it after it
//! covered a month, once the fans' arrears are paid (`months.rs`, the
//! 2026-10-19 design); `replenish_guarantee` rebuilds it early when no month is
//! open; anyone releases it to the creator after 12 consecutive months paid on
//! time. Every rule reads `Raise.guarantee_usdc`, moved exactly as the vault
//! is, never the vault balance (ruling P3).

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::raise::checked::{add, sub};
use crate::raise::constants::*;
use crate::errors::FanexError;
use crate::raise::events::{GuaranteeReleased, GuaranteeReplenished};
use crate::raise::ledger::{guarantee_releasable, has_open_months};
use crate::raise::state::*;
use crate::raise::transfer::{usdc_transfer, RaiseSeeds};
use crate::raise::validation::is_associated_token_account;

#[derive(Accounts)]
pub struct ReplenishGuarantee<'info> {
    pub creator: Signer<'info>,

    #[account(seeds = [RAISE_CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, RaiseConfig>>,

    #[account(address = config.usdc_mint @ FanexError::RaiseAccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    #[account(mut, has_one = creator @ FanexError::RaiseUnauthorized)]
    pub raise: Box<Account<'info, Raise>>,

    #[account(
        mut,
        token::mint = usdc_mint,
        constraint = creator_usdc.owner == creator.key() @ FanexError::RaiseAccountMismatch
    )]
    pub creator_usdc: Box<Account<'info, TokenAccount>>,

    #[account(mut, seeds = [GUARANTEE_SEED, raise.key().as_ref()], bump = raise.guarantee_bump)]
    pub guarantee_vault: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

pub fn replenish_guarantee(ctx: Context<ReplenishGuarantee>, amount: u64) -> Result<()> {
    let raise_key = ctx.accounts.raise.key();
    let accepted = {
        let raise = &ctx.accounts.raise;
        require!(raise.state == RaiseState::Active, FanexError::NotActive);
        require!(
            !has_open_months(raise.oldest_open_month, raise.last_declared_month),
            FanexError::OpenMonthsFirst
        );
        amount.min(raise.guarantee_owed)
    };
    require!(accepted > 0, FanexError::NothingOwed);

    usdc_transfer(
        &ctx.accounts.token_program.to_account_info(),
        &ctx.accounts.usdc_mint.to_account_info(),
        &ctx.accounts.creator_usdc.to_account_info(),
        &ctx.accounts.guarantee_vault.to_account_info(),
        &ctx.accounts.creator.to_account_info(),
        accepted,
        &[],
    )?;

    let raise = &mut ctx.accounts.raise;
    raise.guarantee_owed = sub(raise.guarantee_owed, accepted)?;
    raise.guarantee_usdc = add(raise.guarantee_usdc, accepted)?;
    emit!(GuaranteeReplenished { raise: raise_key, amount: accepted });
    Ok(())
}

#[derive(Accounts)]
pub struct ReleaseGuarantee<'info> {
    pub caller: Signer<'info>,

    #[account(seeds = [RAISE_CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, RaiseConfig>>,

    #[account(address = config.usdc_mint @ FanexError::RaiseAccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    #[account(mut)]
    pub raise: Box<Account<'info, Raise>>,

    #[account(mut, seeds = [GUARANTEE_SEED, raise.key().as_ref()], bump = raise.guarantee_bump)]
    pub guarantee_vault: Box<Account<'info, TokenAccount>>,

    /// The creator's associated USDC account (the internal audit of 2026-10-29).
    #[account(
        mut,
        token::mint = usdc_mint,
        constraint = creator_usdc.owner == raise.creator @ FanexError::RaiseAccountMismatch,
        constraint = is_associated_token_account(&creator_usdc.key(), &raise.creator, &usdc_mint.key()) @ FanexError::RaiseAccountMismatch
    )]
    pub creator_usdc: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

pub fn release_guarantee(ctx: Context<ReleaseGuarantee>) -> Result<()> {
    let raise_key = ctx.accounts.raise.key();
    {
        let raise = &ctx.accounts.raise;
        require!(raise.state == RaiseState::Active, FanexError::NotActive);
        require!(
            guarantee_releasable(raise.on_time_streak, raise.guarantee_owed, raise.guarantee_released),
            FanexError::GuaranteeNotReleasable
        );
    }
    let amount = ctx.accounts.raise.guarantee_usdc;

    let raise_seeds = RaiseSeeds::of(&ctx.accounts.raise);
    let seeds = raise_seeds.seeds();
    let signer: &[&[&[u8]]] = &[&seeds];
    usdc_transfer(
        &ctx.accounts.token_program.to_account_info(),
        &ctx.accounts.usdc_mint.to_account_info(),
        &ctx.accounts.guarantee_vault.to_account_info(),
        &ctx.accounts.creator_usdc.to_account_info(),
        &ctx.accounts.raise.to_account_info(),
        amount,
        signer,
    )?;

    let raise = &mut ctx.accounts.raise;
    raise.guarantee_usdc = 0;
    raise.guarantee_released = true;
    emit!(GuaranteeReleased { raise: raise_key, amount });
    Ok(())
}
