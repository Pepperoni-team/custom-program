//! `claim` (spec §4.6): leaf `index` of a month paid into the holder's own USDC account. The
//! claim stays as today, the holder's «Riscatta» with the sponsor paying the fee (Rick,
//! 2026-10-23); the program takes it from any sender, since the µUSDC can only reach the
//! claimant's associated account, and no Fanex cron sends one.

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::constants::CONFIG_SEED;
use crate::errors::DividendsError;
use crate::events::UsdcDividendClaimed;
use crate::math::{dividend_leaf, verify_merkle_proof};
use crate::state::{DistributorSeeds, DividendsConfig, UsdcDistributor};
use crate::transfer::usdc_transfer;
use crate::validation::{check_claims_open, check_month_claims_open, known_version, leaf_bit};

/// No `payer` account: the program would never read one, and the fee payer is the transaction's.
#[derive(Accounts)]
pub struct Claim<'info> {
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = known_version(config.version) @ DividendsError::UnsupportedVersion,
    )]
    pub config: Box<Account<'info, DividendsConfig>>,

    /// The month. Only this program creates one, at its own address: its owner and discriminator
    /// bind it, and its seeds sign for the vault.
    #[account(mut, constraint = known_version(distributor.version) @ DividendsError::UnsupportedVersion)]
    pub distributor: Box<Account<'info, UsdcDistributor>>,

    /// The month's vault: the distributor's associated USDC account.
    #[account(
        mut,
        associated_token::mint = usdc_mint,
        associated_token::authority = distributor,
        associated_token::token_program = token_program,
    )]
    pub vault: Box<Account<'info, TokenAccount>>,

    /// CHECK: the holder the leaf names: the money reaches only its associated account below.
    pub claimant: UncheckedAccount<'info>,

    /// The claimant's own USDC ATA, which a holder without one creates in the same transaction.
    #[account(
        mut,
        associated_token::mint = usdc_mint,
        associated_token::authority = claimant,
        associated_token::token_program = token_program,
    )]
    pub claimant_usdc: Box<Account<'info, TokenAccount>>,

    /// The config's USDC (`AccountMismatch` for another mint).
    #[account(address = config.usdc_mint @ DividendsError::AccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    pub token_program: Program<'info, Token>,
}

/// After the accounts' checks, in this order: claims open (`ClaimsPaused`), this month's claims
/// not held (`ClaimsHeld`), a positive amount (`ZeroAmount`), the index inside the registry
/// (`InvalidLeafIndex`), the leaf in the root (`InvalidProof`), its bit not set
/// (`AlreadyClaimed`), the claims within the funded total (`ExceedsFundedTotal`). Then `amount`
/// moves from the vault, signed by the distributor, the bit is set and `UsdcDividendClaimed`
/// emitted. As in v2, a claim stays possible until the month closes, its expiry included.
pub fn claim(ctx: Context<Claim>, index: u32, amount: u64, proof: Vec<[u8; 32]>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    check_claims_open(now, ctx.accounts.config.claims_paused_until)?;
    let distributor = &ctx.accounts.distributor;
    check_month_claims_open(now, distributor.claims_held_until)?;
    require!(amount > 0, DividendsError::ZeroAmount);
    require!(index < distributor.leaf_count, DividendsError::InvalidLeafIndex);
    let claimant = ctx.accounts.claimant.key();
    let leaf = dividend_leaf(&distributor.key(), &ctx.accounts.usdc_mint.key(), index, &claimant, amount);
    require!(verify_merkle_proof(&proof, distributor.merkle_root, leaf), DividendsError::InvalidProof);
    let (byte, mask) = leaf_bit(index);
    let registry_byte = *distributor.claimed.get(byte).ok_or(DividendsError::InvalidLeafIndex)?;
    require!(registry_byte & mask == 0, DividendsError::AlreadyClaimed);
    let claimed_amount = distributor.claimed_amount.checked_add(amount).ok_or(DividendsError::MathOverflow)?;
    require!(claimed_amount <= distributor.total_amount, DividendsError::ExceedsFundedTotal);

    let seeds = DistributorSeeds::of(distributor);
    usdc_transfer(
        &ctx.accounts.token_program.to_account_info(),
        &ctx.accounts.usdc_mint.to_account_info(),
        &ctx.accounts.vault.to_account_info(),
        &ctx.accounts.claimant_usdc.to_account_info(),
        &ctx.accounts.distributor.to_account_info(),
        amount,
        &[&seeds.seeds()],
    )?;

    let distributor = &mut ctx.accounts.distributor;
    distributor.claimed_amount = claimed_amount;
    distributor.claimed[byte] |= mask;
    emit!(UsdcDividendClaimed { mint: distributor.mint, month: distributor.month, claimant, index, amount });
    Ok(())
}
