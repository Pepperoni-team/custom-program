//! USDC moves, copied from `pepper_raise` (programs/pepper_raise/src/transfer.rs).
//! Always `transfer_checked`: the mint and its 6 decimals are part of every
//! transfer, so a wrong mint fails instead of moving the wrong token. The
//! funding, the claim, the close and the void share them.

use anchor_lang::prelude::*;
use anchor_spl::token::{self, CloseAccount, TransferChecked};

use crate::constants::USDC_DECIMALS;

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

/// Closes a token account the distributor PDA owns whose balance is zero; its
/// rent goes to `destination`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::solana_program::entrypoint::ProgramResult;
    use anchor_lang::solana_program::instruction::Instruction;
    use anchor_lang::solana_program::program_stubs::{set_syscall_stubs, SyscallStubs};
    use std::cell::RefCell;

    thread_local! {
        /// The CPIs this thread sent, with the number of signer seed sets of each.
        static SENT: RefCell<Vec<(Instruction, usize)>> = const { RefCell::new(Vec::new()) };
    }

    /// Records each CPI on the thread that sent it instead of running it: the
    /// host has no runtime, and the default stub answers `Ok` to any call, so it
    /// could never tell a skipped transfer from a sent one.
    struct Recorder;

    impl SyscallStubs for Recorder {
        fn sol_invoke_signed(&self, instruction: &Instruction, _accounts: &[AccountInfo], seeds: &[&[&[u8]]]) -> ProgramResult {
            SENT.with(|sent| sent.borrow_mut().push((instruction.clone(), seeds.len())));
            Ok(())
        }
    }

    fn sent() -> Vec<(Instruction, usize)> {
        SENT.with(|sent| sent.take())
    }

    /// An account nothing on the host can move: only its address is read.
    struct Stub {
        key: Pubkey,
        owner: Pubkey,
        lamports: u64,
        data: Vec<u8>,
    }

    impl Stub {
        fn new() -> Self {
            Self { key: Pubkey::new_unique(), owner: Pubkey::new_unique(), lamports: 0, data: Vec::new() }
        }

        fn info(&mut self) -> AccountInfo<'_> {
            AccountInfo::new(&self.key, false, false, &mut self.lamports, &mut self.data, &self.owner, false, 0)
        }
    }

    #[test]
    fn a_zero_amount_sends_nothing() {
        set_syscall_stubs(Box::new(Recorder));
        sent();
        let (mut program, mut mint, mut from, mut to, mut authority) = (Stub::new(), Stub::new(), Stub::new(), Stub::new(), Stub::new());
        usdc_transfer(&program.info(), &mint.info(), &from.info(), &to.info(), &authority.info(), 0, &[]).unwrap();
        assert!(sent().is_empty());
    }

    #[test]
    fn a_transfer_is_transfer_checked_at_six_decimals_on_the_token_program() {
        set_syscall_stubs(Box::new(Recorder));
        sent();
        let (mut program, mut mint, mut from, mut to, mut authority) = (Stub::new(), Stub::new(), Stub::new(), Stub::new(), Stub::new());
        let (mint_key, from_key, to_key, authority_key) = (mint.key, from.key, to.key, authority.key);
        let seeds: &[&[u8]] = &[b"distributor", &[7]];
        usdc_transfer(&program.info(), &mint.info(), &from.info(), &to.info(), &authority.info(), 1_234_567, &[seeds]).unwrap();

        let calls = sent();
        assert_eq!(calls.len(), 1);
        let (ix, seed_sets) = &calls[0];
        assert_eq!(ix.program_id, anchor_spl::token::ID);
        // SPL Token's TransferChecked: tag 12, the amount u64 LE, the decimals.
        let mut data = vec![12u8];
        data.extend_from_slice(&1_234_567u64.to_le_bytes());
        data.push(6);
        assert_eq!(ix.data, data);
        let metas: Vec<(Pubkey, bool, bool)> = ix.accounts.iter().map(|m| (m.pubkey, m.is_signer, m.is_writable)).collect();
        assert_eq!(
            metas,
            [(from_key, false, true), (mint_key, false, false), (to_key, false, true), (authority_key, true, false)]
        );
        assert_eq!(*seed_sets, 1);
    }

    #[test]
    fn a_vault_close_is_close_account_on_the_token_program() {
        set_syscall_stubs(Box::new(Recorder));
        sent();
        let (mut program, mut vault, mut destination, mut authority) = (Stub::new(), Stub::new(), Stub::new(), Stub::new());
        let (vault_key, destination_key, authority_key) = (vault.key, destination.key, authority.key);
        let seeds: &[&[u8]] = &[b"distributor", &[7]];
        vault_close(&program.info(), &vault.info(), &destination.info(), &authority.info(), &[seeds]).unwrap();

        let calls = sent();
        assert_eq!(calls.len(), 1);
        let (ix, seed_sets) = &calls[0];
        assert_eq!(ix.program_id, anchor_spl::token::ID);
        // SPL Token's CloseAccount: tag 9, no argument.
        assert_eq!(ix.data, vec![9u8]);
        let metas: Vec<(Pubkey, bool, bool)> = ix.accounts.iter().map(|m| (m.pubkey, m.is_signer, m.is_writable)).collect();
        assert_eq!(metas, [(vault_key, false, true), (destination_key, false, true), (authority_key, true, false)]);
        assert_eq!(*seed_sets, 1);
    }
}
