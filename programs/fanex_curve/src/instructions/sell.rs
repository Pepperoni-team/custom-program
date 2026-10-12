use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer};

use crate::constants::*;
use crate::errors::FanexError;
use crate::events::Trade;
use crate::math::{platform_swap_fee, sell_sol_out};
use crate::payout::pay_out;
use crate::state::*;

#[derive(Accounts)]
pub struct Sell<'info> {
    #[account(mut)]
    pub seller: Signer<'info>,

    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, GlobalConfig>,

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

    #[account(
        mut,
        associated_token::mint = mint,
        associated_token::authority = seller,
    )]
    pub seller_token_account: Account<'info, TokenAccount>,

    /// CHECK: must equal `config.fanex_fee_wallet`; receives the 1% swap fee.
    #[account(mut, address = config.fanex_fee_wallet @ FanexError::AccountMismatch)]
    pub fanex: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn handler<'info>(
    ctx: Context<'_, '_, '_, 'info, Sell<'info>>,
    token_in: u64,
    min_sol_out: u64,
) -> Result<()> {
    require!(!ctx.accounts.config.paused, FanexError::GloballyPaused);
    require!(!ctx.accounts.curve.paused, FanexError::CurvePaused);
    require!(ctx.accounts.curve.phase == CurvePhase::Bonding, FanexError::NotBonding);
    require!(token_in > 0, FanexError::ZeroAmount);

    let curve = &ctx.accounts.curve;
    let sol_out = sell_sol_out(
        curve.virtual_sol_reserves,
        curve.virtual_token_reserves,
        token_in,
    )?;
    require!(sol_out > 0, FanexError::ZeroAmount);
    require!(sol_out <= curve.real_sol_reserves, FanexError::InsufficientReserves);

    let platform_fee = platform_swap_fee(sol_out)?;
    let seller_proceeds = sol_out
        .checked_sub(platform_fee)
        .ok_or(FanexError::MathOverflow)?;
    require!(seller_proceeds >= min_sol_out, FanexError::SlippageExceeded);

    token::transfer(
        CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.seller_token_account.to_account_info(),
                to: ctx.accounts.token_vault.to_account_info(),
                authority: ctx.accounts.seller.to_account_info(),
            },
        ),
        token_in,
    )?;

    pay_out(
        &ctx.accounts.sol_vault.to_account_info(),
        &ctx.accounts.seller.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        ctx.remaining_accounts,
        seller_proceeds,
    )?;
    if platform_fee > 0 {
        let vault_ai = ctx.accounts.sol_vault.to_account_info();
        let fanex_ai = ctx.accounts.fanex.to_account_info();
        let mut vault_lamports = vault_ai.try_borrow_mut_lamports()?;
        let mut fanex_lamports = fanex_ai.try_borrow_mut_lamports()?;
        **vault_lamports = vault_lamports
            .checked_sub(platform_fee)
            .ok_or(FanexError::InsufficientReserves)?;
        **fanex_lamports = fanex_lamports
            .checked_add(platform_fee)
            .ok_or(FanexError::MathOverflow)?;
    }

    let (mint_for_event, real_sol, real_tok) = {
        let curve = &mut ctx.accounts.curve;
        curve.virtual_token_reserves = curve
            .virtual_token_reserves
            .checked_add(token_in)
            .ok_or(FanexError::MathOverflow)?;
        curve.virtual_sol_reserves = curve
            .virtual_sol_reserves
            .checked_sub(sol_out)
            .ok_or(FanexError::MathOverflow)?;
        curve.real_sol_reserves = curve
            .real_sol_reserves
            .checked_sub(sol_out)
            .ok_or(FanexError::MathOverflow)?;
        curve.real_token_reserves = curve
            .real_token_reserves
            .checked_add(token_in)
            .ok_or(FanexError::MathOverflow)?;
        (curve.mint, curve.real_sol_reserves, curve.real_token_reserves)
    };

    emit!(Trade {
        mint: mint_for_event,
        trader: ctx.accounts.seller.key(),
        is_buy: false,
        sol_amount: sol_out,
        token_amount: token_in,
        creator_fee: 0,
        fanex_fee: 0,
        platform_fee,
        real_sol_reserves: real_sol,
        real_token_reserves: real_tok,
    });

    Ok(())
}
