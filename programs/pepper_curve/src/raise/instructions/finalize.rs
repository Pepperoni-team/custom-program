//! Closing the raise (spec §4.5): at once when sold out, otherwise at the
//! deadline; anyone may call it. Above the minimum the money splits 90/5/5/10
//! (§4.2), the 10 open the exit pool at the average price fans paid (amended
//! spec §3.1), and payouts start with the first full month after close. Below
//! it the raise turns to refunding, and `refund` pays each buyer back into
//! their associated USDC account, with nothing needed from Fanex.

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::raise::calendar::first_paying_month;
use crate::raise::checked::add;
use crate::raise::constants::*;
use crate::errors::FanexError;
use crate::raise::events::{RaiseFinalized, Refunded};
use crate::raise::money::{close_split, meets_minimum};
use crate::raise::pool_math::virtual_units_for;
use crate::raise::state::*;
use crate::raise::transfer::{usdc_transfer, RaiseSeeds};
use crate::raise::validation::is_associated_token_account;

#[derive(Accounts)]
pub struct Finalize<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(seeds = [RAISE_CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, RaiseConfig>>,

    #[account(address = config.usdc_mint @ FanexError::RaiseAccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    #[account(mut)]
    pub raise: Box<Account<'info, Raise>>,

    /// The pool's position, owned by the raise PDA itself.
    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + Position::INIT_SPACE,
        seeds = [POSITION_SEED, raise.key().as_ref(), raise.key().as_ref()],
        bump
    )]
    pub pool_position: Box<Account<'info, Position>>,

    #[account(mut, seeds = [ESCROW_SEED, raise.key().as_ref()], bump = raise.escrow_bump)]
    pub escrow: Box<Account<'info, TokenAccount>>,

    #[account(mut, seeds = [GUARANTEE_SEED, raise.key().as_ref()], bump = raise.guarantee_bump)]
    pub guarantee_vault: Box<Account<'info, TokenAccount>>,

    #[account(mut, seeds = [POOL_SEED, raise.key().as_ref()], bump = raise.pool_bump)]
    pub pool_vault: Box<Account<'info, TokenAccount>>,

    /// The creator's associated USDC account (the internal audit of 2026-10-29).
    #[account(
        mut,
        token::mint = usdc_mint,
        constraint = creator_usdc.owner == raise.creator @ FanexError::RaiseAccountMismatch,
        constraint = is_associated_token_account(&creator_usdc.key(), &raise.creator, &usdc_mint.key()) @ FanexError::RaiseAccountMismatch
    )]
    pub creator_usdc: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        token::mint = usdc_mint,
        constraint = fee_usdc.owner == config.fee_wallet @ FanexError::RaiseAccountMismatch
    )]
    pub fee_usdc: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn finalize(ctx: Context<Finalize>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let raise_key = ctx.accounts.raise.key();
    {
        let raise = &ctx.accounts.raise;
        require!(raise.state == RaiseState::Raising, FanexError::NotRaising);
        require!(now >= raise.deadline || raise.sold_units == raise.max_units, FanexError::RaiseStillOpen);
    }
    // Recorded whatever the outcome, so `close_position` can always return its rent.
    let created = ctx.accounts.pool_position.init_if_new(raise_key, raise_key, ctx.accounts.payer.key(), ctx.bumps.pool_position);
    if created {
        ctx.accounts.raise.count_position_opened()?;
    }

    let fan_paid = ctx.accounts.raise.fan_paid;
    let split = close_split(fan_paid)?;
    if !meets_minimum(split.creator_part, ctx.accounts.raise.min_creator_part) {
        let raise = &mut ctx.accounts.raise;
        raise.state = RaiseState::Refunding;
        raise.finalized_at = now;
        emit!(RaiseFinalized {
            raise: raise_key,
            succeeded: false,
            fan_paid,
            creator_part: 0,
            pool_usdc: 0,
            virtual_units: 0,
        });
        return Ok(());
    }

    let raise_seeds = RaiseSeeds::of(&ctx.accounts.raise);
    let seeds = raise_seeds.seeds();
    let signer: &[&[&[u8]]] = &[&seeds];
    let token_program = ctx.accounts.token_program.to_account_info();
    let mint = ctx.accounts.usdc_mint.to_account_info();
    let escrow = ctx.accounts.escrow.to_account_info();
    let authority = ctx.accounts.raise.to_account_info();
    usdc_transfer(&token_program, &mint, &escrow, &ctx.accounts.creator_usdc.to_account_info(), &authority, split.to_creator, signer)?;
    usdc_transfer(&token_program, &mint, &escrow, &ctx.accounts.fee_usdc.to_account_info(), &authority, split.to_fanex, signer)?;
    usdc_transfer(&token_program, &mint, &escrow, &ctx.accounts.guarantee_vault.to_account_info(), &authority, split.to_guarantee, signer)?;
    usdc_transfer(&token_program, &mint, &escrow, &ctx.accounts.pool_vault.to_account_info(), &authority, split.to_pool, signer)?;

    let virtual_units = virtual_units_for(split.to_pool, ctx.accounts.raise.sold_units, fan_paid)?;
    let first_month = first_paying_month(now, ctx.accounts.raise.deadline)?;
    let raise = &mut ctx.accounts.raise;
    raise.state = RaiseState::Active;
    raise.creator_part = split.creator_part;
    raise.units_outstanding = raise.sold_units;
    raise.first_month = first_month;
    raise.term_end_month = first_month + raise.term_months as u32 - 1;
    raise.last_declared_month = first_month - 1;
    raise.last_finalized_month = first_month - 1;
    raise.oldest_open_month = first_month;
    raise.guarantee_usdc = split.to_guarantee;
    raise.pool_usdc = split.to_pool;
    raise.pool_units = 0;
    raise.virtual_units = virtual_units;
    raise.finalized_at = now;

    emit!(RaiseFinalized {
        raise: raise_key,
        succeeded: true,
        fan_paid,
        creator_part: split.creator_part,
        pool_usdc: split.to_pool,
        virtual_units,
    });
    Ok(())
}

#[derive(Accounts)]
pub struct Refund<'info> {
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

    /// The refund can only land in the associated USDC account of the position's owner, the one
    /// its wallet shows (the internal audit of 2026-10-29).
    #[account(
        mut,
        token::mint = usdc_mint,
        constraint = owner_usdc.owner == position.owner @ FanexError::RaiseAccountMismatch,
        constraint = is_associated_token_account(&owner_usdc.key(), &position.owner, &usdc_mint.key()) @ FanexError::RaiseAccountMismatch
    )]
    pub owner_usdc: Box<Account<'info, TokenAccount>>,

    #[account(mut, seeds = [ESCROW_SEED, raise.key().as_ref()], bump = raise.escrow_bump)]
    pub escrow: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

pub fn refund(ctx: Context<Refund>) -> Result<()> {
    require!(ctx.accounts.raise.state == RaiseState::Refunding, FanexError::NotRefunding);
    let raise_key = ctx.accounts.raise.key();
    // The pool's own position paid nothing and has nothing to refund; its rent
    // goes back through `close_position`.
    require!(ctx.accounts.position.owner != raise_key, FanexError::RaiseAccountMismatch);
    let amount = ctx.accounts.position.paid;
    let owner = ctx.accounts.position.owner;

    let raise_seeds = RaiseSeeds::of(&ctx.accounts.raise);
    let seeds = raise_seeds.seeds();
    let signer: &[&[&[u8]]] = &[&seeds];
    usdc_transfer(
        &ctx.accounts.token_program.to_account_info(),
        &ctx.accounts.usdc_mint.to_account_info(),
        &ctx.accounts.escrow.to_account_info(),
        &ctx.accounts.owner_usdc.to_account_info(),
        &ctx.accounts.raise.to_account_info(),
        amount,
        signer,
    )?;

    let raise = &mut ctx.accounts.raise;
    raise.refunded = add(raise.refunded, amount)?;
    raise.count_position_closed()?;
    emit!(Refunded { raise: raise_key, owner, amount });
    Ok(())
}
