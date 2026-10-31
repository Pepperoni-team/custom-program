//! `init_distributor` (spec §4.4): a month funded. The funder's USDC moves into the
//! distributor's vault against a merkle root, approved by the config's approver; the dividend
//! payer pays the three rents. A month `pepper_curve` still holds, or one this program opened,
//! closed or voided wrong, never opens twice: the record checks it first. Its claims open a
//! review window after the funding (the internal audit, 2026-10-29), so a wrong one is voided
//! before anyone claims from it.

use anchor_lang::prelude::*;
use anchor_spl::associated_token::{get_associated_token_address, AssociatedToken};
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::constants::{ACCOUNT_VERSION, CONFIG_SEED, CURVE_SEED, DISTRIBUTOR_SEED, MONTH_SEED, MONTH_STATUS_OPEN};
use crate::errors::DividendsError;
use crate::events::UsdcDistributorFunded;
use crate::state::{DividendsConfig, MonthRecord, UsdcDistributor};
use crate::transfer::usdc_transfer;
use crate::validation::{
    check_expiry, check_funder, check_month_record, check_no_legacy_month, claim_registry_bytes, curve_creator,
    is_pepper_funder, known_record_version, known_version, leaf_count_in_range, review_window_end,
};

#[derive(Accounts)]
#[instruction(month: i64, merkle_root: [u8; 32], total_amount: u64, expires_at: i64, leaf_count: u32)]
pub struct InitDistributor<'info> {
    /// Its USDC pays: the config's `pepper_funder` or `payer_funder`, or the curve's creator.
    pub funder: Signer<'info>,

    /// The config's approver, a key of its own, co-signing every month (`Unauthorized` for another).
    #[account(address = config.approver @ DividendsError::Unauthorized)]
    pub approver: Signer<'info>,

    /// Pays the three rents, the record's, the distributor's and the vault's: the config's
    /// dividend payer (`Unauthorized` for any other key, the internal audit of 2026-10-29), a
    /// creator's month included, so a creator spends only USDC and every month's rents go back
    /// to a key `rehome_distributor` can move.
    #[account(mut, address = config.payer_funder @ DividendsError::Unauthorized)]
    pub rent_payer: Signer<'info>,

    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = known_version(config.version) @ DividendsError::UnsupportedVersion,
    )]
    pub config: Box<Account<'info, DividendsConfig>>,

    /// CHECK: the token: bound into the month's three addresses and into its state only.
    pub mint: UncheckedAccount<'info>,

    /// CHECK: bound by its seeds under `config.curve_program` (`ConstraintSeeds` for another mint's),
    /// and read by hand (`curve_creator`) only when the funder is neither Fanex key: a Fanex month
    /// opens for a mint with no curve.
    #[account(seeds = [CURVE_SEED, mint.key().as_ref()], bump, seeds::program = config.curve_program)]
    pub curve: UncheckedAccount<'info>,

    /// CHECK: bound by its seeds under `config.curve_program`, the one address `pepper_curve`'s v1
    /// and v2 distributors of this token and month share (`ConstraintSeeds` for another month's or
    /// mint's), read by hand (`legacy_month_live`): a v1 or v2 month there blocks this one until
    /// its expiry, any other data for good, whatever its owner (`LegacyMonthLive`).
    #[account(
        seeds = [DISTRIBUTOR_SEED, mint.key().as_ref(), &month.to_le_bytes()],
        bump,
        seeds::program = config.curve_program,
    )]
    pub legacy_distributor: UncheckedAccount<'info>,

    /// Created with the month and never closed. Before the distributor, with a check of its own:
    /// Anchor runs the init fields in their order, so a month open or closed is `MonthAlreadyFunded`
    /// before the distributor's `init` would meet an account in use. A voided one opens again. One
    /// this funding creates reads version 0; any version but that and `ACCOUNT_VERSION` is
    /// `UnsupportedVersion`.
    #[account(
        init_if_needed,
        payer = rent_payer,
        space = 8 + MonthRecord::INIT_SPACE,
        seeds = [MONTH_SEED, mint.key().as_ref(), &month.to_le_bytes()],
        bump,
        constraint = known_record_version(month_record.version) @ DividendsError::UnsupportedVersion,
        constraint = check_month_record(month_record.status).is_ok() @ DividendsError::MonthAlreadyFunded,
    )]
    pub month_record: Box<Account<'info, MonthRecord>>,

    #[account(
        init,
        payer = rent_payer,
        space = UsdcDistributor::space(leaf_count),
        seeds = [DISTRIBUTOR_SEED, mint.key().as_ref(), &month.to_le_bytes()],
        bump,
    )]
    pub distributor: Box<Account<'info, UsdcDistributor>>,

    /// The distributor's associated USDC account. `init_if_needed`: anyone may create it first,
    /// which `init` would refuse for good; Anchor checks what it finds (its mint, its owner, its
    /// token program, its address), and the close sweeps whatever a stranger left in it.
    #[account(
        init_if_needed,
        payer = rent_payer,
        associated_token::mint = usdc_mint,
        associated_token::authority = distributor,
        associated_token::token_program = token_program,
    )]
    pub vault: Box<Account<'info, TokenAccount>>,

    /// The funder's own USDC ATA (`AccountMismatch` for any other account), recorded so the close
    /// can always reach it: anyone can open an ATA again, so a funder who closed theirs cannot keep
    /// a month from closing.
    #[account(
        mut,
        address = get_associated_token_address(&funder.key(), &usdc_mint.key()) @ DividendsError::AccountMismatch,
    )]
    pub funder_usdc: Box<Account<'info, TokenAccount>>,

    /// The config's USDC (`AccountMismatch` for another mint).
    #[account(address = config.usdc_mint @ DividendsError::AccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

/// After the accounts' checks above, in this order: funding not paused (`Paused`), a positive
/// total (`ZeroAmount`), 1 to 65,536 leaves (`InvalidLeafCount`), an expiry 364 to 400 days out
/// (`ExpiryTooSoon`, `ExpiryTooFar`), the funder one of Fanex's two or the curve's creator
/// (`NotAFunder`, `CurveUnreadable`), no month of `pepper_curve` at the legacy address before its
/// expiry (`LegacyMonthLive`). Then the distributor and the record written, the month's claims
/// held for its review window and both funding flags recorded, `total_amount` moved from the
/// funder's USDC into the vault with `transfer_checked`, and `UsdcDistributorFunded` emitted.
pub fn init_distributor(
    ctx: Context<InitDistributor>,
    month: i64,
    merkle_root: [u8; 32],
    total_amount: u64,
    expires_at: i64,
    leaf_count: u32,
) -> Result<()> {
    let config = &ctx.accounts.config;
    require!(!config.paused, DividendsError::Paused);
    require!(total_amount > 0, DividendsError::ZeroAmount);
    require!(leaf_count_in_range(leaf_count), DividendsError::InvalidLeafCount);
    let now = Clock::get()?.unix_timestamp;
    check_expiry(expires_at, now)?;
    let funder = ctx.accounts.funder.key();
    let curve = &ctx.accounts.curve;
    check_funder(&funder, &config.pepper_funder, &config.payer_funder, || {
        let data = curve.try_borrow_data()?;
        curve_creator(curve.owner, &config.curve_program, &data[..])
    })?;
    let legacy = &ctx.accounts.legacy_distributor;
    check_no_legacy_month(legacy.owner, &config.curve_program, &legacy.try_borrow_data()?, now)?;
    let funded_by_fanex = is_pepper_funder(&funder, &config.pepper_funder, &config.payer_funder);

    let mint = ctx.accounts.mint.key();
    let distributor_key = ctx.accounts.distributor.key();
    let distributor = &mut ctx.accounts.distributor;
    distributor.version = ACCOUNT_VERSION;
    distributor.bump = ctx.bumps.distributor;
    distributor.mint = mint;
    distributor.month = month;
    distributor.merkle_root = merkle_root;
    distributor.total_amount = total_amount;
    distributor.claimed_amount = 0;
    distributor.funder = funder;
    distributor.funder_usdc_account = ctx.accounts.funder_usdc.key();
    distributor.rent_payer = ctx.accounts.rent_payer.key();
    distributor.expires_at = expires_at;
    distributor.leaf_count = leaf_count;
    distributor.claims_held_until = review_window_end(now);
    distributor.funded_by_fanex = funded_by_fanex;
    // The rent payer is the config's payer, held by its account's constraint.
    distributor.rents_by_payer = true;
    distributor.reserved = [0; 54];
    distributor.claimed = vec![0; claim_registry_bytes(leaf_count)];

    let record = &mut ctx.accounts.month_record;
    record.version = ACCOUNT_VERSION;
    record.bump = ctx.bumps.month_record;
    record.status = MONTH_STATUS_OPEN;
    record.distributor = distributor_key;
    record.funded_at = now;
    record.reserved = [0; 24];

    usdc_transfer(
        &ctx.accounts.token_program.to_account_info(),
        &ctx.accounts.usdc_mint.to_account_info(),
        &ctx.accounts.funder_usdc.to_account_info(),
        &ctx.accounts.vault.to_account_info(),
        &ctx.accounts.funder.to_account_info(),
        total_amount,
        &[],
    )?;

    emit!(UsdcDistributorFunded { mint, month, total_amount, funder });
    Ok(())
}
