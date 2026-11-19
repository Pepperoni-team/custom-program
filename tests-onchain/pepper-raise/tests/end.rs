mod common;

use common::*;
use pepper_curve::raise::calendar::{declare_open_ts, due_ts};
use pepper_curve::raise::constants::*;
use pepper_curve::errors::FanexError;
use pepper_curve::raise::ledger::close_fallback_ts;
use pepper_curve::raise::state::{Month, Position, Raise, RaiseState};
use pepper_curve::raise::{RaiseParams, TierInput};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signer};

#[test]
fn closing_before_the_term_is_over_is_refused() {
    let mut env = Env::new();
    let (_, creator_usdc, raise, _) = env.active_raise(1);
    expect_err(env.close_raise(&raise, &creator_usdc), code(FanexError::NotClosable));
}

#[test]
fn an_undeclared_term_closes_after_the_fallback() {
    let mut env = Env::new();
    let (_, creator_usdc, raise, buyers) = env.active_raise(1);
    // The attester declares two months and then stops.
    for month in [NOVEMBER_2026, NOVEMBER_2026 + 1] {
        env.set_time(declare_open_ts(month));
        env.declare(&raise, month, 0).unwrap();
    }
    let fallback = close_fallback_ts(env.fetch::<Raise>(&raise).term_end_month);
    env.set_time(fallback - 1);
    expect_err(env.close_raise(&raise, &creator_usdc), code(FanexError::NotClosable));
    env.set_time(fallback);
    let before = env.balance(&creator_usdc);
    env.close_raise(&raise, &creator_usdc).unwrap();
    assert_eq!(env.fetch::<Raise>(&raise).state, RaiseState::Closed);
    // Nothing waits on the attester: the guarantee goes back, the pool is shared.
    assert_eq!(env.balance(&creator_usdc) - before, 45_454_545);
    let (holder, holder_usdc) = &buyers[3];
    env.claim(&raise, &holder.pubkey(), holder_usdc).unwrap();
    assert_eq!(env.balance(holder_usdc), 9_090_909);
    // Once closed, no month can be declared any more.
    expect_err(env.declare(&raise, NOVEMBER_2026 + 2, USD), code(FanexError::NotActive));
}

#[test]
fn closing_at_the_end_of_the_term_shares_the_pool() {
    let mut env = Env::new();
    let (_, creator_usdc, raise, buyers) = env.active_raise(1);
    env.declare_zero_months_to_term_end(&raise);
    let before = env.balance(&creator_usdc);
    env.close_raise(&raise, &creator_usdc).unwrap();
    assert_eq!(env.fetch::<Raise>(&raise).state, RaiseState::Closed);
    // The guarantee goes back to the creator; the pool is emptied into the payouts.
    assert_eq!(env.balance(&creator_usdc) - before, 45_454_545);
    assert_eq!(env.balance(&vault(POOL_SEED, &raise)), 0);
    // A holder of 100 of the 1,000 quotas: a tenth of the pool's 90.909091.
    let (holder, holder_usdc) = &buyers[3];
    env.claim(&raise, &holder.pubkey(), holder_usdc).unwrap();
    assert_eq!(env.balance(holder_usdc), 9_090_909);
}

#[test]
fn closing_cancels_the_pools_quotas_and_shares_its_usdc_with_the_fans() {
    let mut env = Env::new();
    let (_, creator_usdc, raise, buyers) = env.active_raise(1);
    let (seller, seller_usdc) = &buyers[0];
    // The pool buys 100 quotas for 47,142,856 and keeps 43,766,235.
    env.sell_to_pool(&raise, seller, seller_usdc, 100 * Q).unwrap();
    env.declare_zero_months_to_term_end(&raise);
    env.close_raise(&raise, &creator_usdc).unwrap();
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.units_outstanding, state.pool_units, state.pool_usdc), (900 * Q, 0, 0));
    assert_eq!(env.fetch::<Position>(&pool_position_pda(&raise)).units, 0);
    // 900 fan quotas share 43,766,235: 43,766,235 x 10^12 / 900,000,000 =
    // 48,629,150,000 per unit, 4,862,915 for 100 quotas.
    let (holder, holder_usdc) = &buyers[1];
    env.claim(&raise, &holder.pubkey(), holder_usdc).unwrap();
    assert_eq!(env.balance(holder_usdc), 4_862_915);
    // The seller holds nothing any more and receives nothing from the close.
    expect_err(env.claim(&raise, &seller.pubkey(), seller_usdc), code(FanexError::NothingOwed));
}

#[test]
fn usdc_sent_straight_to_the_pool_is_shared_at_close() {
    let mut env = Env::new();
    let (_, creator_usdc, raise, buyers) = env.active_raise(1);
    env.mint_to(&vault(POOL_SEED, &raise), 9 * USD);
    env.declare_zero_months_to_term_end(&raise);
    env.close_raise(&raise, &creator_usdc).unwrap();
    // 90,909,091 + 9,000,000 over 1,000 quotas: 9,990,909 for 100.
    let (holder, holder_usdc) = &buyers[2];
    env.claim(&raise, &holder.pubkey(), holder_usdc).unwrap();
    assert_eq!(env.balance(holder_usdc), 9_990_909);
}

#[test]
fn close_with_every_quota_in_the_pool_pays_the_creator() {
    let mut env = Env::new();
    let (_, creator_usdc, raise, buyers) = env.active_raise(1);
    for (seller, seller_usdc) in &buyers {
        env.sell_to_pool(&raise, seller, seller_usdc, 100 * Q).unwrap();
    }
    assert_eq!(env.fetch::<Raise>(&raise).pool_units, 1_000 * Q);
    env.declare_zero_months_to_term_end(&raise);
    let pool_cash = env.balance(&vault(POOL_SEED, &raise));
    let guarantee = env.balance(&vault(GUARANTEE_SEED, &raise));
    // Ten sales of 100 quotas paid out 90,879,832 of the 90,909,091: the first five at the curve's
    // price, the last five at their share of the pool (N+3, `sell_pro_rata`: 2,837,023 for the sixth,
    // where the curve paid 2,073,847 less its fee), the last one the whole counter but its fee. Only
    // the fees are left, and the pool never empties (83,097,012 paid and 7,812,079 left before N+3).
    assert_eq!((pool_cash, guarantee), (29_259, 45_454_545));
    let before = env.balance(&creator_usdc);
    env.close_raise(&raise, &creator_usdc).unwrap();
    // No fan holds a quota and every seller was paid: the pool's USDC goes to the creator.
    assert_eq!(env.balance(&creator_usdc) - before, pool_cash + guarantee);
    assert_eq!(env.fetch::<Raise>(&raise).units_outstanding, 0);
}

#[test]
fn arrears_stay_payable_after_close() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, buyers) = env.active_raise(1);
    let state: Raise = env.fetch(&raise);
    // November owes 100 USD and is not paid on time; every later month owes nothing.
    for month in state.first_month..=state.term_end_month {
        env.set_time(declare_open_ts(month));
        let due = if month == NOVEMBER_2026 { 100 * USD } else { 0 };
        env.declare(&raise, month, due).unwrap();
    }
    env.set_time(due_ts(state.term_end_month));
    // The guarantee covers 45.454545 of November; 54.545455 stays owed.
    env.settle(&raise, NOVEMBER_2026).unwrap();
    env.close_raise(&raise, &creator_usdc).unwrap();
    env.mint_to(&creator_usdc, 100 * USD);
    let before = env.balance(&creator_usdc);
    env.fund_month(&creator, &creator_usdc, &raise, NOVEMBER_2026, 100 * USD).unwrap();
    // Only what November still owes moves.
    assert_eq!(before - env.balance(&creator_usdc), 54_545_455);
    let november: Month = env.fetch(&month_pda(&raise, NOVEMBER_2026));
    assert_eq!((november.covered, november.paid), (45_454_545, 54_545_455));
    // A holder of 100 quotas: a tenth of the guarantee's 44,090,909, of the
    // pool's 90,909,091 and of the late 52,909,092.
    let (holder, holder_usdc) = &buyers[0];
    env.claim(&raise, &holder.pubkey(), holder_usdc).unwrap();
    assert_eq!(env.balance(holder_usdc), 18_790_909);
}

#[test]
fn usdc_sent_straight_to_the_guarantee_does_not_block_the_close() {
    let mut env = Env::new();
    let (_, creator_usdc, raise, _) = env.active_raise(1);
    let state: Raise = env.fetch(&raise);
    for month in state.first_month..=state.term_end_month {
        env.set_time(declare_open_ts(month));
        let due = if month == NOVEMBER_2026 { 100 * USD } else { 0 };
        env.declare(&raise, month, due).unwrap();
    }
    env.set_time(due_ts(state.term_end_month));
    // The whole guarantee is spent on November, which stays open.
    env.settle(&raise, NOVEMBER_2026).unwrap();
    assert_eq!(env.fetch::<Raise>(&raise).guarantee_usdc, 0);
    // One base unit sent straight to the guarantee vault moves no counter: an
    // open month with an empty guarantee still lets the raise close (ruling P3).
    env.mint_to(&vault(GUARANTEE_SEED, &raise), 1);
    assert_eq!(env.fetch::<Raise>(&raise).guarantee_usdc, 0);
    let before = env.balance(&creator_usdc);
    env.close_raise(&raise, &creator_usdc).unwrap();
    // The vault's whole balance goes where the guarantee left goes: to the creator.
    assert_eq!(env.balance(&creator_usdc) - before, 1);
    assert_eq!(env.balance(&vault(GUARANTEE_SEED, &raise)), 0);
    assert_eq!(env.fetch::<Raise>(&raise).guarantee_usdc, 0);
}

#[test]
fn an_empty_position_is_closed_and_its_rent_returned() {
    let mut env = Env::new();
    let (_, _, raise, buyers) = env.active_raise(1);
    let (seller, seller_usdc) = &buyers[0];
    env.sell_to_pool(&raise, seller, seller_usdc, 100 * Q).unwrap();
    env.close_position(&raise, &seller.pubkey(), seller_usdc).unwrap();
    assert!(!env.exists(&position_pda(&raise, &seller.pubkey())));
}

#[test]
fn a_position_with_quotas_cannot_be_closed_while_the_raise_is_active() {
    let mut env = Env::new();
    let (_, _, raise, buyers) = env.active_raise(1);
    let (holder, holder_usdc) = &buyers[0];
    expect_err(env.close_position(&raise, &holder.pubkey(), holder_usdc), code(FanexError::PositionNotEmpty));
}

#[test]
fn the_pools_position_stays_while_the_raise_is_active() {
    let mut env = Env::new();
    let (_, _, raise, _) = env.active_raise(1);
    expect_err(env.close_position(&raise, &raise, &vault(POOL_SEED, &raise)), code(FanexError::NotClosable));
}

#[test]
fn a_failed_raise_gives_back_the_pools_rent() {
    let mut env = Env::new();
    let (_, creator_usdc, raise) = env.launch(1);
    let (buyer, buyer_usdc) = env.user(100 * USD);
    env.buy(&raise, &buyer, &buyer_usdc, 100 * USD).unwrap();
    let deadline = env.fetch::<Raise>(&raise).deadline;
    env.set_time(deadline);
    // Below the minimum: refunding, and the pool never opens.
    env.finalize(&raise, &creator_usdc).unwrap();
    // The pool's own position paid nothing: `refund` refuses it, and `close_position` returns its rent.
    expect_err(env.refund(&raise, &raise, &vault(POOL_SEED, &raise)), code(FanexError::RaiseAccountMismatch));
    env.close_position(&raise, &raise, &vault(POOL_SEED, &raise)).unwrap();
    assert!(!env.exists(&pool_position_pda(&raise)));
    // The buyer's holding cannot be closed this way: a refunding raise pays it back through `refund`.
    expect_err(env.close_position(&raise, &buyer.pubkey(), &buyer_usdc), code(FanexError::PositionNotEmpty));
}

#[test]
fn a_holders_position_closes_after_the_raise_with_its_pending_and_its_rent() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, buyers) = env.active_raise(1);
    env.declare_zero_months_to_term_end(&raise);
    env.close_raise(&raise, &creator_usdc).unwrap();
    let (holder, holder_usdc) = &buyers[0];
    let position = position_pda(&raise, &holder.pubkey());
    let rent = env.svm.get_account(&position).unwrap().lamports;
    let sponsor = env.admin.pubkey();
    let before = env.svm.get_account(&sponsor).unwrap().lamports;
    env.close_position(&raise, &holder.pubkey(), holder_usdc).unwrap();
    // Exactly its pending (a tenth of the pool's 90,909,091, never claimed), and
    // the rent back to the sponsor that paid it, which here also pays the
    // transaction's 5,000-lamport fee.
    assert_eq!(env.balance(holder_usdc), 9_090_909);
    assert!(!env.exists(&position));
    assert_eq!(env.svm.get_account(&sponsor).unwrap().lamports, before + rent - 5_000);
    assert_eq!(env.fetch::<Raise>(&raise).units_outstanding, 900 * Q);
    // A later payment is shared by the 900 quotas still held: 97,000,000 x 10^12
    // / 900,000,000 = 107,777,777,777 per unit; with the pool's 90,909,091,000,
    // 100 quotas are owed 19,868,686 (18,790,909 if the closed quotas had stayed).
    env.mint_to(&creator_usdc, 100 * USD);
    env.fund_direct(&creator, &creator_usdc, &raise, 100 * USD).unwrap();
    let (other, other_usdc) = &buyers[1];
    env.claim(&raise, &other.pubkey(), other_usdc).unwrap();
    assert_eq!(env.balance(other_usdc), 19_868_686);
}

#[test]
fn a_holders_position_stays_while_a_month_is_open() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, buyers) = env.active_raise(1);
    let state: Raise = env.fetch(&raise);
    for month in state.first_month..=state.term_end_month {
        env.set_time(declare_open_ts(month));
        let due = if month == NOVEMBER_2026 { 100 * USD } else { 0 };
        env.declare(&raise, month, due).unwrap();
    }
    env.set_time(due_ts(state.term_end_month));
    // The guarantee covers 45.454545 of November; the raise closes with November open.
    env.settle(&raise, NOVEMBER_2026).unwrap();
    env.close_raise(&raise, &creator_usdc).unwrap();
    let (holder, holder_usdc) = &buyers[0];
    expect_err(env.close_position(&raise, &holder.pubkey(), holder_usdc), code(FanexError::PositionNotEmpty));
    // November is paid, then the months behind it are settled: nothing is open.
    env.mint_to(&creator_usdc, 100 * USD);
    env.fund_month(&creator, &creator_usdc, &raise, NOVEMBER_2026, 100 * USD).unwrap();
    for month in NOVEMBER_2026 + 1..=state.term_end_month {
        env.settle(&raise, month).unwrap();
    }
    env.close_position(&raise, &holder.pubkey(), holder_usdc).unwrap();
    // A tenth of the guarantee's 44,090,909, of the pool's 90,909,091 and of the
    // late 52,909,092.
    assert_eq!(env.balance(holder_usdc), 18_790_909);
}

#[test]
fn closing_a_holders_position_pays_only_its_owner() {
    let mut env = Env::new();
    let (_, creator_usdc, raise, buyers) = env.active_raise(1);
    env.declare_zero_months_to_term_end(&raise);
    env.close_raise(&raise, &creator_usdc).unwrap();
    let (_attacker, attacker_usdc) = env.user(0);
    let (holder, holder_usdc) = &buyers[2];
    expect_err(env.close_position(&raise, &holder.pubkey(), &attacker_usdc), code(FanexError::RaiseAccountMismatch));
    env.close_position(&raise, &holder.pubkey(), holder_usdc).unwrap();
    assert_eq!(env.balance(holder_usdc), 9_090_909);
    assert_eq!(env.balance(&attacker_usdc), 0);
}

/// 10^12 + 1 units: one more than a million quotas.
const UNEVEN_UNITS: u64 = 1_000_000 * Q + 1;

/// Ten fans buy 1,000,000.000001 quotas at 1.00 (the last one a unit more than
/// the others), the raise succeeds at its deadline, and the creator pays the
/// fans 100 USD three times. Every other raise of these suites divides its
/// credits exactly and its rounding remainder is zero; 10^12 + 1 units do not:
/// each 97 USD credit raises the accumulator by
/// floor(97,000,000 x 10^12 / (10^12 + 1)) = 96,999,999, keeping back
/// (10^12 + 1 - 97,000,000) / 10^12 = 0.999903 of a base unit no position can
/// claim. Three credits keep back floor(2.999709) = 2: the remainder the close
/// shares out.
fn uneven_raise_paid_three_times(env: &mut Env) -> (Pubkey, Pubkey, Vec<(Keypair, Pubkey)>) {
    let (creator, creator_usdc) = env.user(10 * USD);
    let now = env.now();
    let params = RaiseParams {
        raise_id: 1,
        tier_count: 1,
        tiers: [TierInput { units: 1_200_000 * Q, price: USD }, TierInput::default(), TierInput::default()],
        // 25% of the creator's part at a full raise (1,090,909.090909 USD), rounded up.
        min_creator_part: 272_727_272_728,
        share_bps: 10_000,
        term_months: 24,
        deadline: now + 21 * DAY,
    };
    let raise = env.create_raise(&creator, &creator_usdc, params).unwrap();
    let mut buyers = Vec::new();
    for units in [100_000 * Q; 9].into_iter().chain([100_000 * Q + 1]) {
        // At 1.00 a quota, a unit costs one base unit.
        let (buyer, buyer_usdc) = env.user(units);
        env.buy(&raise, &buyer, &buyer_usdc, units).unwrap();
        buyers.push((buyer, buyer_usdc));
    }
    env.set_time(now + 21 * DAY);
    env.finalize(&raise, &creator_usdc).unwrap();
    env.mint_to(&creator_usdc, 300 * USD);
    for _ in 0..3 {
        env.fund_direct(&creator, &creator_usdc, &raise, 100 * USD).unwrap();
    }
    let state: Raise = env.fetch(&raise);
    assert_eq!(state.units_outstanding, UNEVEN_UNITS);
    assert_eq!((state.credited_total, state.acc_per_unit), (291_000_000, 3 * 96_999_999));
    (creator_usdc, raise, buyers)
}

#[test]
fn the_close_shares_the_rounding_remainder_with_the_holders() {
    let mut env = Env::new();
    let (creator_usdc, raise, buyers) = uneven_raise_paid_three_times(&mut env);
    env.declare_zero_months_to_term_end(&raise);
    let payouts = vault(PAYOUTS_SEED, &raise);
    let before = env.balance(&creator_usdc);
    let closed = env.close_raise_logged(&raise, &creator_usdc).unwrap();
    // Fans paid 1,000,000.000001 USD: the pool holds the 10 of 110, 90,909.090910,
    // and the floors kept back 2 base units; both go to the quotas fans hold.
    assert_eq!((closed.distributed, closed.to_creator), (90_909_090_910 + 2, 0));
    // The creator receives only the guarantee left.
    assert_eq!(closed.guarantee_returned, 45_454_545_454);
    assert_eq!(env.balance(&creator_usdc) - before, 45_454_545_454);
    assert_eq!(env.balance(&payouts), 291_000_000 + 90_909_090_910);
    // The accumulator ends at 290,999,997 + floor(90,909,090,912 x 10^12 / (10^12 + 1))
    // = 91,200,090,908: each holder, half claiming and half closing its position,
    // is paid floor(units x 91,200,090,908 / 10^12) = 9,120,009,090 from a vault
    // that always holds it, and 10 base units of floor dust stay behind.
    for (i, (holder, holder_usdc)) in buyers.iter().enumerate() {
        let in_vault = env.balance(&payouts);
        let held = env.balance(holder_usdc);
        if i % 2 == 0 {
            env.claim(&raise, &holder.pubkey(), holder_usdc).unwrap();
        } else {
            env.close_position(&raise, &holder.pubkey(), holder_usdc).unwrap();
        }
        let paid = env.balance(holder_usdc) - held;
        assert_eq!(paid, 9_120_009_090);
        assert!(paid <= in_vault, "holder {i} was paid {paid} from a vault holding {in_vault}");
        assert_eq!(env.balance(&payouts), in_vault - paid);
    }
    assert_eq!(env.balance(&payouts), 10);
}

#[test]
fn the_close_pays_the_rounding_remainder_to_the_creator_when_the_pool_holds_every_quota() {
    let mut env = Env::new();
    let (creator_usdc, raise, buyers) = uneven_raise_paid_three_times(&mut env);
    // Every fan sells everything to the pool, keeping what its quotas earned
    // before: floor(units x 290,999,997 / 10^12) = 29,099,999 each.
    for (seller, seller_usdc) in &buyers {
        let units = env.fetch::<Position>(&position_pda(&raise, &seller.pubkey())).units;
        env.sell_to_pool(&raise, seller, seller_usdc, units).unwrap();
    }
    assert_eq!(env.fetch::<Raise>(&raise).pool_units, UNEVEN_UNITS);
    env.declare_zero_months_to_term_end(&raise);
    let payouts = vault(PAYOUTS_SEED, &raise);
    // The pool bought its quotas after the payments: it earned nothing, and its cash is its counter.
    let pool_cash = env.balance(&vault(POOL_SEED, &raise));
    assert_eq!(pool_cash, env.fetch::<Raise>(&raise).pool_usdc);
    let guarantee = env.balance(&vault(GUARANTEE_SEED, &raise));
    let before = env.balance(&creator_usdc);
    let closed = env.close_raise_logged(&raise, &creator_usdc).unwrap();
    // No fan holds a quota: the pool's USDC and the 2 base units the floors kept
    // back go to the creator, with the guarantee left.
    let remainder = closed.to_creator - pool_cash;
    assert_eq!((remainder, closed.distributed), (2, 0));
    assert_eq!(env.balance(&creator_usdc) - before, pool_cash + guarantee + remainder);
    assert_eq!(env.balance(&payouts), 291_000_000 - remainder);
    // Each seller still collects the 29,099,999 it earned, half by a claim and
    // half by closing its empty position, from a vault that always holds it;
    // 8 base units of floor dust stay behind.
    for (i, (seller, seller_usdc)) in buyers.iter().enumerate() {
        let in_vault = env.balance(&payouts);
        let held = env.balance(seller_usdc);
        if i % 2 == 0 {
            env.claim(&raise, &seller.pubkey(), seller_usdc).unwrap();
        } else {
            env.close_position(&raise, &seller.pubkey(), seller_usdc).unwrap();
        }
        let paid = env.balance(seller_usdc) - held;
        assert_eq!(paid, 29_099_999);
        assert!(paid <= in_vault, "seller {i} was paid {paid} from a vault holding {in_vault}");
        assert_eq!(env.balance(&payouts), in_vault - paid);
    }
    assert_eq!(env.balance(&payouts), 8);
}
