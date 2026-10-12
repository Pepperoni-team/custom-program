use anchor_lang::prelude::*;
use anchor_lang::system_program;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer};

use crate::constants::*;
use crate::errors::FanexError;
use crate::events::{GraduationReady, Trade};
use crate::math::{
    buy_tokens_out, curve_sol_for_tokens, platform_swap_fee, platform_swap_fee_on_net,
    split_with_cap,
};
use crate::state::*;
use crate::validation::launch_cap_allows;

#[derive(Accounts)]
pub struct Buy<'info> {
    #[account(mut)]
    pub buyer: Signer<'info>,

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
        init_if_needed,
        payer = buyer,
        associated_token::mint = mint,
        associated_token::authority = buyer,
    )]
    pub buyer_token_account: Account<'info, TokenAccount>,

    /// CHECK: must equal `curve.creator`; receives the atomic 94% split.
    #[account(mut, address = curve.creator @ FanexError::AccountMismatch)]
    pub creator: UncheckedAccount<'info>,

    /// CHECK: must equal `config.fanex_fee_wallet`; receives the atomic 6% split.
    #[account(mut, address = config.fanex_fee_wallet @ FanexError::AccountMismatch)]
    pub fanex: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<Buy>, sol_in: u64, min_tokens_out: u64) -> Result<()> {
    require!(!ctx.accounts.config.paused, FanexError::GloballyPaused);
    require!(!ctx.accounts.curve.paused, FanexError::CurvePaused);
    require!(ctx.accounts.curve.phase == CurvePhase::Bonding, FanexError::NotBonding);
    require!(sol_in > 0, FanexError::ZeroAmount);
    require!(ctx.accounts.curve.real_token_reserves > 0, FanexError::CurveComplete);

    let curve = &ctx.accounts.curve;

    let mut platform_fee = platform_swap_fee(sol_in)?;
    let net_in = sol_in
        .checked_sub(platform_fee)
        .ok_or(FanexError::MathOverflow)?;

    let split = split_with_cap(
        net_in,
        curve.creator_split_bps,
        curve.fanex_split_bps,
        curve.creator_cap_lamports,
        curve.fanex_cap_lamports,
        curve.creator_received_lamports,
        curve.fanex_received_lamports,
    )?;

    let mut tokens_out = buy_tokens_out(
        curve.virtual_sol_reserves,
        curve.virtual_token_reserves,
        split.curve,
    )?;

    let mut curve_amount = split.curve;
    if tokens_out > curve.real_token_reserves {
        tokens_out = curve.real_token_reserves;
        curve_amount = curve_sol_for_tokens(
            curve.virtual_sol_reserves,
            curve.virtual_token_reserves,
            tokens_out,
        )?
        .min(split.curve);
        let net_used = split
            .creator
            .checked_add(split.fanex)
            .ok_or(FanexError::MathOverflow)?
            .checked_add(curve_amount)
            .ok_or(FanexError::MathOverflow)?;
        platform_fee = platform_swap_fee_on_net(net_used)?.min(platform_fee);
    }

    let now = Clock::get()?.unix_timestamp;
    require!(
        launch_cap_allows(
            curve.created_at,
            now,
            ctx.accounts.buyer_token_account.amount,
            tokens_out,
        ),
        FanexError::LaunchWalletCapExceeded
    );

    require!(tokens_out >= min_tokens_out, FanexError::SlippageExceeded);
    require!(tokens_out > 0, FanexError::ZeroAmount);

    if split.creator > 0 {
        system_program::transfer(
            CpiContext::new(
                ctx.accounts.system_program.to_account_info(),
                system_program::Transfer {
                    from: ctx.accounts.buyer.to_account_info(),
                    to: ctx.accounts.creator.to_account_info(),
                },
            ),
            split.creator,
        )?;
    }
    let fanex_out = split
        .fanex
        .checked_add(platform_fee)
        .ok_or(FanexError::MathOverflow)?;
    if fanex_out > 0 {
        system_program::transfer(
            CpiContext::new(
                ctx.accounts.system_program.to_account_info(),
                system_program::Transfer {
                    from: ctx.accounts.buyer.to_account_info(),
                    to: ctx.accounts.fanex.to_account_info(),
                },
            ),
            fanex_out,
        )?;
    }
    system_program::transfer(
        CpiContext::new(
            ctx.accounts.system_program.to_account_info(),
            system_program::Transfer {
                from: ctx.accounts.buyer.to_account_info(),
                to: ctx.accounts.sol_vault.to_account_info(),
            },
        ),
        curve_amount,
    )?;

    let mint_key = ctx.accounts.mint.key();
    let curve_bump = ctx.accounts.curve.bump;
    {
        let curve = &mut ctx.accounts.curve;
        curve.virtual_sol_reserves = curve
            .virtual_sol_reserves
            .checked_add(curve_amount)
            .ok_or(FanexError::MathOverflow)?;
        curve.virtual_token_reserves = curve
            .virtual_token_reserves
            .checked_sub(tokens_out)
            .ok_or(FanexError::MathOverflow)?;
        curve.real_sol_reserves = curve
            .real_sol_reserves
            .checked_add(curve_amount)
            .ok_or(FanexError::MathOverflow)?;
        curve.real_token_reserves = curve
            .real_token_reserves
            .checked_sub(tokens_out)
            .ok_or(FanexError::MathOverflow)?;
        curve.creator_received_lamports = curve
            .creator_received_lamports
            .checked_add(split.creator)
            .ok_or(FanexError::MathOverflow)?;
        curve.fanex_received_lamports = curve
            .fanex_received_lamports
            .checked_add(split.fanex)
            .ok_or(FanexError::MathOverflow)?;
    }

    let signer_seeds: &[&[&[u8]]] = &[&[CURVE_SEED, mint_key.as_ref(), &[curve_bump]]];
    token::transfer(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.token_vault.to_account_info(),
                to: ctx.accounts.buyer_token_account.to_account_info(),
                authority: ctx.accounts.curve.to_account_info(),
            },
            signer_seeds,
        ),
        tokens_out,
    )?;

    let (mint_for_event, real_sol, real_tok) = {
        let curve = &mut ctx.accounts.curve;
        if curve.real_sol_reserves >= curve.cpmm_seed_lamports || curve.real_token_reserves == 0 {
            curve.phase = CurvePhase::Graduating;
            emit!(GraduationReady {
                mint: curve.mint,
                real_sol_reserves: curve.real_sol_reserves,
            });
        }
        (curve.mint, curve.real_sol_reserves, curve.real_token_reserves)
    };

    let sol_charged = split
        .creator
        .checked_add(split.fanex)
        .ok_or(FanexError::MathOverflow)?
        .checked_add(curve_amount)
        .ok_or(FanexError::MathOverflow)?
        .checked_add(platform_fee)
        .ok_or(FanexError::MathOverflow)?;

    emit!(Trade {
        mint: mint_for_event,
        trader: ctx.accounts.buyer.key(),
        is_buy: true,
        sol_amount: sol_charged,
        token_amount: tokens_out,
        creator_fee: split.creator,
        fanex_fee: split.fanex,
        platform_fee,
        real_sol_reserves: real_sol,
        real_token_reserves: real_tok,
    });

    Ok(())
}
