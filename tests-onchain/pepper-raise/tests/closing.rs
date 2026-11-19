mod common;

use common::*;
use pepper_curve::raise::calendar::{month_index, month_start_ts};
use pepper_curve::raise::constants::*;
use pepper_curve::errors::FanexError;
use pepper_curve::raise::state::{Raise, RaiseState};
use solana_sdk::signature::Signer;

#[test]
fn a_sold_out_raise_splits_90_5_5_10() {
    let mut env = Env::new();
    let (_creator, creator_usdc, raise, _buyers) = env.active_raise(1);
    // Fans paid 1,000 USD: creator's part 909.090909, pool 90.909091.
    assert_eq!(env.balance(&creator_usdc), 5 * USD + 818_181_819);
    assert_eq!(env.balance(&env.fee_usdc.clone()), 5 * USD + 45_454_545);
    assert_eq!(env.balance(&vault(GUARANTEE_SEED, &raise)), 45_454_545);
    assert_eq!(env.balance(&vault(POOL_SEED, &raise)), 90_909_091);
    assert_eq!(env.balance(&vault(ESCROW_SEED, &raise)), 0);
    let state: Raise = env.fetch(&raise);
    assert_eq!(state.state, RaiseState::Active);
    // The pool opens with the 90.909091 and as many virtual quotas: 1.00 a
    // quota, the price fans paid. It holds no real quota yet.
    assert_eq!((state.pool_usdc, state.pool_units, state.virtual_units), (90_909_091, 0, 90_909_091));
    assert_eq!((state.first_month, state.term_end_month), (24_322, 24_345));
    assert_eq!(state.oldest_open_month, 24_322);
    assert_eq!(state.units_outstanding, 1_000 * Q);
    // The guarantee's counter starts equal to its vault (ruling P3).
    assert_eq!(state.guarantee_usdc, 45_454_545);
}

#[test]
fn an_open_raise_cannot_be_finalized() {
    let mut env = Env::new();
    let (_, creator_usdc, raise) = env.launch(1);
    let (buyer, buyer_usdc) = env.user(100 * USD);
    env.buy(&raise, &buyer, &buyer_usdc, 100 * USD).unwrap();
    expect_err(env.finalize(&raise, &creator_usdc), code(FanexError::RaiseStillOpen));
}

#[test]
fn above_the_minimum_at_the_deadline_it_succeeds() {
    let mut env = Env::new();
    let (_, creator_usdc, raise) = env.launch(1);
    for _ in 0..3 {
        let (buyer, buyer_usdc) = env.user(100 * USD);
        env.buy(&raise, &buyer, &buyer_usdc, 100 * USD).unwrap();
    }
    let deadline = env.fetch::<Raise>(&raise).deadline;
    env.set_time(deadline);
    env.finalize(&raise, &creator_usdc).unwrap();
    // Fans paid 300 USD: creator's part 272.727272.
    assert_eq!(env.balance(&creator_usdc), 5 * USD + 245_454_546);
    assert_eq!(env.balance(&vault(POOL_SEED, &raise)), 27_272_728);
    let state: Raise = env.fetch(&raise);
    assert_eq!(state.state, RaiseState::Active);
    assert_eq!((state.pool_usdc, state.virtual_units), (27_272_728, 27_272_728));
}

#[test]
fn a_finalize_sent_after_the_month_turned_keeps_the_deadlines_month() {
    // The keeper runs every 30 minutes: a deadline late on a month's last day is finalized in the next one.
    let mut env = Env::new();
    let (_, creator_usdc, raise) = env.launch(1);
    for _ in 0..3 {
        let (buyer, buyer_usdc) = env.user(100 * USD);
        env.buy(&raise, &buyer, &buyer_usdc, 100 * USD).unwrap();
    }
    let deadline = env.fetch::<Raise>(&raise).deadline;
    let deadline_month = month_index(deadline).unwrap();
    env.set_time(month_start_ts(deadline_month + 1) + 60);
    env.finalize(&raise, &creator_usdc).unwrap();
    let state: Raise = env.fetch(&raise);
    assert_eq!(state.first_month, deadline_month + 1);
    assert_eq!(state.oldest_open_month, deadline_month + 1);
}

#[test]
fn below_the_minimum_at_the_deadline_everyone_is_refunded() {
    let mut env = Env::new();
    let (_, creator_usdc, raise) = env.launch(1);
    let (buyer, buyer_usdc) = env.user(100 * USD);
    env.buy(&raise, &buyer, &buyer_usdc, 100 * USD).unwrap();
    let deadline = env.fetch::<Raise>(&raise).deadline;
    env.set_time(deadline);
    env.finalize(&raise, &creator_usdc).unwrap();
    assert_eq!(env.fetch::<Raise>(&raise).state, RaiseState::Refunding);
    env.refund(&raise, &buyer.pubkey(), &buyer_usdc).unwrap();
    assert_eq!(env.balance(&buyer_usdc), 100 * USD);
    assert_eq!(env.balance(&vault(ESCROW_SEED, &raise)), 0);
    assert!(!env.exists(&position_pda(&raise, &buyer.pubkey())));
}

#[test]
fn a_refund_goes_only_to_the_buyer() {
    let mut env = Env::new();
    let (creator, _, raise) = env.launch(1);
    let (buyer, buyer_usdc) = env.user(100 * USD);
    env.buy(&raise, &buyer, &buyer_usdc, 100 * USD).unwrap();
    env.cancel(&creator, &raise).unwrap();
    let (_attacker, attacker_usdc) = env.user(0);
    expect_err(env.refund(&raise, &buyer.pubkey(), &attacker_usdc), code(FanexError::RaiseAccountMismatch));
    env.refund(&raise, &buyer.pubkey(), &buyer_usdc).unwrap();
    assert_eq!(env.balance(&buyer_usdc), 100 * USD);
    assert_eq!(env.balance(&attacker_usdc), 0);
}

#[test]
fn a_refund_is_paid_once() {
    let mut env = Env::new();
    let (creator, _, raise) = env.launch(1);
    let (buyer, buyer_usdc) = env.user(100 * USD);
    env.buy(&raise, &buyer, &buyer_usdc, 100 * USD).unwrap();
    env.cancel(&creator, &raise).unwrap();
    env.refund(&raise, &buyer.pubkey(), &buyer_usdc).unwrap();
    // The position is closed: a second refund has no account to read.
    assert!(!env.exists(&position_pda(&raise, &buyer.pubkey())));
    assert_eq!(env.balance(&buyer_usdc), 100 * USD);
}

#[test]
fn the_pause_never_blocks_a_refund() {
    let mut env = Env::new();
    let (creator, _, raise) = env.launch(1);
    let (buyer, buyer_usdc) = env.user(100 * USD);
    env.buy(&raise, &buyer, &buyer_usdc, 100 * USD).unwrap();
    env.set_paused(true).unwrap();
    env.cancel(&creator, &raise).unwrap();
    env.refund(&raise, &buyer.pubkey(), &buyer_usdc).unwrap();
    assert_eq!(env.balance(&buyer_usdc), 100 * USD);
}
