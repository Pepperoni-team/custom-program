//! `fund_month_from_reserve` (the reserve's spec §4.2, 2026-11-07): `init_distributor`'s twin, a
//! month funded from its token's reserve. The approver and the dividend payer sign, as for every
//! month; the program, never an argument, splits the month over the reserve's balance: the
//! holders first, the payer fronting what the reserve lacks, then Pepper's 3% from what is left,
//! the part the reserve cannot pay owed. The month records the reserve as its funder, so the
//! close sends what nobody claimed back to it.

use anchor_lang::prelude::*;
use anchor_spl::associated_token::{get_associated_token_address, AssociatedToken};
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::constants::{
    ACCOUNT_VERSION, CONFIG_SEED, DISTRIBUTOR_SEED, MONTH_SEED, MONTH_STATUS_OPEN, RESERVE_SEED, USDC_CONFIG_SEED,
};
use crate::errors::DividendsError;
use crate::events::UsdcDistributorFundedFromReserve;
use crate::state::{DividendReserve, DividendsConfig, MonthRecord, ReserveSeeds, UsdcDistributor};
use crate::transfer::usdc_transfer;
use crate::validation::{
    check_expiry, check_month_before_withdrawal, check_month_record, check_no_legacy_month, claim_registry_bytes,
    known_record_version, known_version, leaf_count_in_range, reserve_split, review_window_end,
    usdc_config_fee_account,
};

#[derive(Accounts)]
#[instruction(month: i64, merkle_root: [u8; 32], total_amount: u64, expires_at: i64, leaf_count: u32)]
pub struct FundMonthFromReserve<'info> {
    /// The config's approver, a key of its own, co-signing every month (`Unauthorized` for another).
    #[account(address = config.approver @ DividendsError::Unauthorized)]
    pub approver: Signer<'info>,

    /// The config's dividend payer (`Unauthorized` for any other key): it pays the three rents, as
    /// for every month, and fronts from its USDC what the reserve lacks.
    #[account(mut, address = config.payer_funder @ DividendsError::Unauthorized)]
    pub rent_payer: Signer<'info>,

    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = known_version(config.version) @ DividendsError::UnsupportedVersion,
    )]
    pub config: Box<Account<'info, DividendsConfig>>,

    /// CHECK: the token: bound into the month's and the reserve's addresses and into the month's state.
    pub mint: UncheckedAccount<'info>,

    /// CHECK: `pepper_curve`'s v1 and v2 month of this token and month, bound and read as by
    /// `init_distributor` (`LegacyMonthLive`).
    #[account(
        seeds = [DISTRIBUTOR_SEED, mint.key().as_ref(), &month.to_le_bytes()],
        bump,
        seeds::program = config.curve_program,
    )]
    pub legacy_distributor: UncheckedAccount<'info>,

    /// Created with the month and never closed, checked before the distributor as by
    /// `init_distributor` (`MonthAlreadyFunded`, `UnsupportedVersion`).
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

    /// The distributor's associated USDC account, `init_if_needed` as at `init_distributor`.
    #[account(
        init_if_needed,
        payer = rent_payer,
        associated_token::mint = usdc_mint,
        associated_token::authority = distributor,
        associated_token::token_program = token_program,
    )]
    pub vault: Box<Account<'info, TokenAccount>>,

    /// The token's reserve, born by a deposit (`AccountNotInitialized` before one).
    #[account(
        seeds = [RESERVE_SEED, mint.key().as_ref()],
        bump = reserve.bump,
        constraint = known_version(reserve.version) @ DividendsError::UnsupportedVersion,
    )]
    pub reserve: Box<Account<'info, DividendReserve>>,

    /// The reserve's vault: its balance is what the reserve can pay.
    #[account(
        mut,
        associated_token::mint = usdc_mint,
        associated_token::authority = reserve,
        associated_token::token_program = token_program,
    )]
    pub reserve_vault: Box<Account<'info, TokenAccount>>,

    /// CHECK: the dividend payer's own USDC ATA (`AccountMismatch` for any other account), read by the
    /// transfer alone and only when the reserve is short of the month, so a payer with no USDC
    /// account funds a month the reserve covers.
    #[account(
        mut,
        address = get_associated_token_address(&rent_payer.key(), &usdc_mint.key()) @ DividendsError::AccountMismatch,
    )]
    pub payer_usdc: UncheckedAccount<'info>,

    /// CHECK: `pepper_curve`'s USDC config, bound by its seeds under `config.curve_program`
    /// (`ConstraintSeeds` for another address), read by hand (`usdc_config_fee_account`) for its USDC
    /// and its fee account.
    #[account(seeds = [USDC_CONFIG_SEED], bump, seeds::program = config.curve_program)]
    pub usdc_config: UncheckedAccount<'info>,

    /// CHECK: the USDC config's `fee_usdc_account`, held to it in the handler (`AccountMismatch`), where
    /// Pepper's 3% goes; read by the transfer alone, and only when the reserve pays some of it.
    #[account(mut)]
    pub fee_usdc_account: UncheckedAccount<'info>,

    /// The config's USDC (`AccountMismatch` for another mint).
    #[account(address = config.usdc_mint @ DividendsError::AccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

/// After the accounts' checks above, in this order: `init_distributor`'s (`Paused`, `ZeroAmount`,
/// `InvalidLeafCount`, `ExpiryTooSoon`, `ExpiryTooFar`, `LegacyMonthLive`), then the reserve's: with a
/// withdrawal pending, a month accrued before it (`InvalidMonth`, `MonthAfterWithdrawalRequest`); the
/// reserve holding something (`ReserveEmpty`); the USDC config readable and on the config's USDC, the
/// fee account its own (`UsdcConfigUnreadable`, `AccountMismatch`). Then the distributor and the record
/// written, the reserve as the funder, its vault as the funder's account, the two amounts the split
/// leaves; the month's claims held for its review window; the reserve's part and the payer's moved into
/// the vault, the reserve's fee to the fee account, each with `transfer_checked` (a zero amount sends
/// nothing); `UsdcDistributorFundedFromReserve` emitted.
pub fn fund_month_from_reserve(
    ctx: Context<FundMonthFromReserve>,
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
    let legacy = &ctx.accounts.legacy_distributor;
    check_no_legacy_month(legacy.owner, &config.curve_program, &legacy.try_borrow_data()?, now)?;
    check_month_before_withdrawal(month, ctx.accounts.reserve.withdrawal_requested_at)?;
    let split = reserve_split(ctx.accounts.reserve_vault.amount, total_amount)?;
    let (config_usdc, fee_account) = {
        let usdc_config = &ctx.accounts.usdc_config;
        let data = usdc_config.try_borrow_data()?;
        usdc_config_fee_account(usdc_config.owner, &config.curve_program, &data[..])?
    };
    require_keys_eq!(config_usdc, config.usdc_mint, DividendsError::AccountMismatch);
    require_keys_eq!(ctx.accounts.fee_usdc_account.key(), fee_account, DividendsError::AccountMismatch);

    let mint = ctx.accounts.mint.key();
    let reserve_key = ctx.accounts.reserve.key();
    let distributor_key = ctx.accounts.distributor.key();
    let distributor = &mut ctx.accounts.distributor;
    distributor.version = ACCOUNT_VERSION;
    distributor.bump = ctx.bumps.distributor;
    distributor.mint = mint;
    distributor.month = month;
    distributor.merkle_root = merkle_root;
    distributor.total_amount = total_amount;
    distributor.claimed_amount = 0;
    distributor.funder = reserve_key;
    distributor.funder_usdc_account = ctx.accounts.reserve_vault.key();
    distributor.rent_payer = ctx.accounts.rent_payer.key();
    distributor.expires_at = expires_at;
    distributor.leaf_count = leaf_count;
    distributor.claims_held_until = review_window_end(now);
    // A creator's money: no re-home ever moves its funder.
    distributor.funded_by_fanex = false;
    // The rent payer is the config's payer, held by its account's constraint.
    distributor.rents_by_payer = true;
    distributor.fronted_amount = split.fronted;
    distributor.fee_unpaid = split.fee_unpaid;
    distributor.reserved = [0; 38];
    distributor.claimed = vec![0; claim_registry_bytes(leaf_count)];

    let record = &mut ctx.accounts.month_record;
    record.version = ACCOUNT_VERSION;
    record.bump = ctx.bumps.month_record;
    record.status = MONTH_STATUS_OPEN;
    record.distributor = distributor_key;
    record.funded_at = now;
    record.reserved = [0; 24];

    let token_program = ctx.accounts.token_program.to_account_info();
    let usdc_mint = ctx.accounts.usdc_mint.to_account_info();
    let reserve_vault = ctx.accounts.reserve_vault.to_account_info();
    let vault = ctx.accounts.vault.to_account_info();
    let seeds = ReserveSeeds::of(&ctx.accounts.reserve);
    let seeds = seeds.seeds();
    let reserve_signs = &[&seeds[..]];
    let reserve = ctx.accounts.reserve.to_account_info();
    usdc_transfer(&token_program, &usdc_mint, &reserve_vault, &vault, &reserve, split.from_reserve, reserve_signs)?;
    usdc_transfer(
        &token_program,
        &usdc_mint,
        &ctx.accounts.payer_usdc.to_account_info(),
        &vault,
        &ctx.accounts.rent_payer.to_account_info(),
        split.fronted,
        &[],
    )?;
    usdc_transfer(
        &token_program,
        &usdc_mint,
        &reserve_vault,
        &ctx.accounts.fee_usdc_account.to_account_info(),
        &reserve,
        split.fee_from_reserve,
        reserve_signs,
    )?;

    emit!(UsdcDistributorFundedFromReserve {
        mint,
        month,
        total_amount,
        from_reserve: split.from_reserve,
        fronted: split.fronted,
        fee_from_reserve: split.fee_from_reserve,
        fee_unpaid: split.fee_unpaid,
    });
    Ok(())
}
