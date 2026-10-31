//! `top_up_distributor(amount)` (the internal audit, 2026-10-29): more µUSDC moved into an open
//! month by its funder, with the config's approver's signature, and the month's total raised by as
//! much. The program cannot check a root against its total: a month funded short of its leaves pays
//! them first come, first served until `ExceedsFundedTotal`, and once a claim has landed the void no
//! longer fixes it. The top-up completes it; the root, and so what each leaf claims, stay as funded,
//! and whatever no leaf claims goes back to the funder at the close, as the rest.

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::constants::CONFIG_SEED;
use crate::errors::DividendsError;
use crate::events::UsdcDistributorToppedUp;
use crate::state::{DividendsConfig, UsdcDistributor};
use crate::transfer::usdc_transfer;
use crate::validation::{known_version, topped_up_total};

#[derive(Accounts)]
pub struct TopUpDistributor<'info> {
    /// The month's funder as the month records it (`Unauthorized` for any other key): its USDC pays.
    pub funder: Signer<'info>,

    /// The config's approver, co-signing as at the funding (`Unauthorized` for another key).
    #[account(address = config.approver @ DividendsError::Unauthorized)]
    pub approver: Signer<'info>,

    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = known_version(config.version) @ DividendsError::UnsupportedVersion,
    )]
    pub config: Box<Account<'info, DividendsConfig>>,

    /// The month. Only this program creates one, at its own address: its owner and discriminator
    /// bind it; a month closed or voided is gone (`AccountNotInitialized`).
    #[account(
        mut,
        has_one = funder @ DividendsError::Unauthorized,
        has_one = funder_usdc_account @ DividendsError::AccountMismatch,
        constraint = known_version(distributor.version) @ DividendsError::UnsupportedVersion,
    )]
    pub distributor: Box<Account<'info, UsdcDistributor>>,

    /// The month's vault: the distributor's associated USDC account.
    #[account(
        mut,
        associated_token::mint = usdc_mint,
        associated_token::authority = distributor,
        associated_token::token_program = token_program,
    )]
    pub vault: Box<Account<'info, TokenAccount>>,

    /// CHECK: the funder's USDC account the month records (`has_one` above), its owner's own
    /// associated account: the transfer reads it, signed by the funder.
    #[account(mut)]
    pub funder_usdc_account: UncheckedAccount<'info>,

    /// The config's USDC (`AccountMismatch` for another mint).
    #[account(address = config.usdc_mint @ DividendsError::AccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    pub token_program: Program<'info, Token>,
}

/// After the accounts' checks above, in this order: funding not paused (`Paused`), a positive
/// amount and a total that fits a u64 (`ZeroAmount`, `MathOverflow`). Then `amount` moves from the
/// funder's USDC account into the vault with `transfer_checked`, the total grows by as much, and
/// `UsdcDistributorToppedUp` is emitted.
pub fn top_up_distributor(ctx: Context<TopUpDistributor>, amount: u64) -> Result<()> {
    require!(!ctx.accounts.config.paused, DividendsError::Paused);
    let total_amount = topped_up_total(ctx.accounts.distributor.total_amount, amount)?;
    usdc_transfer(
        &ctx.accounts.token_program.to_account_info(),
        &ctx.accounts.usdc_mint.to_account_info(),
        &ctx.accounts.funder_usdc_account.to_account_info(),
        &ctx.accounts.vault.to_account_info(),
        &ctx.accounts.funder.to_account_info(),
        amount,
        &[],
    )?;
    let distributor = &mut ctx.accounts.distributor;
    distributor.total_amount = total_amount;
    emit!(UsdcDistributorToppedUp { mint: distributor.mint, month: distributor.month, amount, total_amount });
    Ok(())
}
