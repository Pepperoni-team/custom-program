//! The reserve (docs/superpowers/specs/2026-11-07-dividend-reserve-design.md): a creator's USDC
//! deposited once into the token's reserve, a PDA of this program, then each month funded from it by
//! the approver and the dividend payer, the payer fronting what it lacks, Pepper's 3% from what is
//! left; what nobody claimed back to it at the close; a withdrawal in two steps, 45 days apart, signed
//! by its owner alone. Every refusal is read by its code.

mod common;

use anchor_lang::error::ErrorCode;
use anchor_lang::Space;
use anchor_spl::token::spl_token;
use common::*;
use pepper_dividends::constants::{MONTH_STATUS_OPEN, MONTH_STATUS_VOIDED, RESERVE_WITHDRAWAL_DELAY_SECONDS, REVIEW_WINDOW_SECONDS};
use pepper_dividends::errors::DividendsError;
use pepper_dividends::events::{
    ReserveDeposited, ReserveWithdrawalCancelled, ReserveWithdrawalRequested, ReserveWithdrawn, UsdcDistributorClosed,
    UsdcDistributorFrontedReturned, UsdcDistributorFundedFromReserve, UsdcDistributorVoided,
};
use pepper_dividends::state::{DividendReserve, MonthRecord, UsdcDistributor};
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};

/// October 2026, whose holders are fixed at 00:01 UTC on 1 November.
const OCTOBER: i64 = 202_610;
/// A token account's size: a reserve's vault.
const TOKEN_ACCOUNT_LEN: usize = 165;
/// A reserve's size, the discriminator included.
const RESERVE_LEN: usize = 146;

/// A month of `amounts` (one holder each) of `env.mint`'s `month`: its holders and its tree.
fn month_of(env: &Env, month: i64, amounts: &[u64]) -> Month {
    let holders: Vec<(Keypair, u64)> = amounts.iter().map(|amount| (Keypair::new(), *amount)).collect();
    let distributor = distributor_pda(&env.mint, month);
    let tree = build_tree(&dividend_leaves(&distributor, &env.usdc.mint, &holders));
    let total = amounts.iter().sum();
    Month { mint: env.mint, month, distributor, vault: env.usdc_ata(&distributor), holders, tree, total }
}

/// The config, `pepper_curve`'s USDC config, and the creator's reserve holding `deposit` µUSDC,
/// deposited through the instruction. Answers the fee account.
fn with_reserve(env: &mut Env, deposit: u64) -> Pubkey {
    env.write_config();
    let fee_account = env.put_usdc_config();
    let creator = env.creator.insecure_clone();
    env.usdc_holding(&creator.pubkey(), deposit);
    env.deposit(&creator, deposit).unwrap();
    fee_account
}

fn reserve(env: &Env) -> DividendReserve {
    env.fetch(&reserve_pda(&env.mint))
}

fn reserve_balance(env: &Env) -> u64 {
    env.token_amount(&env.reserve_vault(&env.mint))
}

// ── deposit_reserve ──────────────────────────────────────────────────────────

#[test]
fn a_creator_deposits_and_the_payer_pays_the_rents() {
    let mut env = Env::new();
    env.write_config();
    let creator = env.creator.insecure_clone();
    let creator_usdc = env.usdc_holding(&creator.pubkey(), 1_000 * USDC);
    let (creator_lamports, payer_lamports) = (env.lamports(&creator.pubkey()), env.lamports(&env.payer.pubkey()));
    let sent = env.deposit(&creator, 600 * USDC).unwrap();
    // The reserve is born the creator's, its vault holding the deposit.
    let r = reserve(&env);
    assert_eq!((r.version, r.mint, r.owner, r.withdrawal_requested_at), (1, env.mint, creator.pubkey(), 0));
    assert_eq!(r.bump, Pubkey::find_program_address(&[b"reserve", env.mint.as_ref()], &pepper_dividends::ID).1);
    assert_eq!(env.data_len(&reserve_pda(&env.mint)), RESERVE_LEN);
    assert_eq!(reserve_balance(&env), 600 * USDC);
    assert_eq!(env.token_amount(&creator_usdc), 400 * USDC);
    // The creator spends only USDC: the payer pays both rents and the fee.
    assert_eq!(env.lamports(&creator.pubkey()), creator_lamports);
    assert_eq!(
        payer_lamports - env.lamports(&env.payer.pubkey()),
        env.rent(RESERVE_LEN) + env.rent(TOKEN_ACCOUNT_LEN) + 2 * FEE_PER_SIGNATURE
    );
    let ev = event::<ReserveDeposited>(&sent.logs);
    assert_eq!((ev.mint, ev.owner, ev.amount, ev.balance), (env.mint, creator.pubkey(), 600 * USDC, 600 * USDC));
    // A second deposit adds to it, no rent paid again.
    let payer_lamports = env.lamports(&env.payer.pubkey());
    let sent = env.deposit(&creator, 400 * USDC).unwrap();
    assert_eq!(reserve_balance(&env), 1_000 * USDC);
    assert_eq!(payer_lamports - env.lamports(&env.payer.pubkey()), 2 * FEE_PER_SIGNATURE);
    assert_eq!(event::<ReserveDeposited>(&sent.logs).balance, 1_000 * USDC);
}

#[test]
fn only_the_curves_creator_deposits() {
    let mut env = Env::new();
    env.write_config();
    let stranger = env.funded_wallet(SOL);
    env.usdc_holding(&stranger.pubkey(), 10 * USDC);
    expect_err(env.deposit(&stranger, 10 * USDC), code(DividendsError::NotTheCreator));
    assert!(!env.exists(&reserve_pda(&env.mint)));
    // A curve that is no curve of pepper_curve's: unreadable, never «no creator».
    let creator = env.creator.insecure_clone();
    env.usdc_holding(&creator.pubkey(), 10 * USDC);
    env.put(curve_pda(&env.mint), vec![0u8; 196], Pubkey::new_unique());
    expect_err(env.deposit(&creator, 10 * USDC), code(DividendsError::CurveUnreadable));
}

#[test]
fn a_deposit_is_refused_paused_empty_or_with_another_payer_account_or_mint() {
    let mut env = Env::new();
    let mut config = env.write_config();
    let creator = env.creator.insecure_clone();
    env.usdc_holding(&creator.pubkey(), 10 * USDC);
    expect_err(env.deposit(&creator, 0), code(DividendsError::ZeroAmount));
    // Another rent payer than the config's.
    let stranger = env.funded_wallet(SOL);
    let mut deposit = env.deposit_ix(&creator.pubkey(), &env.mint, USDC);
    deposit.accounts[1].pubkey = stranger.pubkey();
    expect_err(env.send_paid_by(&stranger, &[deposit], &[&creator]), code(DividendsError::Unauthorized));
    // Another USDC account than the creator's own.
    let other_usdc = env.usdc_holding(&stranger.pubkey(), 10 * USDC);
    let mut deposit = env.deposit_ix(&creator.pubkey(), &env.mint, USDC);
    deposit.accounts[7].pubkey = other_usdc;
    let payer = env.payer.insecure_clone();
    expect_err(env.send_paid_by(&payer, &[deposit], &[&creator]), code(DividendsError::AccountMismatch));
    // Paused: nothing deposited.
    config.paused = true;
    env.put_config(&config);
    expect_err(env.deposit(&creator, USDC), code(DividendsError::Paused));
    assert!(!env.exists(&reserve_pda(&env.mint)));
}

#[test]
fn a_reserve_holding_anothers_money_is_never_taken_over() {
    let mut env = Env::new();
    with_reserve(&mut env, 50 * USDC);
    let (old, new) = (env.creator.insecure_clone(), env.funded_wallet(SOL));
    // pepper_curve hands the curve to another creator.
    env.put_curve(env.mint, new.pubkey());
    env.usdc_holding(&new.pubkey(), 10 * USDC);
    env.usdc_holding(&old.pubkey(), 10 * USDC);
    expect_err(env.deposit(&new, 10 * USDC), code(DividendsError::ReserveOwnedByAnother));
    // The old owner deposits no more, and still takes its money back.
    expect_err(env.deposit(&old, 10 * USDC), code(DividendsError::NotTheCreator));
    let request = env.request_withdrawal_ix(&old.pubkey(), &env.mint);
    env.send_paid_by(&old, &[request], &[]).unwrap();
    let now = env.now();
    env.set_time(now + RESERVE_WITHDRAWAL_DELAY_SECONDS);
    let withdraw = env.withdraw_ix(&old.pubkey(), &env.mint);
    env.send_paid_by(&old, &[withdraw], &[]).unwrap();
    assert_eq!(env.token_amount(&env.usdc_ata(&old.pubkey())), 60 * USDC);
    // Empty, the reserve passes to the creator of the day.
    env.deposit(&new, 10 * USDC).unwrap();
    assert_eq!(reserve(&env).owner, new.pubkey());
    assert_eq!(reserve_balance(&env), 10 * USDC);
}

// ── fund_month_from_reserve ──────────────────────────────────────────────────

#[test]
fn a_month_the_reserve_covers_is_paid_from_it_with_peppers_fee() {
    let mut env = Env::new();
    let fee_account = with_reserve(&mut env, 200 * USDC);
    let month = month_of(&env, MONTH, &[60 * USDC, 37 * USDC]);
    // The payer holds no USDC account: a month the reserve covers never reads one.
    assert!(!env.exists(&env.usdc_ata(&env.payer.pubkey())));
    let funding = env.reserve_funding(MONTH, month.tree.root, month.total, 2);
    let sent = env.fund_from_reserve(&funding).unwrap();
    assert_eq!(env.token_amount(&month.vault), 97 * USDC);
    assert_eq!(env.token_amount(&fee_account), 3 * USDC);
    assert_eq!(reserve_balance(&env), 100 * USDC);
    let d: UsdcDistributor = env.fetch(&month.distributor);
    assert_eq!((d.funder, d.funder_usdc_account, d.rent_payer), (reserve_pda(&env.mint), env.reserve_vault(&env.mint), env.payer.pubkey()));
    assert_eq!((d.total_amount, d.fronted_amount, d.fee_unpaid), (97 * USDC, 0, 0));
    assert_eq!((d.funded_by_fanex, d.rents_by_payer), (false, true));
    assert_eq!((d.claims_held_until, d.leaf_count), (env.now() + REVIEW_WINDOW_SECONDS, 2));
    let record: MonthRecord = env.fetch(&month_record_pda(&env.mint, MONTH));
    assert_eq!((record.status, record.distributor), (MONTH_STATUS_OPEN, month.distributor));
    let ev = event::<UsdcDistributorFundedFromReserve>(&sent.logs);
    assert_eq!(
        (ev.mint, ev.month, ev.total_amount, ev.from_reserve, ev.fronted, ev.fee_from_reserve, ev.fee_unpaid),
        (env.mint, MONTH, 97 * USDC, 97 * USDC, 0, 3 * USDC, 0)
    );
    // The holders claim as from any month once its review window has passed; what one never claims
    // goes back to the reserve at the close, by the closer, since a PDA never signs.
    env.past_review_window();
    env.usdc_account(&month.holders[0].0.pubkey());
    let claim = env.claim_ix(&month, 0);
    let sponsor = env.sponsor.insecure_clone();
    env.send_paid_by(&sponsor, &[claim], &[]).unwrap();
    assert_eq!(env.token_amount(&env.usdc_ata(&month.holders[0].0.pubkey())), 60 * USDC);
    env.set_time(d.expires_at);
    let closer = env.closer.insecure_clone();
    let close = env.close_ix(&closer.pubkey(), MONTH);
    let sent = env.send_logged_paid_by(&closer, &[close], &[]).unwrap();
    assert_eq!(reserve_balance(&env), 137 * USDC);
    let closed = event::<UsdcDistributorClosed>(&sent.logs);
    assert_eq!((closed.returned, closed.to), (37 * USDC, env.reserve_vault(&env.mint)));
}

#[test]
fn a_month_the_reserve_cannot_cover_is_fronted_by_the_payer_and_its_fee_owed() {
    let mut env = Env::new();
    let fee_account = with_reserve(&mut env, 90 * USDC);
    let payer_usdc = env.usdc_holding(&env.payer.pubkey(), 50 * USDC);
    let month = month_of(&env, MONTH, &[60 * USDC, 37 * USDC]);
    let funding = env.reserve_funding(MONTH, month.tree.root, month.total, 2);
    let sent = env.fund_from_reserve(&funding).unwrap();
    // The holders get the whole month: 90 from the reserve, 7 fronted.
    assert_eq!(env.token_amount(&month.vault), 97 * USDC);
    assert_eq!((reserve_balance(&env), env.token_amount(&payer_usdc), env.token_amount(&fee_account)), (0, 43 * USDC, 0));
    let d: UsdcDistributor = env.fetch(&month.distributor);
    assert_eq!((d.fronted_amount, d.fee_unpaid), (7 * USDC, 3 * USDC));
    let ev = event::<UsdcDistributorFundedFromReserve>(&sent.logs);
    assert_eq!((ev.from_reserve, ev.fronted, ev.fee_from_reserve, ev.fee_unpaid), (90 * USDC, 7 * USDC, 0, 3 * USDC));
    // A payer that cannot front its part funds nothing.
    let mut env = Env::new();
    with_reserve(&mut env, 90 * USDC);
    env.usdc_holding(&env.payer.pubkey(), 6 * USDC);
    let month = month_of(&env, MONTH, &[60 * USDC, 37 * USDC]);
    let funding = env.reserve_funding(MONTH, month.tree.root, month.total, 2);
    assert!(env.fund_from_reserve(&funding).is_err());
    assert_eq!(reserve_balance(&env), 90 * USDC);
    assert!(!env.exists(&month.distributor));
}

#[test]
fn the_fee_comes_from_what_the_reserve_has_left_after_the_holders() {
    let mut env = Env::new();
    let fee_account = with_reserve(&mut env, 98 * USDC);
    let month = month_of(&env, MONTH, &[97 * USDC]);
    let funding = env.reserve_funding(MONTH, month.tree.root, month.total, 1);
    env.fund_from_reserve(&funding).unwrap();
    assert_eq!((env.token_amount(&month.vault), env.token_amount(&fee_account), reserve_balance(&env)), (97 * USDC, USDC, 0));
    let d: UsdcDistributor = env.fetch(&month.distributor);
    assert_eq!((d.fronted_amount, d.fee_unpaid), (0, 2 * USDC));
}

#[test]
fn no_reserve_or_an_empty_one_funds_nothing() {
    let mut env = Env::new();
    env.write_config();
    env.put_usdc_config();
    let month = month_of(&env, MONTH, &[USDC]);
    let funding = env.reserve_funding(MONTH, month.tree.root, month.total, 1);
    expect_err(env.fund_from_reserve(&funding), anchor_code(ErrorCode::AccountNotInitialized));
    env.put_reserve(env.mint, env.creator.pubkey(), 0, 0);
    expect_err(env.fund_from_reserve(&funding), code(DividendsError::ReserveEmpty));
    assert!(!env.exists(&month.distributor));
}

#[test]
fn one_key_alone_opens_no_month_from_a_reserve() {
    let mut env = Env::new();
    with_reserve(&mut env, 200 * USDC);
    let month = month_of(&env, MONTH, &[USDC]);
    let base = env.reserve_funding(MONTH, month.tree.root, month.total, 1);
    let (payer, approver) = (env.payer.insecure_clone(), env.approver.insecure_clone());
    // The payer signing as the approver too.
    let funding = ReserveFunding { approver: payer.pubkey(), ..base.clone() };
    let fund = env.fund_from_reserve_ix(&funding);
    expect_err(env.send_paid_by(&payer, &[fund], &[]), code(DividendsError::Unauthorized));
    // The approver signing as the rent payer too.
    let funding = ReserveFunding { rent_payer: approver.pubkey(), payer_usdc: env.usdc_ata(&approver.pubkey()), ..base.clone() };
    let fund = env.fund_from_reserve_ix(&funding);
    expect_err(env.send_paid_by(&approver, &[fund], &[]), code(DividendsError::Unauthorized));
    assert_eq!(reserve_balance(&env), 200 * USDC);
    env.fund_from_reserve(&base).unwrap();
}

#[test]
fn the_fee_account_and_the_usdc_config_are_pepper_curves_own() {
    let mut env = Env::new();
    env.write_config();
    let creator = env.creator.insecure_clone();
    env.usdc_holding(&creator.pubkey(), 200 * USDC);
    env.deposit(&creator, 200 * USDC).unwrap();
    let month = month_of(&env, MONTH, &[USDC]);
    let base = env.reserve_funding(MONTH, month.tree.root, month.total, 1);
    // No USDC config at its address: unreadable, never «no fee».
    expect_err(env.fund_from_reserve(&base), code(DividendsError::UsdcConfigUnreadable));
    env.put_usdc_config();
    // Another address for the config, another fee account.
    let funding = ReserveFunding { usdc_config: Pubkey::new_unique(), ..base.clone() };
    expect_err(env.fund_from_reserve(&funding), anchor_code(ErrorCode::ConstraintSeeds));
    let stranger_usdc = env.usdc_account(&Pubkey::new_unique());
    let funding = ReserveFunding { fee_usdc_account: stranger_usdc, ..base.clone() };
    expect_err(env.fund_from_reserve(&funding), code(DividendsError::AccountMismatch));
    // A USDC config on another USDC than the program's.
    let mut written: pepper_curve::state::UsdcConfig = env.fetch(&usdc_config_pda());
    written.usdc_mint = Pubkey::new_unique();
    env.put(usdc_config_pda(), account_data(&written, pepper_curve::state::UsdcConfig::INIT_SPACE), pepper_curve::ID);
    expect_err(env.fund_from_reserve(&base), code(DividendsError::AccountMismatch));
    assert_eq!(reserve_balance(&env), 200 * USDC);
}

#[test]
fn a_month_is_never_funded_twice_whatever_the_source() {
    let mut env = Env::new();
    with_reserve(&mut env, 200 * USDC);
    let month = month_of(&env, MONTH, &[USDC]);
    let funding = env.reserve_funding(MONTH, month.tree.root, month.total, 1);
    env.fund_from_reserve(&funding).unwrap();
    expect_err(env.fund_from_reserve(&funding), code(DividendsError::MonthAlreadyFunded));
    // Nor by the payer's own funding, nor topped up: the reserve never signs.
    let payer = env.payer.insecure_clone();
    env.usdc_holding(&payer.pubkey(), 10 * USDC);
    let init = env.funding(&payer.pubkey(), month.tree.root, month.total, 1);
    expect_err(env.fund(&init, &payer), code(DividendsError::MonthAlreadyFunded));
    let top_up = env.top_up_ix(&payer.pubkey(), MONTH, USDC);
    let approver = env.approver.insecure_clone();
    expect_err(env.send_paid_by(&payer, &[top_up], &[&approver]), code(DividendsError::Unauthorized));
    assert_eq!(reserve_balance(&env), 200 * USDC - USDC - reserve_fee(USDC));
}

/// Pepper's 3% of a month whose holders' total is `total`, as the program floors it.
fn reserve_fee(total: u64) -> u64 {
    pepper_dividends::validation::reserve_month_fee(total)
}

// ── void_distributor on a reserve's month ────────────────────────────────────

#[test]
fn a_void_of_a_short_month_hands_the_payer_its_part_back_and_the_rest_to_the_reserve() {
    let mut env = Env::new();
    with_reserve(&mut env, 90 * USDC);
    let payer_usdc = env.usdc_holding(&env.payer.pubkey(), 7 * USDC);
    let month = month_of(&env, MONTH, &[60 * USDC, 37 * USDC]);
    let funding = env.reserve_funding(MONTH, month.tree.root, month.total, 2);
    env.fund_from_reserve(&funding).unwrap();
    assert_eq!((reserve_balance(&env), env.token_amount(&payer_usdc)), (0, 0));
    let admin = env.admin.insecure_clone();
    let void = env.void_ix(&admin.pubkey(), MONTH);
    let sent = env.send_logged_paid_by(&admin, &[void], &[]).unwrap();
    assert_eq!((env.token_amount(&payer_usdc), reserve_balance(&env)), (7 * USDC, 90 * USDC));
    let fronted = event::<UsdcDistributorFrontedReturned>(&sent.logs);
    assert_eq!((fronted.month, fronted.amount, fronted.to), (MONTH, 7 * USDC, payer_usdc));
    let voided = event::<UsdcDistributorVoided>(&sent.logs);
    assert_eq!((voided.returned, voided.to), (90 * USDC, env.reserve_vault(&env.mint)));
    let record: MonthRecord = env.fetch(&month_record_pda(&env.mint, MONTH));
    assert_eq!(record.status, MONTH_STATUS_VOIDED);
    // Funded again under the right root, it fronts the same part again, from the payer's own USDC.
    env.fund_from_reserve(&env.reserve_funding(MONTH, month.tree.root, month.total, 2)).unwrap();
    assert_eq!((reserve_balance(&env), env.token_amount(&payer_usdc)), (0, 0));
    // Another account than the recorded rent payer's own is refused.
    let mut env = Env::new();
    with_reserve(&mut env, 90 * USDC);
    env.usdc_holding(&env.payer.pubkey(), 7 * USDC);
    let month = month_of(&env, MONTH, &[60 * USDC, 37 * USDC]);
    env.fund_from_reserve(&env.reserve_funding(MONTH, month.tree.root, month.total, 2)).unwrap();
    let stranger_usdc = env.usdc_account(&Pubkey::new_unique());
    let accounts = SettleAccounts { rent_payer_usdc_account: stranger_usdc, ..env.settle_accounts(&env.mint, MONTH) };
    let void = env.void_ix_with(&admin_of(&env).pubkey(), &accounts);
    let admin = admin_of(&env);
    expect_err(env.send_paid_by(&admin, &[void], &[]), code(DividendsError::AccountMismatch));
}

fn admin_of(env: &Env) -> Keypair {
    env.admin.insecure_clone()
}

// ── The withdrawal ───────────────────────────────────────────────────────────

#[test]
fn a_withdrawal_waits_45_days_and_meanwhile_pays_only_the_months_accrued_before() {
    let mut env = Env::new();
    with_reserve(&mut env, 300 * USDC);
    let creator = env.creator.insecure_clone();
    // Ten days after October's holders were fixed.
    env.set_time(NOV_1_2026 + 10 * DAY);
    let request = env.request_withdrawal_ix(&creator.pubkey(), &env.mint);
    let sent = env.send_logged_paid_by(&creator, &[request], &[]).unwrap();
    let requested_at = env.now();
    assert_eq!(reserve(&env).withdrawal_requested_at, requested_at);
    let ev = event::<ReserveWithdrawalRequested>(&sent.logs);
    assert_eq!(
        (ev.mint, ev.owner, ev.requested_at, ev.withdrawable_at),
        (env.mint, creator.pubkey(), requested_at, requested_at + 45 * DAY)
    );
    // No deposit meanwhile.
    env.usdc_holding(&creator.pubkey(), USDC);
    expect_err(env.deposit(&creator, USDC), code(DividendsError::WithdrawalPending));
    // October, accrued before the request, is paid; November, accrued after, is not.
    let october = month_of(&env, OCTOBER, &[50 * USDC]);
    env.fund_from_reserve(&env.reserve_funding(OCTOBER, october.tree.root, october.total, 1)).unwrap();
    let november = month_of(&env, MONTH, &[50 * USDC]);
    let funding = env.reserve_funding(MONTH, november.tree.root, november.total, 1);
    expect_err(env.fund_from_reserve(&funding), code(DividendsError::MonthAfterWithdrawalRequest));
    let left = 300 * USDC - 50 * USDC - reserve_fee(50 * USDC);
    assert_eq!(reserve_balance(&env), left);
    // A second too early, then due: the owner alone signs and pays, and takes everything.
    let withdraw = env.withdraw_ix(&creator.pubkey(), &env.mint);
    env.set_time(requested_at + RESERVE_WITHDRAWAL_DELAY_SECONDS - 1);
    expect_err(env.send_paid_by(&creator, &[withdraw.clone()], &[]), code(DividendsError::WithdrawalNotReady));
    env.set_time(requested_at + RESERVE_WITHDRAWAL_DELAY_SECONDS);
    let before = env.token_amount(&env.usdc_ata(&creator.pubkey()));
    let sent = env.send_logged_paid_by(&creator, &[withdraw], &[]).unwrap();
    assert_eq!(env.token_amount(&env.usdc_ata(&creator.pubkey())) - before, left);
    assert_eq!((reserve_balance(&env), reserve(&env).withdrawal_requested_at), (0, 0));
    let ev = event::<ReserveWithdrawn>(&sent.logs);
    assert_eq!((ev.mint, ev.owner, ev.amount), (env.mint, creator.pubkey(), left));
    // The reserve stays, for a deposit to come, which reopens it to every month.
    env.deposit(&creator, USDC).unwrap();
    assert_eq!(reserve_balance(&env), USDC);
}

#[test]
fn only_the_owner_requests_cancels_or_withdraws() {
    let mut env = Env::new();
    with_reserve(&mut env, 10 * USDC);
    let creator = env.creator.insecure_clone();
    let stranger = env.funded_wallet(SOL);
    env.usdc_account(&stranger.pubkey());
    let mint = env.mint;
    for ix in [
        env.request_withdrawal_ix(&stranger.pubkey(), &mint),
        env.cancel_withdrawal_ix(&stranger.pubkey(), &mint),
        env.withdraw_ix(&stranger.pubkey(), &mint),
    ] {
        expect_err(env.send_paid_by(&stranger, &[ix], &[]), code(DividendsError::Unauthorized));
    }
    // Nothing pending: no cancel, no withdrawal.
    let cancel = env.cancel_withdrawal_ix(&creator.pubkey(), &mint);
    expect_err(env.send_paid_by(&creator, &[cancel.clone()], &[]), code(DividendsError::NoWithdrawalPending));
    let withdraw = env.withdraw_ix(&creator.pubkey(), &mint);
    expect_err(env.send_paid_by(&creator, &[withdraw], &[]), code(DividendsError::NoWithdrawalPending));
    // One request at a time; a cancel resets it.
    let request = env.request_withdrawal_ix(&creator.pubkey(), &mint);
    env.send_paid_by(&creator, &[request.clone()], &[]).unwrap();
    expect_err(env.send_paid_by(&creator, &[request], &[]), code(DividendsError::WithdrawalPending));
    let sent = env.send_logged_paid_by(&creator, &[cancel], &[]).unwrap();
    assert_eq!(reserve(&env).withdrawal_requested_at, 0);
    let ev = event::<ReserveWithdrawalCancelled>(&sent.logs);
    assert_eq!((ev.mint, ev.owner), (mint, creator.pubkey()));
    assert_eq!(reserve_balance(&env), 10 * USDC);
}

#[test]
fn a_pause_stops_deposits_and_fundings_never_a_withdrawal() {
    let mut env = Env::new();
    with_reserve(&mut env, 100 * USDC);
    let creator = env.creator.insecure_clone();
    let mut config = env.dividends_config();
    config.paused = true;
    env.put_config(&config);
    env.usdc_holding(&creator.pubkey(), USDC);
    expect_err(env.deposit(&creator, USDC), code(DividendsError::Paused));
    let month = month_of(&env, MONTH, &[USDC]);
    let funding = env.reserve_funding(MONTH, month.tree.root, month.total, 1);
    expect_err(env.fund_from_reserve(&funding), code(DividendsError::Paused));
    let mint = env.mint;
    let request = env.request_withdrawal_ix(&creator.pubkey(), &mint);
    env.send_paid_by(&creator, &[request], &[]).unwrap();
    let now = env.now();
    env.set_time(now + RESERVE_WITHDRAWAL_DELAY_SECONDS);
    let withdraw = env.withdraw_ix(&creator.pubkey(), &mint);
    env.send_paid_by(&creator, &[withdraw], &[]).unwrap();
    assert_eq!(reserve_balance(&env), 0);
}

#[test]
fn a_withdrawal_needs_the_owners_own_usdc_account() {
    let mut env = Env::new();
    with_reserve(&mut env, 10 * USDC);
    let creator = env.creator.insecure_clone();
    let mint = env.mint;
    let request = env.request_withdrawal_ix(&creator.pubkey(), &mint);
    env.send_paid_by(&creator, &[request], &[]).unwrap();
    let now = env.now();
    env.set_time(now + RESERVE_WITHDRAWAL_DELAY_SECONDS);
    let other = env.usdc_account(&Pubkey::new_unique());
    let mut withdraw = env.withdraw_ix(&creator.pubkey(), &mint);
    withdraw.accounts[4].pubkey = other;
    expect_err(env.send_paid_by(&creator, &[withdraw], &[]), code(DividendsError::AccountMismatch));
    assert_eq!(reserve_balance(&env), 10 * USDC);
}

/// The app's compute limits of the four moves (`RESERVE_MOVE_CU_LIMIT` in
/// lib/solana/pepper-dividends/reserve-moves-rules.ts, measured on devnet on 2026-11-19), less
/// the 300 units of the two compute-budget instructions each message opens with: a move past its
/// limit is refused at the co-sign's simulation, so a creator could neither deposit nor take
/// their money back.
const DEPOSIT_UNITS: u64 = 150_000 - 300;
const REQUEST_OR_CANCEL_UNITS: u64 = 30_000 - 300;
const WITHDRAWAL_UNITS: u64 = 100_000 - 300;

#[test]
fn each_move_fits_the_compute_limit_the_app_gives_it() {
    let mut env = Env::new();
    env.write_config();
    let creator = env.creator.insecure_clone();
    let sponsor = env.sponsor.insecure_clone();
    let mint = env.mint;
    env.usdc_holding(&creator.pubkey(), 20 * USDC);
    // The heaviest deposit opens the reserve and its vault.
    let first = env.deposit(&creator, 10 * USDC).unwrap().compute_units;
    let later = env.deposit(&creator, 10 * USDC).unwrap().compute_units;
    let request = env.request_withdrawal_ix(&creator.pubkey(), &mint);
    let requested = env.send_logged_paid_by(&sponsor, &[request.clone()], &[&creator]).unwrap().compute_units;
    let cancel = env.cancel_withdrawal_ix(&creator.pubkey(), &mint);
    let cancelled = env.send_logged_paid_by(&sponsor, &[cancel], &[&creator]).unwrap().compute_units;
    env.send_paid_by(&sponsor, &[request], &[&creator]).unwrap();
    // The heaviest withdrawal: the owner's USDC account closed, so the message opens it first.
    let owner_usdc = env.usdc_ata(&creator.pubkey());
    let close = spl_token::instruction::close_account(&spl_token::ID, &owner_usdc, &creator.pubkey(), &creator.pubkey(), &[]).unwrap();
    env.send_paid_by(&creator, &[close], &[]).unwrap();
    let now = env.now();
    env.set_time(now + RESERVE_WITHDRAWAL_DELAY_SECONDS);
    let reopen = env.reopen_usdc_ix(&sponsor.pubkey(), &creator.pubkey());
    let withdraw = env.withdraw_ix(&creator.pubkey(), &mint);
    let withdrawn = env.send_logged_paid_by(&sponsor, &[reopen, withdraw], &[&creator]).unwrap().compute_units;
    assert_eq!(env.token_amount(&owner_usdc), 20 * USDC);
    println!(
        "reserve moves: deposit {first} (first) {later} (later), request {requested}, cancel {cancelled}, withdrawal {withdrawn} (account reopened) compute units"
    );
    assert!(first <= DEPOSIT_UNITS && later <= DEPOSIT_UNITS, "deposits {first}, {later}");
    assert!(requested <= REQUEST_OR_CANCEL_UNITS && cancelled <= REQUEST_OR_CANCEL_UNITS, "request {requested}, cancel {cancelled}");
    assert!(withdrawn <= WITHDRAWAL_UNITS, "withdrawal {withdrawn}");
}
