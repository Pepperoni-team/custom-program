//! A throwaway probe for the migration's swap (track M's M2; docs/solana/PEPPER-CURVE-PROGRAM.md
//! «Migration (upgrade N+1)», usdc-curve design §10.10), **never deployed**: it lives outside
//! `programs/`, at an address no key holds, and runs only in
//! `tests-onchain/pepper-curve/tests/migration_route.rs`.
//!
//! It does what `migrate_to_usdc` will do with Jupiter and nothing else: it signs, by
//! `invoke_signed`, for a PDA of its own at `["migration_swap", mint]` whose WSOL account holds the
//! reserve, and forwards a route Jupiter's `/build` returned for that PDA as its `taker`, the
//! route's data verbatim and its accounts in `/build`'s order. None of the migration's checks are
//! here (the floor, the vaults' balances, the forbidden accounts): those are M9 and M10's, in
//! `pepper_curve` itself.
//!
//! Instruction data: `nest`, then `unchecked`, one byte each, then the route's data. With
//! `nest > 0` the probe first calls itself with `nest - 1`, so the suite can stand the route one
//! invoke level deeper per unit, as a caller above `pepper_curve` would (a Squads vault transaction
//! executing the migration). With `unchecked` set, the route goes out by `invoke_signed_unchecked`,
//! which skips `invoke_signed`'s check that no account it passes is borrowed, so the suite can
//! tell that check's compute units apart.
//!
//! Accounts: the swap PDA, the mint it is derived from, the router program, this program, then the
//! route's accounts.

use solana_program::{
    account_info::AccountInfo,
    entrypoint,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    program::{invoke_signed, invoke_signed_unchecked},
    program_error::ProgramError,
    pubkey::Pubkey,
};

/// The swap PDA's seed, the one `pepper_curve`'s `["migration_swap", mint]` uses.
pub const SWAP_SEED: &[u8] = b"migration_swap";

entrypoint!(process_instruction);

pub fn process_instruction(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [nest, unchecked, route_data @ ..] = data else {
        return Err(ProgramError::InvalidInstructionData);
    };
    let [swap, mint, router, _probe, route @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let (expected, bump) = Pubkey::find_program_address(&[SWAP_SEED, mint.key.as_ref()], program_id);
    if swap.key != &expected {
        return Err(ProgramError::InvalidSeeds);
    }
    let signer_seeds: &[&[u8]] = &[SWAP_SEED, mint.key.as_ref(), &[bump]];
    // Every occurrence of the swap PDA is a signer, as `/build` marks its `user_transfer_authority`
    // (the runtime merges a key's privileges across its occurrences anyway); no other account is.
    let metas = |list: &[AccountInfo]| -> Vec<AccountMeta> {
        list.iter()
            .map(|a| AccountMeta { pubkey: *a.key, is_signer: a.key == swap.key, is_writable: a.is_writable })
            .collect()
    };
    let instruction = if *nest > 0 {
        let mut nested = Vec::with_capacity(data.len());
        nested.extend_from_slice(&[nest - 1, *unchecked]);
        nested.extend_from_slice(route_data);
        Instruction { program_id: *program_id, accounts: metas(accounts), data: nested }
    } else {
        Instruction { program_id: *router.key, accounts: metas(route), data: route_data.to_vec() }
    };
    if *unchecked != 0 {
        invoke_signed_unchecked(&instruction, accounts, &[signer_seeds])
    } else {
        invoke_signed(&instruction, accounts, &[signer_seeds])
    }
}
