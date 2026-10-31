//! `close_distributor` (spec §4.7): a month closed by its funder or the config's closer, at any
//! time once fully claimed, otherwise from its expiry and only once claims have been open for the
//! last 30 days, or from 90 days after its expiry whatever pause or hold ran. What is left goes
//! back to the funder and the rents to the rent payer (`settle::settle_month`); the record stays,
//! closed, so the month never opens again.

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::constants::{CONFIG_SEED, MONTH_SEED, MONTH_STATUS_CLOSED};
use crate::errors::DividendsError;
use crate::events::UsdcDistributorClosed;
use crate::settle::{settle_month, Settlement};
use crate::state::{DividendsConfig, MonthRecord, UsdcDistributor};
use crate::validation::{check_close, known_version};

#[derive(Accounts)]
pub struct CloseDistributor<'info> {
    /// The month's funder or the config's closer (`Unauthorized` for any other key).
    pub authority: Signer<'info>,

    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = known_version(config.version) @ DividendsError::UnsupportedVersion,
    )]
    pub config: Box<Account<'info, DividendsConfig>>,

    /// The month. Only this program creates one, at its own address: its owner and discriminator
    /// bind it, and it names the accounts the money and the rents go back to (`AccountMismatch`
    /// for any other), as the funding or a re-home (`rehome_distributor`) last wrote them.
    #[account(
        mut,
        has_one = funder_usdc_account @ DividendsError::AccountMismatch,
        has_one = rent_payer @ DividendsError::AccountMismatch,
        constraint = known_version(distributor.version) @ DividendsError::UnsupportedVersion,
    )]
    pub distributor: Box<Account<'info, UsdcDistributor>>,

    /// The month's record, bound by its seeds (`ConstraintSeeds` for another month's), kept closed.
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

    /// CHECK: the funder's USDC ATA the funding recorded (`has_one` above). Only the transfer reads
    /// it, so an empty vault closes even while its owner has closed it; a vault holding USDC needs
    /// it open again first, `create_idempotent` in the same transaction.
    #[account(mut)]
    pub funder_usdc_account: UncheckedAccount<'info>,

    /// CHECK: the rent payer the funding recorded (`has_one` above): the vault's and the
    /// distributor's rents go back to it.
    #[account(mut)]
    pub rent_payer: UncheckedAccount<'info>,

    /// The config's USDC (`AccountMismatch` for another mint).
    #[account(address = config.usdc_mint @ DividendsError::AccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    pub token_program: Program<'info, Token>,
}

/// After the accounts' checks above: the funder or the closer signed (`Unauthorized`), and the
/// month may close (`check_close`: fully claimed at any time; otherwise `NotExpired` before its
/// expiry and `ClaimsPausedRecently` until its claims have been open for 30 days, from the later
/// of the config's claims pause and this month's hold, and at most until 90 days after its
/// expiry). Then the month is settled closed and `UsdcDistributorClosed` emitted.
pub fn close_distributor(ctx: Context<CloseDistributor>) -> Result<()> {
    let accounts = ctx.accounts;
    let by = accounts.authority.key();
    let distributor = &accounts.distributor;
    require!(by == distributor.funder || by == accounts.config.closer, DividendsError::Unauthorized);
    check_close(
        distributor.claimed_amount,
        distributor.total_amount,
        Clock::get()?.unix_timestamp,
        distributor.expires_at,
        accounts.config.claims_paused_until,
        distributor.claims_held_until,
    )?;
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
        MONTH_STATUS_CLOSED,
    )?;
    emit!(UsdcDistributorClosed { mint, month, returned, to });
    Ok(())
}
