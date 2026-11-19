mod common;

use common::*;
use pepper_curve::raise::calendar::{declare_open_ts, due_ts};
use pepper_curve::raise::constants::*;
use pepper_curve::errors::FanexError;
use pepper_curve::raise::state::{Month, Raise};
use solana_sdk::signature::Keypair;

const DECEMBER_2026: u32 = NOVEMBER_2026 + 1;
const JANUARY_2027: u32 = NOVEMBER_2026 + 2;

#[test]
fn a_month_is_declared_from_the_15th_of_the_next() {
    let mut env = Env::new();
    let (_, _, raise, _) = env.active_raise(1);
    env.set_time(declare_open_ts(NOVEMBER_2026) - 1);
    expect_err(env.declare(&raise, NOVEMBER_2026, 100 * USD), code(FanexError::DeclarationTooEarly));
    env.set_time(declare_open_ts(NOVEMBER_2026));
    env.declare(&raise, NOVEMBER_2026, 100 * USD).unwrap();
    let month: Month = env.fetch(&month_pda(&raise, NOVEMBER_2026));
    assert_eq!((month.due, month.due_ts), (100 * USD, due_ts(NOVEMBER_2026)));
    assert_eq!(env.fetch::<Raise>(&raise).last_declared_month, NOVEMBER_2026);
}

#[test]
fn only_fanex_declares() {
    let mut env = Env::new();
    let (_, _, raise, _) = env.active_raise(1);
    env.set_time(declare_open_ts(NOVEMBER_2026));
    let impostor = Keypair::new();
    expect_err(env.declare_as(&raise, NOVEMBER_2026, USD, &impostor), code(FanexError::RaiseUnauthorized));
}

#[test]
fn a_payment_credits_the_fans_after_three_percent() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, _) = env.active_raise(1);
    env.mint_to(&creator_usdc, 100 * USD);
    env.set_time(declare_open_ts(NOVEMBER_2026));
    env.declare(&raise, NOVEMBER_2026, 100 * USD).unwrap();
    let fee_before = env.balance(&env.fee_usdc.clone());
    env.fund_month(&creator, &creator_usdc, &raise, NOVEMBER_2026, 100 * USD).unwrap();
    assert_eq!(env.balance(&env.fee_usdc.clone()) - fee_before, 3 * USD);
    assert_eq!(env.balance(&vault(PAYOUTS_SEED, &raise)), 97 * USD);
    let state: Raise = env.fetch(&raise);
    assert_eq!(state.credited_total, 97 * USD);
    assert_eq!(state.oldest_open_month, DECEMBER_2026);
    assert_eq!(env.fetch::<Month>(&month_pda(&raise, NOVEMBER_2026)).paid_on_time, 100 * USD);
}

#[test]
fn months_are_paid_oldest_first() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, _) = env.active_raise(1);
    env.mint_to(&creator_usdc, 100 * USD);
    env.set_time(declare_open_ts(NOVEMBER_2026));
    env.declare(&raise, NOVEMBER_2026, 50 * USD).unwrap();
    env.set_time(declare_open_ts(DECEMBER_2026));
    env.declare(&raise, DECEMBER_2026, 50 * USD).unwrap();
    expect_err(
        env.fund_month(&creator, &creator_usdc, &raise, DECEMBER_2026, 50 * USD),
        code(FanexError::NotOldestOpenMonth),
    );
}

#[test]
fn the_guarantee_covers_a_missed_month() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, _) = env.active_raise(1);
    env.mint_to(&creator_usdc, 100 * USD);
    env.set_time(declare_open_ts(NOVEMBER_2026));
    env.declare(&raise, NOVEMBER_2026, 100 * USD).unwrap();
    env.fund_month(&creator, &creator_usdc, &raise, NOVEMBER_2026, 100 * USD).unwrap();
    env.set_time(declare_open_ts(DECEMBER_2026));
    env.declare(&raise, DECEMBER_2026, 100 * USD).unwrap();
    env.set_time(due_ts(DECEMBER_2026));
    env.settle(&raise, NOVEMBER_2026).unwrap();
    assert_eq!(env.fetch::<Raise>(&raise).on_time_streak, 1);
    env.settle(&raise, DECEMBER_2026).unwrap();
    // The whole guarantee (45.454545) covers December: 3% to Fanex, the rest to the fans.
    assert_eq!(env.balance(&vault(GUARANTEE_SEED, &raise)), 0);
    assert_eq!(env.balance(&vault(PAYOUTS_SEED, &raise)), 97 * USD + 44_090_909);
    let state: Raise = env.fetch(&raise);
    // The counter moved with the vault (ruling P3).
    assert_eq!(state.guarantee_usdc, 0);
    assert_eq!(state.guarantee_owed, 45_454_545);
    assert_eq!(state.on_time_streak, 0);
    assert_eq!(state.oldest_open_month, DECEMBER_2026);
    assert_eq!(env.fetch::<Month>(&month_pda(&raise, DECEMBER_2026)).credited, 44_090_909);
}

#[test]
fn a_payment_after_the_due_date_is_late() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, _) = env.active_raise(1);
    env.mint_to(&creator_usdc, 200 * USD);
    env.set_time(declare_open_ts(NOVEMBER_2026));
    env.declare(&raise, NOVEMBER_2026, 100 * USD).unwrap();
    env.fund_month(&creator, &creator_usdc, &raise, NOVEMBER_2026, 100 * USD).unwrap();
    env.set_time(declare_open_ts(DECEMBER_2026));
    env.declare(&raise, DECEMBER_2026, 100 * USD).unwrap();
    // Paid in full, but on the due date itself: before anyone settles it, and late.
    env.set_time(due_ts(DECEMBER_2026));
    env.fund_month(&creator, &creator_usdc, &raise, DECEMBER_2026, 100 * USD).unwrap();
    env.settle(&raise, NOVEMBER_2026).unwrap();
    env.settle(&raise, DECEMBER_2026).unwrap();
    assert_eq!(env.fetch::<Month>(&month_pda(&raise, DECEMBER_2026)).paid_on_time, 0);
    assert_eq!(env.fetch::<Raise>(&raise).on_time_streak, 0);
}

#[test]
fn the_creator_rebuilds_the_guarantee_after_the_fans() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, _) = env.active_raise(1);
    env.mint_to(&creator_usdc, 200 * USD);
    env.set_time(declare_open_ts(NOVEMBER_2026));
    env.declare(&raise, NOVEMBER_2026, 100 * USD).unwrap();
    env.set_time(due_ts(NOVEMBER_2026));
    env.settle(&raise, NOVEMBER_2026).unwrap();
    assert_eq!(env.balance(&vault(GUARANTEE_SEED, &raise)), 0);
    assert_eq!(env.fetch::<Raise>(&raise).guarantee_usdc, 0);
    // November is still open: the fans come first.
    expect_err(env.replenish(&creator, &creator_usdc, &raise, 45_454_545), code(FanexError::OpenMonthsFirst));
    // The late payment pays November's 54.545455 first, then rebuilds the guarantee with the rest
    // (the internal audit of 2026-10-29: before, the rest was not taken and only `replenish_guarantee`
    // rebuilt it).
    let before = env.balance(&creator_usdc);
    env.fund_month(&creator, &creator_usdc, &raise, NOVEMBER_2026, 100 * USD).unwrap();
    assert_eq!(before - env.balance(&creator_usdc), 100 * USD);
    assert_eq!(env.balance(&vault(GUARANTEE_SEED, &raise)), 45_454_545);
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.guarantee_usdc, state.guarantee_owed), (45_454_545, 0));
    // Nothing is left to rebuild.
    expect_err(env.replenish(&creator, &creator_usdc, &raise, 100 * USD), code(FanexError::NothingOwed));
}

#[test]
fn a_zero_due_month_does_not_block_the_guarantee() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, _) = env.active_raise(1);
    env.mint_to(&creator_usdc, 100 * USD);
    env.set_time(declare_open_ts(NOVEMBER_2026));
    env.declare(&raise, NOVEMBER_2026, 100 * USD).unwrap();
    env.set_time(declare_open_ts(DECEMBER_2026));
    env.declare(&raise, DECEMBER_2026, 0).unwrap();
    env.set_time(declare_open_ts(JANUARY_2027));
    env.declare(&raise, JANUARY_2027, 100 * USD).unwrap();
    // November paid late: December (nothing owed) becomes the oldest open month.
    env.fund_month(&creator, &creator_usdc, &raise, NOVEMBER_2026, 100 * USD).unwrap();
    assert_eq!(env.fetch::<Raise>(&raise).oldest_open_month, DECEMBER_2026);
    env.set_time(due_ts(JANUARY_2027));
    env.settle(&raise, NOVEMBER_2026).unwrap();
    env.settle(&raise, DECEMBER_2026).unwrap();
    env.settle(&raise, JANUARY_2027).unwrap();
    assert_eq!(env.fetch::<Month>(&month_pda(&raise, JANUARY_2027)).covered, 45_454_545);
}

#[test]
fn there_is_no_cap_on_what_fans_receive() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, _) = env.active_raise(1);
    env.mint_to(&creator_usdc, 2_001 * USD);
    env.set_time(declare_open_ts(NOVEMBER_2026));
    env.declare(&raise, NOVEMBER_2026, 2_000 * USD).unwrap();
    let before = env.balance(&creator_usdc);
    env.fund_month(&creator, &creator_usdc, &raise, NOVEMBER_2026, 2_000 * USD).unwrap();
    // All of it moves: 60 to Fanex and 1,940 to the fans, past the 1,363.636363
    // the 2026-10-01 design would have capped this 909 USD raise at.
    assert_eq!(before - env.balance(&creator_usdc), 2_000 * USD);
    assert_eq!(env.fetch::<Raise>(&raise).credited_total, 1_940 * USD);
    // And the creator can keep paying without a declaration.
    env.fund_direct(&creator, &creator_usdc, &raise, USD).unwrap();
    assert_eq!(env.fetch::<Raise>(&raise).credited_total, 1_940 * USD + 970_000);
}

#[test]
fn fund_direct_pays_the_fans_without_a_declaration() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, _) = env.active_raise(1);
    env.mint_to(&creator_usdc, 100 * USD);
    env.fund_direct(&creator, &creator_usdc, &raise, 100 * USD).unwrap();
    assert_eq!(env.fetch::<Raise>(&raise).credited_total, 97 * USD);
}

#[test]
fn a_direct_payment_waits_for_the_open_months() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, _) = env.active_raise(1);
    env.mint_to(&creator_usdc, 200 * USD);
    env.set_time(declare_open_ts(NOVEMBER_2026));
    env.declare(&raise, NOVEMBER_2026, 100 * USD).unwrap();
    let creator_before = env.balance(&creator_usdc);
    let payouts_before = env.balance(&vault(PAYOUTS_SEED, &raise));
    expect_err(env.fund_direct(&creator, &creator_usdc, &raise, 100 * USD), code(FanexError::OpenMonthsFirst));
    // Nothing moved: the refusal happens before any transfer.
    assert_eq!(env.balance(&creator_usdc), creator_before);
    assert_eq!(env.balance(&vault(PAYOUTS_SEED, &raise)), payouts_before);
    env.fund_month(&creator, &creator_usdc, &raise, NOVEMBER_2026, 100 * USD).unwrap();
    env.fund_direct(&creator, &creator_usdc, &raise, 100 * USD).unwrap();
}

#[test]
fn the_guarantee_is_released_after_twelve_on_time_months() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, _) = env.active_raise(1);
    env.mint_to(&creator_usdc, 120 * USD);
    for i in 0..12u32 {
        let month = NOVEMBER_2026 + i;
        env.set_time(declare_open_ts(month));
        if i > 0 {
            env.settle(&raise, month - 1).unwrap();
        }
        env.declare(&raise, month, 10 * USD).unwrap();
        env.fund_month(&creator, &creator_usdc, &raise, month, 10 * USD).unwrap();
        if i == 10 {
            expect_err(env.release(&raise, &creator_usdc), code(FanexError::GuaranteeNotReleasable));
        }
    }
    env.set_time(due_ts(NOVEMBER_2026 + 11));
    env.settle(&raise, NOVEMBER_2026 + 11).unwrap();
    assert_eq!(env.fetch::<Raise>(&raise).guarantee_usdc, 45_454_545);
    let before = env.balance(&creator_usdc);
    env.release(&raise, &creator_usdc).unwrap();
    assert_eq!(env.balance(&creator_usdc) - before, 45_454_545);
    assert_eq!(env.balance(&vault(GUARANTEE_SEED, &raise)), 0);
    assert_eq!(env.fetch::<Raise>(&raise).guarantee_usdc, 0);
    expect_err(env.release(&raise, &creator_usdc), code(FanexError::GuaranteeNotReleasable));
}
