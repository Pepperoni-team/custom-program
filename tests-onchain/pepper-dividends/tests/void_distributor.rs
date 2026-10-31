//! `void_distributor` (spec §4.8; track B's B9; Rick, 2026-10-23).
//!
//! A month funded wrong is voided by the admin, the upgrade Squad's vault, while nobody has claimed
//! from it: its money and its rents go back as at a close, through the same `settle_month`, and its
//! record turns voided, so the month is funded again under the right root. One claim, or either
//! half of one in the bank, makes a month unvoidable. The procedure: the pauser pauses claims at
//! once, and the void and the resume execute together when the vault's 48 hours have passed. Every
//! refusal is read by its code, the month, the funder's USDC and the rent payer's lamports left as
//! they were.

mod common;

use anchor_lang::error::ErrorCode;
use anchor_spl::token::spl_token;
use common::*;
use pepper_dividends::constants::{MONTH_STATUS_CLOSED, MONTH_STATUS_OPEN, MONTH_STATUS_VOIDED, REVIEW_WINDOW_SECONDS};
use pepper_dividends::errors::DividendsError;
use pepper_dividends::events::{ClaimsPaused, UsdcDistributorVoided};
use pepper_dividends::math::dividend_leaf;
use pepper_dividends::state::{MonthRecord, UsdcDistributor};
use solana_sdk::{
    instruction::InstructionError,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::TransactionError,
};

const HOUR: i64 = 3_600;
/// The upgrade Squad's time lock (MULTISIG.md §5).
const TIME_LOCK: i64 = 48 * HOUR;
/// A token account's size: the vault's, and a funder's USDC account opened again.
const TOKEN_ACCOUNT_LEN: usize = 165;

/// The month's three accounts, the dividend payer's USDC account (the funder of every month here)
/// and its lamports (the rent payer's): what a refused void leaves as they were.
fn snapshot(env: &Env, month: &Month) -> (Vec<Option<Vec<u8>>>, u64) {
    let payer = env.payer.pubkey();
    let accounts = [month.distributor, month_record_pda(&month.mint, month.month), month.vault, env.usdc_ata(&payer)];
    let data = accounts.iter().map(|address| env.svm.get_account(address).filter(|a| a.lamports > 0).map(|a| a.data)).collect();
    (data, env.lamports(&payer))
}

fn settled(env: &Env, month: &Month) -> bool {
    !env.exists(&month.distributor) && !env.exists(&month.vault)
}

fn record(env: &Env, mint: &Pubkey, month: i64) -> MonthRecord {
    env.fetch(&month_record_pda(mint, month))
}

/// A funding of `holders` by the dividend payer, its µUSDC minted first, under the leaves of
/// `amounts` (the holders' own, or wrong ones). Returns the tree it was funded under.
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

#[test]
fn every_key_but_the_admin_is_refused_a_void() {
    let mut env = Env::new();
    env.write_config();
    let month = env.write_month(MONTH, 2);
    env.usdc_account(&env.payer.pubkey());
    let (sponsor, stranger) = (env.sponsor.insecure_clone(), env.funded_wallet(SOL));
    // The month's own funder and the closer, which may close it, may not void it. The sponsor pays
    // each refused fee, so the rent payer's lamports stay as they were.
    for key in [
        env.payer.insecure_clone(),
        env.closer.insecure_clone(),
        env.pauser.insecure_clone(),
        env.approver.insecure_clone(),
        env.pepper_funder.insecure_clone(),
        env.sponsor.insecure_clone(),
        stranger,
    ] {
        let before = snapshot(&env, &month);
        let void = env.void_ix(&key.pubkey(), MONTH);
        expect_err(env.send_paid_by(&sponsor, &[void], &[&key]), code(DividendsError::Unauthorized));
        assert_eq!(snapshot(&env, &month), before);
    }
    let admin = env.admin.insecure_clone();
    let void = env.void_ix(&admin.pubkey(), MONTH);
    env.send_paid_by(&admin, &[void], &[]).unwrap();
    assert!(settled(&env, &month));
}

#[test]
fn one_claim_makes_a_month_unvoidable_and_so_does_either_half_of_one() {
    let mut env = Env::new();
    env.write_config();
    env.usdc_account(&env.payer.pubkey());
    let (admin, sponsor) = (env.admin.insecure_clone(), env.sponsor.insecure_clone());
    // One claim through the instruction.
    let claimed = env.write_month(MONTH, 3);
    env.usdc_account(&claimed.holders[1].0.pubkey());
    let claim = env.claim_ix(&claimed, 1);
    env.send_paid_by(&sponsor, &[claim], &[]).unwrap();
    // A claim's two halves apart, as only the bank could write them: a bit set with nothing
    // counted, and an amount counted with no bit set.
    let bit_only = env.write_month(MONTH + 1, 3);
    let mut written: UsdcDistributor = env.fetch(&bit_only.distributor);
    written.claimed[0] = 0b100;
    env.put_distributor(&written);
    let amount_only = env.write_month(MONTH + 2, 3);
    let mut written: UsdcDistributor = env.fetch(&amount_only.distributor);
    written.claimed_amount = 1;
    env.put_distributor(&written);
    for month in [&claimed, &bit_only, &amount_only] {
        let before = snapshot(&env, month);
        let void = env.void_ix(&admin.pubkey(), month.month);
        expect_err(env.send_paid_by(&admin, &[void], &[]), code(DividendsError::MonthHasClaims));
        assert_eq!(snapshot(&env, month), before, "month {}", month.month);
    }
}

#[test]
fn a_void_returns_the_money_and_the_rents_as_a_close_and_leaves_the_record_voided() {
    let mut env = Env::new();
    env.write_config();
    let month = env.write_month(MONTH, 3);
    let payer_usdc = env.usdc_account(&env.payer.pubkey());
    // A stray transfer into the vault goes back with the rest.
    env.mint_usdc(&month.vault, 2 * USDC);
    let admin = env.admin.insecure_clone();
    let record_address = month_record_pda(&month.mint, MONTH);
    let rents = env.lamports(&month.vault) + env.lamports(&month.distributor);
    let (payer_lamports, record_rent) = (env.lamports(&env.payer.pubkey()), env.lamports(&record_address));
    // Long before its expiry: a void waits for nothing.
    let void = env.void_ix(&admin.pubkey(), MONTH);
    let sent = env.send_logged_paid_by(&admin, &[void], &[]).unwrap();
    assert_eq!(env.token_amount(&payer_usdc), month.total + 2 * USDC);
    assert!(settled(&env, &month));
    assert_eq!(env.lamports(&env.payer.pubkey()) - payer_lamports, rents);
    let kept = record(&env, &month.mint, MONTH);
    assert_eq!((kept.status, kept.distributor), (MONTH_STATUS_VOIDED, month.distributor));
    assert_eq!(env.lamports(&record_address), record_rent, "the record keeps its rent");
    let event = event::<UsdcDistributorVoided>(&sent.logs);
    assert_eq!((event.mint, event.month, event.returned, event.to), (env.mint, MONTH, month.total + 2 * USDC, payer_usdc));
}

#[test]
fn another_funder_account_rent_payer_record_vault_or_mint_is_refused() {
    let mut env = Env::new();
    env.write_config();
    let month = env.write_month(MONTH, 2);
    let other = env.write_month(MONTH + 1, 2);
    env.usdc_account(&env.payer.pubkey());
    let admin = env.admin.insecure_clone();
    let admin_usdc = env.usdc_account(&admin.pubkey());
    let other_mint = env.second_mint();
    let distributors_other = env.token_ata_of(&month.distributor, &other_mint);
    let base = env.settle_accounts(&env.mint, MONTH);
    let cases: [(SettleAccounts, u32); 6] = [
        (SettleAccounts { funder_usdc_account: admin_usdc, ..base.clone() }, code(DividendsError::AccountMismatch)),
        (SettleAccounts { rent_payer: admin.pubkey(), ..base.clone() }, code(DividendsError::AccountMismatch)),
        (SettleAccounts { month_record: month_record_pda(&env.mint, MONTH + 1), ..base.clone() }, anchor_code(ErrorCode::ConstraintSeeds)),
        (SettleAccounts { vault: other.vault, ..base.clone() }, anchor_code(ErrorCode::ConstraintTokenOwner)),
        // Another mint with the USDC vault: the vault's own check catches it first.
        (SettleAccounts { usdc_mint: other_mint, ..base.clone() }, anchor_code(ErrorCode::ConstraintAssociated)),
        (SettleAccounts { usdc_mint: other_mint, vault: distributors_other, ..base.clone() }, code(DividendsError::AccountMismatch)),
    ];
    for (accounts, refusal) in cases {
        let before = (snapshot(&env, &month), snapshot(&env, &other));
        let void = env.void_ix_with(&admin.pubkey(), &accounts);
        expect_err(env.send_paid_by(&admin, &[void], &[]), refusal);
        assert_eq!((snapshot(&env, &month), snapshot(&env, &other)), before);
    }
}

#[test]
fn a_frozen_funder_account_blocks_the_void_until_thawed() {
    let mut env = Env::new();
    env.write_config();
    let month = env.write_month(MONTH, 2);
    let payer_usdc = env.usdc_account(&env.payer.pubkey());
    let admin = env.admin.insecure_clone();
    env.freeze_usdc(&payer_usdc);
    let before = snapshot(&env, &month);
    let void = env.void_ix(&admin.pubkey(), MONTH);
    expect_err(env.send_paid_by(&admin, &[void.clone()], &[]), SPL_TOKEN_ACCOUNT_FROZEN);
    assert_eq!(snapshot(&env, &month), before);
    env.thaw_usdc(&payer_usdc);
    env.send_paid_by(&admin, &[void], &[]).unwrap();
    assert_eq!(env.token_amount(&payer_usdc), month.total);
}

#[test]
fn a_funder_account_its_owner_closed_is_opened_again_in_the_voids_own_transaction() {
    let mut env = Env::new();
    env.write_config();
    let month = env.write_month(MONTH, 2);
    let payer = env.payer.insecure_clone();
    // The funder opened its USDC account and closed it again.
    let payer_usdc = env.usdc_account(&payer.pubkey());
    let close_account = spl_token::instruction::close_account(&spl_token::ID, &payer_usdc, &payer.pubkey(), &payer.pubkey(), &[]).unwrap();
    env.setup(&[close_account], &[&payer]);
    assert!(!env.exists(&payer_usdc));
    let admin = env.admin.insecure_clone();
    let before = snapshot(&env, &month);
    let void = env.void_ix(&admin.pubkey(), MONTH);
    match env.send_paid_by(&admin, &[void.clone()], &[]) {
        Err(TransactionError::InstructionError(0, InstructionError::InvalidAccountData)) => {}
        other => panic!("expected the token program's InvalidAccountData, got {other:?}"),
    }
    assert_eq!(snapshot(&env, &month), before);
    // Opened again by create_idempotent in the void's own transaction, paid by the vault.
    let reopen = env.reopen_usdc_ix(&admin.pubkey(), &payer.pubkey());
    let admin_lamports = env.lamports(&admin.pubkey());
    env.send_paid_by(&admin, &[reopen, void], &[]).unwrap();
    assert_eq!(env.token_amount(&payer_usdc), month.total);
    assert!(settled(&env, &month));
    assert_eq!(admin_lamports - env.lamports(&admin.pubkey()), env.rent(TOKEN_ACCOUNT_LEN) + FEE_PER_SIGNATURE);
}

#[test]
fn the_compute_units_of_a_void_up_to_a_full_8192_byte_registry() {
    for leaf_count in [1u32, 65_536] {
        let mut env = Env::new();
        env.write_config();
        let payer = env.payer.pubkey();
        let written = env.usdc_distributor(env.mint, MONTH, payer, [0; 32], 10 * USDC, leaf_count);
        env.put_funded_month(&written);
        let payer_usdc = env.usdc_account(&payer);
        let admin = env.admin.insecure_clone();
        let void = env.void_ix(&admin.pubkey(), MONTH);
        let sent = env.send_logged_paid_by(&admin, &[void], &[]).unwrap();
        println!("void_distributor, {leaf_count} leaves ({} bytes of registry): {} compute units", written.claimed.len(), sent.compute_units);
        assert_eq!(env.token_amount(&payer_usdc), 10 * USDC);
        assert!(sent.compute_units < 200_000);
    }
}

#[test]
fn a_voided_month_is_funded_again_under_another_root_claimed_and_closed() {
    let mut env = Env::new();
    env.init_config().unwrap();
    let (payer, admin, sponsor) = (env.payer.insecure_clone(), env.admin.insecure_clone(), env.sponsor.insecure_clone());
    let holders = holders(2);
    let total: u64 = holders.iter().map(|(_, amount)| amount).sum();
    // Funded wrong, every amount doubled, then voided: the payer's µUSDC back.
    let doubled: Vec<u64> = holders.iter().map(|(_, amount)| 2 * amount).collect();
    fund_month(&mut env, &holders, &doubled);
    let payer_usdc = env.usdc_ata(&payer.pubkey());
    let void = env.void_ix(&admin.pubkey(), MONTH);
    env.send_paid_by(&admin, &[void], &[]).unwrap();
    assert_eq!(env.token_amount(&payer_usdc), total);
    assert_eq!(record(&env, &env.mint, MONTH).status, MONTH_STATUS_VOIDED);
    // Funded again under the right root, the record open once more.
    let amounts: Vec<u64> = holders.iter().map(|(_, amount)| *amount).collect();
    let tree = fund_month(&mut env, &holders, &amounts);
    assert_eq!(record(&env, &env.mint, MONTH).status, MONTH_STATUS_OPEN);
    let distributor = distributor_pda(&env.mint, MONTH);
    let month = Month { mint: env.mint, month: MONTH, distributor, vault: env.usdc_ata(&distributor), holders, tree, total };
    env.past_review_window();
    for index in 0..2 {
        env.usdc_account(&month.holders[index].0.pubkey());
        let claim = env.claim_ix(&month, index);
        env.send_paid_by(&sponsor, &[claim], &[]).unwrap();
        assert_eq!(env.token_amount(&env.usdc_ata(&month.holders[index].0.pubkey())), month.holders[index].1);
    }
    // Fully claimed, its funder closes it at once, and the month is closed for good.
    let close = env.close_ix(&payer.pubkey(), MONTH);
    env.send_paid_by(&payer, &[close], &[]).unwrap();
    assert_eq!(record(&env, &env.mint, MONTH).status, MONTH_STATUS_CLOSED);
}

#[test]
fn a_month_funded_wrong_pays_no_claim_in_its_review_window_and_is_voided_within_it() {
    // The internal audit (2026-10-29): a month's claims opened at its funding's own slot, so a holder
    // of a doubled leaf could claim before the pauser reacted, and the month was no longer voidable.
    // Its claims now open a day after the funding: in that window every claim is refused, the
    // pauser needs no hold, and the void finds no claim.
    let mut env = Env::new();
    env.init_config().unwrap();
    let (admin, sponsor) = (env.admin.insecure_clone(), env.sponsor.insecure_clone());
    let holders = holders(2);
    let doubled: Vec<u64> = holders.iter().map(|(_, amount)| 2 * amount).collect();
    let funded_at = env.now();
    let wrong = fund_month(&mut env, &holders, &doubled);
    let distributor = distributor_pda(&env.mint, MONTH);
    assert_eq!(env.fetch::<UsdcDistributor>(&distributor).claims_held_until, funded_at + REVIEW_WINDOW_SECONDS);
    let holder = holders[0].0.insecure_clone();
    env.usdc_account(&holder.pubkey());
    let wrong_claim = ClaimAccounts {
        distributor,
        vault: env.usdc_ata(&distributor),
        claimant: holder.pubkey(),
        claimant_usdc: env.usdc_ata(&holder.pubkey()),
        usdc_mint: env.usdc.mint,
        token_program: spl_token::ID,
    };
    let claim = env.claim_ix_with(&wrong_claim, 0, doubled[0], wrong.proofs[0].clone());
    // `fund_month` funds the holders' own total under the doubled leaves.
    let total: u64 = holders.iter().map(|(_, amount)| amount).sum();
    let month = Month { mint: env.mint, month: MONTH, distributor, vault: env.usdc_ata(&distributor), holders, tree: wrong, total };
    // In the funding's own slot, and a second before the window ends: refused, the month untouched.
    for at in [funded_at, funded_at + REVIEW_WINDOW_SECONDS - 1] {
        env.set_time(at);
        let before = snapshot(&env, &month);
        expect_err(env.send_paid_by(&sponsor, &[claim.clone()], &[]), code(DividendsError::ClaimsHeld));
        assert_eq!(snapshot(&env, &month), before, "at {at}");
        assert_eq!(env.token_amount(&env.usdc_ata(&holder.pubkey())), 0);
    }
    // Still in the window, the void finds no claim and returns the whole total.
    let payer_usdc = env.usdc_ata(&env.payer.pubkey());
    let void = env.void_ix(&admin.pubkey(), MONTH);
    env.send_paid_by(&admin, &[void], &[]).unwrap();
    assert_eq!(record(&env, &env.mint, MONTH).status, MONTH_STATUS_VOIDED);
    assert_eq!(env.token_amount(&payer_usdc), month.total);
}

#[test]
fn the_void_procedure_end_to_end_the_pause_then_the_void_and_the_resume_together() {
    let mut env = Env::new();
    env.init_config().unwrap();
    let (admin, pauser, sponsor, closer) =
        (env.admin.insecure_clone(), env.pauser.insecure_clone(), env.sponsor.insecure_clone(), env.closer.insecure_clone());
    let t = env.now();
    // An older month that expires while the vault's proposal waits.
    let older = env.write_month(MONTH - 1, 2);
    let mut written: UsdcDistributor = env.fetch(&older.distributor);
    written.expires_at = t + DAY;
    env.put_distributor(&written);
    // This month funded wrong, every amount doubled: a holder's claim would drain the vault.
    let holders = holders(2);
    let doubled: Vec<u64> = holders.iter().map(|(_, amount)| 2 * amount).collect();
    let wrong = fund_month(&mut env, &holders, &doubled);
    // At once, the pauser holds every claim: one claim would make the month unvoidable.
    let pause = env.pause_claims_ix(&pauser.pubkey(), t + 7 * DAY);
    env.send_paid_by(&pauser, &[pause], &[]).unwrap();
    // The vault's transaction, written at T: the void, then the resume.
    let vault_transaction = [env.void_ix(&admin.pubkey(), MONTH), env.resume_claims_ix(&admin.pubkey())];
    // While it waits, the holder's claim of the doubled amount is refused.
    env.set_time(t + HOUR);
    let distributor = distributor_pda(&env.mint, MONTH);
    let holder = holders[0].0.insecure_clone();
    env.usdc_account(&holder.pubkey());
    let wrong_claim = ClaimAccounts {
        distributor,
        vault: env.usdc_ata(&distributor),
        claimant: holder.pubkey(),
        claimant_usdc: env.usdc_ata(&holder.pubkey()),
        usdc_mint: env.usdc.mint,
        token_program: spl_token::ID,
    };
    let claim = env.claim_ix_with(&wrong_claim, 0, doubled[0], wrong.proofs[0].clone());
    expect_err(env.send_paid_by(&sponsor, &[claim], &[]), code(DividendsError::ClaimsPaused));
    // Executed 48 hours and a second after: voided, and claims open again at that second.
    let reopened = t + TIME_LOCK + 1;
    env.set_time(reopened);
    let sent = env.send_logged_paid_by(&admin, &vault_transaction, &[]).unwrap();
    assert_eq!(record(&env, &env.mint, MONTH).status, MONTH_STATUS_VOIDED);
    assert_eq!(event::<UsdcDistributorVoided>(&sent.logs).month, MONTH);
    assert_eq!(event::<ClaimsPaused>(&sent.logs).until, reopened);
    // Funded again under the right root, and claimed once its review window has passed.
    let amounts: Vec<u64> = holders.iter().map(|(_, amount)| *amount).collect();
    let tree = fund_month(&mut env, &holders, &amounts);
    let total: u64 = amounts.iter().sum();
    let month = Month { mint: env.mint, month: MONTH, distributor, vault: env.usdc_ata(&distributor), holders, tree, total };
    env.past_review_window();
    let claim = env.claim_ix(&month, 0);
    env.send_paid_by(&sponsor, &[claim], &[]).unwrap();
    assert_eq!(env.token_amount(&env.usdc_ata(&holder.pubkey())), month.holders[0].1);
    // The older month expired during the wait: its close waits 30 days after the resume.
    let close = env.close_ix(&closer.pubkey(), MONTH - 1);
    env.set_time(reopened + 30 * DAY - 1);
    expect_err(env.send_paid_by(&closer, &[close.clone()], &[]), code(DividendsError::ClaimsPausedRecently));
    env.set_time(reopened + 30 * DAY);
    env.send_paid_by(&closer, &[close], &[]).unwrap();
    assert!(settled(&env, &older));
}
