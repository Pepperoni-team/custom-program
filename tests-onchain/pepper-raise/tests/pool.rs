mod common;

use common::*;
use pepper_curve::raise::constants::*;
use pepper_curve::errors::FanexError;
use pepper_curve::raise::pool_math::spot_price;
use pepper_curve::raise::state::{Position, Raise};
use solana_sdk::signature::{Keypair, Signer};

/// After `active_raise`, fans paid 1,000 USD for 1,000 quotas: the pool opens
/// with 90.909091 USD and 90,909,091 virtual units, so a quota starts at 1.00.
/// Every number below is the pool formula on that start, fee 1%.
const POOL_START: u64 = 90_909_091;

#[test]
fn the_pool_opens_at_the_price_fans_paid() {
    let mut env = Env::new();
    let (_, _, raise, _) = env.active_raise(1);
    assert_eq!(spot_price(env.fetch::<Raise>(&raise).pool()).unwrap(), 1_000_000);
}

#[test]
fn pool_trades_need_the_attester() {
    let mut env = Env::new();
    let (_, _, raise, buyers) = env.active_raise(1);
    let (seller, seller_usdc) = &buyers[0];
    let impostor = Keypair::new();
    expect_err(env.sell_to_pool_as(&raise, seller, seller_usdc, 10 * Q, 1, &impostor), code(FanexError::RaiseUnauthorized));
}

#[test]
fn a_sale_pays_the_pool_price_minus_the_fee() {
    let mut env = Env::new();
    let (_, _, raise, buyers) = env.active_raise(1);
    let (seller, seller_usdc) = &buyers[0];
    // gross = floor(90,909,091 x 10,000,000 / (0 + 90,909,091 + 10,000,000)) = 9,009,009
    // fee   = ceil(90,090.09) = 90,091; net = 8,918,918.
    env.sell_to_pool(&raise, seller, seller_usdc, 10 * Q).unwrap();
    assert_eq!(env.balance(seller_usdc), 8_918_918);
    assert_eq!(env.balance(&vault(POOL_SEED, &raise)), POOL_START - 8_918_918);
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.pool_usdc, state.pool_units), (POOL_START - 8_918_918, 10 * Q));
    assert_eq!(env.fetch::<Position>(&pool_position_pda(&raise)).units, 10 * Q);
    assert_eq!(env.fetch::<Position>(&position_pda(&raise, &seller.pubkey())).units, 90 * Q);
}

#[test]
fn a_sale_below_the_sellers_minimum_is_refused() {
    let mut env = Env::new();
    let (_, _, raise, buyers) = env.active_raise(1);
    let (seller, seller_usdc) = &buyers[0];
    let attester = env.attester.insecure_clone();
    expect_err(
        env.sell_to_pool_as(&raise, seller, seller_usdc, 10 * Q, 8_918_919, &attester),
        code(FanexError::PriceOutsideLimit),
    );
    env.sell_to_pool_as(&raise, seller, seller_usdc, 10 * Q, 8_918_918, &attester).unwrap();
}

#[test]
fn selling_more_than_held_is_refused() {
    let mut env = Env::new();
    let (_, _, raise, buyers) = env.active_raise(1);
    let (seller, seller_usdc) = &buyers[0];
    expect_err(env.sell_to_pool(&raise, seller, seller_usdc, 100 * Q + 1), code(FanexError::InsufficientUnits));
}

#[test]
fn selling_to_the_pool_keeps_the_seller_pending() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, buyers) = env.active_raise(1);
    env.pay_november(&creator, &creator_usdc, &raise, 100 * USD);
    let (seller, seller_usdc) = &buyers[0];
    // gross = floor(90,909,091 x 100,000,000 / 190,909,091) = 47,619,047
    // fee   = 476,191; net = 47,142,856.
    env.sell_to_pool(&raise, seller, seller_usdc, 100 * Q).unwrap();
    assert_eq!(env.fetch::<Position>(&position_pda(&raise, &seller.pubkey())).pending, 9_700_000);
    env.claim(&raise, &seller.pubkey(), seller_usdc).unwrap();
    // The sale plus the 9.70 accrued on 100 quotas before it.
    assert_eq!(env.balance(seller_usdc), 47_142_856 + 9_700_000);
}

#[test]
fn a_later_buyer_buys_from_the_pool() {
    let mut env = Env::new();
    let (_, _, raise, buyers) = env.active_raise(1);
    let (seller, seller_usdc) = &buyers[0];
    env.sell_to_pool(&raise, seller, seller_usdc, 10 * Q).unwrap();
    let (late, late_usdc) = env.user(20 * USD);
    // gross = ceil(81,990,173 x 10,000,000 / (10,000,000 + 90,909,091 - 10,000,000)) = 9,018,920
    // fee   = ceil(90,189.20) = 90,190; cost = 9,109,110.
    expect_err(env.buy_from_pool(&raise, &late, &late_usdc, 10 * Q, 9_109_109), code(FanexError::PriceOutsideLimit));
    env.buy_from_pool(&raise, &late, &late_usdc, 10 * Q, 9_109_110).unwrap();
    assert_eq!(env.balance(&late_usdc), 20 * USD - 9_109_110);
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.pool_usdc, state.pool_units), (91_099_283, 0));
    assert_eq!(env.fetch::<Position>(&position_pda(&raise, &late.pubkey())).units, 10 * Q);
}

#[test]
fn a_purchase_larger_than_the_pools_quotas_is_refused() {
    let mut env = Env::new();
    let (_, _, raise, buyers) = env.active_raise(1);
    let (late, late_usdc) = env.user(20 * USD);
    // The pool holds no real quota yet.
    expect_err(env.buy_from_pool(&raise, &late, &late_usdc, Q, u64::MAX), code(FanexError::InsufficientUnits));
    let (seller, seller_usdc) = &buyers[0];
    env.sell_to_pool(&raise, seller, seller_usdc, 10 * Q).unwrap();
    expect_err(env.buy_from_pool(&raise, &late, &late_usdc, 10 * Q + 1, u64::MAX), code(FanexError::InsufficientUnits));
}

#[test]
fn the_wallet_limit_holds_on_pool_purchases() {
    let mut env = Env::new();
    let (_, _, raise, buyers) = env.active_raise(1);
    let (seller, seller_usdc) = &buyers[4];
    env.sell_to_pool(&raise, seller, seller_usdc, 10 * Q).unwrap();
    // buyers[5] already holds 100 quotas, the limit.
    let (whale, whale_usdc) = &buyers[5];
    env.mint_to(whale_usdc, 20 * USD);
    expect_err(env.buy_from_pool(&raise, whale, whale_usdc, Q, u64::MAX), code(FanexError::WalletLimitExceeded));
}

#[test]
fn the_creator_can_trade_in_their_own_pool() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, buyers) = env.active_raise(1);
    let (seller, seller_usdc) = &buyers[0];
    env.sell_to_pool(&raise, seller, seller_usdc, 10 * Q).unwrap();
    // Decision D17: the creator is not excluded. Buy 5 quotas, then sell them back.
    // buy:  gross = ceil(81,990,173 x 5,000,000 / 95,909,091) = 4,274,370; fee 42,744
    // sell: gross = floor(86,307,287 x 5,000,000 / 100,909,091) = 4,276,487; fee 42,765
    let before = env.balance(&creator_usdc);
    env.buy_from_pool(&raise, &creator, &creator_usdc, 5 * Q, u64::MAX).unwrap();
    assert_eq!(before - env.balance(&creator_usdc), 4_317_114);
    assert_eq!(env.fetch::<Position>(&position_pda(&raise, &creator.pubkey())).units, 5 * Q);
    env.sell_to_pool(&raise, &creator, &creator_usdc, 5 * Q).unwrap();
    // The round trip costs the two fees and the price impact.
    assert_eq!(before - env.balance(&creator_usdc), 4_317_114 - 4_233_722);
    assert_eq!(env.fetch::<Position>(&position_pda(&raise, &creator.pubkey())).units, 0);
}

#[test]
fn the_pools_payouts_join_the_pool() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, buyers) = env.active_raise(1);
    let (seller, seller_usdc) = &buyers[0];
    env.sell_to_pool(&raise, seller, seller_usdc, 10 * Q).unwrap();
    env.mint_to(&creator_usdc, 100 * USD);
    env.fund_direct(&creator, &creator_usdc, &raise, 100 * USD).unwrap();
    env.sweep_pool(&raise).unwrap();
    // 10 of 1,000 quotas: a hundredth of the 97 USD that reached the fans.
    assert_eq!(env.balance(&vault(POOL_SEED, &raise)), 81_990_173 + 970_000);
    assert_eq!(env.fetch::<Raise>(&raise).pool_usdc, 81_990_173 + 970_000);
    expect_err(env.sweep_pool(&raise), code(FanexError::NothingOwed));
}

#[test]
fn a_trade_sweeps_the_pools_payouts_first() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, buyers) = env.active_raise(1);
    env.sell_to_pool(&raise, &buyers[0].0, &buyers[0].1, 10 * Q).unwrap();
    env.mint_to(&creator_usdc, 100 * USD);
    env.fund_direct(&creator, &creator_usdc, &raise, 100 * USD).unwrap();
    let (seller, seller_usdc) = &buyers[1];
    env.sell_to_pool(&raise, seller, seller_usdc, 10 * Q).unwrap();
    // Priced on 81,990,173 + 970,000 = 82,960,173:
    // gross = floor(82,960,173 x 10,000,000 / (10,000,000 + 90,909,091 + 10,000,000)) = 7,480,015
    // fee   = 74,801; net = 7,405,214.
    assert_eq!(env.balance(seller_usdc), 7_405_214);
    assert_eq!(env.fetch::<Raise>(&raise).pool_usdc, 82_960_173 - 7_405_214);
}

#[test]
fn usdc_sent_straight_to_the_pool_does_not_move_the_price() {
    let mut env = Env::new();
    let (_, _, raise, buyers) = env.active_raise(1);
    env.mint_to(&vault(POOL_SEED, &raise), 50 * USD);
    assert_eq!(env.fetch::<Raise>(&raise).pool_usdc, POOL_START);
    let (seller, seller_usdc) = &buyers[0];
    env.sell_to_pool(&raise, seller, seller_usdc, 10 * Q).unwrap();
    // The same 8,918,918 as without the gift: the price reads the counter.
    assert_eq!(env.balance(seller_usdc), 8_918_918);
}

#[test]
fn one_base_unit_bought_from_a_pool_holding_every_quota_no_longer_takes_the_whole_pool() {
    // The internal audit of 2026-10-29 (close.rs:116): every fan sold to the pool. One base unit
    // cost two at the curve's price, and at close it was the only fan unit: the pool's whole USDC,
    // the creator's when no fan is left, went to it, and its holding kept M-01's write-off away.
    let mut env = Env::new();
    let (_, creator_usdc, raise, buyers) = env.active_raise(1);
    for (seller, seller_usdc) in &buyers {
        env.sell_to_pool(&raise, seller, seller_usdc, 100 * Q).unwrap();
    }
    let (late, late_usdc) = env.user(USD);
    for units in [1, Q] {
        expect_err(env.buy_from_pool(&raise, &late, &late_usdc, units, u64::MAX), code(FanexError::PoolHoldsEveryQuota));
    }
    assert_eq!(env.balance(&late_usdc), USD);
    // At close the pool's USDC still goes to the creator, and no quota is left outstanding.
    env.declare_zero_months_to_term_end(&raise);
    let (pool_cash, guarantee) = (env.balance(&vault(POOL_SEED, &raise)), env.balance(&vault(GUARANTEE_SEED, &raise)));
    let before = env.balance(&creator_usdc);
    env.close_raise(&raise, &creator_usdc).unwrap();
    assert_eq!(env.balance(&creator_usdc) - before, pool_cash + guarantee);
    assert_eq!(env.fetch::<Raise>(&raise).units_outstanding, 0);
}

#[test]
fn a_quota_under_the_last_fans_share_is_not_sold() {
    // Nine fans sold all, the tenth all but one quota. The curve prices a quota at a thousandth of what
    // it takes at close. From the internal audit of 2026-10-29 the purchase was charged the pool's USDC
    // per fan unit instead; since N+3's second round (Rick, 2026-11-05) the pool does not sell under it.
    let mut env = Env::new();
    let (_, creator_usdc, raise, buyers) = env.active_raise(1);
    for (i, (seller, seller_usdc)) in buyers.iter().enumerate() {
        let units = if i == 9 { 99 * Q } else { 100 * Q };
        env.sell_to_pool(&raise, seller, seller_usdc, units).unwrap();
    }
    let state: Raise = env.fetch(&raise);
    assert_eq!(state.units_outstanding - state.pool_units, Q);
    let curve = pepper_curve::raise::pool_math::buy(state.pool(), Q, POOL_FEE_BPS).unwrap();
    assert!(curve.gross * 100 < state.pool_usdc, "the curve sells the quota for {}", curve.gross);
    let (late, late_usdc) = env.user(2 * state.pool_usdc);
    // (One base unit is the curve's 1 against a share that rounds up to 1 too, the pool holding under one USDC
    // here: sold, and worth at close at most what it paid.)
    for units in [1_000, Q] {
        expect_err(env.buy_from_pool(&raise, &late, &late_usdc, units, u64::MAX), code(FanexError::PoolPriceUnderShare));
    }
    assert_eq!(env.balance(&late_usdc), 2 * state.pool_usdc);
    // At close the last fan's quota takes the whole pool.
    env.declare_zero_months_to_term_end(&raise);
    env.close_raise(&raise, &creator_usdc).unwrap();
    let (last, last_usdc) = &buyers[9];
    let before = env.balance(last_usdc);
    env.close_position(&raise, &last.pubkey(), last_usdc).unwrap();
    let back = env.balance(last_usdc) - before;
    assert!(back + 1 >= state.pool_usdc, "back {back}, pool {}", state.pool_usdc);
}

#[test]
fn a_late_sale_is_paid_the_sellers_share_of_the_pool() {
    // N+3 (`pool_math::sell_pro_rata`, Rick 2026-11-04, option B): five fans sold their 100 quotas at
    // the curve's price; the sixth's 100 are a fifth of the 500 the fans still hold, so they are paid a
    // fifth of the pool's USDC, more than the curve would, the fee taken as on any sale.
    let mut env = Env::new();
    let (_, _, raise, buyers) = env.active_raise(1);
    for (seller, seller_usdc) in &buyers[..5] {
        env.sell_to_pool(&raise, seller, seller_usdc, 100 * Q).unwrap();
    }
    let state: Raise = env.fetch(&raise);
    assert_eq!(state.units_outstanding - state.pool_units, 500 * Q);
    let curve = pepper_curve::raise::pool_math::sell(state.pool(), 100 * Q, POOL_FEE_BPS).unwrap();
    let share = state.pool_usdc / 5;
    let fee = pepper_curve::raise::pool_math::fee_of(share, POOL_FEE_BPS).unwrap();
    assert!(curve.gross < share, "the curve pays {} against a share of {share}", curve.gross);
    let (seller, seller_usdc) = &buyers[5];
    let before = env.balance(seller_usdc);
    env.sell_to_pool(&raise, seller, seller_usdc, 100 * Q).unwrap();
    // The curve's 2,073,847 less its fee would have been 2,053,108.
    assert_eq!((share, share - fee, env.balance(seller_usdc) - before), (2_865_680, 2_837_023, 2_837_023));
    // Each of the 400 quotas the fans still hold keeps at least its share of the pool.
    let after: Raise = env.fetch(&raise);
    assert!(after.pool_usdc as u128 * 500 >= state.pool_usdc as u128 * 400);
}

#[test]
fn the_pause_stops_trades_but_not_the_sweep() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, buyers) = env.active_raise(1);
    env.sell_to_pool(&raise, &buyers[0].0, &buyers[0].1, 10 * Q).unwrap();
    env.mint_to(&creator_usdc, 100 * USD);
    env.set_paused(true).unwrap();
    env.fund_direct(&creator, &creator_usdc, &raise, 100 * USD).unwrap();
    let (seller, seller_usdc) = &buyers[1];
    expect_err(env.sell_to_pool(&raise, seller, seller_usdc, Q), code(FanexError::RaiseGloballyPaused));
    let (late, late_usdc) = env.user(10 * USD);
    expect_err(env.buy_from_pool(&raise, &late, &late_usdc, Q, u64::MAX), code(FanexError::RaiseGloballyPaused));
    env.sweep_pool(&raise).unwrap();
}

// ── Units bought from the pool (N+3's audit rounds, Rick 2026-11-04 and 2026-11-05) ──

#[test]
fn the_audits_captures_cannot_start_once_the_fans_share_passes_the_curve() {
    // Both rounds' captures began the same way: nine fans sold their 100 quotas each to the pool and a
    // newcomer bought 100 at the fans' share (2,955,063), before the creator paid a month. Round 1 sold
    // them back floored after the credit (USD 4.77 gained on 2.96), round 2 held them to `close_raise`
    // (4.84). The curve prices the 100 at a tenth of that share, and the pool no longer sells under it.
    let mut env = Env::new();
    let (_, _, raise, buyers) = env.active_raise(1);
    for (seller, seller_usdc) in &buyers[..9] {
        env.sell_to_pool(&raise, seller, seller_usdc, 100 * Q).unwrap();
    }
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.pool_usdc, state.units_outstanding - state.pool_units), (2_925_804, 100 * Q));
    let (late, late_usdc) = env.user(10 * USD);
    expect_err(env.buy_from_pool(&raise, &late, &late_usdc, 100 * Q, u64::MAX), code(FanexError::PoolPriceUnderShare));
    expect_err(env.buy_from_pool(&raise, &late, &late_usdc, Q, u64::MAX), code(FanexError::PoolPriceUnderShare));
    assert_eq!(env.balance(&late_usdc), 10 * USD);
}

#[test]
fn a_fan_who_bought_back_from_the_pool_sells_those_units_first_at_the_curve() {
    // One fan sold its 100 quotas; the sixth sells 40 of its 100 and buys 40 back while the fans still
    // hold 86% (the curve above their share). Four more fans then sell their 100, the fans down to 50%,
    // and the sixth sells all 100: the 40 at the curve alone, the 60 it bought in the raise at its floor.
    let mut env = Env::new();
    let (_, _, raise, buyers) = env.active_raise(1);
    env.sell_to_pool(&raise, &buyers[0].0, &buyers[0].1, 100 * Q).unwrap();
    let (fan, fan_usdc) = &buyers[5];
    env.sell_to_pool(&raise, fan, fan_usdc, 40 * Q).unwrap();
    env.mint_to(fan_usdc, 10 * USD);
    env.buy_from_pool(&raise, fan, fan_usdc, 40 * Q, u64::MAX).unwrap();
    for (seller, seller_usdc) in &buyers[1..5] {
        env.sell_to_pool(&raise, seller, seller_usdc, 100 * Q).unwrap();
    }
    let position: Position = env.fetch(&position_pda(&raise, &fan.pubkey()));
    assert_eq!((position.units, position.from_pool), (100 * Q, 40 * Q));
    let state: Raise = env.fetch(&raise);
    let fans = state.units_outstanding - state.pool_units;
    let expected = pepper_curve::raise::pool_math::sell_position(state.pool(), 100 * Q, 40 * Q, POOL_FEE_BPS, fans).unwrap();
    let floored = pepper_curve::raise::pool_math::sell_pro_rata(state.pool(), 100 * Q, POOL_FEE_BPS, fans).unwrap();
    assert!(expected.net < floored.net);
    let before = env.balance(fan_usdc);
    env.sell_to_pool(&raise, fan, fan_usdc, 100 * Q).unwrap();
    assert_eq!(env.balance(fan_usdc) - before, expected.net);
    assert_eq!(env.fetch::<Position>(&position_pda(&raise, &fan.pubkey())).from_pool, 0);
}

#[test]
fn a_curve_account_given_as_a_raise_is_refused_by_its_discriminator() {
    // Since N+3 the raise's accounts and the curves' share pepper_curve's program id: only the
    // discriminator tells a `Raise` from a `CurveUsdc`.
    use anchor_lang::{Discriminator, Space};
    let mut env = Env::new();
    let (_, _, raise, _) = env.active_raise(1);
    let fake = Keypair::new().pubkey();
    let mut data = pepper_curve::state::CurveUsdc::DISCRIMINATOR.to_vec();
    data.resize(8 + pepper_curve::state::CurveUsdc::INIT_SPACE, 0);
    let lamports = env.svm.minimum_balance_for_rent_exemption(data.len());
    env.svm
        .set_account(fake, solana_sdk::account::Account { lamports, data, owner: pepper_curve::ID, executable: false, rent_epoch: 0 })
        .unwrap();
    expect_err(env.sweep_pool(&fake), anchor_code(anchor_lang::error::ErrorCode::AccountDiscriminatorMismatch));
    // The real raise goes through the same instruction's checks.
    assert!(env.fetch::<Raise>(&raise).pool_usdc > 0);
}
