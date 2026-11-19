mod common;
use common::*;
use pepper_curve::raise::calendar::{declare_open_ts, due_ts};
use pepper_curve::raise::constants::*;
use pepper_curve::errors::FanexError;
use pepper_curve::raise::state::{Month, Position, Raise};
use solana_sdk::{pubkey::Pubkey, signature::Signer};

fn write_off(env: &mut Env, raise: &Pubkey, month: u32) -> Result<pepper_curve::raise::events::MonthWrittenOff, solana_sdk::transaction::TransactionError> {
    let instruction = ix(pepper_curve::accounts::WriteOffMonth {
        caller: env.admin.pubkey(), raise: *raise, month_account: month_pda(raise, month),
    }, pepper_curve::instruction::WriteOffMonth {});
    env.send_logged(&[instruction], &[]).map(|logs| event(&logs))
}

/// M-01: a real payment earned before all fans sell must survive the later write-off and account close.
#[test]
fn all_quotas_sold_arrears_extinguished_pending_paid_and_rent_returned() {
    close_no_holder_raise(USD);
}

#[test]
fn the_original_no_payout_audit_scenario_also_finishes() {
    close_no_holder_raise(0);
}

fn close_no_holder_raise(prior_payment: u64) {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, buyers) = env.active_raise(1);
    // Credit less than MIN_CLAIM to each fan, then sell every quota. These tiny pending amounts survive.
    env.set_time(declare_open_ts(NOVEMBER_2026));
    env.declare(&raise, NOVEMBER_2026, 100 * USD).unwrap();
    if prior_payment > 0 {
        env.mint_to(&creator_usdc, prior_payment);
        env.fund_month(&creator, &creator_usdc, &raise, NOVEMBER_2026, prior_payment).unwrap();
    }
    for (seller, seller_usdc) in &buyers {
        env.sell_to_pool(&raise, seller, seller_usdc, 100 * Q).unwrap();
    }
    let before: Vec<u64> = buyers.iter().map(|(seller, _)| env.fetch::<Position>(&position_pda(&raise, &seller.pubkey())).pending).collect();
    assert!(before.iter().all(|pending| *pending < MIN_CLAIM && (*pending > 0) == (prior_payment > 0)));
    let state: Raise = env.fetch(&raise);
    let (first, last) = (state.first_month, state.term_end_month);
    // November was declared by pay_november. It has only a tiny payment; every following month owes zero.
    for month in first + 1..=last {
        env.set_time(declare_open_ts(month));
        env.declare(&raise, month, 0).unwrap();
    }
    env.set_time(due_ts(last));
    for month in first..=last { env.settle(&raise, month).unwrap(); }
    env.close_raise(&raise, &creator_usdc).unwrap();
    env.set_paused(true).unwrap();
    assert_eq!(env.fetch::<Raise>(&raise).units_outstanding, 0);
    // The old positive-payment path is deliberately still guarded against division by zero.
    env.mint_to(&creator_usdc, 100 * USD);
    expect_err(env.fund_month(&creator, &creator_usdc, &raise, first, 100 * USD), code(FanexError::RaiseZeroAmount));
    let owed: Month = env.fetch(&month_pda(&raise, first));
    let vault_before = env.balance(&vault(PAYOUTS_SEED, &raise));
    let creator_before = env.balance(&creator_usdc);
    let (stranger, _) = env.user(0);
    for month in first..=last {
        // A stranger can retire the debt even under the global pause; no privileged signer is needed.
        let instruction = ix(pepper_curve::accounts::WriteOffMonth {
            caller: stranger.pubkey(), raise, month_account: month_pda(&raise, month),
        }, pepper_curve::instruction::WriteOffMonth {});
        let logs = env.send_logged(&[instruction], &[&stranger]).unwrap();
        let written: pepper_curve::raise::events::MonthWrittenOff = event(&logs);
        if month == first {
            assert_eq!(written.written_off, owed.due - owed.paid - owed.covered);
            assert!(written.written_off > 0);
            assert_eq!((written.paid, written.covered), (owed.paid, owed.covered));
        } else { assert_eq!(written.written_off, 0); }
        // Repeating the retirement cannot advance a second time.
        assert!(write_off(&mut env, &raise, month).is_err());
        env.close_month(&raise, month).unwrap();
    }
    assert_eq!(env.balance(&creator_usdc), creator_before);
    assert_eq!(env.balance(&vault(PAYOUTS_SEED, &raise)), vault_before);
    assert_eq!(env.fetch::<Raise>(&raise).written_off_months, (1u64 << 24) - 1);
    for ((seller, seller_usdc), pending) in buyers.iter().zip(before) {
        let balance = env.balance(seller_usdc);
        env.close_position(&raise, &seller.pubkey(), seller_usdc).unwrap();
        assert_eq!(env.balance(seller_usdc) - balance, pending);
    }
    env.close_position(&raise, &raise, &vault(POOL_SEED, &raise)).unwrap();
    let rent_before = env.lamports(&env.admin.pubkey());
    env.close_raise_accounts(&raise).unwrap();
    assert!(!env.exists(&raise));
    assert!(env.lamports(&env.admin.pubkey()) > rent_before);
}

#[test]
fn a_holder_or_active_raise_prevents_write_off_and_other_raises_months_are_refused() {
    let mut env = Env::new();
    let (_, creator_usdc, raise, _) = env.active_raise(1);
    let state: Raise = env.fetch(&raise);
    for month in state.first_month..=state.term_end_month {
        env.set_time(declare_open_ts(month));
        env.declare(&raise, month, if month == state.first_month { 100 * USD } else { 0 }).unwrap();
    }
    env.set_time(due_ts(state.term_end_month));
    for month in state.first_month..=state.term_end_month { env.settle(&raise, month).unwrap(); }
    assert!(write_off(&mut env, &raise, state.first_month).is_err());
    env.close_raise(&raise, &creator_usdc).unwrap();
    assert!(write_off(&mut env, &raise, state.first_month).is_err());
    let (_, _, other, _) = env.active_raise(2);
    let forged = ix(pepper_curve::accounts::WriteOffMonth { caller: env.admin.pubkey(), raise: other,
        month_account: month_pda(&raise, state.first_month) }, pepper_curve::instruction::WriteOffMonth {});
    expect_err(env.send(&[forged], &[]), code(FanexError::RaiseAccountMismatch));
}
