//! The USDC side's token moves (USDC curve design §10.3.5 step 8, §10.3.7): every one is a
//! `transfer_checked`, so the mint and its decimals travel with the transfer and a wrong mint fails
//! instead of moving the wrong token. The SOL instructions keep their own `token::transfer` calls.

use anchor_lang::prelude::*;
use anchor_spl::token::{self, TransferChecked};

/// Moves `amount` base units of `mint`, a mint of `decimals`, from `from` to `to`; a zero amount
/// moves nothing and calls nothing. `signer_seeds` is empty when `authority` signs the transaction
/// itself, and names the PDA otherwise (the curve, for its vaults). A copy of `pepper_raise`'s
/// `usdc_transfer` with the decimals a parameter, since the USDC side moves the curve's own token
/// too (the vesting, the swaps' token leg).
#[allow(clippy::too_many_arguments)]
pub fn transfer_checked<'info>(
    token_program: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    from: &AccountInfo<'info>,
    to: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    amount: u64,
    decimals: u8,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    let accounts = TransferChecked {
        from: from.clone(),
        mint: mint.clone(),
        to: to.clone(),
        authority: authority.clone(),
    };
    token::transfer_checked(
        CpiContext::new_with_signer(token_program.clone(), accounts, signer_seeds),
        amount,
        decimals,
    )
}
