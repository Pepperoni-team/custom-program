//! `close_distributor` (spec §4.7; track B's B8), and the settlement it shares with the void.
//!
//! A month closes by its funder or the config's closer: at any time once fully claimed; otherwise
//! from its expiry, to the second, and only once claims have been open for the last 30 days. The
//! vault's whole balance, stray transfers included, goes back to the funder's recorded USDC
//! account, opened again in the close's own transaction when its owner closed it; the vault's and
//! the distributor's lamports go to the recorded rent payer; the record stays, closed, so the
//! month never opens again. Every refusal is read by its code, the month, the funder's USDC and the
//! rent payer's lamports left as they were.

mod common;

use anchor_lang::error::ErrorCode;
use anchor_spl::token::spl_token;
use common::*;
use pepper_dividends::constants::{CLAIMS_OPEN_BEFORE_CLOSE_SECONDS, MAX_CLOSE_DELAY_SECONDS, MONTH_STATUS_CLOSED};
use pepper_dividends::errors::DividendsError;
use pepper_dividends::events::UsdcDistributorClosed;
use pepper_dividends::state::{DividendsConfig, MonthRecord, UsdcDistributor};
use solana_sdk::{instruction::InstructionError, signature::Signer, transaction::TransactionError};

/// A token account's size: the vault's, and a funder's USDC account opened again.
const TOKEN_ACCOUNT_LEN: usize = 165;

/// The month's three accounts, the dividend payer's USDC account (the funder of every month the
/// harness writes) and its lamports (the rent payer's): what a refused close leaves as they were.
fn snapshot(env: &Env, month: &Month) -> (Vec<Option<Vec<u8>>>, u64) {
    let payer = env.payer.pubkey();
    let accounts = [month.distributor, month_record_pda(&month.mint, month.month), month.vault, env.usdc_ata(&payer)];
    let data = accounts.iter().map(|address| env.svm.get_account(address).filter(|a| a.lamports > 0).map(|a| a.data)).collect();
    (data, env.lamports(&payer))
}

fn closed(env: &Env, month: &Month) -> bool {
    !env.exists(&month.distributor) && !env.exists(&month.vault)
}

fn record(env: &Env, month: &Month) -> MonthRecord {
    env.fetch(&month_record_pda(&month.mint, month.month))
}

fn expires_at(env: &Env, month: &Month) -> i64 {
    env.fetch::<UsdcDistributor>(&month.distributor).expires_at
}

#[test]
fn the_first_month_end_to_end_through_the_instructions_themselves() {
    // The config born by initialize_config, the month funded by init_distributor, two of its three
    // holders paid by claim, the third's share back to the dividend payer at the close.
    let mut env = Env::new();
    env.init_config().unwrap();
    let (payer, closer, sponsor) = (env.payer.insecure_clone(), env.closer.insecure_clone(), env.sponsor.insecure_clone());
    let distributor = distributor_pda(&env.mint, MONTH);
    let holders = holders(3);
    let tree = build_tree(&dividend_leaves(&distributor, &env.usdc.mint, &holders));
    let total: u64 = holders.iter().map(|(_, amount)| amount).sum();
    let payer_usdc = env.usdc_account(&payer.pubkey());
    env.mint_usdc(&payer_usdc, total);
    let funding = env.funding(&payer.pubkey(), tree.root, total, 3);
    env.fund(&funding, &payer).unwrap();
    let month = Month { mint: env.mint, month: MONTH, distributor, vault: env.usdc_ata(&distributor), holders, tree, total };
    // Its claims open a day after the funding, its review window.
    env.past_review_window();
    for index in 0..2 {
        env.usdc_account(&month.holders[index].0.pubkey());
        let claim = env.claim_ix(&month, index);
        env.send_paid_by(&sponsor, &[claim], &[]).unwrap();
    }
    let unclaimed = month.holders[2].1;
    assert_eq!(env.token_amount(&month.vault), unclaimed);

    env.set_time(funding.expires_at);
    let record_address = month_record_pda(&env.mint, MONTH);
    let rents = env.lamports(&month.vault) + env.lamports(&distributor);
    let (payer_lamports, record_rent) = (env.lamports(&payer.pubkey()), env.lamports(&record_address));
    let close = env.close_ix(&closer.pubkey(), MONTH);
    let sent = env.send_logged_paid_by(&closer, &[close], &[]).unwrap();
    println!("close_distributor, a month of 3 holders: {} compute units", sent.compute_units);

    assert_eq!(env.token_amount(&payer_usdc), unclaimed);
    assert!(closed(&env, &month));
    // Both rents back to the rent payer, which paid them; the closer paid the fee.
    assert_eq!(env.lamports(&payer.pubkey()) - payer_lamports, rents);
    let kept = record(&env, &month);
    assert_eq!((kept.status, kept.distributor), (MONTH_STATUS_CLOSED, distributor));
    assert_eq!(env.lamports(&record_address), record_rent, "the record keeps its rent");
    let event = event::<UsdcDistributorClosed>(&sent.logs);
    assert_eq!((event.mint, event.month, event.returned, event.to), (env.mint, MONTH, unclaimed, payer_usdc));
    // The record stays: the month never opens again.
    env.mint_usdc(&payer_usdc, total);
    let again = env.funding(&payer.pubkey(), month.tree.root, total, 3);
    expect_err(env.fund(&again, &payer), code(DividendsError::MonthAlreadyFunded));
}

#[test]
fn a_fully_claimed_month_closes_at_once_by_its_funder_or_the_closer_and_by_nobody_else() {
    for closer_signs in [false, true] {
        let mut env = Env::new();
        env.write_config();
        let month = env.write_month(MONTH, 2);
        let sponsor = env.sponsor.insecure_clone();
        for index in 0..2 {
            env.usdc_account(&month.holders[index].0.pubkey());
            let claim = env.claim_ix(&month, index);
            env.send_paid_by(&sponsor, &[claim], &[]).unwrap();
        }
        // Fully claimed: no wait for the expiry, nor for a claims pause running now. The vault is
        // empty, so the funder's USDC account, never opened, is never read.
        let paused = DividendsConfig { claims_paused_until: env.now() + DAY, ..env.dividends_config() };
        env.put_config(&paused);
        assert!(!env.exists(&env.usdc_ata(&env.payer.pubkey())));
        let stranger = env.funded_wallet(SOL);
        // Another month's funder, the admin and every other role are no closer of this month.
        for key in [
            env.admin.insecure_clone(),
            env.pauser.insecure_clone(),
            env.approver.insecure_clone(),
            env.pepper_funder.insecure_clone(),
            env.sponsor.insecure_clone(),
            stranger,
        ] {
            let before = snapshot(&env, &month);
            let close = env.close_ix(&key.pubkey(), MONTH);
            expect_err(env.send_paid_by(&key, &[close], &[]), code(DividendsError::Unauthorized));
            assert_eq!(snapshot(&env, &month), before);
        }
        let signer = if closer_signs { env.closer.insecure_clone() } else { env.payer.insecure_clone() };
        let close = env.close_ix(&signer.pubkey(), MONTH);
        let sent = env.send_logged_paid_by(&signer, &[close], &[]).unwrap();
        assert!(closed(&env, &month));
        assert_eq!(record(&env, &month).status, MONTH_STATUS_CLOSED);
        assert_eq!(event::<UsdcDistributorClosed>(&sent.logs).returned, 0);
    }
}

#[test]
fn an_unclaimed_month_closes_from_its_expiry_to_the_second() {
    let mut env = Env::new();
    env.write_config();
    let month = env.write_month(MONTH, 3);
    let payer_usdc = env.usdc_account(&env.payer.pubkey());
    let closer = env.closer.insecure_clone();
    let expires_at = expires_at(&env, &month);
    env.set_time(expires_at - 1);
    let before = snapshot(&env, &month);
    let close = env.close_ix(&closer.pubkey(), MONTH);
    expect_err(env.send_paid_by(&closer, &[close.clone()], &[]), code(DividendsError::NotExpired));
    assert_eq!(snapshot(&env, &month), before);
    env.set_time(expires_at);
    env.send_paid_by(&closer, &[close], &[]).unwrap();
    assert_eq!(env.token_amount(&payer_usdc), month.total);
    assert!(closed(&env, &month));
}

#[test]
fn an_expired_month_waits_until_claims_have_been_open_30_days_to_the_second() {
    let mut env = Env::new();
    env.write_config();
    let month = env.write_month(MONTH, 3);
    env.usdc_account(&env.payer.pubkey());
    let (pauser, closer) = (env.pauser.insecure_clone(), env.closer.insecure_clone());
    let expires_at = expires_at(&env, &month);
    // The pauser holds claims across the expiry, from two days before it to five days after.
    env.set_time(expires_at - 2 * DAY);
    let pause = env.pause_claims_ix(&pauser.pubkey(), expires_at + 5 * DAY);
    env.send_paid_by(&pauser, &[pause], &[]).unwrap();
    let reopened = expires_at + 5 * DAY;
    assert_eq!(CLAIMS_OPEN_BEFORE_CLOSE_SECONDS, 30 * DAY);
    let close = env.close_ix(&closer.pubkey(), MONTH);
    for at in [expires_at, reopened, reopened + 30 * DAY - 1] {
        env.set_time(at);
        let before = snapshot(&env, &month);
        expect_err(env.send_paid_by(&closer, &[close.clone()], &[]), code(DividendsError::ClaimsPausedRecently));
        assert_eq!(snapshot(&env, &month), before);
    }
    env.set_time(reopened + 30 * DAY);
    env.send_paid_by(&closer, &[close], &[]).unwrap();
    assert!(closed(&env, &month));
}

#[test]
fn pauses_renewed_within_every_30_days_delay_an_expired_months_close_90_days_at_most() {
    // The internal audit (2026-10-29): a pauser pausing claims for an hour within every 30 days kept
    // every expired month from closing for ever. From 90 days after its expiry the month closes,
    // to the second, whatever pause ran since.
    let mut env = Env::new();
    env.write_config();
    let month = env.write_month(MONTH, 3);
    let payer_usdc = env.usdc_account(&env.payer.pubkey());
    let (pauser, closer) = (env.pauser.insecure_clone(), env.closer.insecure_clone());
    let expires_at = expires_at(&env, &month);
    assert_eq!(MAX_CLOSE_DELAY_SECONDS, 90 * DAY);
    let close = env.close_ix(&closer.pubkey(), MONTH);
    // An hour's pause at the expiry, then every 29 days.
    for renewal in 0..4i64 {
        let at = expires_at + renewal * 29 * DAY;
        env.set_time(at);
        let pause = env.pause_claims_ix(&pauser.pubkey(), at + 3_600);
        env.send_paid_by(&pauser, &[pause], &[]).unwrap();
        env.set_time(at + 3_600);
        let before = snapshot(&env, &month);
        expect_err(env.send_paid_by(&closer, &[close.clone()], &[]), code(DividendsError::ClaimsPausedRecently));
        assert_eq!(snapshot(&env, &month), before);
    }
    // The last pause ended at day 87 and an hour, so its 30 days run past the cap, which wins.
    let cap = expires_at + MAX_CLOSE_DELAY_SECONDS;
    env.set_time(cap - 1);
    let before = snapshot(&env, &month);
    expect_err(env.send_paid_by(&closer, &[close.clone()], &[]), code(DividendsError::ClaimsPausedRecently));
    assert_eq!(snapshot(&env, &month), before);
    env.set_time(cap);
    env.send_paid_by(&closer, &[close], &[]).unwrap();
    assert_eq!(env.token_amount(&payer_usdc), month.total);
    assert!(closed(&env, &month));
}

#[test]
fn a_config_month_or_record_of_another_layout_version_is_refused() {
    // The internal audit (2026-10-29): this program reads its own layout alone, so a binary rolled
    // back past a later layout refuses that layout's accounts instead of reading them as its own.
    for which in 0..3 {
        let mut env = Env::new();
        env.write_config();
        let month = env.write_month(MONTH, 2);
        env.usdc_account(&env.payer.pubkey());
        match which {
            0 => {
                let later = DividendsConfig { version: 2, ..env.dividends_config() };
                env.put_config(&later);
            }
            1 => {
                let mut later: UsdcDistributor = env.fetch(&month.distributor);
                later.version = 2;
                env.put_distributor(&later);
            }
            _ => {
                let record = month_record_pda(&env.mint, MONTH);
                let mut data = env.svm.get_account(&record).unwrap().data;
                data[8] = 2;
                env.put(record, data, pepper_dividends::ID);
            }
        }
        let closer = env.closer.insecure_clone();
        env.set_time(expires_at(&env, &month));
        let before = snapshot(&env, &month);
        let close = env.close_ix(&closer.pubkey(), MONTH);
        expect_err(env.send_paid_by(&closer, &[close], &[]), code(DividendsError::UnsupportedVersion));
        assert_eq!(snapshot(&env, &month), before, "account {which}");
    }
}

#[test]
fn stray_usdc_goes_back_with_the_rest_and_stray_lamports_with_the_rents() {
    let mut env = Env::new();
    env.write_config();
    let month = env.write_month(MONTH, 2);
    let payer_usdc = env.usdc_account(&env.payer.pubkey());
    // Anyone may send USDC to the vault and lamports to the distributor.
    env.mint_usdc(&month.vault, 7 * USDC);
    env.svm.airdrop(&month.distributor, 1_000_000).unwrap();
    let closer = env.closer.insecure_clone();
    env.set_time(expires_at(&env, &month));
    let rents = env.lamports(&month.vault) + env.lamports(&month.distributor);
    let payer_lamports = env.lamports(&env.payer.pubkey());
    let close = env.close_ix(&closer.pubkey(), MONTH);
    let sent = env.send_logged_paid_by(&closer, &[close], &[]).unwrap();
    assert_eq!(env.token_amount(&payer_usdc), month.total + 7 * USDC);
    assert_eq!(event::<UsdcDistributorClosed>(&sent.logs).returned, month.total + 7 * USDC);
    assert_eq!(env.lamports(&env.payer.pubkey()) - payer_lamports, rents);
}

#[test]
fn another_funder_account_rent_payer_record_vault_or_mint_is_refused() {
    let mut env = Env::new();
    env.write_config();
    let month = env.write_month(MONTH, 2);
    let other = env.write_month(MONTH + 1, 2);
    env.usdc_account(&env.payer.pubkey());
    let closer = env.closer.insecure_clone();
    let closer_usdc = env.usdc_account(&closer.pubkey());
    let other_mint = env.second_mint();
    let distributors_other = env.token_ata_of(&month.distributor, &other_mint);
    env.set_time(expires_at(&env, &month));
    let base = env.settle_accounts(&env.mint, MONTH);
    let cases: [(SettleAccounts, u32); 6] = [
        (SettleAccounts { funder_usdc_account: closer_usdc, ..base.clone() }, code(DividendsError::AccountMismatch)),
        (SettleAccounts { rent_payer: closer.pubkey(), ..base.clone() }, code(DividendsError::AccountMismatch)),
        (SettleAccounts { month_record: month_record_pda(&env.mint, MONTH + 1), ..base.clone() }, anchor_code(ErrorCode::ConstraintSeeds)),
        (SettleAccounts { vault: other.vault, ..base.clone() }, anchor_code(ErrorCode::ConstraintTokenOwner)),
        // Another mint with the USDC vault: the vault's own check catches it first.
        (SettleAccounts { usdc_mint: other_mint, ..base.clone() }, anchor_code(ErrorCode::ConstraintAssociated)),
        (SettleAccounts { usdc_mint: other_mint, vault: distributors_other, ..base.clone() }, code(DividendsError::AccountMismatch)),
    ];
    for (accounts, refusal) in cases {
        let before = (snapshot(&env, &month), snapshot(&env, &other));
        let close = env.close_ix_with(&closer.pubkey(), &accounts);
        expect_err(env.send_paid_by(&closer, &[close], &[]), refusal);
        assert_eq!((snapshot(&env, &month), snapshot(&env, &other)), before);
    }
}

#[test]
fn a_funder_account_its_owner_closed_is_opened_again_in_the_closes_own_transaction() {
    let mut env = Env::new();
    env.write_config();
    let month = env.write_month(MONTH, 2);
    let payer = env.payer.insecure_clone();
    // The funder opened its USDC account and closed it again.
    let payer_usdc = env.usdc_account(&payer.pubkey());
    let close_account = spl_token::instruction::close_account(&spl_token::ID, &payer_usdc, &payer.pubkey(), &payer.pubkey(), &[]).unwrap();
    env.setup(&[close_account], &[&payer]);
    assert!(!env.exists(&payer_usdc));
    let closer = env.closer.insecure_clone();
    env.set_time(expires_at(&env, &month));
    // Alone, the close has no account to pay: the token program refuses the transfer.
    let before = snapshot(&env, &month);
    let close = env.close_ix(&closer.pubkey(), MONTH);
    match env.send_paid_by(&closer, &[close.clone()], &[]) {
        Err(TransactionError::InstructionError(0, InstructionError::InvalidAccountData)) => {}
        other => panic!("expected the token program's InvalidAccountData, got {other:?}"),
    }
    assert_eq!(snapshot(&env, &month), before);
    // Opened again by create_idempotent in the same transaction, paid by its sender.
    let reopen = env.reopen_usdc_ix(&closer.pubkey(), &payer.pubkey());
    let closer_lamports = env.lamports(&closer.pubkey());
    env.send_paid_by(&closer, &[reopen, close], &[]).unwrap();
    assert_eq!(env.token_amount(&payer_usdc), month.total);
    assert!(closed(&env, &month));
    assert_eq!(closer_lamports - env.lamports(&closer.pubkey()), env.rent(TOKEN_ACCOUNT_LEN) + FEE_PER_SIGNATURE);
}

#[test]
fn a_frozen_funder_account_or_vault_keeps_the_month_open_until_thawed() {
    let mut env = Env::new();
    env.write_config();
    let month = env.write_month(MONTH, 2);
    let payer_usdc = env.usdc_account(&env.payer.pubkey());
    let closer = env.closer.insecure_clone();
    env.set_time(expires_at(&env, &month));
    for frozen in [payer_usdc, month.vault] {
        env.freeze_usdc(&frozen);
        let before = snapshot(&env, &month);
        let close = env.close_ix(&closer.pubkey(), MONTH);
        expect_err(env.send_paid_by(&closer, &[close], &[]), SPL_TOKEN_ACCOUNT_FROZEN);
        assert_eq!(snapshot(&env, &month), before);
        env.thaw_usdc(&frozen);
    }
    let close = env.close_ix(&closer.pubkey(), MONTH);
    env.send_paid_by(&closer, &[close], &[]).unwrap();
    assert_eq!(env.token_amount(&payer_usdc), month.total);
}

#[test]
fn the_compute_units_of_a_close_from_one_leaf_to_a_full_registry() {
    for leaf_count in [1u32, 65_536] {
        let mut env = Env::new();
        env.write_config();
        let payer = env.payer.pubkey();
        let written = env.usdc_distributor(env.mint, MONTH, payer, [0; 32], 10 * USDC, leaf_count);
        env.put_funded_month(&written);
        let payer_usdc = env.usdc_account(&payer);
        let closer = env.closer.insecure_clone();
        env.set_time(written.expires_at);
        let close = env.close_ix(&closer.pubkey(), MONTH);
        let sent = env.send_logged_paid_by(&closer, &[close], &[]).unwrap();
        println!("close_distributor, {leaf_count} leaves: {} compute units", sent.compute_units);
        assert_eq!(env.token_amount(&payer_usdc), 10 * USDC);
        assert!(sent.compute_units < 200_000);
    }
}
