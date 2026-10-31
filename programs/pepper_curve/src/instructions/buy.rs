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
use crate::validation::{launch_cap_allows, launch_guard_waived};

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

    /// CHECK: must equal `config.fanex_fee_wallet`; receives the 1% platform fee
    /// and Fanex's share of the raise, in one transfer.
    #[account(mut, address = config.fanex_fee_wallet @ FanexError::AccountMismatch)]
    pub fanex: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn handler<'info>(
    ctx: Context<'_, '_, '_, 'info, Buy<'info>>,
    sol_in: u64,
    min_tokens_out: u64,
) -> Result<()> {
    require!(!ctx.accounts.config.paused, FanexError::GloballyPaused);
    require!(!ctx.accounts.curve.paused, FanexError::CurvePaused);
    require!(ctx.accounts.curve.phase == CurvePhase::Bonding, FanexError::NotBonding);
    require!(sol_in > 0, FanexError::ZeroAmount);
    require!(ctx.accounts.curve.real_token_reserves > 0, FanexError::CurveComplete);

    let curve = &ctx.accounts.curve;

    // 0) Skim the 1% platform swap fee off the input FIRST, then split the
    //    remaining 99%. The fee goes to Fanex on top of the fundraise share and
    //    does NOT count toward any cap. Buyer bears the fee (skim-from-input).
    //    `platform_fee` is `mut` because a graduation partial-fill (step 2) may
    //    shrink it to 1% of the SOL actually transacted, not the full input.
    let mut platform_fee = platform_swap_fee(sol_in)?;
    let net_in = sol_in
        .checked_sub(platform_fee)
        .ok_or(FanexError::MathOverflow)?;

    // 1) Cap-aware split of the net — enforced atomically. A buyer cannot strip
    //    this: it is part of the same instruction that delivers their tokens
    //    (B1 root fix).
    let split = split_with_cap(
        net_in,
        curve.creator_split_bps,
        curve.fanex_split_bps,
        curve.creator_cap_lamports,
        curve.fanex_cap_lamports,
        curve.creator_received_lamports,
        curve.fanex_received_lamports,
    )?;

    // 2) Tokens out for the curve portion only.
    let mut tokens_out = buy_tokens_out(
        curve.virtual_sol_reserves,
        curve.virtual_token_reserves,
        split.curve,
    )?;

    // Graduation partial-fill + refund. If the curve portion would buy MORE
    // tokens than remain, the buyer is paying for tokens that don't exist. Clamp
    // to the remaining tokens AND recompute the curve SOL actually needed for
    // them, so the unused SOL is simply never transferred out (it stays in the
    // buyer's wallet — an implicit refund). creator/fanex are already clamped to
    // their remaining fundraise caps by `split_with_cap`, so only the curve
    // portion — and its proportional share of the 1% platform fee — shrink.
    let mut curve_amount = split.curve;
    if tokens_out > curve.real_token_reserves {
        tokens_out = curve.real_token_reserves;
        // Exact curve SOL for the remaining tokens (ceil), never above the
        // computed curve share (defensive — keeps the buyer's total ≤ sol_in).
        curve_amount = curve_sol_for_tokens(
            curve.virtual_sol_reserves,
            curve.virtual_token_reserves,
            tokens_out,
        )?
        .min(split.curve);
        // Re-skim the 1% fee on the SOL actually swapped (creator + fanex +
        // curve_amount), capped at the original fee so the buyer can only ever
        // pay LESS than their input, never more.
        let net_used = split
            .creator
            .checked_add(split.fanex)
            .ok_or(FanexError::MathOverflow)?
            .checked_add(curve_amount)
            .ok_or(FanexError::MathOverflow)?;
        platform_fee = platform_swap_fee_on_net(net_used)?.min(platform_fee);
    }

    // The launch cap (anti-sniper, 2026-10-12, docs/solana/PEPPER-CURVE-PROGRAM.md
    // §5.2): for the first five minutes after the launch the buyer's token
    // account may hold at most 1% of the supply once this buy lands. Checked on
    // the final `tokens_out`, before any SOL moves, for every buyer, the
    // launch's own first buy included. A curve launched with a waiver (the
    // switch, 2026-10-14) skips it when the waiver is passed as an extra
    // account; read only when the cap would refuse.
    let now = Clock::get()?.unix_timestamp;
    if !launch_cap_allows(
        curve.created_at,
        now,
        ctx.accounts.buyer_token_account.amount,
        tokens_out,
    ) {
        require!(
            launch_guard_waived(ctx.remaining_accounts, &curve.mint),
            FanexError::LaunchWalletCapExceeded
        );
    }

    require!(tokens_out >= min_tokens_out, FanexError::SlippageExceeded);
    require!(tokens_out > 0, FanexError::ZeroAmount);

    // 3) Move SOL: buyer → creator / fanex / curve vault (all in this ix).
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
    // Fanex receives the fundraise share + the 1% platform fee in one transfer
    // (only the fundraise share is later counted toward the cap).
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

    // 4) Update reserves + running totals.
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

    // 5) Deliver tokens from the curve vault (signed by the curve PDA).
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

    // 6) Graduation trigger — freeze trading until the keeper hands off to CPMM.
    //    Triggers when the SOL seed is reached OR the curve is sold out
    //    (real_token_reserves == 0). The second condition is a safety net for the
    //    partial-fill path: ceil rounding on the last `curve_amount` should land
    //    real_sol_reserves at/above the seed, but if a lamport of rounding left it
    //    a hair short while the last token was sold, the curve would otherwise be
    //    stuck in Bonding with nothing left to sell. `graduate` only gates on the
    //    Graduating phase (no SOL floor), so this can't create an unservable state.
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

    // Report the SOL actually charged (creator + fanex + curve + fee), which on a
    // full fill equals `sol_in` and on a graduation partial-fill is less (the
    // refund stayed in the buyer's wallet). This keeps the indexed price exact.
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
