use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer};

use crate::constants::*;
use crate::errors::FanexError;
use crate::events::VestingReleased;
use crate::math::vested_amount;
use crate::state::*;

#[derive(Accounts)]
pub struct ClaimVesting<'info> {
    #[account(mut)]
    pub beneficiary: Signer<'info>,

    #[account(
        seeds = [CURVE_SEED, mint.key().as_ref()],
        bump = curve.bump,
        has_one = mint @ FanexError::AccountMismatch,
    )]
    pub curve: Account<'info, Curve>,

    pub mint: Account<'info, Mint>,

    #[account(
        mut,
        seeds = [VESTING_SEED, mint.key().as_ref()],
        bump = vesting.bump,
        has_one = mint @ FanexError::AccountMismatch,
        constraint = vesting.beneficiary == beneficiary.key() @ FanexError::Unauthorized,
    )]
    pub vesting: Account<'info, Vesting>,

    #[account(
        mut,
        seeds = [VESTING_VAULT_SEED, mint.key().as_ref()],
        bump,
        token::mint = mint,
        token::authority = curve,
    )]
    pub vesting_vault: Account<'info, TokenAccount>,

    #[account(
        init_if_needed,
        payer = beneficiary,
        associated_token::mint = mint,
        associated_token::authority = beneficiary,
    )]
    pub beneficiary_token_account: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<ClaimVesting>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let vesting = &ctx.accounts.vesting;

    let vested = vested_amount(
        vesting.total_amount,
        vesting.start_time,
        vesting.cliff_seconds,
        vesting.duration_seconds,
        now,
    );
    let releasable = vested
        .checked_sub(vesting.released_amount)
        .ok_or(FanexError::MathOverflow)?;
    require!(releasable > 0, FanexError::NothingToRelease);

    let mint_key = ctx.accounts.mint.key();
    let curve_bump = ctx.accounts.curve.bump;
    let signer_seeds: &[&[&[u8]]] = &[&[CURVE_SEED, mint_key.as_ref(), &[curve_bump]]];

    token::transfer(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.vesting_vault.to_account_info(),
                to: ctx.accounts.beneficiary_token_account.to_account_info(),
                authority: ctx.accounts.curve.to_account_info(),
            },
            signer_seeds,
        ),
        releasable,
    )?;

    ctx.accounts.vesting.released_amount = ctx
        .accounts
        .vesting
        .released_amount
        .checked_add(releasable)
        .ok_or(FanexError::MathOverflow)?;

    emit!(VestingReleased {
        mint: mint_key,
        beneficiary: ctx.accounts.beneficiary.key(),
        amount: releasable,
    });

    Ok(())
}
