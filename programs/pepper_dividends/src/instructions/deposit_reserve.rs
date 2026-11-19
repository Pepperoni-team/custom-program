//! `deposit_reserve(amount)` (the reserve's spec §4.1, 2026-11-07): the curve's creator moves USDC
//! from their own account into the token's reserve, where it waits for the months it pays. The
//! dividend payer pays the reserve's and its vault's rents at the first deposit, so a creator
//! spends only USDC.

use anchor_lang::prelude::*;
use anchor_spl::associated_token::{get_associated_token_address, AssociatedToken};
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::constants::{ACCOUNT_VERSION, CONFIG_SEED, CURVE_SEED, RESERVE_SEED};
use crate::errors::DividendsError;
use crate::events::ReserveDeposited;
use crate::state::{DividendReserve, DividendsConfig};
use crate::transfer::usdc_transfer;
use crate::validation::{curve_creator, known_record_version, known_version, reserve_owner_after_deposit};

#[derive(Accounts)]
pub struct DepositReserve<'info> {
    /// The curve's creator: its USDC pays.
    pub creator: Signer<'info>,

    /// Pays the reserve's and its vault's rents at the first deposit: the config's dividend payer
    /// (`Unauthorized` for any other key), as for a month's rents.
    #[account(mut, address = config.payer_funder @ DividendsError::Unauthorized)]
    pub rent_payer: Signer<'info>,

    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = known_version(config.version) @ DividendsError::UnsupportedVersion,
    )]
    pub config: Box<Account<'info, DividendsConfig>>,

    /// CHECK: the token: bound into the reserve's address and its state only.
    pub mint: UncheckedAccount<'info>,

    /// CHECK: bound by its seeds under `config.curve_program` (`ConstraintSeeds` for another mint's),
    /// read by hand (`curve_creator`): only the curve's creator deposits.
    #[account(seeds = [CURVE_SEED, mint.key().as_ref()], bump, seeds::program = config.curve_program)]
    pub curve: UncheckedAccount<'info>,

    /// The token's reserve, born at its first deposit. One this deposit creates reads version 0, as
    /// a month's record does at its funding; any version but that and `ACCOUNT_VERSION` is
    /// `UnsupportedVersion`.
    #[account(
        init_if_needed,
        payer = rent_payer,
        space = 8 + DividendReserve::INIT_SPACE,
        seeds = [RESERVE_SEED, mint.key().as_ref()],
        bump,
        constraint = known_record_version(reserve.version) @ DividendsError::UnsupportedVersion,
    )]
    pub reserve: Box<Account<'info, DividendReserve>>,

    /// The reserve's vault, its associated USDC account, born at the first deposit. Anchor checks
    /// what it finds (its mint, its owner, its token program, its address).
    #[account(
        init_if_needed,
        payer = rent_payer,
        associated_token::mint = usdc_mint,
        associated_token::authority = reserve,
        associated_token::token_program = token_program,
    )]
    pub reserve_vault: Box<Account<'info, TokenAccount>>,

    /// The creator's own USDC ATA (`AccountMismatch` for any other account).
    #[account(
        mut,
        address = get_associated_token_address(&creator.key(), &usdc_mint.key()) @ DividendsError::AccountMismatch,
    )]
    pub creator_usdc: Box<Account<'info, TokenAccount>>,

    /// The config's USDC (`AccountMismatch` for another mint).
    #[account(address = config.usdc_mint @ DividendsError::AccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

/// After the accounts' checks above, in this order: funding not paused (`Paused`), a positive
/// amount (`ZeroAmount`), the signer the curve's creator (`NotTheCreator`, `CurveUnreadable`), then
/// the reserve's owner rule (`reserve_owner_after_deposit`: `WithdrawalPending`,
/// `ReserveOwnedByAnother`). Then a new reserve is written, its owner set, `amount` moved from the
/// creator's USDC into the vault with `transfer_checked`, and `ReserveDeposited` emitted.
pub fn deposit_reserve(ctx: Context<DepositReserve>, amount: u64) -> Result<()> {
    let config = &ctx.accounts.config;
    require!(!config.paused, DividendsError::Paused);
    require!(amount > 0, DividendsError::ZeroAmount);
    let creator = ctx.accounts.creator.key();
    let curve = &ctx.accounts.curve;
    let curves_creator = {
        let data = curve.try_borrow_data()?;
        curve_creator(curve.owner, &config.curve_program, &data[..])?
    };
    require_keys_eq!(curves_creator, creator, DividendsError::NotTheCreator);

    let held = ctx.accounts.reserve_vault.amount;
    let reserve = &mut ctx.accounts.reserve;
    let owner = reserve_owner_after_deposit(&reserve.owner, &creator, held, reserve.withdrawal_requested_at)?;
    if reserve.version == 0 {
        reserve.version = ACCOUNT_VERSION;
        reserve.bump = ctx.bumps.reserve;
        reserve.mint = ctx.accounts.mint.key();
        reserve.withdrawal_requested_at = 0;
        reserve.reserved = [0; 64];
    }
    reserve.owner = owner;
    let mint = reserve.mint;

    usdc_transfer(
        &ctx.accounts.token_program.to_account_info(),
        &ctx.accounts.usdc_mint.to_account_info(),
        &ctx.accounts.creator_usdc.to_account_info(),
        &ctx.accounts.reserve_vault.to_account_info(),
        &ctx.accounts.creator.to_account_info(),
        amount,
        &[],
    )?;

    let balance = held.checked_add(amount).ok_or_else(|| error!(DividendsError::MathOverflow))?;
    emit!(ReserveDeposited { mint, owner, amount, balance });
    Ok(())
}
