//! How a payout reaches a user's wallet.
//!
//! `sell` pays out of the `SolVault` and `claim_dividend` out of the
//! `Distributor`: program-owned accounts with data, which the System Program
//! cannot debit, so the program moves their lamports itself. A lamport move is
//! not an instruction, so an explorer shows it only as a balance change: Solscan
//! listed a sell as tokens leaving the wallet and nothing arriving
//! (docs/solana/PEPPER-CURVE-PROGRAM.md §5.5).
//!
//! With the payout transit passed as the ONE extra account after the
//! instruction's own list, the lamports pass through it and reach the user by a
//! System Program transfer this program signs for the transit. Without it they
//! move directly, as every transaction built before 2026-09-29 expects.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{instruction::AccountMeta, program::invoke_signed, system_instruction};

use crate::constants::PAYOUT_TRANSIT_SEED;
use crate::errors::FanexError;
use crate::validation::validate_payout_transit;

/// Move `amount` lamports out of `source` (an account this program owns) into
/// `recipient`, through the payout transit when the caller passed it.
///
/// The transit is credited by a direct move and debited by the transfer within
/// this call, so it ends where it started: empty, or holding whatever someone
/// once sent it.
///
/// ⚠️ **`source` rides along as a third, unused account of the transfer.** At a
/// CPI the runtime brings into its own ledger only the accounts the invoked
/// instruction lists, then refuses the call if this instruction's accounts no
/// longer sum to what they held (`UnbalancedInstruction`). Listing only the
/// transfer's two accounts showed it the transit already credited and the
/// source not yet debited, and the first devnet sell failed exactly so
/// (2026-09-29). The System Program reads its first two accounts and ignores the
/// rest, and so does the RPC's parsed view of a transfer.
pub fn pay_out<'info>(
    source: &AccountInfo<'info>,
    recipient: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    extra_accounts: &[AccountInfo<'info>],
    amount: u64,
) -> Result<()> {
    if extra_accounts.is_empty() {
        return move_lamports(source, recipient, amount);
    }

    let source_key = *source.key;
    let (expected_transit, bump) =
        Pubkey::find_program_address(&[PAYOUT_TRANSIT_SEED, source_key.as_ref()], &crate::ID);
    let extra_keys: Vec<Pubkey> = extra_accounts.iter().map(|account| *account.key).collect();
    validate_payout_transit(&extra_keys, &expected_transit)?;
    let transit = &extra_accounts[0];
    require!(transit.is_writable, FanexError::AccountMismatch);

    move_lamports(source, transit, amount)?;

    let mut transfer = system_instruction::transfer(transit.key, recipient.key, amount);
    transfer.accounts.push(AccountMeta::new(source_key, false));

    let bump_bytes = [bump];
    let seeds: [&[u8]; 3] = [PAYOUT_TRANSIT_SEED, source_key.as_ref(), &bump_bytes];
    invoke_signed(
        &transfer,
        &[transit.clone(), recipient.clone(), source.clone(), system_program.clone()],
        &[&seeds[..]],
    )?;
    Ok(())
}

/// A direct lamport move. The borrows end with this function, before any CPI.
fn move_lamports<'info>(from: &AccountInfo<'info>, to: &AccountInfo<'info>, amount: u64) -> Result<()> {
    let mut from_lamports = from.try_borrow_mut_lamports()?;
    let mut to_lamports = to.try_borrow_mut_lamports()?;
    **from_lamports = from_lamports
        .checked_sub(amount)
        .ok_or(FanexError::InsufficientReserves)?;
    **to_lamports = to_lamports
        .checked_add(amount)
        .ok_or(FanexError::MathOverflow)?;
    Ok(())
}
