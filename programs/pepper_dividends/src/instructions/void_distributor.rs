//! `void_distributor` (spec §4.8; Rick, 2026-10-23): a month funded wrong, by its root or its
//! total, voided by the admin while nobody has claimed from it. The money and the rents go back as
//! at a close (`settle::settle_month`), and the record turns voided, so the month may be funded
//! again under the right root.

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::constants::{CONFIG_SEED, MONTH_SEED, MONTH_STATUS_VOIDED};
use crate::errors::DividendsError;
use crate::events::UsdcDistributorVoided;
use crate::settle::{settle_month, Settlement};
use crate::state::{DividendsConfig, MonthRecord, UsdcDistributor};
use crate::validation::{check_void, known_version};

#[derive(Accounts)]
pub struct VoidDistributor<'info> {
    /// The admin, the upgrade Squad's vault (`Unauthorized` for any other key).
    #[account(address = config.admin @ DividendsError::Unauthorized)]
    pub admin: Signer<'info>,

    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = known_version(config.version) @ DividendsError::UnsupportedVersion,
    )]
    pub config: Box<Account<'info, DividendsConfig>>,

    /// The month, naming the accounts the money and the rents go back to, as at a close
    /// (`AccountMismatch` for any other).
    #[account(
        mut,
        has_one = funder_usdc_account @ DividendsError::AccountMismatch,
        has_one = rent_payer @ DividendsError::AccountMismatch,
        constraint = known_version(distributor.version) @ DividendsError::UnsupportedVersion,
    )]
    pub distributor: Box<Account<'info, UsdcDistributor>>,

    /// The month's record, bound by its seeds (`ConstraintSeeds` for another month's), kept voided.
    #[account(
        mut,
        seeds = [MONTH_SEED, distributor.mint.as_ref(), &distributor.month.to_le_bytes()],
        bump = month_record.bump,
        constraint = known_version(month_record.version) @ DividendsError::UnsupportedVersion,
    )]
    pub month_record: Box<Account<'info, MonthRecord>>,

    /// The month's vault: the distributor's associated USDC account.
    #[account(
        mut,
        associated_token::mint = usdc_mint,
        associated_token::authority = distributor,
        associated_token::token_program = token_program,
    )]
    pub vault: Box<Account<'info, TokenAccount>>,

    /// CHECK: the funder's USDC ATA the funding recorded (`has_one` above), read by the transfer
    /// alone, as at a close.
    #[account(mut)]
    pub funder_usdc_account: UncheckedAccount<'info>,

    /// CHECK: the rent payer the funding recorded (`has_one` above).
    #[account(mut)]
    pub rent_payer: UncheckedAccount<'info>,

    /// The config's USDC (`AccountMismatch` for another mint).
    #[account(address = config.usdc_mint @ DividendsError::AccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    pub token_program: Program<'info, Token>,
}

/// After the accounts' checks above: nobody has claimed from the month, `claimed_amount` at 0 and
/// not one bit of the registry set (`MonthHasClaims`), since the void drops the bitmap and a
/// holder already paid could be paid again. Then the month is settled voided, which
/// `init_distributor` opens again, and `UsdcDistributorVoided` emitted.
pub fn void_distributor(ctx: Context<VoidDistributor>) -> Result<()> {
    let accounts = ctx.accounts;
    let distributor = &accounts.distributor;
    check_void(distributor.claimed_amount, &distributor.claimed)?;
    let (mint, month, to) = (distributor.mint, distributor.month, distributor.funder_usdc_account);
    let returned = settle_month(
        Settlement {
            distributor: &accounts.distributor,
            month_record: &mut accounts.month_record,
            vault: &accounts.vault,
            funder_usdc_account: accounts.funder_usdc_account.to_account_info(),
            rent_payer: accounts.rent_payer.to_account_info(),
            usdc_mint: &accounts.usdc_mint,
            token_program: &accounts.token_program,
        },
        MONTH_STATUS_VOIDED,
    )?;
    emit!(UsdcDistributorVoided { mint, month, returned, to });
    Ok(())
}
