//! The vesting lock of new curves, run against the built program
//! (docs/solana/PEPPER-CURVE-PROGRAM.md §5.4, 2026-10-15).
//!
//! A curve created at or after `VESTING_LOCKED_UNTIL_GRADUATION_FROM` releases
//! its vesting only once it has graduated: before, the vested tokens would be
//! sold into the curve and paid from the reserve that backs what the holders
//! bought (docs/solana/ECONOMIC-MODEL.md §6.2). A curve created before keeps the
//! terms it launched with.

mod common;

use anchor_lang::solana_program::program_option::COption;
use anchor_lang::solana_program::program_pack::Pack;
use anchor_lang::{AccountSerialize, Space};
use anchor_spl::associated_token::get_associated_token_address as ata;
use anchor_spl::token::spl_token;
use common::*;
use pepper_curve::constants::{VESTING_LOCKED_UNTIL_GRADUATION_FROM, VESTING_SEED, VESTING_VAULT_SEED};
use pepper_curve::errors::FanexError;
use pepper_curve::state::{Curve, CurvePhase, Vesting};
use solana_sdk::{account::Account, pubkey::Pubkey, signature::Signer, system_program};

const TOTAL: u64 = 10_000_000 * 1_000_000;

/// A tradeable curve created at `created_at`, in `phase`, whose creator's vesting
/// (a one-day cliff, released whole) is fully vested two days later, the clock
/// set there.
fn curve_with_vesting(created_at: i64, phase: CurvePhase) -> Env {
    let mut env = Env::new();
    env.write_tradeable_curve(created_at);

    let curve_address = curve_pda(&env.mint);
    let mut curve: Curve = env.fetch(&curve_address);
    curve.phase = phase;
    let mut data = Vec::with_capacity(8 + Curve::INIT_SPACE);
    curve.try_serialize(&mut data).unwrap();
    data.resize(8 + Curve::INIT_SPACE, 0);
    write(&mut env, curve_address, data, pepper_curve::ID);

    let (vesting, bump) = Pubkey::find_program_address(&[VESTING_SEED, env.mint.as_ref()], &pepper_curve::ID);
    let mut data = Vec::with_capacity(8 + Vesting::INIT_SPACE);
    Vesting {
        mint: env.mint,
        beneficiary: env.creator.pubkey(),
        total_amount: TOTAL,
        released_amount: 0,
        start_time: created_at,
        cliff_seconds: DAY,
        duration_seconds: 0,
        bump,
    }
    .try_serialize(&mut data)
    .unwrap();
    data.resize(8 + Vesting::INIT_SPACE, 0);
    write(&mut env, vesting, data, pepper_curve::ID);

    let mut vault = vec![0u8; spl_token::state::Account::LEN];
    spl_token::state::Account {
        mint: env.mint,
        owner: curve_address,
        amount: TOTAL,
        delegate: COption::None,
        state: spl_token::state::AccountState::Initialized,
        is_native: COption::None,
        delegated_amount: 0,
        close_authority: COption::None,
    }
    .pack_into_slice(&mut vault);
    let vault_address = vesting_vault(&env.mint);
    write(&mut env, vault_address, vault, spl_token::ID);

    env.set_time(created_at + 2 * DAY);
    env
}

fn vesting_vault(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[VESTING_VAULT_SEED, mint.as_ref()], &pepper_curve::ID).0
}

fn write(env: &mut Env, address: Pubkey, data: Vec<u8>, owner: Pubkey) {
    let lamports = env.svm.minimum_balance_for_rent_exemption(data.len());
    env.svm
        .set_account(address, Account { lamports, data, owner, executable: false, rent_epoch: 0 })
        .unwrap();
}

/// `claim_vesting`, signed and paid by the creator, the beneficiary.
fn claim(env: &mut Env) -> Result<(), solana_sdk::transaction::TransactionError> {
    let beneficiary = env.creator.pubkey();
    let claim = ix(
        pepper_curve::accounts::ClaimVesting {
            beneficiary,
            curve: curve_pda(&env.mint),
            mint: env.mint,
            vesting: Pubkey::find_program_address(&[VESTING_SEED, env.mint.as_ref()], &pepper_curve::ID).0,
            vesting_vault: vesting_vault(&env.mint),
            beneficiary_token_account: ata(&beneficiary, &env.mint),
            token_program: spl_token::ID,
            associated_token_program: anchor_spl::associated_token::ID,
            system_program: system_program::id(),
        },
        pepper_curve::instruction::ClaimVesting {},
    );
    let creator = env.creator.insecure_clone();
    env.send_paid_by(&creator, &[claim], &[])
}

fn claimed(env: &Env) -> u64 {
    let vesting: Vesting = env.fetch(&Pubkey::find_program_address(&[VESTING_SEED, env.mint.as_ref()], &pepper_curve::ID).0);
    vesting.released_amount
}

#[test]
fn refuses_the_vesting_of_a_new_curve_that_has_not_graduated() {
    for phase in [CurvePhase::Bonding, CurvePhase::Graduating] {
        let mut env = curve_with_vesting(VESTING_LOCKED_UNTIL_GRADUATION_FROM, phase);

        expect_err(claim(&mut env), code(FanexError::VestingLockedUntilGraduation));
        assert_eq!(claimed(&env), 0);
    }
}

#[test]
fn releases_the_vesting_of_a_new_curve_once_graduated() {
    let mut env = curve_with_vesting(VESTING_LOCKED_UNTIL_GRADUATION_FROM, CurvePhase::Graduated);

    claim(&mut env).unwrap();
    assert_eq!(claimed(&env), TOTAL);
}

#[test]
fn leaves_a_curve_created_before_the_threshold_its_terms() {
    let mut env = curve_with_vesting(VESTING_LOCKED_UNTIL_GRADUATION_FROM - DAY, CurvePhase::Bonding);

    claim(&mut env).unwrap();
    assert_eq!(claimed(&env), TOTAL);
}

