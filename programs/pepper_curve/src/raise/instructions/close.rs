//! The end of a raise (amended spec §4). It closes once the term is over (the
//! last month declared and due, or 90 days after that due date whatever was
//! declared) and the guarantee has been spent on any arrears; no cap ends it
//! earlier. The pool's quotas are cancelled; its USDC (the counter plus
//! whatever the vault holds above it) and the payouts vault's rounding
//! remainder are shared among the quotas fans still hold, or go to the creator
//! when the pool holds every quota; the guarantee left goes to the creator.
//! Arrears stay payable after close while quotas remain; with no quotas, explicit
//! write-offs retire the debt. Once no month is left, any fan's position
//! closes with everything it is owed.

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::raise::accrual::{acc_increment, rounding_remainder};
use crate::raise::checked::{add, sub};
use crate::raise::constants::*;
use crate::errors::FanexError;
use crate::raise::events::{Claimed, RaiseClosed};
use crate::raise::instructions::pool::sweep_pool;
use crate::raise::ledger::{closable, has_open_months, pool_destination, PoolDestination};
use crate::raise::state::*;
use crate::raise::transfer::{usdc_transfer, RaiseSeeds};
use crate::raise::validation::is_associated_token_account;

#[derive(Accounts)]
pub struct CloseRaise<'info> {
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

    #[account(mut, seeds = [POOL_SEED, raise.key().as_ref()], bump = raise.pool_bump)]
    pub pool_vault: Box<Account<'info, TokenAccount>>,

    #[account(mut, seeds = [GUARANTEE_SEED, raise.key().as_ref()], bump = raise.guarantee_bump)]
    pub guarantee_vault: Box<Account<'info, TokenAccount>>,

    #[account(mut, seeds = [PAYOUTS_SEED, raise.key().as_ref()], bump = raise.payouts_bump)]
    pub payouts_vault: Box<Account<'info, TokenAccount>>,

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

pub fn close_raise(ctx: Context<CloseRaise>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let raise_key = ctx.accounts.raise.key();
    {
        let raise = &ctx.accounts.raise;
        require!(raise.state == RaiseState::Active, FanexError::NotActive);
        let open = has_open_months(raise.oldest_open_month, raise.last_declared_month);
        require!(
            closable(raise.last_declared_month, raise.term_end_month, now, open, raise.guarantee_usdc),
            FanexError::NotClosable
        );
    }

    let token_program = ctx.accounts.token_program.to_account_info();
    let mint = ctx.accounts.usdc_mint.to_account_info();
    let payouts_vault = ctx.accounts.payouts_vault.to_account_info();
    let pool_vault = ctx.accounts.pool_vault.to_account_info();
    let creator_usdc = ctx.accounts.creator_usdc.to_account_info();
    let authority = ctx.accounts.raise.to_account_info();
    // The vault, not the counter: whatever was sent to it straight is shared too.
    let pool_vault_before = ctx.accounts.pool_vault.amount;

    // 1. What the pool's quotas earned joins the pool.
    let swept = sweep_pool(
        &token_program,
        &mint,
        &payouts_vault,
        &pool_vault,
        &authority,
        &mut ctx.accounts.raise,
        &mut ctx.accounts.pool_position,
    )?;
    let pool_cash = add(pool_vault_before, swept)?;

    // 2. The rounding remainder, measured on every unit before the pool's are cancelled.
    let (acc, credited_total, all_units) = {
        let raise = &ctx.accounts.raise;
        (raise.acc_per_unit, raise.credited_total, raise.units_outstanding)
    };
    let remainder = rounding_remainder(credited_total, all_units, acc)?;

    // 3. The pool's quotas are cancelled.
    let pool_units = ctx.accounts.pool_position.units;
    ctx.accounts.pool_position.set_units(acc, 0)?;
    let fan_units = sub(all_units, pool_units)?;

    // 4. The pool's USDC and the remainder go to the fans' quotas, or to the
    //    creator when the pool holds every quota (Review Focus 5).
    let shared = add(pool_cash, remainder)?;
    let raise_seeds = RaiseSeeds::of(&ctx.accounts.raise);
    let seeds = raise_seeds.seeds();
    let signer: &[&[&[u8]]] = &[&seeds];
    let (distributed, to_creator) = match pool_destination(fan_units) {
        PoolDestination::Holders => {
            usdc_transfer(&token_program, &mint, &pool_vault, &payouts_vault, &authority, pool_cash, signer)?;
            (shared, 0)
        }
        PoolDestination::Creator => {
            usdc_transfer(&token_program, &mint, &pool_vault, &creator_usdc, &authority, pool_cash, signer)?;
            usdc_transfer(&token_program, &mint, &payouts_vault, &creator_usdc, &authority, remainder, signer)?;
            (0, shared)
        }
    };

    // 5. The guarantee left goes to the creator: the vault's whole balance, the
    //    counter plus anything sent to it straight (ruling P3).
    let guarantee_left = ctx.accounts.guarantee_vault.amount;
    usdc_transfer(
        &token_program,
        &mint,
        &ctx.accounts.guarantee_vault.to_account_info(),
        &creator_usdc,
        &authority,
        guarantee_left,
        signer,
    )?;

    let raise = &mut ctx.accounts.raise;
    if distributed > 0 {
        let increment = acc_increment(distributed, fan_units)?;
        raise.acc_per_unit = raise
            .acc_per_unit
            .checked_add(increment)
            .ok_or_else(|| error!(FanexError::RaiseMathOverflow))?;
    }
    raise.units_outstanding = fan_units;
    raise.guarantee_usdc = 0;
    raise.pool_units = 0;
    raise.pool_usdc = 0;
    raise.state = RaiseState::Closed;
    raise.closed_at = now;

    emit!(RaiseClosed { raise: raise_key, distributed, to_creator, guarantee_returned: guarantee_left });
    Ok(())
}

#[derive(Accounts)]
pub struct ClosePosition<'info> {
    pub caller: Signer<'info>,

    #[account(seeds = [RAISE_CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, RaiseConfig>>,

    #[account(address = config.usdc_mint @ FanexError::RaiseAccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    #[account(mut)]
    pub raise: Box<Account<'info, Raise>>,

    #[account(mut, has_one = raise @ FanexError::RaiseAccountMismatch, close = rent_payer)]
    pub position: Box<Account<'info, Position>>,

    /// CHECK: receives the position's rent; bound to the payer recorded when it was opened.
    #[account(mut, address = position.rent_payer @ FanexError::RaiseAccountMismatch)]
    pub rent_payer: UncheckedAccount<'info>,

    /// What the position is still owed can only land in its owner's associated USDC account, the
    /// one its wallet shows (the internal audit of 2026-10-29); for the pool's own position, which
    /// is paid nothing here, a vault of the raise.
    #[account(
        mut,
        token::mint = usdc_mint,
        constraint = owner_usdc.owner == position.owner @ FanexError::RaiseAccountMismatch,
        constraint = position.owner == raise.key()
            || is_associated_token_account(&owner_usdc.key(), &position.owner, &usdc_mint.key()) @ FanexError::RaiseAccountMismatch
    )]
    pub owner_usdc: Box<Account<'info, TokenAccount>>,

    #[account(mut, seeds = [PAYOUTS_SEED, raise.key().as_ref()], bump = raise.payouts_bump)]
    pub payouts_vault: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

/// Returns a position's rent to whoever paid it; anyone may call it. Before a
/// raise closes, and while it still has an open month, only an empty position
/// closes, and the pool's own position stays while the raise is active (every
/// trade and the close need it). Once the raise is closed with no month left
/// open, a fan's position that still holds quotas closes too (ruling P2): it is
/// settled, everything pending goes to its owner with no MIN_CLAIM floor, and
/// its quotas leave `units_outstanding`, so a later payment is shared by the
/// holdings that remain. A refunding raise never closes a holding this way:
/// `refund` does.
pub fn close_position(ctx: Context<ClosePosition>) -> Result<()> {
    let raise_key = ctx.accounts.raise.key();
    let (state, open) = {
        let raise = &ctx.accounts.raise;
        (raise.state, has_open_months(raise.oldest_open_month, raise.last_declared_month))
    };
    let is_pool = ctx.accounts.position.owner == raise_key;
    require!(
        !is_pool || matches!(state, RaiseState::Closed | RaiseState::Refunding),
        FanexError::NotClosable
    );
    // The position closes on every `Ok` exit below, the early one included.
    ctx.accounts.raise.count_position_closed()?;
    let acc = ctx.accounts.raise.acc_per_unit;
    if is_pool || state != RaiseState::Closed || open {
        ctx.accounts.position.settle(acc)?;
        let position = &ctx.accounts.position;
        require!(position.units == 0 && position.pending == 0, FanexError::PositionNotEmpty);
        return Ok(());
    }

    let units = ctx.accounts.position.units;
    let amount = ctx.accounts.position.take_pending(acc)?;
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

    let raise = &mut ctx.accounts.raise;
    raise.units_outstanding = sub(raise.units_outstanding, units)?;
    if amount > 0 {
        emit!(Claimed { raise: raise_key, owner: ctx.accounts.position.owner, amount });
    }
    Ok(())
}
