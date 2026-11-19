//! The rent of a finished raise goes back to the sponsor (the 2026-10-21 design,
//! docs/solana/PEPPER-RAISE-PROGRAM.md §5 *The rent*). The sponsor of every test
//! raise is `env.admin`, which also pays each transaction's 5,000 lamports, except
//! where a test sends as another payer (`send_as`).

mod common;

use common::*;
use pepper_curve::raise::calendar::{declare_open_ts, due_ts};
use pepper_curve::raise::constants::*;
use pepper_curve::errors::FanexError;
use pepper_curve::raise::state::{Raise, RaiseState};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signer};

/// The rent of 127, 433 and 165 bytes (the account plus 128 bytes of overhead, 6,960 lamports a
/// byte): a `Month` (95 bytes until its 32 reserved ones of 2026-10-29), the `Raise`, and one of its
/// four vaults.
const MONTH_RENT: u64 = 1_774_800;
const RAISE_RENT: u64 = 3_904_560;
const VAULT_RENT: u64 = 2_039_280;
const FEE: u64 = 5_000;

/// Declares every month of the term with nothing due, then settles each in order:
/// (first month, last month). The clock ends on the last month's due date.
fn live_through_the_term(env: &mut Env, raise: &Pubkey) -> (u32, u32) {
    env.declare_zero_months_to_term_end(raise);
    let state: Raise = env.fetch(raise);
    for month in state.first_month..=state.term_end_month {
        env.settle(raise, month).unwrap();
    }
    (state.first_month, state.term_end_month)
}

fn vaults_of(raise: &Pubkey) -> [Pubkey; 4] {
    [vault(ESCROW_SEED, raise), vault(GUARANTEE_SEED, raise), vault(POOL_SEED, raise), vault(PAYOUTS_SEED, raise)]
}

/// Every fan's position, then the pool's own.
fn close_every_position(env: &mut Env, raise: &Pubkey, holders: &[(Keypair, Pubkey)]) {
    for (holder, holder_usdc) in holders {
        env.close_position(raise, &holder.pubkey(), holder_usdc).unwrap();
    }
    env.close_position(raise, raise, &vault(POOL_SEED, raise)).unwrap();
}

/// (`open_positions`, `live_months`) as the raise counts them.
fn counters(env: &Env, raise: &Pubkey) -> (u32, u16) {
    let state: Raise = env.fetch(raise);
    (state.open_positions, state.live_months)
}

/// A month's rent goes to the sponsor, to the lamport, while the raise is still active; only a
/// settled month leaves.
#[test]
fn a_finished_month_closes_during_the_term_and_its_rent_goes_back() {
    let mut env = Env::new();
    let (_, _, raise, _) = env.active_raise(1);
    env.declare_zero_months_to_term_end(&raise);
    let state: Raise = env.fetch(&raise);
    let sponsor = env.admin.pubkey();
    assert_eq!((state.rent_payer, state.open_positions, state.live_months), (sponsor, 11, 24));
    let first = state.first_month;
    // Declared and owing nothing, but not settled: the settlement still reads it.
    expect_err(env.close_month(&raise, first), code(FanexError::MonthNotClosable));
    env.settle(&raise, first).unwrap();
    let before = env.lamports(&sponsor);
    let closed = env.close_month_logged(&raise, first).unwrap();
    assert_eq!((closed.raise, closed.month, closed.lamports), (raise, first, MONTH_RENT));
    assert!(!env.exists(&month_pda(&raise, first)));
    assert_eq!(env.lamports(&sponsor), before + MONTH_RENT - FEE);
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.state, state.live_months), (RaiseState::Active, 23));
    // The settlement of the next month, and its close, need nothing from the closed one.
    env.settle(&raise, first + 1).unwrap();
    env.close_month(&raise, first + 1).unwrap();
    assert_eq!(counters(&env, &raise), (11, 22));
}

#[test]
fn a_finished_month_closes_after_the_raise_closed_too() {
    let mut env = Env::new();
    let (_, creator_usdc, raise, _) = env.active_raise(1);
    let (first, _) = live_through_the_term(&mut env, &raise);
    env.close_raise(&raise, &creator_usdc).unwrap();
    env.close_month(&raise, first).unwrap();
    assert!(!env.exists(&month_pda(&raise, first)));
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.state, state.live_months), (RaiseState::Closed, 23));
}

/// Review Focus 5: a month the settlement or the payment still reads is refused, and another raise's
/// month never closes through this raise.
#[test]
fn a_month_still_needed_is_refused() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, _) = env.active_raise(1);
    let sponsor = env.admin.pubkey();
    // November is paid in full before its due date: the pointer steps past it, but nothing settled it.
    env.pay_november(&creator, &creator_usdc, &raise, 100 * USD);
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.oldest_open_month, state.last_finalized_month), (NOVEMBER_2026 + 1, NOVEMBER_2026 - 1));
    expect_err(env.close_month(&raise, NOVEMBER_2026), code(FanexError::MonthNotClosable));
    // Accounts of close_month: caller 0, raise 1, month_account 2, rent_payer 3.
    let (_, _, other, _) = env.active_raise(2);
    let mut forged = env.close_month_ix(&raise, NOVEMBER_2026, &sponsor);
    forged.accounts[1].pubkey = other;
    expect_err(env.send(&[forged], &[]), code(FanexError::RaiseAccountMismatch));
    assert!(env.exists(&month_pda(&raise, NOVEMBER_2026)));
    // Settled, behind the pointer and at the last settled month: it closes.
    env.set_time(due_ts(NOVEMBER_2026));
    env.settle(&raise, NOVEMBER_2026).unwrap();
    env.close_month(&raise, NOVEMBER_2026).unwrap();
    assert!(!env.exists(&month_pda(&raise, NOVEMBER_2026)));
}

/// Review Focus 2: an arrear holds its month, the positions and the raise open; the creator can
/// still pay it after the close; once it is paid everything closes, and every holder is paid.
#[test]
fn arrears_stay_payable_and_hold_the_rent_until_paid() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, buyers) = env.active_raise(1);
    let state: Raise = env.fetch(&raise);
    let (first, last) = (state.first_month, state.term_end_month);
    assert_eq!(first, NOVEMBER_2026);
    // November owes 100 USD and is not paid; every later month owes nothing.
    for month in first..=last {
        env.set_time(declare_open_ts(month));
        env.declare(&raise, month, if month == NOVEMBER_2026 { 100 * USD } else { 0 }).unwrap();
    }
    env.set_time(due_ts(last));
    for month in first..=last {
        env.settle(&raise, month).unwrap();
    }
    // The guarantee covered 45,454,545 of November; 54,545,455 stay owed: it is the oldest open month.
    assert_eq!(env.fetch::<Raise>(&raise).oldest_open_month, NOVEMBER_2026);
    env.close_raise(&raise, &creator_usdc).unwrap();
    // Nothing closes while the arrear is open: no month is behind the pointer, and the raise is not finished.
    for month in [first, first + 1, last] {
        expect_err(env.close_month(&raise, month), code(FanexError::MonthNotClosable));
    }
    expect_err(env.close_raise_accounts(&raise), code(FanexError::RaiseNotFinished));
    // The creator still pays it, and only what it owes moves.
    env.mint_to(&creator_usdc, 100 * USD);
    let before = env.balance(&creator_usdc);
    env.fund_month(&creator, &creator_usdc, &raise, NOVEMBER_2026, 100 * USD).unwrap();
    assert_eq!(before - env.balance(&creator_usdc), 54_545_455);
    // Paid, November is behind the pointer and closes. The zero-due months after it are settled, but the
    // pointer sits on the first of them until the creator steps past each one (PEPPER-RAISE-PROGRAM.md §9).
    env.close_month(&raise, NOVEMBER_2026).unwrap();
    expect_err(env.close_month(&raise, first + 1), code(FanexError::MonthNotClosable));
    for month in first + 1..=last {
        env.fund_month(&creator, &creator_usdc, &raise, month, 1).unwrap();
        env.close_month(&raise, month).unwrap();
    }
    assert_eq!(counters(&env, &raise), (11, 0));
    // A holder of 100 quotas is paid a tenth of the guarantee, of the pool and of the late payment.
    close_every_position(&mut env, &raise, &buyers);
    assert_eq!(env.balance(&buyers[0].1), 18_790_909);
    env.close_raise_accounts(&raise).unwrap();
    assert!(!env.exists(&raise));
}

/// Review Focus 1 and 3: a month left behind refuses the close; the dust a vault holds goes to the fee
/// wallet in the same instruction; every lamport goes to the recorded payer and nowhere else.
#[test]
fn the_raise_closes_after_a_whole_life_and_every_lamport_goes_back() {
    let mut env = Env::new();
    let (_, creator_usdc, raise, buyers) = env.active_raise(1);
    let (first, last) = live_through_the_term(&mut env, &raise);
    let sponsor = env.admin.pubkey();
    // Five months close during the term, the rest after the close.
    for month in first..first + 5 {
        env.close_month(&raise, month).unwrap();
    }
    assert_eq!(counters(&env, &raise), (11, 19));
    env.close_raise(&raise, &creator_usdc).unwrap();
    expect_err(env.close_raise_accounts(&raise), code(FanexError::RaiseNotFinished));
    for month in first + 5..last {
        env.close_month(&raise, month).unwrap();
    }
    // Every position closed, one month left: not finished.
    close_every_position(&mut env, &raise, &buyers);
    assert_eq!(counters(&env, &raise), (0, 1));
    expect_err(env.close_raise_accounts(&raise), code(FanexError::RaiseNotFinished));
    assert!(env.exists(&raise));
    env.close_month(&raise, last).unwrap();
    // 7 base units sent straight to the escrow and 3 to the pool vault, after `close_raise` emptied it.
    env.mint_to(&vault(ESCROW_SEED, &raise), 7);
    env.mint_to(&vault(POOL_SEED, &raise), 3);
    let leftover: u64 = vaults_of(&raise).iter().map(|v| env.balance(v)).sum();
    assert!(leftover >= 10);
    let rent_before = env.lamports(&raise) + vaults_of(&raise).iter().map(|v| env.lamports(v)).sum::<u64>();
    assert_eq!(rent_before, RAISE_RENT + 4 * VAULT_RENT);
    let (fee_before, sponsor_before) = (env.balance(&env.fee_usdc), env.lamports(&sponsor));
    let closed = env.close_raise_accounts_logged(&raise).unwrap();
    assert_eq!((closed.raise, closed.leftover_usdc, closed.lamports), (raise, leftover, rent_before));
    assert_eq!(env.balance(&env.fee_usdc) - fee_before, leftover);
    assert!(!env.exists(&raise));
    for account in vaults_of(&raise) {
        assert!(!env.exists(&account));
    }
    assert_eq!(env.lamports(&sponsor), sponsor_before + rent_before - FEE);
}

/// Review Focus 1, the other side: a raise under its minimum keeps the pool's own position
/// (`finalize` opens it whatever the outcome), and no month was ever declared.
#[test]
fn a_failed_raise_closes_once_the_pools_position_is_closed_too() {
    let mut env = Env::new();
    let (_, creator_usdc, raise) = env.launch(1);
    let sponsor = env.admin.pubkey();
    let (buyer, buyer_usdc) = env.user(100 * USD);
    env.buy(&raise, &buyer, &buyer_usdc, 100 * USD).unwrap();
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.rent_payer, state.open_positions), (sponsor, 1));
    // Still raising: never.
    expect_err(env.close_raise_accounts(&raise), code(FanexError::RaiseNotFinished));
    env.set_time(state.deadline);
    env.finalize(&raise, &creator_usdc).unwrap();
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.state, state.open_positions), (RaiseState::Refunding, 2));
    env.refund(&raise, &buyer.pubkey(), &buyer_usdc).unwrap();
    assert_eq!(counters(&env, &raise), (1, 0));
    // Only the pool's position is left.
    expect_err(env.close_raise_accounts(&raise), code(FanexError::RaiseNotFinished));
    env.close_position(&raise, &raise, &vault(POOL_SEED, &raise)).unwrap();
    assert_eq!(counters(&env, &raise), (0, 0));
    env.mint_to(&vault(ESCROW_SEED, &raise), 5);
    let fee_before = env.balance(&env.fee_usdc);
    let closed = env.close_raise_accounts_logged(&raise).unwrap();
    assert_eq!((closed.leftover_usdc, closed.lamports), (5, RAISE_RENT + 4 * VAULT_RENT));
    assert_eq!(env.balance(&env.fee_usdc) - fee_before, 5);
    assert!(!env.exists(&raise));
}

/// A cancelled raise never reaches `finalize` (it needs `Raising`), so it never has a pool
/// position: its last refund is its last account.
#[test]
fn a_cancelled_raise_closes_after_its_refunds() {
    let mut env = Env::new();
    let (creator, _, raise) = env.launch(1);
    let (first, first_usdc) = env.user(100 * USD);
    let (second, second_usdc) = env.user(100 * USD);
    env.buy(&raise, &first, &first_usdc, 100 * USD).unwrap();
    env.buy(&raise, &second, &second_usdc, 100 * USD).unwrap();
    env.cancel(&creator, &raise).unwrap();
    assert_eq!(counters(&env, &raise), (2, 0));
    env.refund(&raise, &first.pubkey(), &first_usdc).unwrap();
    expect_err(env.close_raise_accounts(&raise), code(FanexError::RaiseNotFinished));
    env.refund(&raise, &second.pubkey(), &second_usdc).unwrap();
    assert_eq!(counters(&env, &raise), (0, 0));
    let closed = env.close_raise_accounts_logged(&raise).unwrap();
    assert_eq!((closed.leftover_usdc, closed.lamports), (0, RAISE_RENT + 4 * VAULT_RENT));
    assert!(!env.exists(&raise));
    assert_eq!((env.balance(&first_usdc), env.balance(&second_usdc)), (100 * USD, 100 * USD));
}

/// Review Focus 4: a raise written before the upgrade has no rent payer and counters that never
/// counted. Its positions and months are left alone, and both instructions refuse it.
#[test]
fn a_raise_from_before_the_upgrade_keeps_its_rent() {
    let mut env = Env::new();
    let (creator, _, raise) = env.launch(1);
    let (first, first_usdc) = env.user(100 * USD);
    env.buy(&raise, &first, &first_usdc, 100 * USD).unwrap();
    env.forget_rent_payer(&raise);
    let (second, second_usdc) = env.user(100 * USD);
    env.buy(&raise, &second, &second_usdc, 100 * USD).unwrap();
    assert_eq!(counters(&env, &raise), (0, 0));
    env.cancel(&creator, &raise).unwrap();
    // Refunds close positions that were never counted: no underflow.
    env.refund(&raise, &first.pubkey(), &first_usdc).unwrap();
    env.refund(&raise, &second.pubkey(), &second_usdc).unwrap();
    let state: Raise = env.fetch(&raise);
    assert_eq!((state.rent_payer, state.open_positions, state.live_months), (Pubkey::default(), 0, 0));
    expect_err(env.close_raise_accounts(&raise), code(FanexError::RentPayerUnknown));
    // A cluster demotes the zero payer (the system program's key) to read-only, which LiteSVM does
    // not (it sanitizes with no reserved keys): the refusal still names the raise, not the payer's `mut`.
    let mut demoted = env.close_raise_accounts_ix(&raise, &Pubkey::default());
    demoted.accounts[4].is_writable = false;
    expect_err(env.send(&[demoted], &[]), code(FanexError::RentPayerUnknown));
    assert!(env.exists(&raise));
    // A settled month of such a raise stays too.
    let (_, _, old, _) = env.active_raise(2);
    let first_month = env.fetch::<Raise>(&old).first_month;
    env.set_time(declare_open_ts(first_month));
    env.declare(&old, first_month, 0).unwrap();
    env.set_time(due_ts(first_month));
    env.settle(&old, first_month).unwrap();
    env.forget_rent_payer(&old);
    expect_err(env.close_month(&old, first_month), code(FanexError::RentPayerUnknown));
    let mut demoted = env.close_month_ix(&old, first_month, &Pubkey::default());
    demoted.accounts[3].is_writable = false;
    expect_err(env.send(&[demoted], &[]), code(FanexError::RentPayerUnknown));
    assert!(env.exists(&month_pda(&old, first_month)));
}

#[test]
fn the_counters_follow_the_accounts_that_exist() {
    let mut env = Env::new();
    let (_, creator_usdc, raise) = env.launch(1);
    let (a, a_usdc) = env.user(100 * USD);
    env.buy(&raise, &a, &a_usdc, 50 * USD).unwrap();
    // The second purchase finds the position: nothing is counted.
    env.buy(&raise, &a, &a_usdc, 50 * USD).unwrap();
    assert_eq!(counters(&env, &raise), (1, 0));
    for _ in 0..9 {
        let (buyer, buyer_usdc) = env.user(100 * USD);
        env.buy(&raise, &buyer, &buyer_usdc, 100 * USD).unwrap();
    }
    assert_eq!(counters(&env, &raise), (10, 0));
    // `finalize` opens the pool's position.
    env.finalize(&raise, &creator_usdc).unwrap();
    assert_eq!(counters(&env, &raise), (11, 0));
    let first = env.fetch::<Raise>(&raise).first_month;
    env.set_time(declare_open_ts(first));
    env.declare(&raise, first, 0).unwrap();
    assert_eq!(counters(&env, &raise), (11, 1));
    // A sale empties a position without closing it; closing it takes one off.
    env.sell_to_pool(&raise, &a, &a_usdc, 100 * Q).unwrap();
    assert_eq!(counters(&env, &raise), (11, 1));
    env.close_position(&raise, &a.pubkey(), &a_usdc).unwrap();
    assert_eq!(counters(&env, &raise), (10, 1));
    // A newcomer buying from the pool opens one; buying again finds it.
    let (c, c_usdc) = env.user(10 * USD);
    env.buy_from_pool(&raise, &c, &c_usdc, Q, 5 * USD).unwrap();
    assert_eq!(counters(&env, &raise), (11, 1));
    env.buy_from_pool(&raise, &c, &c_usdc, Q, 5 * USD).unwrap();
    assert_eq!(counters(&env, &raise), (11, 1));
}

#[test]
fn the_rent_goes_only_to_the_recorded_payer() {
    let mut env = Env::new();
    let (_, _, raise, _) = env.active_raise(1);
    let (first, _) = live_through_the_term(&mut env, &raise);
    let (thief, thief_usdc) = env.user(0);
    let thief_before = env.lamports(&thief.pubkey());
    let (_, _, other, _) = env.active_raise(2);
    let sponsor = env.admin.pubkey();
    let month_rent = env.lamports(&month_pda(&raise, first));
    // Constraints are read before the handler: each of these is refused whatever the raise's state.
    let forged = env.close_month_ix(&raise, first, &thief.pubkey());
    expect_err(env.send(&[forged], &[]), code(FanexError::RaiseAccountMismatch));
    let forged = env.close_raise_accounts_ix(&raise, &thief.pubkey());
    expect_err(env.send(&[forged], &[]), code(FanexError::RaiseAccountMismatch));
    // Accounts of close_raise_accounts: caller 0, config 1, usdc_mint 2, raise 3, rent_payer 4, escrow 5,
    // guarantee 6, pool 7, payouts 8, fee_usdc 9, token_program 10. The leftover goes only to the fee wallet.
    let mut forged = env.close_raise_accounts_ix(&raise, &sponsor);
    forged.accounts[9].pubkey = thief_usdc;
    expect_err(env.send(&[forged], &[]), code(FanexError::RaiseAccountMismatch));
    let mut forged = env.close_raise_accounts_ix(&raise, &sponsor);
    forged.accounts[5].pubkey = vault(ESCROW_SEED, &other);
    expect_err(env.send(&[forged], &[]), anchor_lang::error::ErrorCode::ConstraintSeeds as u32);
    assert_eq!(env.lamports(&thief.pubkey()), thief_before);
    assert_eq!(env.lamports(&month_pda(&raise, first)), month_rent);
    assert!(env.exists(&raise));
}

/// The rent never follows the caller or the fee payer: a stranger who pays for and signs both closes
/// is out exactly its fees, and the sponsor, which signs and pays for neither, gains every lamport.
#[test]
fn a_stranger_who_closes_gains_nothing_and_the_sponsor_gets_every_lamport() {
    let mut env = Env::new();
    let (_, creator_usdc, raise, buyers) = env.active_raise(1);
    let (first, last) = live_through_the_term(&mut env, &raise);
    let sponsor = env.admin.pubkey();
    let stranger = Keypair::new();
    env.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    // A month, closed by the stranger as caller and fee payer.
    let close = env.close_month_ix_by(&stranger.pubkey(), &raise, first, &sponsor);
    let (stranger_before, sponsor_before) = (env.lamports(&stranger.pubkey()), env.lamports(&sponsor));
    env.send_as(&stranger, &[close], &[]).unwrap();
    assert!(!env.exists(&month_pda(&raise, first)));
    assert_eq!(env.lamports(&stranger.pubkey()), stranger_before - FEE);
    assert_eq!(env.lamports(&sponsor), sponsor_before + MONTH_RENT);
    // The rest of the whole life, sent by the sponsor: the close, the other months, every position.
    env.close_raise(&raise, &creator_usdc).unwrap();
    for month in first + 1..=last {
        env.close_month(&raise, month).unwrap();
    }
    close_every_position(&mut env, &raise, &buyers);
    assert_eq!(counters(&env, &raise), (0, 0));
    // The raise's accounts, closed by the stranger as caller and fee payer.
    let close = env.close_raise_accounts_ix_by(&stranger.pubkey(), &raise, &sponsor);
    let (stranger_before, sponsor_before) = (env.lamports(&stranger.pubkey()), env.lamports(&sponsor));
    env.send_as(&stranger, &[close], &[]).unwrap();
    assert!(!env.exists(&raise));
    for account in vaults_of(&raise) {
        assert!(!env.exists(&account));
    }
    assert_eq!(env.lamports(&stranger.pubkey()), stranger_before - FEE);
    assert_eq!(env.lamports(&sponsor), sponsor_before + RAISE_RENT + 4 * VAULT_RENT);
}

/// The pause stops what opens exposure; closing a finished account opens none.
#[test]
fn neither_instruction_stops_under_the_pause() {
    let mut env = Env::new();
    let (_, _, raise, _) = env.active_raise(1);
    let (first, _) = live_through_the_term(&mut env, &raise);
    let (_, creator_usdc, failed) = env.launch(2);
    let (buyer, buyer_usdc) = env.user(100 * USD);
    env.buy(&failed, &buyer, &buyer_usdc, 100 * USD).unwrap();
    env.set_paused(true).unwrap();
    env.close_month(&raise, first).unwrap();
    let deadline = env.fetch::<Raise>(&failed).deadline;
    env.set_time(deadline);
    env.finalize(&failed, &creator_usdc).unwrap();
    env.refund(&failed, &buyer.pubkey(), &buyer_usdc).unwrap();
    env.close_position(&failed, &failed, &vault(POOL_SEED, &failed)).unwrap();
    env.close_raise_accounts(&failed).unwrap();
    assert!(!env.exists(&failed));
}
