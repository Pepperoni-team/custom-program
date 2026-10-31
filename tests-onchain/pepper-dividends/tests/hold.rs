//! A month's claims hold, `hold_claims` and `release_claims` (track B's B9b; Rick, 2026-10-26:
//! «Con il blocco per mese: si ferma solo il mese sbagliato, e tutti gli altri riscatti
//! continuano normalmente»).
//!
//! The pauser or the admin holds one month's claims at once, by the global claims pause's rule:
//! at most 7 days from each call, renewed, never shortened. Only the admin releases it early, and
//! the void ends it with the distributor it closes. A held month refuses its claims, while every
//! other month and token keeps claiming; its close waits 30 days after the hold, as after a
//! pause, so a hold delays a close, never a holder's window to claim. In §4.8's procedure the
//! hold takes the global pause's place, and only the wrong month waits. Every refusal is read by
//! its code with the month left as it was.

mod common;

use anchor_lang::error::ErrorCode;
use anchor_lang::AccountSerialize;
use anchor_spl::token::spl_token;
use common::*;
use pepper_dividends::constants::{MIN_CLAIMS_PAUSE_SECONDS, MONTH_STATUS_OPEN, MONTH_STATUS_VOIDED, REVIEW_WINDOW_SECONDS};
use pepper_dividends::errors::DividendsError;
use pepper_dividends::events::ClaimsHeld;
use pepper_dividends::math::dividend_leaf;
use pepper_dividends::state::{DividendsConfig, MonthRecord, UsdcDistributor};
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};

const HOUR: i64 = 3_600;
/// The upgrade Squad's time lock (MULTISIG.md §5).
const TIME_LOCK: i64 = 48 * HOUR;

fn distributor(env: &Env, month: &Month) -> UsdcDistributor {
    env.fetch(&month.distributor)
}

fn distributor_bytes(env: &Env, month: &Month) -> Vec<u8> {
    env.svm.get_account(&month.distributor).expect("the distributor").data
}

/// `distributor` as the program stores it, to compare a whole month at once.
fn bytes_of(distributor: &UsdcDistributor) -> Vec<u8> {
    let mut data = Vec::new();
    distributor.try_serialize(&mut data).unwrap();
    data.resize(UsdcDistributor::space(distributor.leaf_count), 0);
    data
}

/// An env with the config written and a month of `n` holders, each with a USDC account.
fn month_with_accounts(n: usize) -> (Env, Month) {
    let mut env = Env::new();
    env.write_config();
    let month = env.write_month(MONTH, n);
    open_accounts(&mut env, &month);
    (env, month)
}

fn open_accounts(env: &mut Env, month: &Month) {
    for (holder, _) in &month.holders {
        env.usdc_account(&holder.pubkey());
    }
}

/// The month of another token, written beside `env.mint`'s.
fn other_tokens_month(env: &mut Env, month: i64, n: usize) -> Month {
    let token = env.mint;
    env.mint = Pubkey::new_unique();
    let other = env.write_month(month, n);
    env.mint = token;
    open_accounts(env, &other);
    other
}

/// `self.mint`'s `MONTH` funded through `init_distributor` by the dividend payer, under the leaves
/// of `amounts` (the holders' own, or wrong ones). Returns the tree it was funded under.
fn fund_month(env: &mut Env, holders: &[(Keypair, u64)], amounts: &[u64]) -> Tree {
    let distributor = distributor_pda(&env.mint, MONTH);
    let leaves: Vec<[u8; 32]> = holders
        .iter()
        .zip(amounts)
        .enumerate()
        .map(|(i, ((holder, _), amount))| dividend_leaf(&distributor, &env.usdc.mint, i as u32, &holder.pubkey(), *amount))
        .collect();
    let tree = build_tree(&leaves);
    let total: u64 = holders.iter().map(|(_, amount)| amount).sum();
    let payer = env.payer.insecure_clone();
    let payer_usdc = env.usdc_account(&payer.pubkey());
    env.mint_usdc(&payer_usdc, total);
    let funding = env.funding(&payer.pubkey(), tree.root, total, holders.len() as u32);
    env.fund(&funding, &payer).unwrap();
    tree
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    Hold,
    Release,
}

#[test]
fn the_pauser_and_the_admin_hold_a_month_and_only_the_admin_releases_it() {
    let (mut env, month) = month_with_accounts(2);
    let stranger = env.funded_wallet(SOL);
    let (admin, pauser) = (env.admin.pubkey(), env.pauser.pubkey());
    let keys = [
        env.admin.insecure_clone(),
        env.pauser.insecure_clone(),
        env.approver.insecure_clone(),
        env.closer.insecure_clone(),
        env.pepper_funder.insecure_clone(),
        env.payer.insecure_clone(),
        env.sponsor.insecure_clone(),
        stranger,
    ];
    let now = env.now();
    let fresh = distributor(&env, &month);
    assert_eq!(fresh.claims_held_until, 0, "a fresh month is held by nobody");
    for act in [Act::Hold, Act::Release] {
        for key in &keys {
            let by = key.pubkey();
            // Each act from a month where it changes something, and the month it leaves.
            let held = UsdcDistributor { claims_held_until: now + DAY, ..fresh.clone() };
            let (start, end, ix, allowed) = match act {
                Act::Hold => (fresh.clone(), held.clone(), env.hold_claims_ix(&by, &month.distributor, now + DAY), by == pauser || by == admin),
                Act::Release => (
                    held.clone(),
                    UsdcDistributor { claims_held_until: now, ..fresh.clone() },
                    env.release_claims_ix(&by, &month.distributor),
                    by == admin,
                ),
            };
            env.put_distributor(&start);
            let result = env.send_logged_paid_by(key, &[ix], &[]);
            if !allowed {
                expect_err(result, code(DividendsError::Unauthorized));
                assert_eq!(distributor_bytes(&env, &month), bytes_of(&start), "{act:?} by a key that may not");
                continue;
            }
            let sent = result.unwrap_or_else(|e| panic!("{act:?} by {by}: {e:?}"));
            assert_eq!(distributor_bytes(&env, &month), bytes_of(&end), "{act:?} by {by}");
            let event = event::<ClaimsHeld>(&sent.logs);
            assert_eq!((event.mint, event.month, event.until, event.by), (env.mint, MONTH, end.claims_held_until, by), "{act:?}");
        }
    }
}

#[test]
fn a_held_month_refuses_its_claims_to_the_second_and_every_other_month_and_token_keeps_claiming() {
    let (mut env, held) = month_with_accounts(3);
    let next = env.write_month(MONTH + 1, 2);
    open_accounts(&mut env, &next);
    let other_token = other_tokens_month(&mut env, MONTH, 2);
    let (pauser, sponsor) = (env.pauser.insecure_clone(), env.sponsor.insecure_clone());
    let t = env.now();
    let hold = env.hold_claims_ix(&pauser.pubkey(), &held.distributor, t + DAY);
    env.send_paid_by(&pauser, &[hold], &[]).unwrap();
    // Nothing but this month moved: the config's claims pause stays as it was.
    assert_eq!(env.fetch::<DividendsConfig>(&config_pda()).claims_paused_until, 0);

    env.set_time(t + DAY - 1);
    let before = (env.token_amount(&held.vault), distributor_bytes(&env, &held));
    let claim = env.claim_ix(&held, 0);
    expect_err(env.send_paid_by(&sponsor, &[claim.clone()], &[]), code(DividendsError::ClaimsHeld));
    assert_eq!((env.token_amount(&held.vault), distributor_bytes(&env, &held)), before);
    // The same token's next month and another token's month pay as ever.
    for (month, index) in [(&next, 0), (&next, 1), (&other_token, 0)] {
        let claim = env.claim_ix(month, index);
        env.send_paid_by(&sponsor, &[claim], &[]).unwrap();
        let (holder, amount, _) = month.leaf(index);
        assert_eq!(env.token_amount(&env.usdc_ata(&holder.pubkey())), amount);
    }
    // At the hold's end, the held month pays too.
    env.set_time(t + DAY);
    env.send_paid_by(&sponsor, &[claim], &[]).unwrap();
    assert_eq!(env.token_amount(&env.usdc_ata(&held.holders[0].0.pubkey())), held.holders[0].1);
}

#[test]
fn a_hold_lasts_seven_days_at_most_only_starts_or_extends_and_is_renewed_at_day_seven() {
    let (mut env, month) = month_with_accounts(2);
    let (pauser, admin) = (env.pauser.insecure_clone(), env.admin.insecure_clone());
    let t = env.now();
    let send = |env: &mut Env, signer: &Keypair, until: i64| {
        let hold = env.hold_claims_ix(&signer.pubkey(), &month.distributor, until);
        env.send_paid_by(signer, &[hold], &[])
    };
    // From a fresh month's 0: 7 days and a second is too long, a second ago is in the past.
    let before = distributor_bytes(&env, &month);
    expect_err(send(&mut env, &pauser, t + 7 * DAY + 1), code(DividendsError::PauseTooLong));
    expect_err(send(&mut env, &pauser, t - 1), code(DividendsError::PauseEndTooEarly));
    assert_eq!(distributor_bytes(&env, &month), before);
    send(&mut env, &pauser, t + 7 * DAY).unwrap();
    assert_eq!(distributor(&env, &month).claims_held_until, t + 7 * DAY);
    // Nobody shortens it, the admin no more than the pauser; the same end is accepted.
    let before = distributor_bytes(&env, &month);
    for signer in [&pauser, &admin] {
        expect_err(send(&mut env, signer, t + 7 * DAY - 1), code(DividendsError::PauseEndTooEarly));
    }
    assert_eq!(distributor_bytes(&env, &month), before);
    send(&mut env, &admin, t + 7 * DAY).unwrap();
    // At day 7 the pauser renews it for 7 days more, and not a second further.
    env.set_time(t + 7 * DAY);
    expect_err(send(&mut env, &pauser, t + 14 * DAY + 1), code(DividendsError::PauseTooLong));
    send(&mut env, &pauser, t + 14 * DAY).unwrap();
    assert_eq!(distributor(&env, &month).claims_held_until, t + 14 * DAY);
    // Once it has ended, an end between it and now lies in the past.
    env.set_time(t + 15 * DAY);
    expect_err(send(&mut env, &pauser, t + 15 * DAY - 1), code(DividendsError::PauseEndTooEarly));
    assert_eq!(distributor(&env, &month).claims_held_until, t + 14 * DAY);
}

#[test]
fn a_release_written_at_t_and_executed_48_hours_later_leaves_the_time_it_runs() {
    let (mut env, month) = month_with_accounts(2);
    let (pauser, admin, sponsor) = (env.pauser.insecure_clone(), env.admin.insecure_clone(), env.sponsor.insecure_clone());
    let t = env.now();
    let hold = env.hold_claims_ix(&pauser.pubkey(), &month.distributor, t + 7 * DAY);
    env.send_paid_by(&pauser, &[hold], &[]).unwrap();
    // The vault's transaction is written at T; the release in it carries no time.
    let release = env.release_claims_ix(&admin.pubkey(), &month.distributor);
    env.set_time(t + HOUR);
    let claim = env.claim_ix(&month, 0);
    expect_err(env.send_paid_by(&sponsor, &[claim.clone()], &[]), code(DividendsError::ClaimsHeld));
    // Executed 48 hours and a second later, it leaves that second, and the month pays at it.
    env.set_time(t + TIME_LOCK + 1);
    let sent = env.send_logged_paid_by(&admin, &[release], &[]).unwrap();
    assert_eq!(distributor(&env, &month).claims_held_until, t + TIME_LOCK + 1);
    let event = event::<ClaimsHeld>(&sent.logs);
    assert_eq!((event.until, event.by), (t + TIME_LOCK + 1, admin.pubkey()));
    env.send_paid_by(&sponsor, &[claim], &[]).unwrap();
    assert_eq!(env.token_amount(&env.usdc_ata(&month.holders[0].0.pubkey())), month.holders[0].1);
}

#[test]
fn a_release_when_no_hold_runs_leaves_the_value_as_it_was_and_fails_nothing() {
    let (mut env, month) = month_with_accounts(2);
    let (pauser, admin) = (env.pauser.insecure_clone(), env.admin.insecure_clone());
    let t = env.now();
    // A fresh month's 0 stays 0.
    let release = env.release_claims_ix(&admin.pubkey(), &month.distributor);
    let sent = env.send_logged_paid_by(&admin, &[release.clone()], &[]).unwrap();
    assert_eq!(distributor(&env, &month).claims_held_until, 0);
    assert_eq!(event::<ClaimsHeld>(&sent.logs).until, 0);
    // A hold that ran out keeps its end, the time this month's claims last opened again.
    let hold = env.hold_claims_ix(&pauser.pubkey(), &month.distributor, t + DAY);
    env.send_paid_by(&pauser, &[hold], &[]).unwrap();
    env.set_time(t + 3 * DAY);
    let sent = env.send_logged_paid_by(&admin, &[release], &[]).unwrap();
    assert_eq!(distributor(&env, &month).claims_held_until, t + DAY);
    assert_eq!(event::<ClaimsHeld>(&sent.logs).until, t + DAY);
}

#[test]
fn an_expired_month_closes_30_days_after_its_hold_and_after_the_later_of_hold_and_pause() {
    let (mut env, month) = month_with_accounts(3);
    let pauser = env.pauser.insecure_clone();
    let closer = env.closer.insecure_clone();
    env.usdc_account(&env.payer.pubkey());
    let expires_at = distributor(&env, &month).expires_at;
    // The pauser holds this month across its expiry, from two days before it to five after.
    env.set_time(expires_at - 2 * DAY);
    let hold = env.hold_claims_ix(&pauser.pubkey(), &month.distributor, expires_at + 5 * DAY);
    env.send_paid_by(&pauser, &[hold], &[]).unwrap();
    let reopened = expires_at + 5 * DAY;
    let close = env.close_ix(&closer.pubkey(), MONTH);
    for at in [expires_at, reopened, reopened + 30 * DAY - 1] {
        env.set_time(at);
        let before = distributor_bytes(&env, &month);
        expect_err(env.send_paid_by(&closer, &[close.clone()], &[]), code(DividendsError::ClaimsPausedRecently));
        assert_eq!(distributor_bytes(&env, &month), before);
    }
    // A claims pause that ended later than the hold counts from its own end.
    let paused = DividendsConfig { claims_paused_until: reopened + 2 * DAY, ..env.dividends_config() };
    env.put_config(&paused);
    env.set_time(reopened + 30 * DAY);
    expect_err(env.send_paid_by(&closer, &[close.clone()], &[]), code(DividendsError::ClaimsPausedRecently));
    env.set_time(reopened + 32 * DAY);
    env.send_paid_by(&closer, &[close], &[]).unwrap();
    assert!(!env.exists(&month.distributor));
}

#[test]
fn a_fully_claimed_month_closes_at_once_while_held() {
    let (mut env, month) = month_with_accounts(2);
    let (pauser, sponsor, payer) = (env.pauser.insecure_clone(), env.sponsor.insecure_clone(), env.payer.insecure_clone());
    for index in 0..2 {
        let claim = env.claim_ix(&month, index);
        env.send_paid_by(&sponsor, &[claim], &[]).unwrap();
    }
    let hold = env.hold_claims_ix(&pauser.pubkey(), &month.distributor, env.now() + 7 * DAY);
    env.send_paid_by(&pauser, &[hold], &[]).unwrap();
    // Nothing is owed to anyone: its funder closes it at once.
    let close = env.close_ix(&payer.pubkey(), MONTH);
    env.send_paid_by(&payer, &[close], &[]).unwrap();
    assert!(!env.exists(&month.distributor));
}

#[test]
fn the_void_ends_the_hold_and_the_month_funded_again_opens_after_its_own_review_window() {
    let mut env = Env::new();
    env.init_config().unwrap();
    let (admin, pauser, sponsor) = (env.admin.insecure_clone(), env.pauser.insecure_clone(), env.sponsor.insecure_clone());
    let holders = holders(2);
    let doubled: Vec<u64> = holders.iter().map(|(_, amount)| 2 * amount).collect();
    fund_month(&mut env, &holders, &doubled);
    let distributor_address = distributor_pda(&env.mint, MONTH);
    let hold = env.hold_claims_ix(&pauser.pubkey(), &distributor_address, env.now() + 7 * DAY);
    env.send_paid_by(&pauser, &[hold], &[]).unwrap();
    // The hold never blocks the void: it is what the hold is for.
    let void = env.void_ix(&admin.pubkey(), MONTH);
    env.send_paid_by(&admin, &[void], &[]).unwrap();
    assert!(!env.exists(&distributor_address));
    assert_eq!(env.fetch::<MonthRecord>(&month_record_pda(&env.mint, MONTH)).status, MONTH_STATUS_VOIDED);
    // Funded again under the right root: a fresh distributor, held only for its own review window,
    // which the void's hold does not lengthen; it pays the second that window ends.
    let amounts: Vec<u64> = holders.iter().map(|(_, amount)| *amount).collect();
    let refunded_at = env.now();
    let tree = fund_month(&mut env, &holders, &amounts);
    let total: u64 = amounts.iter().sum();
    let month = Month { mint: env.mint, month: MONTH, distributor: distributor_address, vault: env.usdc_ata(&distributor_address), holders, tree, total };
    assert_eq!(distributor(&env, &month).claims_held_until, refunded_at + REVIEW_WINDOW_SECONDS);
    open_accounts(&mut env, &month);
    let claim = env.claim_ix(&month, 1);
    env.set_time(refunded_at + REVIEW_WINDOW_SECONDS - 1);
    expect_err(env.send_paid_by(&sponsor, &[claim.clone()], &[]), code(DividendsError::ClaimsHeld));
    env.set_time(refunded_at + REVIEW_WINDOW_SECONDS);
    env.send_paid_by(&sponsor, &[claim], &[]).unwrap();
    assert_eq!(env.token_amount(&env.usdc_ata(&month.holders[1].0.pubkey())), month.holders[1].1);
}

#[test]
fn a_hold_ending_within_the_hour_is_refused_to_the_second() {
    // The internal audit (2026-10-29): a hold ending now refused no claim, yet delayed the month's
    // close by 30 days, renewable without end. A hold now lasts an hour at least.
    let (mut env, month) = month_with_accounts(2);
    let (pauser, admin) = (env.pauser.insecure_clone(), env.admin.insecure_clone());
    let t = env.now();
    assert_eq!(MIN_CLAIMS_PAUSE_SECONDS, 3_600);
    let before = distributor_bytes(&env, &month);
    for (signer, until) in [(&pauser, t), (&pauser, t + 60), (&admin, t + 3_599)] {
        let hold = env.hold_claims_ix(&signer.pubkey(), &month.distributor, until);
        expect_err(env.send_paid_by(signer, &[hold], &[]), code(DividendsError::PauseTooShort));
    }
    assert_eq!(distributor_bytes(&env, &month), before);
    let hold = env.hold_claims_ix(&pauser.pubkey(), &month.distributor, t + 3_600);
    env.send_paid_by(&pauser, &[hold], &[]).unwrap();
    assert_eq!(distributor(&env, &month).claims_held_until, t + 3_600);
    // Ten minutes before it ends, it is extended past the hour or not at all.
    env.set_time(t + 3_000);
    let keep = env.hold_claims_ix(&pauser.pubkey(), &month.distributor, t + 3_600);
    expect_err(env.send_paid_by(&pauser, &[keep], &[]), code(DividendsError::PauseTooShort));
    let extend = env.hold_claims_ix(&pauser.pubkey(), &month.distributor, t + 3_000 + 3_600);
    env.send_paid_by(&pauser, &[extend], &[]).unwrap();
    assert_eq!(distributor(&env, &month).claims_held_until, t + 6_600);
}

#[test]
fn a_hold_extends_a_months_review_window_and_a_release_opens_it_early() {
    let mut env = Env::new();
    env.init_config().unwrap();
    let (pauser, admin, sponsor) = (env.pauser.insecure_clone(), env.admin.insecure_clone(), env.sponsor.insecure_clone());
    let holders = holders(2);
    let amounts: Vec<u64> = holders.iter().map(|(_, amount)| *amount).collect();
    let funded_at = env.now();
    let tree = fund_month(&mut env, &holders, &amounts);
    let distributor_address = distributor_pda(&env.mint, MONTH);
    let total: u64 = amounts.iter().sum();
    let month = Month { mint: env.mint, month: MONTH, distributor: distributor_address, vault: env.usdc_ata(&distributor_address), holders, tree, total };
    open_accounts(&mut env, &month);
    let window_ends = funded_at + REVIEW_WINDOW_SECONDS;
    assert_eq!(distributor(&env, &month).claims_held_until, window_ends);
    // The window is a hold already: the pauser may lengthen it, never shorten it.
    let shorter = env.hold_claims_ix(&pauser.pubkey(), &month.distributor, window_ends - 1);
    expect_err(env.send_paid_by(&pauser, &[shorter], &[]), code(DividendsError::PauseEndTooEarly));
    let longer = env.hold_claims_ix(&pauser.pubkey(), &month.distributor, funded_at + 3 * DAY);
    env.send_paid_by(&pauser, &[longer], &[]).unwrap();
    env.set_time(window_ends);
    let claim = env.claim_ix(&month, 0);
    expect_err(env.send_paid_by(&sponsor, &[claim.clone()], &[]), code(DividendsError::ClaimsHeld));
    // The admin's release opens it at the second it runs.
    let release = env.release_claims_ix(&admin.pubkey(), &month.distributor);
    env.send_paid_by(&admin, &[release], &[]).unwrap();
    assert_eq!(distributor(&env, &month).claims_held_until, window_ends);
    env.send_paid_by(&sponsor, &[claim], &[]).unwrap();
    assert_eq!(env.token_amount(&env.usdc_ata(&month.holders[0].0.pubkey())), month.holders[0].1);
}

#[test]
fn the_void_procedure_with_the_hold_stops_the_wrong_month_alone() {
    let mut env = Env::new();
    env.init_config().unwrap();
    let (admin, pauser, sponsor, closer) =
        (env.admin.insecure_clone(), env.pauser.insecure_clone(), env.sponsor.insecure_clone(), env.closer.insecure_clone());
    env.usdc_account(&env.payer.pubkey());
    let t = env.now();
    // The same token's previous month, still claiming, and another token's month that expires
    // while the vault's proposal waits.
    let previous = env.write_month(MONTH - 1, 2);
    open_accounts(&mut env, &previous);
    let expiring = other_tokens_month(&mut env, MONTH - 1, 2);
    let mut written: UsdcDistributor = env.fetch(&expiring.distributor);
    written.expires_at = t + DAY;
    env.put_distributor(&written);
    // This month funded wrong, every amount doubled: a holder's claim would drain the vault.
    let holders = holders(2);
    let doubled: Vec<u64> = holders.iter().map(|(_, amount)| 2 * amount).collect();
    let wrong = fund_month(&mut env, &holders, &doubled);
    let distributor_address = distributor_pda(&env.mint, MONTH);
    // At once, the pauser holds this month alone: one claim would make it unvoidable.
    let hold = env.hold_claims_ix(&pauser.pubkey(), &distributor_address, t + 7 * DAY);
    env.send_paid_by(&pauser, &[hold], &[]).unwrap();
    // The vault's transaction, written at T: the void alone, which ends the hold with the month.
    let vault_transaction = [env.void_ix(&admin.pubkey(), MONTH)];

    // While it waits, the doubled claim is refused, and every other month carries on: the same
    // token's previous month pays, and the expired month of the other token closes with no wait.
    env.set_time(t + DAY + HOUR);
    let holder = holders[0].0.insecure_clone();
    env.usdc_account(&holder.pubkey());
    let wrong_claim = ClaimAccounts {
        distributor: distributor_address,
        vault: env.usdc_ata(&distributor_address),
        claimant: holder.pubkey(),
        claimant_usdc: env.usdc_ata(&holder.pubkey()),
        usdc_mint: env.usdc.mint,
        token_program: spl_token::ID,
    };
    let claim = env.claim_ix_with(&wrong_claim, 0, doubled[0], wrong.proofs[0].clone());
    expect_err(env.send_paid_by(&sponsor, &[claim], &[]), code(DividendsError::ClaimsHeld));
    let previous_claim = env.claim_ix(&previous, 0);
    env.send_paid_by(&sponsor, &[previous_claim], &[]).unwrap();
    let close = env.close_ix_with(&closer.pubkey(), &env.settle_accounts(&expiring.mint, MONTH - 1));
    env.send_paid_by(&closer, &[close], &[]).unwrap();
    assert!(!env.exists(&expiring.distributor));
    assert_eq!(env.fetch::<DividendsConfig>(&config_pda()).claims_paused_until, 0);

    // Executed 48 hours and a second after it was written: the month voided.
    env.set_time(t + TIME_LOCK + 1);
    env.send_paid_by(&admin, &vault_transaction, &[]).unwrap();
    assert_eq!(env.fetch::<MonthRecord>(&month_record_pda(&env.mint, MONTH)).status, MONTH_STATUS_VOIDED);
    // Funded again under the right root, and claimed once its own review window has passed.
    let amounts: Vec<u64> = holders.iter().map(|(_, amount)| *amount).collect();
    let tree = fund_month(&mut env, &holders, &amounts);
    assert_eq!(env.fetch::<MonthRecord>(&month_record_pda(&env.mint, MONTH)).status, MONTH_STATUS_OPEN);
    let total: u64 = amounts.iter().sum();
    let month = Month { mint: env.mint, month: MONTH, distributor: distributor_address, vault: env.usdc_ata(&distributor_address), holders, tree, total };
    env.past_review_window();
    let claim = env.claim_ix(&month, 0);
    env.send_paid_by(&sponsor, &[claim], &[]).unwrap();
    assert_eq!(env.token_amount(&env.usdc_ata(&holder.pubkey())), month.holders[0].1);
}

#[test]
fn a_forged_config_another_programs_account_and_a_closed_month_are_refused() {
    let (mut env, month) = month_with_accounts(2);
    let (pauser, payer) = (env.pauser.insecure_clone(), env.payer.insecure_clone());
    let now = env.now();
    // A config at another address naming a stranger as pauser, as only the bank could write one.
    let stranger = env.funded_wallet(SOL);
    let copy = Pubkey::new_unique();
    let forged = DividendsConfig { pauser: stranger.pubkey(), admin: stranger.pubkey(), ..env.dividends_config() };
    env.put(copy, account_data(&forged, <DividendsConfig as anchor_lang::Space>::INIT_SPACE), pepper_dividends::ID);
    let accounts = || pepper_dividends::accounts::Hold { authority: stranger.pubkey(), config: copy, distributor: month.distributor };
    let before = distributor_bytes(&env, &month);
    for act in [
        ix(accounts(), pepper_dividends::instruction::HoldClaims { until: now + DAY }),
        ix(accounts(), pepper_dividends::instruction::ReleaseClaims {}),
    ] {
        expect_err(env.send_paid_by(&stranger, &[act], &[]), anchor_code(ErrorCode::ConstraintSeeds));
    }
    assert_eq!(distributor_bytes(&env, &month), before);
    // The month's bytes under another program's ownership: not a month of this one.
    let lookalike = Pubkey::new_unique();
    env.put(lookalike, before.clone(), pepper_curve::ID);
    let hold = env.hold_claims_ix(&pauser.pubkey(), &lookalike, now + DAY);
    expect_err(env.send_paid_by(&pauser, &[hold], &[]), anchor_code(ErrorCode::AccountOwnedByWrongProgram));
    // A month closed is held no more: its distributor is gone.
    let sponsor = env.sponsor.insecure_clone();
    for index in 0..2 {
        let claim = env.claim_ix(&month, index);
        env.send_paid_by(&sponsor, &[claim], &[]).unwrap();
    }
    let close = env.close_ix(&payer.pubkey(), MONTH);
    env.send_paid_by(&payer, &[close], &[]).unwrap();
    let hold = env.hold_claims_ix(&pauser.pubkey(), &month.distributor, now + DAY);
    expect_err(env.send_paid_by(&pauser, &[hold], &[]), anchor_code(ErrorCode::AccountNotInitialized));
}

#[test]
fn the_compute_units_of_a_hold_and_a_release_up_to_a_full_registry() {
    for leaf_count in [1u32, 65_536] {
        let mut env = Env::new();
        env.write_config();
        let payer = env.payer.pubkey();
        let written = env.usdc_distributor(env.mint, MONTH, payer, [0; 32], 10 * USDC, leaf_count);
        let address = env.put_funded_month(&written);
        let (pauser, admin) = (env.pauser.insecure_clone(), env.admin.insecure_clone());
        let hold = env.hold_claims_ix(&pauser.pubkey(), &address, env.now() + DAY);
        let held = env.send_logged_paid_by(&pauser, &[hold], &[]).unwrap();
        let release = env.release_claims_ix(&admin.pubkey(), &address);
        let released = env.send_logged_paid_by(&admin, &[release], &[]).unwrap();
        println!(
            "hold_claims, {leaf_count} leaves: {} compute units; release_claims: {}",
            held.compute_units, released.compute_units
        );
        assert!(held.compute_units < 50_000 && released.compute_units < 50_000);
    }
}
