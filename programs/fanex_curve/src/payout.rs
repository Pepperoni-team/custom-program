use anchor_lang::prelude::*;
use anchor_lang::solana_program::{instruction::AccountMeta, program::invoke_signed, system_instruction};

use crate::constants::PAYOUT_TRANSIT_SEED;
use crate::errors::FanexError;
use crate::validation::validate_payout_transit;

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
