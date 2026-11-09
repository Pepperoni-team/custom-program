//! USDC moves. Always `transfer_checked`: the mint and its 6 decimals are part
//! of every transfer, so a wrong mint fails instead of moving the wrong token.

use anchor_lang::prelude::*;
use anchor_spl::token::{self, CloseAccount, TransferChecked};

use crate::raise::constants::{RAISE_SEED, USDC_DECIMALS};
use crate::raise::state::Raise;

/// Moves `amount` USDC; a zero amount is a no-op. `signer_seeds` is empty when
/// the authority signs the transaction itself.
pub fn usdc_transfer<'info>(
    token_program: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    from: &AccountInfo<'info>,
    to: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    amount: u64,
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
        USDC_DECIMALS,
    )
}

/// Closes a token account the raise PDA owns whose balance is zero; its rent goes to `destination`.
pub fn vault_close<'info>(
    token_program: &AccountInfo<'info>,
    vault: &AccountInfo<'info>,
    destination: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    let accounts = CloseAccount { account: vault.clone(), destination: destination.clone(), authority: authority.clone() };
    token::close_account(CpiContext::new_with_signer(token_program.clone(), accounts, signer_seeds))
}

/// Owned copies of the raise PDA's seeds, so a handler can sign for a vault
/// while it still mutates the raise.
pub struct RaiseSeeds {
    creator: Pubkey,
    id: [u8; 8],
    bump: [u8; 1],
}

impl RaiseSeeds {
    pub fn of(raise: &Raise) -> Self {
        Self { creator: raise.creator, id: raise.raise_id.to_le_bytes(), bump: [raise.bump] }
    }

    pub fn seeds(&self) -> [&[u8]; 4] {
        [RAISE_SEED, self.creator.as_ref(), &self.id, &self.bump]
    }
}
