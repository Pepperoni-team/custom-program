//! `rehome_distributor` (the internal audit, 2026-10-29): an open month's payees moved to the
//! config's dividend payer by the admin, the upgrade Squad's vault. A funding records who funded the
//! month, the USDC account its leftovers go back to and who paid its rents, and its close and its
//! void pay those three: after `payer_funder` is rotated, on a leak or as planned, every open month
//! would still pay the retired key. The re-home points them at the key the config names when it
//! runs: the funder and its USDC account only for a month one of Fanex's two funders funded, the
//! rent payer only for a month whose rents the dividend payer paid, so a creator's leftovers never
//! move.

use anchor_lang::prelude::*;
use anchor_spl::associated_token::get_associated_token_address;

use crate::constants::CONFIG_SEED;
use crate::errors::DividendsError;
use crate::events::UsdcDistributorRehomed;
use crate::state::{DividendsConfig, UsdcDistributor};
use crate::validation::{known_version, rehome_plan};

#[derive(Accounts)]
pub struct RehomeDistributor<'info> {
    /// The admin, the upgrade Squad's vault (`Unauthorized` for any other key, the month's funder
    /// and the closer included).
    #[account(address = config.admin @ DividendsError::Unauthorized)]
    pub admin: Signer<'info>,

    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = known_version(config.version) @ DividendsError::UnsupportedVersion,
    )]
    pub config: Box<Account<'info, DividendsConfig>>,

    /// The month. Only this program creates one, at its own address: its owner and discriminator
    /// bind it; a month closed or voided is gone (`AccountNotInitialized`).
    #[account(mut, constraint = known_version(distributor.version) @ DividendsError::UnsupportedVersion)]
    pub distributor: Box<Account<'info, UsdcDistributor>>,
}

/// After the accounts' checks above: what `rehome_plan` allows moves to `config.payer_funder` as it
/// stands when this runs (`NotRehomable` for a month that records neither funding flag, or a config
/// that names no payer): the funder, and its USDC account as that key's associated account of the
/// config's USDC, for a month a Fanex key funded; the rent payer, for a month the dividend payer
/// paid the rents of. It takes no argument: the vault executes 48 hours or more after a proposal is
/// written, and the payer's rotation and the re-homes go through in one vault transaction. Nothing
/// else moves (the root, the amounts, the bitmap, the expiry, the hold), and a month already homed
/// there is written as it is. Emits `UsdcDistributorRehomed`.
pub fn rehome_distributor(ctx: Context<RehomeDistributor>) -> Result<()> {
    let config = &ctx.accounts.config;
    let payer = config.payer_funder;
    let distributor = &mut ctx.accounts.distributor;
    let plan = rehome_plan(distributor.funded_by_fanex, distributor.rents_by_payer, &payer)?;
    if plan.funder {
        distributor.funder = payer;
        distributor.funder_usdc_account = get_associated_token_address(&payer, &config.usdc_mint);
    }
    if plan.rent_payer {
        distributor.rent_payer = payer;
    }
    emit!(UsdcDistributorRehomed {
        mint: distributor.mint,
        month: distributor.month,
        funder: distributor.funder,
        funder_usdc_account: distributor.funder_usdc_account,
        rent_payer: distributor.rent_payer,
    });
    Ok(())
}
