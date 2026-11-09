//! The exit pool (amended spec §3, D14-D18). A fan sells quotas to the pool or
//! buys quotas it holds, at the constant-product price of `pool_math`; the 1%
//! fee stays in the pool. Both trades need the attester's co-signature, so they
//! stay off until the exit screens ship, and both stop with the pause. Everyone
//! trades, the creator included (D17). Before units move the seller's pending
//! payouts are settled to the seller, and the pool's own payouts are swept into
//! the pool, so a trade always prices against every payout the pool received.
//! A purchase is sold at the curve, and only while the curve prices it at least
//! at its share of the pool's USDC at close (N+3's second audit round, Rick's
//! call of 2026-11-05, `PoolPriceUnderShare`); nothing is sold while the pool
//! holds every quota (`pool_math::buy_pro_rata`); a sale never pays less than the
//! seller's share of it (N+3, `pool_math::sell_pro_rata`), but for the units the
//! seller bought from the pool, which it sells first, at the curve alone
//! (`Position::from_pool`, `pool_math::sell_position`, N+3's audit).

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::raise::checked::{add, sub};
use crate::raise::constants::*;
use crate::errors::FanexError;
use crate::raise::events::{BoughtFromPool, PoolEarningsSwept, SoldToPool};
use crate::raise::pool_math;
use crate::raise::state::*;
use crate::raise::transfer::{usdc_transfer, RaiseSeeds};
use crate::raise::validation::within_wallet_limit;

fn require_open_pool(config: &RaiseConfig, raise: &Raise) -> Result<()> {
    require!(!config.paused, FanexError::RaiseGloballyPaused);
    require!(raise.state == RaiseState::Active, FanexError::NotActive);
    Ok(())
}

/// Moves what the pool's own quotas earned from the payouts vault into the pool
/// vault and raises `pool_usdc` by it. Returns the amount moved (zero is a no-op).
///
/// ⚠️ It checks none of its accounts: every caller's accounts struct must bind
/// them to the raise, `pool_position` by its seeds [POSITION_SEED, raise, raise]
/// and stored bump, `pool_vault` and `payouts_vault` by their seeds and
/// `raise.pool_bump` / `raise.payouts_bump`.
pub fn sweep_pool<'info>(
    token_program: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    payouts_vault: &AccountInfo<'info>,
    pool_vault: &AccountInfo<'info>,
    raise_info: &AccountInfo<'info>,
    raise: &mut Raise,
    pool_position: &mut Position,
) -> Result<u64> {
    let earned = pool_position.take_pending(raise.acc_per_unit)?;
    let raise_seeds = RaiseSeeds::of(raise);
    let seeds = raise_seeds.seeds();
    let signer: &[&[&[u8]]] = &[&seeds];
    usdc_transfer(token_program, mint, payouts_vault, pool_vault, raise_info, earned, signer)?;
    raise.pool_usdc = add(raise.pool_usdc, earned)?;
    Ok(earned)
}

#[derive(Accounts)]
pub struct SellToPool<'info> {
    pub seller: Signer<'info>,

    #[account(address = config.attester @ FanexError::RaiseUnauthorized)]
    pub attester: Signer<'info>,

    #[account(seeds = [RAISE_CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, RaiseConfig>>,

    #[account(address = config.usdc_mint @ FanexError::RaiseAccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    #[account(mut)]
    pub raise: Box<Account<'info, Raise>>,

    #[account(
        mut,
        has_one = raise @ FanexError::RaiseAccountMismatch,
        constraint = position.owner == seller.key() @ FanexError::RaiseUnauthorized
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(
        mut,
        seeds = [POSITION_SEED, raise.key().as_ref(), raise.key().as_ref()],
        bump = pool_position.bump
    )]
    pub pool_position: Box<Account<'info, Position>>,

    #[account(mut, seeds = [POOL_SEED, raise.key().as_ref()], bump = raise.pool_bump)]
    pub pool_vault: Box<Account<'info, TokenAccount>>,

    #[account(mut, seeds = [PAYOUTS_SEED, raise.key().as_ref()], bump = raise.payouts_bump)]
    pub payouts_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        token::mint = usdc_mint,
        constraint = seller_usdc.owner == seller.key() @ FanexError::RaiseAccountMismatch
    )]
    pub seller_usdc: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

pub fn sell_to_pool(ctx: Context<SellToPool>, units: u64, min_out: u64) -> Result<()> {
    require_open_pool(&ctx.accounts.config, &ctx.accounts.raise)?;
    require!(units > 0, FanexError::RaiseZeroAmount);
    require!(units <= ctx.accounts.position.units, FanexError::InsufficientUnits);

    let token_program = ctx.accounts.token_program.to_account_info();
    let mint = ctx.accounts.usdc_mint.to_account_info();
    let payouts_vault = ctx.accounts.payouts_vault.to_account_info();
    let pool_vault = ctx.accounts.pool_vault.to_account_info();
    let authority = ctx.accounts.raise.to_account_info();
    sweep_pool(
        &token_program,
        &mint,
        &payouts_vault,
        &pool_vault,
        &authority,
        &mut ctx.accounts.raise,
        &mut ctx.accounts.pool_position,
    )?;

    // The units the seller bought from the pool first, at the curve alone, then the rest never under
    // the seller's share of the pool's USDC, the units the fans hold the seller's included
    // (`pool_math::sell_position`, N+3 and its audit).
    let fan_units = sub(ctx.accounts.raise.units_outstanding, ctx.accounts.raise.pool_units)?;
    let from_pool = ctx.accounts.position.from_pool;
    let sale = pool_math::sell_position(ctx.accounts.raise.pool(), units, from_pool, ctx.accounts.config.pool_fee_bps, fan_units)?;
    require!(sale.net >= min_out, FanexError::PriceOutsideLimit);

    let raise_seeds = RaiseSeeds::of(&ctx.accounts.raise);
    let seeds = raise_seeds.seeds();
    let signer: &[&[&[u8]]] = &[&seeds];
    usdc_transfer(
        &token_program,
        &mint,
        &pool_vault,
        &ctx.accounts.seller_usdc.to_account_info(),
        &authority,
        sale.net,
        signer,
    )?;

    // Both holdings settle before they change: the seller keeps what it accrued.
    let acc = ctx.accounts.raise.acc_per_unit;
    let seller = &mut ctx.accounts.position;
    let left = sub(seller.units, units)?;
    seller.set_units(acc, left)?;
    seller.from_pool = sub(from_pool, units.min(from_pool))?;
    let pool = &mut ctx.accounts.pool_position;
    let pool_held = add(pool.units, units)?;
    pool.set_units(acc, pool_held)?;

    let raise_key = ctx.accounts.raise.key();
    let raise = &mut ctx.accounts.raise;
    raise.pool_usdc = sale.after.usdc;
    raise.pool_units = sale.after.units;
    emit!(SoldToPool {
        raise: raise_key,
        seller: ctx.accounts.seller.key(),
        units,
        gross: sale.gross,
        fee: sale.fee,
        net: sale.net,
        pool_usdc: sale.after.usdc,
        pool_units: sale.after.units,
    });
    Ok(())
}

#[derive(Accounts)]
pub struct BuyFromPool<'info> {
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
        mut,
        seeds = [POSITION_SEED, raise.key().as_ref(), raise.key().as_ref()],
        bump = pool_position.bump
    )]
    pub pool_position: Box<Account<'info, Position>>,

    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + Position::INIT_SPACE,
        seeds = [POSITION_SEED, raise.key().as_ref(), buyer.key().as_ref()],
        bump
    )]
    pub buyer_position: Box<Account<'info, Position>>,

    #[account(mut, seeds = [POOL_SEED, raise.key().as_ref()], bump = raise.pool_bump)]
    pub pool_vault: Box<Account<'info, TokenAccount>>,

    #[account(mut, seeds = [PAYOUTS_SEED, raise.key().as_ref()], bump = raise.payouts_bump)]
    pub payouts_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        token::mint = usdc_mint,
        constraint = buyer_usdc.owner == buyer.key() @ FanexError::RaiseAccountMismatch
    )]
    pub buyer_usdc: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn buy_from_pool(ctx: Context<BuyFromPool>, units: u64, max_in: u64) -> Result<()> {
    require_open_pool(&ctx.accounts.config, &ctx.accounts.raise)?;
    require!(units > 0, FanexError::RaiseZeroAmount);
    // The per-wallet limit counts the buyer's whole position.
    let held = add(ctx.accounts.buyer_position.units, units)?;
    require!(within_wallet_limit(held, ctx.accounts.raise.max_units), FanexError::WalletLimitExceeded);

    let token_program = ctx.accounts.token_program.to_account_info();
    let mint = ctx.accounts.usdc_mint.to_account_info();
    let payouts_vault = ctx.accounts.payouts_vault.to_account_info();
    let pool_vault = ctx.accounts.pool_vault.to_account_info();
    let authority = ctx.accounts.raise.to_account_info();
    sweep_pool(
        &token_program,
        &mint,
        &payouts_vault,
        &pool_vault,
        &authority,
        &mut ctx.accounts.raise,
        &mut ctx.accounts.pool_position,
    )?;

    // At the curve, and only while it prices the units at least at the pool's USDC per unit the fans
    // hold, which the close shares over their units (`pool_math::buy_pro_rata`, N+3's second audit).
    let fan_units = sub(ctx.accounts.raise.units_outstanding, ctx.accounts.raise.pool_units)?;
    let purchase = pool_math::buy_pro_rata(ctx.accounts.raise.pool(), units, ctx.accounts.config.pool_fee_bps, fan_units)?;
    require!(purchase.cost <= max_in, FanexError::PriceOutsideLimit);

    usdc_transfer(
        &token_program,
        &mint,
        &ctx.accounts.buyer_usdc.to_account_info(),
        &pool_vault,
        &ctx.accounts.buyer.to_account_info(),
        purchase.cost,
        &[],
    )?;

    let acc = ctx.accounts.raise.acc_per_unit;
    let raise_key = ctx.accounts.raise.key();
    let pool = &mut ctx.accounts.pool_position;
    let pool_left = sub(pool.units, units)?;
    pool.set_units(acc, pool_left)?;
    let buyer = &mut ctx.accounts.buyer_position;
    let created = buyer.init_if_new(raise_key, ctx.accounts.buyer.key(), ctx.accounts.payer.key(), ctx.bumps.buyer_position);
    buyer.set_units(acc, held)?;
    buyer.paid = add(buyer.paid, purchase.cost)?;
    // Sold back first, at the curve alone (`sell_to_pool`, N+3's audit).
    buyer.from_pool = add(buyer.from_pool, units)?;

    let raise = &mut ctx.accounts.raise;
    raise.pool_usdc = purchase.after.usdc;
    raise.pool_units = purchase.after.units;
    if created {
        raise.count_position_opened()?;
    }
    emit!(BoughtFromPool {
        raise: raise_key,
        buyer: ctx.accounts.buyer.key(),
        units,
        gross: purchase.gross,
        fee: purchase.fee,
        cost: purchase.cost,
        pool_usdc: purchase.after.usdc,
        pool_units: purchase.after.units,
    });
    Ok(())
}

#[derive(Accounts)]
pub struct SweepPoolEarnings<'info> {
    pub caller: Signer<'info>,

    #[account(seeds = [RAISE_CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, RaiseConfig>>,

    #[account(address = config.usdc_mint @ FanexError::RaiseAccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    #[account(mut)]
    pub raise: Box<Account<'info, Raise>>,

    #[account(
        mut,
        seeds = [POSITION_SEED, raise.key().as_ref(), raise.key().as_ref()],
        bump = pool_position.bump
    )]
    pub pool_position: Box<Account<'info, Position>>,

    #[account(mut, seeds = [PAYOUTS_SEED, raise.key().as_ref()], bump = raise.payouts_bump)]
    pub payouts_vault: Box<Account<'info, TokenAccount>>,

    #[account(mut, seeds = [POOL_SEED, raise.key().as_ref()], bump = raise.pool_bump)]
    pub pool_vault: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

/// Permissionless and never paused: the payouts earned by the quotas the pool
/// bought join the pool and lift its price (amended spec §3.2).
pub fn sweep_pool_earnings(ctx: Context<SweepPoolEarnings>) -> Result<()> {
    require!(ctx.accounts.raise.state == RaiseState::Active, FanexError::NotActive);
    let token_program = ctx.accounts.token_program.to_account_info();
    let mint = ctx.accounts.usdc_mint.to_account_info();
    let payouts_vault = ctx.accounts.payouts_vault.to_account_info();
    let pool_vault = ctx.accounts.pool_vault.to_account_info();
    let authority = ctx.accounts.raise.to_account_info();
    let swept = sweep_pool(
        &token_program,
        &mint,
        &payouts_vault,
        &pool_vault,
        &authority,
        &mut ctx.accounts.raise,
        &mut ctx.accounts.pool_position,
    )?;
    require!(swept > 0, FanexError::NothingOwed);
    emit!(PoolEarningsSwept {
        raise: ctx.accounts.raise.key(),
        amount: swept,
        pool_usdc: ctx.accounts.raise.pool_usdc,
    });
    Ok(())
}
