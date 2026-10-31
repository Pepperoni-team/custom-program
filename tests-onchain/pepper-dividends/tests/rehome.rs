//! `rehome_distributor` (the internal audit of 2026-10-29, its one confirmed finding on this program:
//! `close_distributor.rs:78`).
//!
//! A funding records the month's funder, the USDC account its leftovers go back to and its rent
//! payer, and the close and the void pay those for good: after the config's `payer_funder` is
//! rotated, on a leak or as planned, every open month still pays the retired key, and that key can
//! still sign its close. (a) pins that hazard as the program still has it without a re-home; (b) the
//! re-home, the admin's alone, points a Fanex month's three payees at the config's payer as it stands,
//! and only the rent payer of a creator's month, whose leftovers stay the creator's. Then the
//! runbook after a leak: an hour's claims pause at once, which holds every expired month's close
//! while the vault rotates the payer and re-homes the months in one transaction. Every refusal is
//! read by its code with the month left as it was.

mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use pepper_dividends::constants::{CLAIMS_OPEN_BEFORE_CLOSE_SECONDS, MONTH_STATUS_VOIDED};
use pepper_dividends::errors::DividendsError;
use pepper_dividends::events::{ConfigUpdated, UsdcDistributorRehomed};
use pepper_dividends::instructions::ConfigUpdate;
use pepper_dividends::state::{DividendsConfig, MonthRecord, UsdcDistributor};
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};

const HOUR: i64 = 3_600;
/// The upgrade Squad's time lock (MULTISIG.md §5).
const TIME_LOCK: i64 = 48 * HOUR;

/// A month of `n` holders funded through `init_distributor` by `funder`, the dividend payer paying
/// the rents, every holder's USDC account open. Its review window still runs.
fn funded_month(env: &mut Env, funder: &Keypair, month: i64, n: usize) -> (Month, Funding) {
    let distributor = distributor_pda(&env.mint, month);
    let holders = holders(n);
    let tree = build_tree(&dividend_leaves(&distributor, &env.usdc.mint, &holders));
    let total: u64 = holders.iter().map(|(_, amount)| amount).sum();
    let usdc = env.usdc_account(&funder.pubkey());
    env.mint_usdc(&usdc, total);
    let base = env.funding(&funder.pubkey(), tree.root, total, n as u32);
    let funding = Funding { month, legacy_distributor: legacy_distributor_pda(&env.mint, month), ..base };
    env.fund(&funding, funder).unwrap();
    for (holder, _) in &holders {
        env.usdc_account(&holder.pubkey());
    }
    (Month { mint: env.mint, month, distributor, vault: env.usdc_ata(&distributor), holders, tree, total }, funding)
}

/// The config's `payer_funder` handed to `next` by the admin.
fn rotation(env: &Env, next: &Pubkey) -> solana_sdk::instruction::Instruction {
    env.update_config_ix(&env.admin.pubkey(), None, ConfigUpdate { payer_funder: Some(*next), ..ConfigUpdate::default() })
}

fn distributor(env: &Env, month: &Month) -> UsdcDistributor {
    env.fetch(&month.distributor)
}

fn distributor_bytes(env: &Env, month: &Month) -> Vec<u8> {
    env.svm.get_account(&month.distributor).expect("the distributor").data
}

/// The bytes a re-home leaves as they were: everything but the funder, its USDC account and the
/// rent payer (98 to 194).
fn untouched_by_a_rehome(bytes: &[u8]) -> (Vec<u8>, Vec<u8>) {
    (bytes[..98].to_vec(), bytes[194..].to_vec())
}

/// The env with the config born by `initialize_config`, a month the dividend payer funded through
/// the instruction, its review window passed, its first holder paid: a void now fails.
fn a_payer_month_with_a_claim() -> (Env, Month, Funding) {
    let mut env = Env::new();
    env.init_config().unwrap();
    let payer = env.payer.insecure_clone();
    let (month, funding) = funded_month(&mut env, &payer, MONTH, 3);
    env.past_review_window();
    let sponsor = env.sponsor.insecure_clone();
    let claim = env.claim_ix(&month, 0);
    env.send_paid_by(&sponsor, &[claim], &[]).unwrap();
    let admin = env.admin.insecure_clone();
    let void = env.void_ix(&admin.pubkey(), MONTH);
    expect_err(env.send_paid_by(&admin, &[void], &[]), code(DividendsError::MonthHasClaims));
    (env, month, funding)
}

#[test]
fn a_without_a_rehome_a_rotated_payers_months_still_close_to_the_retired_key() {
    // (a) The hazard, pinned: the payer rotated, nothing re-homed. At the expiry, whether the closer
    // or the retired key signs, the leftovers go to the retired key's USDC account and both rents
    // to the retired key, which still signs the close.
    for retired_signs in [false, true] {
        let (mut env, month, funding) = a_payer_month_with_a_claim();
        let retired = env.payer.insecure_clone();
        let next = env.funded_wallet(SOL);
        let admin = env.admin.insecure_clone();
        let rotate = rotation(&env, &next.pubkey());
        env.send_paid_by(&admin, &[rotate], &[]).unwrap();
        assert_eq!(env.fetch::<DividendsConfig>(&config_pda()).payer_funder, next.pubkey());
        let recorded = distributor(&env, &month);
        assert_eq!((recorded.funder, recorded.rent_payer), (retired.pubkey(), retired.pubkey()));

        env.set_time(funding.expires_at);
        let leftovers = month.total - month.holders[0].1;
        let rents = env.lamports(&month.vault) + env.lamports(&month.distributor);
        let (retired_usdc, next_usdc) = (env.usdc_ata(&retired.pubkey()), env.usdc_ata(&next.pubkey()));
        let (retired_lamports, next_lamports) = (env.lamports(&retired.pubkey()), env.lamports(&next.pubkey()));
        let signer = if retired_signs { retired.insecure_clone() } else { env.closer.insecure_clone() };
        let close = env.close_ix(&signer.pubkey(), MONTH);
        // The closer pays the fee; when the retired key signs, it pays its own.
        let sponsor = env.sponsor.insecure_clone();
        env.send_paid_by(&sponsor, &[close], &[&signer]).unwrap();
        assert_eq!(env.token_amount(&retired_usdc), leftovers, "the leftovers went to the retired key");
        assert_eq!(env.lamports(&retired.pubkey()) - retired_lamports, rents, "and both rents");
        assert!(!env.exists(&next_usdc));
        assert_eq!(env.lamports(&next.pubkey()), next_lamports);
    }
}

#[test]
fn b_a_rehome_points_a_fanex_months_three_payees_at_the_configs_payer_and_its_close_pays_it() {
    // (b) The fix: the admin re-homes the month to the payer the config names; the retired key can
    // no longer close it, and the close pays the new payer, opening its USDC account in its own
    // transaction.
    let (mut env, month, funding) = a_payer_month_with_a_claim();
    let retired = env.payer.insecure_clone();
    let next = env.funded_wallet(SOL);
    let admin = env.admin.insecure_clone();
    let rotate = rotation(&env, &next.pubkey());
    env.send_paid_by(&admin, &[rotate], &[]).unwrap();
    let before = untouched_by_a_rehome(&distributor_bytes(&env, &month));
    let rehome = env.rehome_ix(&admin.pubkey(), &month.distributor);
    let sent = env.send_logged_paid_by(&admin, &[rehome], &[]).unwrap();
    let homed = distributor(&env, &month);
    let next_usdc = env.usdc_ata(&next.pubkey());
    assert_eq!((homed.funder, homed.funder_usdc_account, homed.rent_payer), (next.pubkey(), next_usdc, next.pubkey()));
    // Nothing else moved: the root, the amounts, the bitmap, the expiry, the hold, the flags.
    assert_eq!(untouched_by_a_rehome(&distributor_bytes(&env, &month)), before);
    let event = event::<UsdcDistributorRehomed>(&sent.logs);
    assert_eq!(
        (event.mint, event.month, event.funder, event.funder_usdc_account, event.rent_payer),
        (env.mint, MONTH, next.pubkey(), next_usdc, next.pubkey())
    );
    println!("rehome_distributor, a month of 3 leaves: {} compute units", sent.compute_units);

    env.set_time(funding.expires_at);
    // The retired key is no closer of it any more.
    let by_retired = env.close_ix(&retired.pubkey(), MONTH);
    expect_err(env.send_paid_by(&retired, &[by_retired], &[]), code(DividendsError::Unauthorized));
    // The closer's close pays the new payer: its USDC account opened in the close's own transaction.
    let closer = env.closer.insecure_clone();
    let leftovers = month.total - month.holders[0].1;
    let rents = env.lamports(&month.vault) + env.lamports(&month.distributor);
    let retired_usdc = env.usdc_ata(&retired.pubkey());
    let (retired_usdc_before, retired_lamports) = (env.token_amount(&retired_usdc), env.lamports(&retired.pubkey()));
    let next_lamports = env.lamports(&next.pubkey());
    let reopen = env.reopen_usdc_ix(&closer.pubkey(), &next.pubkey());
    let close = env.close_ix(&closer.pubkey(), MONTH);
    env.send_paid_by(&closer, &[reopen, close], &[]).unwrap();
    assert_eq!(env.token_amount(&next_usdc), leftovers);
    assert_eq!(env.lamports(&next.pubkey()) - next_lamports, rents);
    assert_eq!((env.token_amount(&retired_usdc), env.lamports(&retired.pubkey())), (retired_usdc_before, retired_lamports));
}

#[test]
fn a_creators_month_moves_its_rent_payer_and_never_its_funder_or_its_leftovers() {
    let mut env = Env::new();
    env.init_config().unwrap();
    let creator = env.creator.insecure_clone();
    let (month, funding) = funded_month(&mut env, &creator, MONTH, 2);
    let next = env.funded_wallet(SOL);
    let admin = env.admin.insecure_clone();
    let creator_usdc = env.usdc_ata(&creator.pubkey());
    let rotate = rotation(&env, &next.pubkey());
    let rehome = env.rehome_ix(&admin.pubkey(), &month.distributor);
    env.send_paid_by(&admin, &[rotate, rehome], &[]).unwrap();
    let homed = distributor(&env, &month);
    assert_eq!((homed.funded_by_fanex, homed.rents_by_payer), (false, true));
    assert_eq!((homed.funder, homed.funder_usdc_account, homed.rent_payer), (creator.pubkey(), creator_usdc, next.pubkey()));
    // At the expiry, nothing claimed: the creator's USDC back to the creator, the rents to the payer.
    env.set_time(funding.expires_at);
    let rents = env.lamports(&month.vault) + env.lamports(&month.distributor);
    let next_lamports = env.lamports(&next.pubkey());
    let close = env.close_ix(&creator.pubkey(), MONTH);
    env.send_paid_by(&creator, &[close], &[]).unwrap();
    assert_eq!(env.token_amount(&creator_usdc), month.total);
    assert_eq!(env.lamports(&next.pubkey()) - next_lamports, rents);
}

#[test]
fn after_a_leak_an_hours_pause_holds_every_close_while_the_vault_rotates_and_rehomes_in_one_transaction() {
    // The runbook (KEY-ROTATION.md, the program doc §9): the payer's key leaks with three months
    // open, each expired a day ago, two the payer's and one pepper_funder's. The closer cron is
    // stopped; the pauser pauses claims for an hour at once, so no month that is not fully claimed
    // may close for the next 30 days, whoever signs; the vault writes the rotation and the three
    // re-homes; executed 48 hours later, every month pays the new payer, and the closes resume.
    let mut env = Env::new();
    env.init_config().unwrap();
    let thief = env.payer.insecure_clone();
    let (first, funding) = funded_month(&mut env, &thief, MONTH, 2);
    let (second, _) = funded_month(&mut env, &thief, MONTH + 1, 2);
    let fanex = env.pepper_funder.insecure_clone();
    let (fanex_month, _) = funded_month(&mut env, &fanex, MONTH + 2, 2);
    env.set_time(funding.expires_at + DAY);
    let t = env.now();
    // At once: the pauser's hour.
    let pauser = env.pauser.insecure_clone();
    let pause = env.pause_claims_ix(&pauser.pubkey(), t + HOUR);
    env.send_paid_by(&pauser, &[pause], &[]).unwrap();
    // The thief tries the first month's close right then: refused.
    let by_thief = env.close_ix(&thief.pubkey(), MONTH);
    expect_err(env.send_paid_by(&thief, &[by_thief.clone()], &[]), code(DividendsError::ClaimsPausedRecently));
    // An hour on, claims open again, and the thief's close is still refused.
    env.set_time(t + HOUR);
    let sponsor = env.sponsor.insecure_clone();
    let claim = env.claim_ix(&first, 0);
    env.send_paid_by(&sponsor, &[claim], &[]).unwrap();
    expect_err(env.send_paid_by(&thief, &[by_thief.clone()], &[]), code(DividendsError::ClaimsPausedRecently));
    // The vault's transaction, written then: the rotation, then every open month re-homed.
    let next = env.funded_wallet(SOL);
    let admin = env.admin.insecure_clone();
    let vault_transaction = [
        rotation(&env, &next.pubkey()),
        env.rehome_ix(&admin.pubkey(), &first.distributor),
        env.rehome_ix(&admin.pubkey(), &second.distributor),
        env.rehome_ix(&admin.pubkey(), &fanex_month.distributor),
    ];
    env.set_time(t + HOUR + TIME_LOCK + 1);
    let sent = env.send_logged_paid_by(&admin, &vault_transaction, &[]).unwrap();
    assert_eq!(event::<ConfigUpdated>(&sent.logs).payer_funder, next.pubkey());
    assert_eq!(events_of::<UsdcDistributorRehomed>(&sent.logs).len(), 3);
    // pepper_funder's month moves to the payer too: both keys are Fanex's.
    for month in [&first, &second, &fanex_month] {
        let homed = distributor(&env, month);
        assert_eq!((homed.funder, homed.rent_payer), (next.pubkey(), next.pubkey()), "month {}", month.month);
    }
    // From then on the thief's close naming the payees it knew meets the month's new ones, and one
    // naming the new payees meets a signer that is no closer of the month.
    expect_err(env.send_paid_by(&thief, &[by_thief], &[]), code(DividendsError::AccountMismatch));
    let renamed = env.close_ix(&thief.pubkey(), MONTH);
    expect_err(env.send_paid_by(&thief, &[renamed], &[]), code(DividendsError::Unauthorized));
    // The closes resume once the pause's 30 days are over, to the second, and pay the new payer.
    let closer = env.closer.insecure_clone();
    let reopen = env.reopen_usdc_ix(&closer.pubkey(), &next.pubkey());
    let close = env.close_ix(&closer.pubkey(), MONTH);
    env.set_time(t + HOUR + CLAIMS_OPEN_BEFORE_CLOSE_SECONDS - 1);
    expect_err(env.send_paid_by(&closer, &[reopen.clone(), close.clone()], &[]), code(DividendsError::ClaimsPausedRecently));
    env.set_time(t + HOUR + CLAIMS_OPEN_BEFORE_CLOSE_SECONDS);
    env.send_paid_by(&closer, &[reopen, close], &[]).unwrap();
    assert_eq!(env.token_amount(&env.usdc_ata(&next.pubkey())), first.total - first.holders[0].1);
    assert_eq!(env.token_amount(&env.usdc_ata(&thief.pubkey())), 0);
}

#[test]
fn a_void_after_a_rehome_returns_the_whole_total_to_the_new_payer() {
    // Before a re-home a void after a leak would hand the thief the month's whole total: the
    // runbook never voids until the month is re-homed.
    let mut env = Env::new();
    env.init_config().unwrap();
    let payer = env.payer.insecure_clone();
    let (month, _) = funded_month(&mut env, &payer, MONTH, 2);
    let next = env.funded_wallet(SOL);
    let admin = env.admin.insecure_clone();
    let rotate = rotation(&env, &next.pubkey());
    let rehome = env.rehome_ix(&admin.pubkey(), &month.distributor);
    env.send_paid_by(&admin, &[rotate, rehome], &[]).unwrap();
    let reopen = env.reopen_usdc_ix(&admin.pubkey(), &next.pubkey());
    let void = env.void_ix(&admin.pubkey(), MONTH);
    env.send_paid_by(&admin, &[reopen, void], &[]).unwrap();
    assert_eq!(env.token_amount(&env.usdc_ata(&next.pubkey())), month.total);
    assert_eq!(env.token_amount(&env.usdc_ata(&payer.pubkey())), 0);
    assert_eq!(env.fetch::<MonthRecord>(&month_record_pda(&env.mint, MONTH)).status, MONTH_STATUS_VOIDED);
}

#[test]
fn every_key_but_the_admin_is_refused_a_rehome() {
    let (mut env, month, _) = a_payer_month_with_a_claim();
    let next = env.funded_wallet(SOL);
    let admin = env.admin.insecure_clone();
    let rotate = rotation(&env, &next.pubkey());
    env.send_paid_by(&admin, &[rotate], &[]).unwrap();
    let stranger = env.funded_wallet(SOL);
    let before = distributor_bytes(&env, &month);
    // The month's own funder, the new payer and the closer among them.
    for key in [
        env.payer.insecure_clone(),
        next.insecure_clone(),
        env.closer.insecure_clone(),
        env.pauser.insecure_clone(),
        env.approver.insecure_clone(),
        env.pepper_funder.insecure_clone(),
        env.sponsor.insecure_clone(),
        env.creator.insecure_clone(),
        stranger,
    ] {
        let rehome = env.rehome_ix(&key.pubkey(), &month.distributor);
        expect_err(env.send_paid_by(&key, &[rehome], &[]), code(DividendsError::Unauthorized));
    }
    assert_eq!(distributor_bytes(&env, &month), before);
}

#[test]
fn a_month_funded_before_the_fix_and_a_config_naming_no_payer_are_not_rehomable() {
    let mut env = Env::new();
    env.write_config();
    let month = env.write_month(MONTH, 2);
    let admin = env.admin.insecure_clone();
    // As every month funded before 2026-10-29 reads: neither flag.
    let mut written: UsdcDistributor = env.fetch(&month.distributor);
    written.funded_by_fanex = false;
    written.rents_by_payer = false;
    env.put_distributor(&written);
    let before = distributor_bytes(&env, &month);
    let rehome = env.rehome_ix(&admin.pubkey(), &month.distributor);
    expect_err(env.send_paid_by(&admin, &[rehome.clone()], &[]), code(DividendsError::NotRehomable));
    assert_eq!(distributor_bytes(&env, &month), before);
    // A month the payer funded, with a config naming no payer: its USDC account nobody could spend.
    written.funded_by_fanex = true;
    written.rents_by_payer = true;
    env.put_distributor(&written);
    let nobody = DividendsConfig { payer_funder: Pubkey::default(), ..env.dividends_config() };
    env.put_config(&nobody);
    let before = distributor_bytes(&env, &month);
    expect_err(env.send_paid_by(&admin, &[rehome], &[]), code(DividendsError::NotRehomable));
    assert_eq!(distributor_bytes(&env, &month), before);
}

#[test]
fn a_rehome_where_the_month_is_homed_already_writes_it_as_it_is() {
    let mut env = Env::new();
    env.write_config();
    let month = env.write_month(MONTH, 2);
    let admin = env.admin.insecure_clone();
    let before = distributor_bytes(&env, &month);
    let rehome = env.rehome_ix(&admin.pubkey(), &month.distributor);
    let sent = env.send_logged_paid_by(&admin, &[rehome], &[]).unwrap();
    assert_eq!(distributor_bytes(&env, &month), before);
    assert_eq!(event::<UsdcDistributorRehomed>(&sent.logs).funder, env.payer.pubkey());
}

#[test]
fn a_closed_month_another_programs_account_and_a_month_of_another_layout_are_refused() {
    let mut env = Env::new();
    env.write_config();
    let month = env.write_month(MONTH, 2);
    let admin = env.admin.insecure_clone();
    // A later layout of the month.
    let mut later: UsdcDistributor = env.fetch(&month.distributor);
    later.version = 2;
    env.put_distributor(&later);
    let rehome = env.rehome_ix(&admin.pubkey(), &month.distributor);
    expect_err(env.send_paid_by(&admin, &[rehome], &[]), code(DividendsError::UnsupportedVersion));
    // The month's bytes under another program's ownership.
    let lookalike = Pubkey::new_unique();
    let bytes = distributor_bytes(&env, &month);
    env.put(lookalike, bytes, pepper_curve::ID);
    let rehome = env.rehome_ix(&admin.pubkey(), &lookalike);
    expect_err(env.send_paid_by(&admin, &[rehome], &[]), anchor_code(ErrorCode::AccountOwnedByWrongProgram));
    // A month closed: its distributor is gone.
    let current: UsdcDistributor = UsdcDistributor { version: 1, ..later };
    env.put_distributor(&current);
    env.usdc_account(&env.payer.pubkey());
    let closer = env.closer.insecure_clone();
    env.set_time(current.expires_at);
    let close = env.close_ix(&closer.pubkey(), MONTH);
    env.send_paid_by(&closer, &[close], &[]).unwrap();
    let rehome = env.rehome_ix(&admin.pubkey(), &month.distributor);
    expect_err(env.send_paid_by(&admin, &[rehome], &[]), anchor_code(ErrorCode::AccountNotInitialized));
}

#[test]
fn the_compute_units_of_a_rehome_up_to_a_full_registry() {
    for leaf_count in [1u32, 65_536] {
        let mut env = Env::new();
        env.write_config();
        let payer = env.payer.pubkey();
        let written = env.usdc_distributor(env.mint, MONTH, payer, [0; 32], 10 * USDC, leaf_count);
        let address = env.put_funded_month(&written);
        let next = Keypair::new();
        let config = DividendsConfig { payer_funder: next.pubkey(), ..env.dividends_config() };
        env.put_config(&config);
        let admin = env.admin.insecure_clone();
        let rehome = env.rehome_ix(&admin.pubkey(), &address);
        let sent = env.send_logged_paid_by(&admin, &[rehome], &[]).unwrap();
        println!("rehome_distributor, {leaf_count} leaves: {} compute units", sent.compute_units);
        assert_eq!(env.fetch::<UsdcDistributor>(&address).funder, next.pubkey());
        assert!(sent.compute_units < 50_000);
    }
}
