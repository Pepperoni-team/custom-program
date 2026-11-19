mod common;

use common::*;
use pepper_curve::raise::constants::*;
use pepper_curve::errors::FanexError;
use pepper_curve::raise::state::{Position, Raise, RaiseState};
use pepper_curve::raise::{RaiseParams, TierInput};
use solana_sdk::signature::{Keypair, Signer};

#[test]
fn a_launch_takes_the_fee_and_opens_four_vaults() {
    let mut env = Env::new();
    let (_creator, creator_usdc, raise) = env.launch(1);
    assert_eq!(env.balance(&env.fee_usdc.clone()), 5 * USD);
    assert_eq!(env.balance(&creator_usdc), 5 * USD);
    for seed in [ESCROW_SEED, GUARANTEE_SEED, POOL_SEED, PAYOUTS_SEED] {
        assert_eq!(env.balance(&vault(seed, &raise)), 0);
    }
    let state: Raise = env.fetch(&raise);
    assert_eq!(state.state, RaiseState::Raising);
    assert_eq!(state.fan_max, 1_000 * USD);
    assert_eq!(state.max_units, 1_000 * Q);
}

#[test]
fn a_launch_never_pays_more_than_the_fee_it_was_built_on() {
    // The internal audit of 2026-10-29 (admin.rs:84): the fee moves at landing, from the config as
    // it is then. Built on the config's USD 5, a launch landing after the admin raised the fee to
    // USD 50 is refused and the creator's USDC stays; built on USD 50, it lands.
    let mut env = Env::new();
    let (creator, creator_usdc) = env.user(60 * USD);
    let attester = env.attester.insecure_clone();
    let now = env.now();
    let built_on = env.launch_fee();
    let raise_fee = ix(
        pepper_curve::accounts::UpdateRaiseConfig { config: config_pda(), admin: env.admin.pubkey() },
        pepper_curve::instruction::UpdateRaiseConfig {
            new_admin: None,
            new_attester: None,
            new_fee_wallet: None,
            new_launch_fee: Some(50 * USD),
            new_pool_fee_bps: None,
        },
    );
    env.send(&[raise_fee], &[]).unwrap();
    let refused = env.create_raise_capped(&creator, &creator_usdc, small_params(1, now), &attester, built_on);
    expect_err(refused.map(|_| ()), code(FanexError::PriceOutsideLimit));
    assert_eq!(env.balance(&creator_usdc), 60 * USD);
    env.create_raise_capped(&creator, &creator_usdc, small_params(1, now), &attester, 50 * USD).unwrap();
    assert_eq!(env.balance(&creator_usdc), 10 * USD);
}

#[test]
fn a_launch_needs_the_attester() {
    let mut env = Env::new();
    let (creator, creator_usdc) = env.user(10 * USD);
    let impostor = Keypair::new();
    let now = env.now();
    let result = env.create_raise_as(&creator, &creator_usdc, small_params(1, now), &impostor);
    expect_err(result.map(|_| ()), code(FanexError::RaiseUnauthorized));
}

#[test]
fn invalid_parameters_are_refused() {
    let mut env = Env::new();
    let (creator, creator_usdc) = env.user(10 * USD);
    let mut params = small_params(1, env.now());
    params.term_months = 23;
    expect_err(env.create_raise(&creator, &creator_usdc, params).map(|_| ()), code(FanexError::InvalidTerm));
}

#[test]
fn buying_holds_the_money_in_escrow() {
    let mut env = Env::new();
    let (_, _, raise) = env.launch(1);
    let (buyer, buyer_usdc) = env.user(100 * USD);
    env.buy(&raise, &buyer, &buyer_usdc, 100 * USD).unwrap();
    assert_eq!(env.balance(&vault(ESCROW_SEED, &raise)), 100 * USD);
    assert_eq!(env.balance(&buyer_usdc), 0);
    let position: Position = env.fetch(&position_pda(&raise, &buyer.pubkey()));
    assert_eq!((position.owner, position.units, position.paid), (buyer.pubkey(), 100 * Q, 100 * USD));
    assert_eq!(env.fetch::<Raise>(&raise).sold_units, 100 * Q);
}

#[test]
fn buying_needs_the_attester() {
    let mut env = Env::new();
    let (_, _, raise) = env.launch(1);
    let (buyer, buyer_usdc) = env.user(10 * USD);
    let impostor = Keypair::new();
    expect_err(env.buy_as(&raise, &buyer, &buyer_usdc, 10 * USD, 0, &impostor), code(FanexError::RaiseUnauthorized));
}

#[test]
fn a_wallet_holds_at_most_10_percent() {
    let mut env = Env::new();
    let (_, _, raise) = env.launch(1);
    let (buyer, buyer_usdc) = env.user(101 * USD);
    expect_err(env.buy(&raise, &buyer, &buyer_usdc, 101 * USD), code(FanexError::WalletLimitExceeded));
}

#[test]
fn a_purchase_under_two_dollars_is_refused() {
    let mut env = Env::new();
    let (_, _, raise) = env.launch(1);
    let (buyer, buyer_usdc) = env.user(USD);
    expect_err(env.buy(&raise, &buyer, &buyer_usdc, USD), code(FanexError::BelowMinimumPurchase));
}

#[test]
fn buying_after_the_deadline_is_refused() {
    let mut env = Env::new();
    let (_, _, raise) = env.launch(1);
    let deadline = env.fetch::<Raise>(&raise).deadline;
    env.set_time(deadline);
    let (buyer, buyer_usdc) = env.user(10 * USD);
    expect_err(env.buy(&raise, &buyer, &buyer_usdc, 10 * USD), code(FanexError::RaiseExpired));
}

#[test]
fn the_creator_cancels_and_buying_stops() {
    let mut env = Env::new();
    let (creator, _, raise) = env.launch(1);
    env.cancel(&creator, &raise).unwrap();
    assert_eq!(env.fetch::<Raise>(&raise).state, RaiseState::Refunding);
    let (buyer, buyer_usdc) = env.user(10 * USD);
    expect_err(env.buy(&raise, &buyer, &buyer_usdc, 10 * USD), code(FanexError::NotRaising));
}

#[test]
fn the_creator_cannot_cancel_after_the_deadline() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise) = env.launch(1);
    for _ in 0..3 {
        let (buyer, buyer_usdc) = env.user(100 * USD);
        env.buy(&raise, &buyer, &buyer_usdc, 100 * USD).unwrap();
    }
    let deadline = env.fetch::<Raise>(&raise).deadline;
    env.set_time(deadline);
    // Raising is over (spec §4.5): only `finalize` is left, and above the minimum it succeeds.
    expect_err(env.cancel(&creator, &raise), code(FanexError::NotRaising));
    env.finalize(&raise, &creator_usdc).unwrap();
    assert_eq!(env.fetch::<Raise>(&raise).state, RaiseState::Active);
}

#[test]
fn the_creator_cannot_cancel_a_sold_out_raise() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise) = env.launch(1);
    env.sell_out(&raise);
    // A raise closes at once at its maximum (spec §4.5), before anyone calls `finalize`.
    expect_err(env.cancel(&creator, &raise), code(FanexError::NotRaising));
    env.finalize(&raise, &creator_usdc).unwrap();
    assert_eq!(env.fetch::<Raise>(&raise).state, RaiseState::Active);
}

#[test]
fn only_the_creator_cancels() {
    let mut env = Env::new();
    let (_, _, raise) = env.launch(1);
    let (stranger, _) = env.user(0);
    expect_err(env.cancel(&stranger, &raise), code(FanexError::RaiseUnauthorized));
}

#[test]
fn the_pause_stops_buying() {
    let mut env = Env::new();
    let (_, _, raise) = env.launch(1);
    env.set_paused(true).unwrap();
    let (buyer, buyer_usdc) = env.user(10 * USD);
    expect_err(env.buy(&raise, &buyer, &buyer_usdc, 10 * USD), code(FanexError::RaiseGloballyPaused));
}

/// Spec §4.4's tiers on 1,000 quotas: 400 at 1.00, 300 at 1.10, 300 at 1.20.
/// Fans pay at most 1,090 USD; the creator's part is 990.909090 USD.
fn three_tier_params(id: u64, now: i64) -> RaiseParams {
    RaiseParams {
        raise_id: id,
        tier_count: 3,
        tiers: [
            TierInput { units: 400 * Q, price: 1_000_000 },
            TierInput { units: 300 * Q, price: 1_100_000 },
            TierInput { units: 300 * Q, price: 1_200_000 },
        ],
        min_creator_part: 247_727_273,
        share_bps: 10_000,
        term_months: 24,
        deadline: now + 21 * DAY,
    }
}

#[test]
fn a_purchase_across_two_tiers_pays_each_part_at_its_own_price() {
    let mut env = Env::new();
    let (creator, creator_usdc) = env.user(10 * USD);
    let now = env.now();
    let raise = env.create_raise(&creator, &creator_usdc, three_tier_params(1, now)).unwrap();
    // The first tier sells out at 1.00, then 245.454545 quotas of the second go
    // at 1.10: the last 50 USD buy floor(50 / 1.10) = 45,454,545 units for
    // ceil(49,999,999.5) = 50,000,000.
    for budget in [100 * USD, 100 * USD, 100 * USD, 100 * USD, 110 * USD, 110 * USD, 50 * USD] {
        let (buyer, buyer_usdc) = env.user(budget);
        env.buy(&raise, &buyer, &buyer_usdc, budget).unwrap();
    }
    let before: Raise = env.fetch(&raise);
    assert_eq!(before.tiers.map(|t| t.sold), [400 * Q, 245_454_545, 0]);
    let escrow_before = env.balance(&vault(ESCROW_SEED, &raise));

    // 90.000003 USD: the 54,545,455 units left at 1.10 cost ceil(60,000,000.5) =
    // 60,000,001; the 30,000,002 left buy floor(30,000,002 / 1.20) = 25,000,001
    // units at 1.20 for ceil(30,000,001.2) = 30,000,002. Each part rounds up on
    // its own: one rounding over the whole would charge 90,000,002.
    let (buyer, buyer_usdc) = env.user(100 * USD);
    env.buy(&raise, &buyer, &buyer_usdc, 90_000_003).unwrap();
    let after: Raise = env.fetch(&raise);
    assert_eq!(after.tiers.map(|t| t.sold), [400 * Q, 300 * Q, 25_000_001]);
    assert_eq!(after.sold_units - before.sold_units, 54_545_455 + 25_000_001);
    assert_eq!(after.fan_paid - before.fan_paid, 60_000_001 + 30_000_002);
    assert_eq!(env.balance(&vault(ESCROW_SEED, &raise)) - escrow_before, 90_000_003);
    assert_eq!(env.balance(&buyer_usdc), 100 * USD - 90_000_003);
    let position: Position = env.fetch(&position_pda(&raise, &buyer.pubkey()));
    assert_eq!((position.units, position.paid), (79_545_456, 90_000_003));
}
