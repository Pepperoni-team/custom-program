use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer};

use crate::constants::*;
use crate::errors::FanexError;
use crate::events::Graduated;
use crate::state::*;

#[derive(Accounts)]
pub struct Graduate<'info> {
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
    )]
    pub config: Account<'info, GlobalConfig>,

    #[account(
        address = config.graduation_authority @ FanexError::Unauthorized
    )]
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [CURVE_SEED, mint.key().as_ref()],
        bump = curve.bump,
        has_one = mint @ FanexError::AccountMismatch,
    )]
    pub curve: Account<'info, Curve>,

    pub mint: Account<'info, Mint>,

    #[account(
        mut,
        seeds = [SOL_VAULT_SEED, mint.key().as_ref()],
        bump = curve.sol_vault_bump,
    )]
    pub sol_vault: Account<'info, SolVault>,

    #[account(
        mut,
        associated_token::mint = mint,
        associated_token::authority = curve,
    )]
    pub token_vault: Account<'info, TokenAccount>,

    /// CHECK: keeper-controlled SOL destination (used to seed the CPMM).
    #[account(mut)]
    pub keeper_sol_dest: UncheckedAccount<'info>,

    #[account(
        mut,
        token::mint = mint,
    )]
    pub keeper_token_account: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<Graduate>) -> Result<()> {
    require!(
        ctx.accounts.curve.phase == CurvePhase::Graduating,
        FanexError::NotGraduating
    );

    let mint_key = ctx.accounts.mint.key();
    let curve_bump = ctx.accounts.curve.bump;
    let sol_to_release = ctx.accounts.curve.real_sol_reserves;
    let tokens_to_release = ctx.accounts.token_vault.amount;

    if tokens_to_release > 0 {
        let signer_seeds: &[&[&[u8]]] = &[&[CURVE_SEED, mint_key.as_ref(), &[curve_bump]]];
        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.token_vault.to_account_info(),
                    to: ctx.accounts.keeper_token_account.to_account_info(),
                    authority: ctx.accounts.curve.to_account_info(),
                },
                signer_seeds,
            ),
            tokens_to_release,
        )?;
    }

    if sol_to_release > 0 {
        let vault_ai = ctx.accounts.sol_vault.to_account_info();
        let dest_ai = ctx.accounts.keeper_sol_dest.to_account_info();
        let mut vault_lamports = vault_ai.try_borrow_mut_lamports()?;
        let mut dest_lamports = dest_ai.try_borrow_mut_lamports()?;
        **vault_lamports = vault_lamports
            .checked_sub(sol_to_release)
            .ok_or(FanexError::InsufficientReserves)?;
        **dest_lamports = dest_lamports
            .checked_add(sol_to_release)
            .ok_or(FanexError::MathOverflow)?;
    }

    let curve = &mut ctx.accounts.curve;
    curve.phase = CurvePhase::Graduated;
    curve.graduated_at = Clock::get()?.unix_timestamp;
    curve.real_sol_reserves = 0;
    curve.real_token_reserves = 0;
    curve.migration_token_reserves = 0;

    emit!(Graduated {
        mint: mint_key,
        sol_released: sol_to_release,
        tokens_released: tokens_to_release,
        keeper: ctx.accounts.authority.key(),
    });

    Ok(())
}
