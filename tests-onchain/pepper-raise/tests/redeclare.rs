//! A declared month's due corrected before its due date (the internal audit of 2026-10-29,
//! months.rs:117). A declaration was final: a stolen attester key or a keeper's fault fixed a
//! month's obligation for good, and only the 48-hour admin could pause. `redeclare_month` lets the
//! attester lower a due it declared too high and the admin correct one either way (under the pause,
//! the admin alone), until the month's due date, never below what the creator already paid.

mod common;

use common::*;
use pepper_curve::raise::calendar::{declare_open_ts, due_ts};
use pepper_curve::errors::FanexError;
use pepper_curve::raise::events::MonthDeclared;
use pepper_curve::raise::state::{Month, Raise};
use solana_sdk::instruction::Instruction;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signer};

const DECEMBER_2026: u32 = NOVEMBER_2026 + 1;
/// What a test raise's guarantee holds once finalized: 5% of the creator's 909.090909.
const GUARANTEE: u64 = 45_454_545;

fn redeclare_ix(signer: &Pubkey, raise: &Pubkey, month: u32, due: u64) -> Instruction {
    ix(
        pepper_curve::accounts::RedeclareMonth { signer: *signer, config: config_pda(), raise: *raise, month_account: month_pda(raise, month) },
        pepper_curve::instruction::RedeclareMonth { due, estimated: false },
    )
}

fn by_attester(env: &mut Env, raise: &Pubkey, month: u32, due: u64) -> Result<Vec<String>, solana_sdk::transaction::TransactionError> {
    let attester = env.attester.insecure_clone();
    env.send_logged(&[redeclare_ix(&attester.pubkey(), raise, month, due)], &[&attester])
}

fn by_admin(env: &mut Env, raise: &Pubkey, month: u32, due: u64) -> Result<Vec<String>, solana_sdk::transaction::TransactionError> {
    let admin = env.admin.pubkey();
    env.send_logged(&[redeclare_ix(&admin, raise, month, due)], &[])
}

fn due_of(env: &Env, raise: &Pubkey) -> u64 {
    env.fetch::<Month>(&month_pda(raise, NOVEMBER_2026)).due
}

/// An active raise with November declared at `due`; the clock on the day it was declared.
fn november_declared(env: &mut Env, due: u64) -> (Keypair, Pubkey, Pubkey) {
    let (creator, creator_usdc, raise, _) = env.active_raise(1);
    env.set_time(declare_open_ts(NOVEMBER_2026));
    env.declare(&raise, NOVEMBER_2026, due).unwrap();
    (creator, creator_usdc, raise)
}

#[test]
fn the_attester_lowers_a_due_it_declared_too_high() {
    // A keeper's fault declared 100 for a month that earned 40.
    let mut env = Env::new();
    let (_, _, raise) = november_declared(&mut env, 100 * USD);
    let logs = by_attester(&mut env, &raise, NOVEMBER_2026, 40 * USD).unwrap();
    assert_eq!(due_of(&env, &raise), 40 * USD);
    // The correction is logged as a declaration: the last one for a month is the one in force.
    let declared: MonthDeclared = event(&logs);
    assert_eq!((declared.month, declared.due, declared.estimated), (NOVEMBER_2026, 40 * USD, false));
    // At the due date the corrected due is what the guarantee covers.
    env.set_time(due_ts(NOVEMBER_2026));
    env.settle(&raise, NOVEMBER_2026).unwrap();
    let november: Month = env.fetch(&month_pda(&raise, NOVEMBER_2026));
    assert_eq!((november.covered, env.fetch::<Raise>(&raise).guarantee_usdc), (40 * USD, GUARANTEE - 40 * USD));
}

#[test]
fn raising_a_due_takes_the_admin() {
    // The attester's key gains no way to raise what an honest creator owes at the last second;
    // the admin's raise comes through a proposal the creator sees for 48 hours.
    let mut env = Env::new();
    let (_, _, raise) = november_declared(&mut env, 40 * USD);
    expect_err(by_attester(&mut env, &raise, NOVEMBER_2026, 100 * USD).map(|_| ()), code(FanexError::RaiseUnauthorized));
    assert_eq!(due_of(&env, &raise), 40 * USD);
    by_admin(&mut env, &raise, NOVEMBER_2026, 100 * USD).unwrap();
    assert_eq!(due_of(&env, &raise), 100 * USD);
}

#[test]
fn a_zero_declaration_by_a_stolen_key_is_corrected_by_the_admin_under_the_pause() {
    // The audit's scenario: the attester's key declares nothing due, and the month steps past.
    let mut env = Env::new();
    let (creator, creator_usdc, raise) = november_declared(&mut env, 0);
    assert_eq!(env.fetch::<Raise>(&raise).oldest_open_month, DECEMBER_2026);
    // The admin pauses: the stolen key can no longer correct anything...
    env.set_paused(true).unwrap();
    expect_err(by_attester(&mut env, &raise, NOVEMBER_2026, 0).map(|_| ()), code(FanexError::RaiseGloballyPaused));
    // ...and the admin re-declares the true due: November owes again and holds the pointer.
    by_admin(&mut env, &raise, NOVEMBER_2026, 40 * USD).unwrap();
    assert_eq!(due_of(&env, &raise), 40 * USD);
    assert_eq!(env.fetch::<Raise>(&raise).oldest_open_month, NOVEMBER_2026);
    // The creator pays it on time, as if it had been declared right.
    env.mint_to(&creator_usdc, 40 * USD);
    env.fund_month(&creator, &creator_usdc, &raise, NOVEMBER_2026, 40 * USD).unwrap();
    env.set_time(due_ts(NOVEMBER_2026));
    env.settle(&raise, NOVEMBER_2026).unwrap();
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.on_time_streak, state.oldest_open_month, state.guarantee_usdc), (1, DECEMBER_2026, GUARANTEE));
}

#[test]
fn nobody_else_redeclares() {
    let mut env = Env::new();
    let (creator, _, raise) = november_declared(&mut env, 40 * USD);
    for signer in [Keypair::new(), creator] {
        let instruction = redeclare_ix(&signer.pubkey(), &raise, NOVEMBER_2026, 0);
        expect_err(env.send(&[instruction], &[&signer]), code(FanexError::RaiseUnauthorized));
    }
    assert_eq!(due_of(&env, &raise), 40 * USD);
}

#[test]
fn the_due_is_final_from_the_due_date() {
    let mut env = Env::new();
    let (_, _, raise) = november_declared(&mut env, 40 * USD);
    env.set_time(due_ts(NOVEMBER_2026) - 1);
    by_attester(&mut env, &raise, NOVEMBER_2026, 30 * USD).unwrap();
    env.set_time(due_ts(NOVEMBER_2026));
    // From the due date `settle_month` opens, settled or not: nobody corrects it, the admin included.
    expect_err(by_attester(&mut env, &raise, NOVEMBER_2026, 20 * USD).map(|_| ()), code(FanexError::MonthNotRedeclarable));
    expect_err(by_admin(&mut env, &raise, NOVEMBER_2026, 60 * USD).map(|_| ()), code(FanexError::MonthNotRedeclarable));
    env.settle(&raise, NOVEMBER_2026).unwrap();
    expect_err(by_admin(&mut env, &raise, NOVEMBER_2026, 60 * USD).map(|_| ()), code(FanexError::MonthNotRedeclarable));
    assert_eq!(due_of(&env, &raise), 30 * USD);
}

#[test]
fn a_redeclaration_never_goes_below_what_was_paid() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise) = november_declared(&mut env, 40 * USD);
    env.mint_to(&creator_usdc, 30 * USD);
    env.fund_month(&creator, &creator_usdc, &raise, NOVEMBER_2026, 30 * USD).unwrap();
    // No money moves back: below the 30 paid is refused, the admin's correction included.
    expect_err(by_attester(&mut env, &raise, NOVEMBER_2026, 30 * USD - 1).map(|_| ()), code(FanexError::MonthNotRedeclarable));
    expect_err(by_admin(&mut env, &raise, NOVEMBER_2026, 30 * USD - 1).map(|_| ()), code(FanexError::MonthNotRedeclarable));
    // At what was paid, November is paid in full and the pointer steps past it...
    by_attester(&mut env, &raise, NOVEMBER_2026, 30 * USD).unwrap();
    assert_eq!(env.fetch::<Raise>(&raise).oldest_open_month, DECEMBER_2026);
    // ...and raised above it by the admin, it owes again and the pointer comes back.
    by_admin(&mut env, &raise, NOVEMBER_2026, 35 * USD).unwrap();
    assert_eq!(env.fetch::<Raise>(&raise).oldest_open_month, NOVEMBER_2026);
    let november: Month = env.fetch(&month_pda(&raise, NOVEMBER_2026));
    assert_eq!((november.due, november.paid, november.paid_on_time), (35 * USD, 30 * USD, 30 * USD));
}
