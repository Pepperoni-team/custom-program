//! `initialize_config`, `update_config`, the pause and the admin hand-over.
//!
//! Since the internal audit of 2026-10-29 the config is born only by the upgrade authority this
//! program's ProgramData records (it was first-caller-wins: a stranger could take it at deploy).
//! A stranger, an immutable program (its header recording no authority, or the loader's own
//! `--final` leaving the former key's bytes behind the option byte), a program with no ProgramData
//! and another program's ProgramData are refused. The admin is named as an account the system
//! program owns (never a multisig account, never the all-zero key), the launch fee stops at USD 50
//! on both paths, and the admin moves in two steps. Every refusal is read by its code, with the
//! bank left as it was.

mod common;

use anchor_lang::error::ErrorCode;
use anchor_lang::solana_program::bpf_loader_upgradeable;
use anchor_lang::Space;
use common::*;
use pepper_curve::raise::constants::{LAUNCH_FEE_MAX, PENDING_ADMIN_SEED};
use pepper_curve::errors::FanexError;
use pepper_curve::raise::state::{PendingAdmin, RaiseConfig};
use solana_sdk::account::Account;
use solana_sdk::instruction::Instruction;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signer};
use solana_sdk::transaction::TransactionError;

/// The Squads v4 program, which owns every multisig account.
const SQUADS_V4: Pubkey = Pubkey::from_str_const("SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf");

fn config_bytes(env: &Env) -> Option<Vec<u8>> {
    env.svm.get_account(&config_pda()).filter(|a| a.lamports > 0).map(|a| a.data)
}

fn pending_admin_pda() -> Pubkey {
    pda(&[PENDING_ADMIN_SEED])
}

fn update(signer: &Pubkey, args: pepper_curve::instruction::UpdateRaiseConfig) -> Instruction {
    ix(pepper_curve::accounts::UpdateRaiseConfig { config: config_pda(), admin: *signer }, args)
}

fn no_change() -> pepper_curve::instruction::UpdateRaiseConfig {
    pepper_curve::instruction::UpdateRaiseConfig {
        new_admin: None,
        new_attester: None,
        new_fee_wallet: None,
        new_launch_fee: None,
        new_pool_fee_bps: None,
    }
}

fn set_pool_fee(env: &mut Env, bps: u16) -> Result<(), TransactionError> {
    let admin = env.admin.pubkey();
    let update = update(&admin, pepper_curve::instruction::UpdateRaiseConfig { new_pool_fee_bps: Some(bps), ..no_change() });
    env.send(&[update], &[])
}

fn set_launch_fee(env: &mut Env, fee: u64) -> Result<(), TransactionError> {
    let admin = env.admin.pubkey();
    let update = update(&admin, pepper_curve::instruction::UpdateRaiseConfig { new_launch_fee: Some(fee), ..no_change() });
    env.send(&[update], &[])
}

fn propose_ix(admin: &Pubkey, new_admin: &Pubkey) -> Instruction {
    ix(
        pepper_curve::accounts::ProposeAdmin {
            admin: *admin,
            config: config_pda(),
            pending_admin: pending_admin_pda(),
            new_admin: *new_admin,
            system_program: solana_sdk::system_program::id(),
        },
        pepper_curve::instruction::ProposeAdmin {},
    )
}

fn accept_ix(new_admin: &Pubkey, payer: &Pubkey) -> Instruction {
    ix(
        pepper_curve::accounts::AcceptAdmin { new_admin: *new_admin, config: config_pda(), pending_admin: pending_admin_pda(), payer: *payer },
        pepper_curve::instruction::AcceptAdmin {},
    )
}

/// An account the Squads program owns, as a multisig account is.
fn put_multisig_account(env: &mut Env) -> Pubkey {
    let address = Pubkey::new_unique();
    let lamports = env.svm.minimum_balance_for_rent_exemption(120);
    env.svm.set_account(address, Account { lamports, data: vec![7; 120], owner: SQUADS_V4, executable: false, rent_epoch: 0 }).unwrap();
    address
}

fn funded(env: &mut Env) -> Keypair {
    let wallet = Keypair::new();
    env.svm.airdrop(&wallet.pubkey(), 1_000_000_000).unwrap();
    wallet
}

// ── The config's birth ───────────────────────────────────────────────────────

#[test]
fn a_stranger_cannot_take_the_config_after_the_deploy() {
    // The audit's finding: the program is deployed, its config not yet made. A stranger pays, signs
    // as if it were the upgrade authority and names itself the admin.
    let mut env = Env::without_config(ProgramLoad::UpgradeableByAdmin);
    let stranger = funded(&mut env);
    let init = env.init_config_ix(&stranger.pubkey(), &stranger.pubkey(), &stranger.pubkey(), 5 * USD, POOL_FEE_BPS);
    expect_err(env.send_as(&stranger, &[init], &[]), code(FanexError::RaiseUnauthorized));
    assert_eq!(config_bytes(&env), None, "a stranger initialized RaiseConfig and became its admin");
    // The upgrade authority still can, afterwards.
    env.init_config(5 * USD, POOL_FEE_BPS);
    assert_eq!(env.fetch::<RaiseConfig>(&config_pda()).admin, env.admin.pubkey());
}

#[test]
fn the_upgrade_authority_initializes_the_config_and_names_a_vault_as_admin() {
    // The mainnet shape when the deploy key still holds the upgrade authority: it signs, another
    // wallet pays, and the admin named is a vault that need not sign, nor even exist yet.
    let mut env = Env::without_config(ProgramLoad::UpgradeableByAdmin);
    let (authority, payer) = (env.admin.insecure_clone(), funded(&mut env));
    let vault = Pubkey::new_unique();
    let (payer_before, authority_before) = (env.lamports(&payer.pubkey()), env.lamports(&authority.pubkey()));
    let init = env.init_config_ix(&payer.pubkey(), &authority.pubkey(), &vault, 5 * USD, POOL_FEE_BPS);
    env.send_as(&payer, &[init], &[&authority]).unwrap();
    let rent = env.svm.minimum_balance_for_rent_exemption(8 + RaiseConfig::INIT_SPACE);
    assert_eq!(payer_before - env.lamports(&payer.pubkey()), rent + 2 * 5_000);
    assert_eq!(env.lamports(&authority.pubkey()), authority_before);
    let config: RaiseConfig = env.fetch(&config_pda());
    assert_eq!(config.admin, vault);
    assert_eq!((config.attester, config.fee_wallet, config.usdc_mint), (env.attester.pubkey(), env.fee_wallet.pubkey(), env.mint));
    assert_eq!((config.launch_fee, config.pool_fee_bps, config.paused), (5 * USD, POOL_FEE_BPS, false));
}

#[test]
fn an_immutable_program_takes_no_config() {
    // Two immutable programs: a header that records no authority, and one the loader itself made
    // immutable, as `set-upgrade-authority --final` leaves one on a cluster.
    let mut made_final = Env::without_config(ProgramLoad::UpgradeableByAdmin);
    let former = made_final.admin.insecure_clone();
    let to_none = bpf_loader_upgradeable::set_upgrade_authority(&pepper_curve::ID, &former.pubkey(), None);
    made_final.send(&[to_none], &[]).unwrap();
    let header = made_final.svm.get_account(&program_data_pda(&pepper_curve::ID)).unwrap().data[..PROGRAM_DATA_HEADER].to_vec();
    assert_eq!(header[12], 0, "the loader recorded no authority");
    assert_eq!(&header[13..], former.pubkey().as_ref(), "the former key's bytes stay after the option byte");

    for mut env in [Env::without_config(ProgramLoad::Immutable), made_final] {
        let admin = env.admin.pubkey();
        let init = env.init_config_ix(&admin, &admin, &admin, 5 * USD, POOL_FEE_BPS);
        expect_err(env.send(&[init], &[]), code(FanexError::RaiseUnauthorized));
        assert_eq!(config_bytes(&env), None);
    }
}

#[test]
fn a_program_with_no_program_data_takes_no_config() {
    // LiteSVM's `add_program` gives the program no ProgramData: nothing the loader owns sits at its
    // address, so no key is its upgrade authority.
    let mut env = Env::without_config(ProgramLoad::WithoutProgramData);
    assert!(!env.exists(&program_data_pda(&pepper_curve::ID)));
    let admin = env.admin.pubkey();
    let init = env.init_config_ix(&admin, &admin, &admin, 5 * USD, POOL_FEE_BPS);
    expect_err(env.send(&[init], &[]), code(FanexError::RaiseUnauthorized));
    assert_eq!(config_bytes(&env), None);
}

#[test]
fn another_programs_program_data_is_refused_by_its_seeds() {
    // The same bytes deployed at another address, `Env::admin` its upgrade authority: its ProgramData
    // records the signer, but it is not this program's.
    let mut env = Env::without_config(ProgramLoad::UpgradeableByAdmin);
    let elf = std::fs::read(program_so()).unwrap();
    let other = Pubkey::new_unique();
    let other_data = load_upgradeable_program(&mut env.svm, other, &elf, Some(env.admin.pubkey()));
    let admin = env.admin.pubkey();
    let init = env.init_config_ix_with(&admin, &admin, &other_data, &admin, 5 * USD, POOL_FEE_BPS);
    expect_err(env.send(&[init], &[]), anchor_code(ErrorCode::ConstraintSeeds));
    assert_eq!(config_bytes(&env), None);
}

#[test]
fn the_admin_is_an_account_of_the_system_program() {
    // A multisig account (the Squads program's, which never signs) and the all-zero key (the system
    // program itself, the native loader's): both would lose the admin for ever.
    let mut env = Env::without_config(ProgramLoad::UpgradeableByAdmin);
    let multisig = put_multisig_account(&mut env);
    let admin = env.admin.pubkey();
    for named in [multisig, Pubkey::default()] {
        let init = env.init_config_ix(&admin, &admin, &named, 5 * USD, POOL_FEE_BPS);
        expect_err(env.send(&[init], &[]), anchor_code(ErrorCode::AccountNotSystemOwned));
    }
    assert_eq!(config_bytes(&env), None);
}

#[test]
fn the_config_carries_every_field() {
    let env = Env::new();
    let config: RaiseConfig = env.fetch(&config_pda());
    assert_eq!(config.admin, env.admin.pubkey());
    assert_eq!(config.attester, env.attester.pubkey());
    assert_eq!(config.fee_wallet, env.fee_wallet.pubkey());
    assert_eq!(config.usdc_mint, env.mint);
    assert_eq!(config.launch_fee, 5 * USD);
    assert_eq!(config.pool_fee_bps, POOL_FEE_BPS);
    assert!(!config.paused);
}

#[test]
fn the_config_cannot_be_initialized_twice() {
    let mut env = Env::new();
    let first = config_bytes(&env).unwrap();
    let admin = env.admin.pubkey();
    let again = env.init_config_ix(&admin, &admin, &admin, 0, 0);
    // The system program refuses to create an account that already exists: its AccountAlreadyInUse.
    expect_err(env.send(&[again], &[]), SYSTEM_ACCOUNT_ALREADY_IN_USE);
    assert_eq!(config_bytes(&env).unwrap(), first);
}

// ── The fees ─────────────────────────────────────────────────────────────────

#[test]
fn the_pool_fee_stops_at_three_percent() {
    let mut env = Env::new();
    expect_err(set_pool_fee(&mut env, 301), code(FanexError::FeeTooHigh));
    set_pool_fee(&mut env, 300).unwrap();
    assert_eq!(env.fetch::<RaiseConfig>(&config_pda()).pool_fee_bps, 300);
}

#[test]
fn the_launch_fee_stops_at_fifty_dollars_on_both_paths() {
    // At the birth.
    let mut fresh = Env::without_config(ProgramLoad::UpgradeableByAdmin);
    let admin = fresh.admin.pubkey();
    let init = fresh.init_config_ix(&admin, &admin, &admin, LAUNCH_FEE_MAX + 1, POOL_FEE_BPS);
    expect_err(fresh.send(&[init], &[]), code(FanexError::LaunchFeeTooHigh));
    assert_eq!(config_bytes(&fresh), None);
    fresh.init_config(LAUNCH_FEE_MAX, POOL_FEE_BPS);
    assert_eq!(fresh.launch_fee(), 50 * USD);
    // And at every update: a creator's whole balance can never be the fee.
    let mut env = Env::new();
    expect_err(set_launch_fee(&mut env, LAUNCH_FEE_MAX + 1), code(FanexError::LaunchFeeTooHigh));
    expect_err(set_launch_fee(&mut env, u64::MAX), code(FanexError::LaunchFeeTooHigh));
    assert_eq!(env.launch_fee(), 5 * USD);
    set_launch_fee(&mut env, LAUNCH_FEE_MAX).unwrap();
    assert_eq!(env.launch_fee(), LAUNCH_FEE_MAX);
}

// ── The admin ────────────────────────────────────────────────────────────────

#[test]
fn only_the_admin_updates_the_config() {
    let mut env = Env::new();
    let stranger = funded(&mut env);
    let update = update(&stranger.pubkey(), pepper_curve::instruction::UpdateRaiseConfig { new_attester: Some(stranger.pubkey()), ..no_change() });
    expect_err(env.send(&[update], &[&stranger]), code(FanexError::RaiseUnauthorized));
}

#[test]
fn the_admin_pauses_and_resumes() {
    let mut env = Env::new();
    env.set_paused(true).unwrap();
    assert!(env.fetch::<RaiseConfig>(&config_pda()).paused);
    env.set_paused(false).unwrap();
    assert!(!env.fetch::<RaiseConfig>(&config_pda()).paused);
}

#[test]
fn update_config_no_longer_hands_the_admin_over() {
    // One step named any 32 bytes the admin, the all-zero key included, with nobody accepting.
    let mut env = Env::new();
    let admin = env.admin.pubkey();
    for named in [Pubkey::default(), Pubkey::new_unique(), admin] {
        let update = update(&admin, pepper_curve::instruction::UpdateRaiseConfig { new_admin: Some(named), ..no_change() });
        expect_err(env.send(&[update], &[]), code(FanexError::AdminMovesInTwoSteps));
    }
    assert_eq!(env.fetch::<RaiseConfig>(&config_pda()).admin, admin);
}

#[test]
fn the_admin_moves_once_the_proposed_admin_accepts() {
    let mut env = Env::new();
    let (admin, next) = (env.admin.insecure_clone(), funded(&mut env));
    env.send(&[propose_ix(&admin.pubkey(), &next.pubkey())], &[]).unwrap();
    // Proposed, not yet the admin: the old admin still rules, the new one does not.
    let pending: PendingAdmin = env.fetch(&pending_admin_pda());
    assert_eq!((pending.admin, pending.payer), (next.pubkey(), admin.pubkey()));
    assert_eq!(env.fetch::<RaiseConfig>(&config_pda()).admin, admin.pubkey());
    let rent = env.lamports(&pending_admin_pda());
    assert_eq!(rent, env.svm.minimum_balance_for_rent_exemption(8 + PendingAdmin::INIT_SPACE));
    // Nobody but the proposed admin accepts.
    let stranger = funded(&mut env);
    expect_err(env.send_as(&stranger, &[accept_ix(&stranger.pubkey(), &admin.pubkey())], &[]), code(FanexError::RaiseUnauthorized));
    // Its rent can only go back to whoever paid it.
    expect_err(env.send_as(&next, &[accept_ix(&next.pubkey(), &stranger.pubkey())], &[]), code(FanexError::RaiseAccountMismatch));
    let payer_before = env.lamports(&admin.pubkey());
    env.send_as(&next, &[accept_ix(&next.pubkey(), &admin.pubkey())], &[]).unwrap();
    assert_eq!(env.fetch::<RaiseConfig>(&config_pda()).admin, next.pubkey());
    assert!(!env.exists(&pending_admin_pda()));
    assert_eq!(env.lamports(&admin.pubkey()), payer_before + rent);
    // The old admin is now a stranger to the config; the new one pauses.
    expect_err(env.set_paused(true), code(FanexError::RaiseUnauthorized));
    let pause = ix(pepper_curve::accounts::UpdateRaiseConfig { config: config_pda(), admin: next.pubkey() }, pepper_curve::instruction::SetRaisePaused { paused: true });
    env.send_as(&next, &[pause], &[]).unwrap();
    assert!(env.fetch::<RaiseConfig>(&config_pda()).paused);
}

#[test]
fn only_the_admin_proposes_and_only_an_account_of_the_system_program() {
    let mut env = Env::new();
    let stranger = funded(&mut env);
    expect_err(env.send_as(&stranger, &[propose_ix(&stranger.pubkey(), &stranger.pubkey())], &[]), code(FanexError::RaiseUnauthorized));
    let admin = env.admin.pubkey();
    let multisig = put_multisig_account(&mut env);
    for named in [multisig, Pubkey::default()] {
        expect_err(env.send(&[propose_ix(&admin, &named)], &[]), anchor_code(ErrorCode::AccountNotSystemOwned));
    }
    assert!(!env.exists(&pending_admin_pda()));
}

#[test]
fn a_later_proposal_replaces_the_first() {
    let mut env = Env::new();
    let admin = env.admin.insecure_clone();
    let (first, second) = (funded(&mut env), funded(&mut env));
    env.send(&[propose_ix(&admin.pubkey(), &first.pubkey())], &[]).unwrap();
    env.send(&[propose_ix(&admin.pubkey(), &second.pubkey())], &[]).unwrap();
    expect_err(env.send_as(&first, &[accept_ix(&first.pubkey(), &admin.pubkey())], &[]), code(FanexError::RaiseUnauthorized));
    env.send_as(&second, &[accept_ix(&second.pubkey(), &admin.pubkey())], &[]).unwrap();
    assert_eq!(env.fetch::<RaiseConfig>(&config_pda()).admin, second.pubkey());
}
