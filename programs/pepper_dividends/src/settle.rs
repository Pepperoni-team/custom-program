//! The one money move a close and a void share (spec §4.7, §4.8): the vault's whole balance,
//! stray transfers included, to the funder's recorded USDC account; the vault and the distributor
//! closed, their rents to the recorded rent payer; the month's record set to the status the
//! instruction names, and kept.

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::state::{DistributorSeeds, MonthRecord, UsdcDistributor};
use crate::transfer::{usdc_transfer, vault_close};

/// What a settlement moves, each account already held to the month by the instruction's own
/// constraints: the distributor's recorded `funder_usdc_account` and `rent_payer`, the record by
/// its seeds, the vault by its address.
pub struct Settlement<'a, 'info> {
    pub distributor: &'a Account<'info, UsdcDistributor>,
    pub month_record: &'a mut Account<'info, MonthRecord>,
    pub vault: &'a Account<'info, TokenAccount>,
    pub funder_usdc_account: AccountInfo<'info>,
    pub rent_payer: AccountInfo<'info>,
    pub usdc_mint: &'a Account<'info, Mint>,
    pub token_program: &'a Program<'info, Token>,
}

/// Settles the month at `status` and returns the µUSDC sent back: the vault's balance moved with
/// `transfer_checked` (nothing sent when it is empty, so the funder's account is then never read),
/// the vault closed with SPL Token's `CloseAccount`, both signed by the distributor's seeds, then
/// the distributor closed by this program. A frozen vault or funder account fails it, and the
/// month stays as it was.
pub fn settle_month(settlement: Settlement<'_, '_>, status: u8) -> Result<u64> {
    let Settlement { distributor, month_record, vault, funder_usdc_account, rent_payer, usdc_mint, token_program } = settlement;
    let returned = vault.amount;
    let seeds = DistributorSeeds::of(distributor);
    let seeds = seeds.seeds();
    let signer = &[&seeds[..]];
    let token_program = token_program.to_account_info();
    let authority = distributor.to_account_info();
    let vault = vault.to_account_info();
    usdc_transfer(&token_program, &usdc_mint.to_account_info(), &vault, &funder_usdc_account, &authority, returned, signer)?;
    vault_close(&token_program, &vault, &rent_payer, &authority, signer)?;
    distributor.close(rent_payer)?;
    month_record.status = status;
    Ok(returned)
}
