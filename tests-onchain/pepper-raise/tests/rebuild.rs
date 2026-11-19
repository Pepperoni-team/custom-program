//! A creator's payment rebuilds the guarantee (the 2026-10-19 design): the
//! arrears first, then the guarantee, then the month or the fans.

mod common;

use common::*;
use pepper_curve::raise::calendar::{declare_open_ts, due_ts};
use pepper_curve::raise::constants::*;
use pepper_curve::errors::FanexError;
use pepper_curve::raise::events::{GuaranteeReplenished, MonthFunded};
use pepper_curve::raise::state::{Month, Raise, RaiseState};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signer};

const DECEMBER_2026: u32 = NOVEMBER_2026 + 1;
const JANUARY_2027: u32 = NOVEMBER_2026 + 2;
/// What a test raise's guarantee holds once finalized: 5% of the creator's 909.090909.
const GUARANTEE: u64 = 45_454_545;
/// What every paying month of these tests owes.
const DUE: u64 = 40 * USD;
/// 40 paid into a month: 1.20 to Fanex, 38.80 to the fans.
const FEE: u64 = 1_200_000;
const CREDIT: u64 = 38_800_000;

/// An active raise whose November (40 USD) went unpaid: at its due date the
/// guarantee covered it whole, and the creator owes the guarantee 40. No month
/// is open; the clock stands on November's due date.
fn november_covered(env: &mut Env) -> (Keypair, Pubkey, Pubkey) {
    let (creator, creator_usdc, raise, _) = env.active_raise(1);
    env.set_time(declare_open_ts(NOVEMBER_2026));
    env.declare(&raise, NOVEMBER_2026, DUE).unwrap();
    env.set_time(due_ts(NOVEMBER_2026));
    env.settle(&raise, NOVEMBER_2026).unwrap();
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.guarantee_usdc, state.guarantee_owed), (GUARANTEE - DUE, DUE));
    assert_eq!(state.oldest_open_month, DECEMBER_2026);
    (creator, creator_usdc, raise)
}

/// `november_covered`, then December is declared at 40 USD: the oldest open
/// month, not yet due.
fn december_open(env: &mut Env) -> (Keypair, Pubkey, Pubkey) {
    let (creator, creator_usdc, raise) = november_covered(env);
    env.set_time(declare_open_ts(DECEMBER_2026));
    env.declare(&raise, DECEMBER_2026, DUE).unwrap();
    (creator, creator_usdc, raise)
}

#[test]
fn the_usual_payment_refills_the_guarantee_and_leaves_the_month_late() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise) = december_open(&mut env);
    let fee_usdc = env.fee_usdc;
    env.mint_to(&creator_usdc, DUE);
    let (creator_before, fee_before) = (env.balance(&creator_usdc), env.balance(&fee_usdc));
    let payouts_before = env.balance(&vault(PAYOUTS_SEED, &raise));
    // The design's example: owing the guarantee what the month owes, the creator pays the usual amount.
    let logs = env.fund_month_logged(&creator, &creator_usdc, &raise, DECEMBER_2026, DUE).unwrap();
    // All of it refills the guarantee, with no fee; December gets nothing and stays open.
    assert_eq!(creator_before - env.balance(&creator_usdc), DUE);
    assert_eq!(env.balance(&vault(GUARANTEE_SEED, &raise)), GUARANTEE);
    assert_eq!(env.balance(&fee_usdc), fee_before);
    assert_eq!(env.balance(&vault(PAYOUTS_SEED, &raise)), payouts_before);
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.guarantee_usdc, state.guarantee_owed, state.oldest_open_month), (GUARANTEE, 0, DECEMBER_2026));
    let december: Month = env.fetch(&month_pda(&raise, DECEMBER_2026));
    assert_eq!((december.paid, december.paid_on_time), (0, 0));
    let refilled = events_of::<GuaranteeReplenished>(&logs);
    assert_eq!(refilled.len(), 1);
    assert_eq!(refilled[0].amount, DUE);
    assert!(events_of::<MonthFunded>(&logs).is_empty());
    // At its due date the guarantee covers December: the fans are paid, and the creator is late again.
    env.set_time(due_ts(DECEMBER_2026));
    env.settle(&raise, DECEMBER_2026).unwrap();
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.guarantee_usdc, state.guarantee_owed, state.on_time_streak), (GUARANTEE - DUE, DUE, 0));
    assert_eq!(env.fetch::<Month>(&month_pda(&raise, DECEMBER_2026)).covered, DUE);
}

#[test]
fn paying_the_guarantee_on_top_keeps_the_month_on_time() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise) = december_open(&mut env);
    let fee_usdc = env.fee_usdc;
    env.mint_to(&creator_usdc, 2 * DUE);
    let (creator_before, fee_before) = (env.balance(&creator_usdc), env.balance(&fee_usdc));
    let credited_before = env.fetch::<Raise>(&raise).credited_total;
    let logs = env.fund_month_logged(&creator, &creator_usdc, &raise, DECEMBER_2026, 2 * DUE).unwrap();
    // 40 to the guarantee, 40 to December: 1.20 to Fanex, 38.80 to the fans.
    assert_eq!(creator_before - env.balance(&creator_usdc), 2 * DUE);
    assert_eq!(env.balance(&vault(GUARANTEE_SEED, &raise)), GUARANTEE);
    assert_eq!(env.balance(&fee_usdc) - fee_before, FEE);
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.guarantee_usdc, state.guarantee_owed, state.oldest_open_month), (GUARANTEE, 0, JANUARY_2027));
    assert_eq!(state.credited_total - credited_before, CREDIT);
    // Only the month's part counts as paid on time.
    let december: Month = env.fetch(&month_pda(&raise, DECEMBER_2026));
    assert_eq!((december.paid, december.paid_on_time, december.credited), (DUE, DUE, CREDIT));
    let refilled = events_of::<GuaranteeReplenished>(&logs);
    assert_eq!(refilled.len(), 1);
    assert_eq!(refilled[0].amount, DUE);
    let funded = events_of::<MonthFunded>(&logs);
    assert_eq!(funded.len(), 1);
    assert_eq!((funded[0].month, funded[0].gross, funded[0].fee, funded[0].credit), (DECEMBER_2026, DUE, FEE, CREDIT));
    // December was paid on time: the streak counts it.
    env.set_time(due_ts(DECEMBER_2026));
    env.settle(&raise, DECEMBER_2026).unwrap();
    assert_eq!(env.fetch::<Raise>(&raise).on_time_streak, 1);
}

#[test]
fn a_payment_on_the_due_date_pays_the_arrears_first_then_refills_the_guarantee() {
    // The internal audit of 2026-10-29: a late payment rebuilt nothing, so a creator who always paid
    // after the due date drained the guarantee and never rebuilt it. Now the arrears come first and
    // what is left refills the guarantee, the 2026-10-19 design's own order.
    let mut env = Env::new();
    let (creator, creator_usdc, raise) = december_open(&mut env);
    env.mint_to(&creator_usdc, 2 * DUE);
    // December's due date itself, before anyone settles it: December is overdue.
    env.set_time(due_ts(DECEMBER_2026));
    let creator_before = env.balance(&creator_usdc);
    // A payment signed for the month and the guarantee together.
    let logs = env.fund_month_logged(&creator, &creator_usdc, &raise, DECEMBER_2026, 2 * DUE).unwrap();
    // December's 40, then the 40 the guarantee is owed.
    assert_eq!(creator_before - env.balance(&creator_usdc), 2 * DUE);
    assert_eq!(env.balance(&vault(GUARANTEE_SEED, &raise)), GUARANTEE);
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.guarantee_usdc, state.guarantee_owed), (GUARANTEE, 0));
    // The month is still late: only a payment before the due date counts as on time.
    let december: Month = env.fetch(&month_pda(&raise, DECEMBER_2026));
    assert_eq!((december.paid, december.paid_on_time), (DUE, 0));
    assert_eq!(events_of::<GuaranteeReplenished>(&logs)[0].amount, DUE);
    let funded = events_of::<MonthFunded>(&logs);
    assert_eq!((funded[0].month, funded[0].gross, funded[0].fee, funded[0].credit), (DECEMBER_2026, DUE, FEE, CREDIT));
}

#[test]
fn a_late_payment_of_the_arrears_alone_refills_nothing() {
    // What the program cannot do: take more than it is paid. A creator who pays exactly what the
    // month owes, late, pays the fans and leaves the guarantee owed (PEPPER-RAISE-PROGRAM.md §9).
    let mut env = Env::new();
    let (creator, creator_usdc, raise) = december_open(&mut env);
    env.mint_to(&creator_usdc, DUE);
    env.set_time(due_ts(DECEMBER_2026));
    env.fund_month(&creator, &creator_usdc, &raise, DECEMBER_2026, DUE).unwrap();
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.guarantee_usdc, state.guarantee_owed), (GUARANTEE - DUE, DUE));
    assert_eq!(state.oldest_open_month, JANUARY_2027);
}

#[test]
fn a_late_payment_refills_nothing_while_the_next_month_is_settled_with_arrears() {
    // December and January both settled unpaid: the guarantee covered December's first 5.454545,
    // January (not the oldest open month) nothing. Paying December's arrears moves the pointer onto
    // January, a settled month no `settle_month` can cover again: a refill then would leave money in
    // the guarantee that `close_raise` must see spent first, and nothing could spend it.
    let mut env = Env::new();
    let (creator, creator_usdc, raise) = december_open(&mut env);
    env.set_time(declare_open_ts(JANUARY_2027));
    env.declare(&raise, JANUARY_2027, DUE).unwrap();
    env.set_time(due_ts(JANUARY_2027));
    env.settle(&raise, DECEMBER_2026).unwrap();
    env.settle(&raise, JANUARY_2027).unwrap();
    let december_arrears = DUE - (GUARANTEE - DUE);
    assert_eq!(env.fetch::<Raise>(&raise).guarantee_owed, GUARANTEE);
    env.mint_to(&creator_usdc, 200 * USD);
    let before = env.balance(&creator_usdc);
    env.fund_month(&creator, &creator_usdc, &raise, DECEMBER_2026, 100 * USD).unwrap();
    // December's arrears alone: the rest is not taken.
    assert_eq!(before - env.balance(&creator_usdc), december_arrears);
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.guarantee_usdc, state.guarantee_owed, state.oldest_open_month), (0, GUARANTEE, JANUARY_2027));
    // January's: its 40, then the guarantee, since February is not settled.
    env.fund_month(&creator, &creator_usdc, &raise, JANUARY_2027, 100 * USD).unwrap();
    assert_eq!(before - env.balance(&creator_usdc), december_arrears + DUE + GUARANTEE);
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.guarantee_usdc, state.guarantee_owed), (GUARANTEE, 0));
}

#[test]
fn with_no_month_open_a_payment_refills_the_guarantee_then_pays_the_fans() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise) = november_covered(&mut env);
    env.mint_to(&creator_usdc, 100 * USD);
    let creator_before = env.balance(&creator_usdc);
    let credited_before = env.fetch::<Raise>(&raise).credited_total;
    let logs = env.fund_direct_logged(&creator, &creator_usdc, &raise, 100 * USD).unwrap();
    // 40 to the guarantee; the other 60 straight to the fans: 1.80 to Fanex, 58.20 credited.
    assert_eq!(creator_before - env.balance(&creator_usdc), 100 * USD);
    assert_eq!(env.balance(&vault(GUARANTEE_SEED, &raise)), GUARANTEE);
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.guarantee_usdc, state.guarantee_owed), (GUARANTEE, 0));
    assert_eq!(state.credited_total - credited_before, 58_200_000);
    assert_eq!(events_of::<GuaranteeReplenished>(&logs)[0].amount, DUE);
    let funded = events_of::<MonthFunded>(&logs);
    assert_eq!((funded[0].month, funded[0].gross, funded[0].fee), (u32::MAX, 60 * USD, 1_800_000));
}

#[test]
fn a_payment_that_only_refills_the_guarantee_is_accepted() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise) = november_covered(&mut env);
    env.mint_to(&creator_usdc, 10 * USD);
    let credited_before = env.fetch::<Raise>(&raise).credited_total;
    let logs = env.fund_direct_logged(&creator, &creator_usdc, &raise, 10 * USD).unwrap();
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.guarantee_usdc, state.guarantee_owed), (GUARANTEE - DUE + 10 * USD, DUE - 10 * USD));
    // Nothing reached the fans.
    assert_eq!(state.credited_total, credited_before);
    assert_eq!(events_of::<GuaranteeReplenished>(&logs)[0].amount, 10 * USD);
    assert!(events_of::<MonthFunded>(&logs).is_empty());
    // Nothing at all is still no payment.
    expect_err(env.fund_direct(&creator, &creator_usdc, &raise, 0), code(FanexError::RaiseZeroAmount));
}

#[test]
fn once_closed_a_payment_goes_to_the_fans_never_to_the_guarantee() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise) = november_covered(&mut env);
    // Every later month owes nothing, and the term ends with the guarantee still owed 40.
    let term_end = env.fetch::<Raise>(&raise).term_end_month;
    for month in DECEMBER_2026..=term_end {
        env.set_time(declare_open_ts(month));
        env.declare(&raise, month, 0).unwrap();
    }
    env.set_time(due_ts(term_end));
    env.close_raise(&raise, &creator_usdc).unwrap();
    let closed: Raise = env.fetch(&raise);
    assert_eq!((closed.state, closed.guarantee_usdc, closed.guarantee_owed), (RaiseState::Closed, 0, DUE));
    env.mint_to(&creator_usdc, 10 * USD);
    let logs = env.fund_direct_logged(&creator, &creator_usdc, &raise, 10 * USD).unwrap();
    // All of it reaches the fans; the guarantee, emptied at close and never released again, takes nothing.
    assert_eq!(env.balance(&vault(GUARANTEE_SEED, &raise)), 0);
    let after: Raise = env.fetch(&raise);
    assert_eq!((after.guarantee_usdc, after.guarantee_owed), (0, DUE));
    assert_eq!(after.credited_total - closed.credited_total, 9_700_000);
    assert!(events_of::<GuaranteeReplenished>(&logs).is_empty());
}

#[test]
fn a_payment_refills_only_the_raises_own_guarantee_vault() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise) = november_covered(&mut env);
    env.mint_to(&creator_usdc, DUE);
    let creator_before = env.balance(&creator_usdc);
    // Another token account named as the guarantee vault (the last account): the
    // counters would move with no USDC behind them.
    let (_, elsewhere) = env.user(0);
    let mut forged = env.fund_direct_ix(&creator.pubkey(), &creator_usdc, &raise, DUE);
    let last = forged.accounts.len() - 1;
    forged.accounts[last].pubkey = elsewhere;
    expect_err(env.send(&[forged], &[&creator]), anchor_lang::error::ErrorCode::ConstraintSeeds as u32);
    assert_eq!(env.balance(&creator_usdc), creator_before);
    assert_eq!(env.fetch::<Raise>(&raise).guarantee_owed, DUE);
}

#[test]
fn a_payment_between_the_owed_and_the_month_leaves_the_month_partly_paid() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise) = december_open(&mut env);
    let part = 10 * USD;
    env.mint_to(&creator_usdc, DUE + part);
    let creator_before = env.balance(&creator_usdc);
    let logs = env.fund_month_logged(&creator, &creator_usdc, &raise, DECEMBER_2026, DUE + part).unwrap();
    // The creator is debited the owed plus the part, nothing else.
    assert_eq!(creator_before - env.balance(&creator_usdc), DUE + part);
    // The guarantee takes exactly what it was owed.
    assert_eq!(env.balance(&vault(GUARANTEE_SEED, &raise)), GUARANTEE);
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.guarantee_usdc, state.guarantee_owed), (GUARANTEE, 0));
    // The month gets the part only, and stays open: the pointer does not move.
    assert_eq!(state.oldest_open_month, DECEMBER_2026);
    let december: Month = env.fetch(&month_pda(&raise, DECEMBER_2026));
    assert_eq!((december.paid, december.paid_on_time), (part, part));
    let refilled = events_of::<GuaranteeReplenished>(&logs);
    assert_eq!(refilled.len(), 1);
    assert_eq!(refilled[0].amount, DUE);
    let funded = events_of::<MonthFunded>(&logs);
    assert_eq!(funded.len(), 1);
    assert_eq!(funded[0].gross, part);
    // The guarantee event comes first, then the month's.
    use anchor_lang::Discriminator;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let order: Vec<bool> = logs
        .iter()
        .filter_map(|l| l.strip_prefix("Program data: "))
        .filter_map(|d| STANDARD.decode(d).ok())
        .filter_map(|bytes| {
            if bytes.starts_with(GuaranteeReplenished::DISCRIMINATOR) {
                Some(true)
            } else if bytes.starts_with(MonthFunded::DISCRIMINATOR) {
                Some(false)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(order, vec![true, false]);
}

#[test]
fn fund_month_refills_only_the_raises_own_guarantee_vault() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise) = december_open(&mut env);
    env.mint_to(&creator_usdc, DUE);
    let creator_before = env.balance(&creator_usdc);
    let (_, elsewhere) = env.user(0);
    let mut forged = env.fund_month_ix(&creator.pubkey(), &creator_usdc, &raise, DECEMBER_2026, DUE);
    let last = forged.accounts.len() - 1;
    forged.accounts[last].pubkey = elsewhere;
    expect_err(env.send(&[forged], &[&creator]), anchor_lang::error::ErrorCode::ConstraintSeeds as u32);
    assert_eq!(env.balance(&creator_usdc), creator_before);
    assert_eq!(env.fetch::<Raise>(&raise).guarantee_owed, DUE);
}

#[test]
fn stepping_past_a_month_that_owes_nothing_moves_no_money() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, _) = env.active_raise(1);
    env.set_time(declare_open_ts(NOVEMBER_2026));
    env.declare(&raise, NOVEMBER_2026, DUE).unwrap();
    // December owes nothing and is declared while November is still open: it stays in line.
    env.set_time(declare_open_ts(DECEMBER_2026));
    env.declare(&raise, DECEMBER_2026, 0).unwrap();
    // November goes unpaid and the guarantee covers it whole: December becomes the oldest open month.
    env.settle(&raise, NOVEMBER_2026).unwrap();
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.oldest_open_month, state.guarantee_owed), (DECEMBER_2026, DUE));
    let creator_before = env.balance(&creator_usdc);
    let logs = env.fund_month_logged(&creator, &creator_usdc, &raise, DECEMBER_2026, DUE).unwrap();
    // The call only moves the pointer on: nothing leaves the wallet and the guarantee is still owed.
    assert_eq!(env.balance(&creator_usdc), creator_before);
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.oldest_open_month, state.guarantee_owed), (JANUARY_2027, DUE));
    assert!(events_of::<GuaranteeReplenished>(&logs).is_empty());
}
