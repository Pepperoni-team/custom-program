//! The migration's third-party programs in LiteSVM 0.6.1 (upgrade N+1,
//! docs/solana/PEPPER-CURVE-PROGRAM.md «Migration (upgrade N+1)»; usdc-curve
//! design §10.10, §10.12).
//!
//! `migrate_to_usdc` sells a curve's SOL reserve through Jupiter v6 by CPI, on a
//! one-hop route over Orca's Whirlpool, Raydium's CLMM or Meteora's DLMM (R11).
//! Jupiter has no devnet deployment, so that swap is proven here only, on the
//! four binaries `scripts/solana/dump-litesvm-fixtures.sh` reads from mainnet,
//! each pinned by `fixtures.rs`. This suite proves the harness can hold them:
//! each passes the checks a deployment applies, with every syscall it imports
//! resolved (Jupiter imports `sol_get_sysvar`, SIMD-0127), and each runs, an
//! instruction it does not know refused by its own code. The deployment check
//! is needed because LiteSVM's own loader compiles with `reject_broken_elfs`
//! off: a binary importing a syscall the runtime lacks loads anyway and fails
//! only when it calls it (a malformed ELF does panic `add_program`).
//!
//! Every LiteSVM here runs under mainnet's Rent sysvar (`MAINNET_RENT`, in
//! `migration_common`), never LiteSVM's default: five live curves hold less than
//! the default's minimum.

mod common;
mod migration_common;

use common::*;
use migration_common::*;
use solana_bpf_loader_program::syscalls::create_program_runtime_environment_v1;
use solana_compute_budget::compute_budget::ComputeBudget;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_program_runtime::solana_sbpf::elf::{ElfError, Executable};
use solana_program_runtime::solana_sbpf::verifier::RequisiteVerifier;
use solana_sdk::{
    feature_set::{get_sysvar_syscall_enabled, FeatureSet},
    hash::hash,
    instruction::{Instruction, InstructionError},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};
use std::sync::Arc;

/// The router and the three venues its route may take, each at its mainnet
/// address, with what its own code answers an instruction it does not know: the
/// venues' Anchor dispatcher `InstructionFallbackNotFound` (101); Jupiter's
/// entrypoint, ahead of its Anchor dispatcher, `InvalidAccountData` for a
/// discriminator it does not route.
const MIGRATION_PROGRAMS: [(Pubkey, &str, InstructionError); 4] = [
    (JUPITER_V6_ID, JUPITER_V6_SO, InstructionError::InvalidAccountData),
    (ORCA_WHIRLPOOL_ID, ORCA_WHIRLPOOL_SO, InstructionError::Custom(ANCHOR_INSTRUCTION_FALLBACK_NOT_FOUND)),
    (RAYDIUM_CLMM_ID, RAYDIUM_CLMM_SO, InstructionError::Custom(ANCHOR_INSTRUCTION_FALLBACK_NOT_FOUND)),
    (METEORA_DLMM_ID, METEORA_DLMM_SO, InstructionError::Custom(ANCHOR_INSTRUCTION_FALLBACK_NOT_FOUND)),
];

/// The four Jupiter instructions the migration's CPI may send (R10).
const JUPITER_ROUTES: [&str; 4] = ["route", "route_v2", "shared_accounts_route", "shared_accounts_route_v2"];

/// Anchor's `InstructionFallbackNotFound`: a discriminator no handler has.
const ANCHOR_INSTRUCTION_FALLBACK_NOT_FOUND: u32 = 101;
/// Anchor's `InstructionDidNotDeserialize`: a known handler whose arguments
/// are missing.
const ANCHOR_INSTRUCTION_DID_NOT_DESERIALIZE: u32 = 102;

fn fixture_bytes(name: &str) -> Vec<u8> {
    let path = fixture_path(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e} (scripts/solana/dump-litesvm-fixtures.sh)", path.display()))
}

/// Loads the fixture `name` as a deployment does (the upgradeable loader's
/// `deploy_program` in solana-bpf-loader-program 2.2.4, the one LiteSVM 0.6.1
/// runs): the loader environment built from `feature_set` with the deployment's
/// stricter rules (`reject_broken_elfs` on and no `sol_alloc_free_`, what
/// `deploy_program`'s `morph_into_deployment_environment_v1` makes of the
/// runtime's), so a syscall it does not register refuses the load
/// (`UnresolvedSymbol`, returned), then the verifier (a refusal panics). The
/// negative control below goes through this same function, so it turns red if
/// the stricter rules are dropped here.
fn deploy_check(name: &str, feature_set: &FeatureSet) -> Result<(), ElfError> {
    let environment = create_program_runtime_environment_v1(feature_set, &ComputeBudget::default(), true, false)
        .unwrap_or_else(|e| panic!("the deployment's loader environment: {e}"));
    let executable = Executable::<InvokeContext>::load(&fixture_bytes(name), Arc::new(environment))?;
    executable.verify::<RequisiteVerifier>().unwrap_or_else(|e| panic!("{name}: the verifier refuses it: {e}"));
    Ok(())
}

/// Sends `data` with no accounts to the fixture `name` loaded at `program_id`
/// under mainnet's rent, and returns its refusal, once the logs show the
/// program's own code ran (compute units consumed inside it), where a program
/// the runtime could not load fails before any.
fn refusal(program_id: Pubkey, name: &str, data: Vec<u8>) -> TransactionError {
    let mut svm = mainnet_rent_svm();
    load_fixture_program(&mut svm, program_id, name);
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), SOL).unwrap();
    let ix = Instruction { program_id, accounts: vec![], data };
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer], svm.latest_blockhash());
    let failed = svm.send_transaction(tx).expect_err("the instruction is refused");
    let consumed = format!("Program {program_id} consumed ");
    let ran = failed.meta.logs.iter().any(|line| {
        line.strip_prefix(&consumed)
            .and_then(|rest| rest.split(' ').next())
            .and_then(|units| units.parse::<u64>().ok())
            .is_some_and(|units| units > 0)
    });
    assert!(ran, "{name}: its code never ran: {:?}", failed.meta.logs);
    failed.err
}

/// Each binary passes a deployment's checks under LiteSVM's own feature set
/// (`LiteSVM::new` takes `FeatureSet::all_enabled()` and builds its loader
/// environment from it, litesvm 0.6.1 `src/lib.rs`): every syscall it imports is
/// one this runtime registers.
#[test]
fn the_migration_programs_pass_a_deployments_checks() {
    let feature_set = FeatureSet::all_enabled();
    for (_, name, _) in MIGRATION_PROGRAMS {
        deploy_check(name, &feature_set).unwrap_or_else(|e| panic!("{name}: {e:?}"));
    }
}

/// The token program mainnet runs at `Tokenkeg…` since SIMD-0266 activated (p-token,
/// `P_TOKEN_SO`), which `migration_route.rs` loads in place of LiteSVM's SPL Token 3.5.0, passes
/// the same checks.
#[test]
fn p_token_passes_a_deployments_checks() {
    deploy_check(P_TOKEN_SO, &FeatureSet::all_enabled()).unwrap_or_else(|e| panic!("{P_TOKEN_SO}: {e:?}"));
}

/// The check above covers `sol_get_sysvar`: without the feature that registers
/// it, the same check refuses Jupiter's load on that very symbol, and the three
/// venues, which do not import it, still load.
#[test]
fn jupiter_needs_sol_get_sysvar_and_the_venues_do_not() {
    let mut without = FeatureSet::all_enabled();
    without.deactivate(&get_sysvar_syscall_enabled::id());
    let err = deploy_check(JUPITER_V6_SO, &without).expect_err("Jupiter does not load without sol_get_sysvar");
    assert!(matches!(&err, ElfError::UnresolvedSymbol(symbol, _, _) if symbol == "sol_get_sysvar"), "{err:?}");
    for (_, name, _) in &MIGRATION_PROGRAMS[1..] {
        deploy_check(name, &without).unwrap_or_else(|e| panic!("{name}: {e:?}"));
    }
}

/// Each binary runs in this LiteSVM, not only loads (as `fixtures.rs` proves of
/// the Raydium CPMM and lock): an instruction it does not know is refused by its
/// own code, with the answer `MIGRATION_PROGRAMS` names.
#[test]
fn the_migration_programs_run_in_litesvm() {
    for (program_id, name, answer) in MIGRATION_PROGRAMS {
        assert_eq!(refusal(program_id, name, vec![0; 8]), TransactionError::InstructionError(0, answer), "{name}");
    }
}

/// Jupiter's four routes (R10) pass its entrypoint and reach its Anchor
/// dispatcher, which knows them and fails on their missing arguments, where an
/// unknown discriminator stops at the entrypoint: this binary routes the four
/// instructions the migration's CPI names.
#[test]
fn jupiters_four_routes_reach_its_dispatcher() {
    for route in JUPITER_ROUTES {
        let discriminator = hash(format!("global:{route}").as_bytes()).to_bytes()[..8].to_vec();
        assert_eq!(
            refusal(JUPITER_V6_ID, JUPITER_V6_SO, discriminator),
            TransactionError::InstructionError(0, InstructionError::Custom(ANCHOR_INSTRUCTION_DID_NOT_DESERIALIZE)),
            "{route}"
        );
    }
}
