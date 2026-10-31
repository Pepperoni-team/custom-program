//! `top_up_distributor(amount)` (the internal audit of 2026-10-29, `init_distributor.rs:152`).
//!
//! The program cannot check a root against its total: a month funded short of its leaves paid them
//! first come, first served until `ExceedsFundedTotal`, and once one claim had landed the void could
//! no longer fix it, nor could anything raise the total. The top-up moves more µUSDC from the month's
//! own funder into its vault, with the config's approver's signature as at the funding, and raises
//! the total by as much; the root stays, so what each leaf claims stays as funded, and whatever no
//! leaf claims goes back to the funder at the close. Every refusal is read by its code with the
//! vault, the month and the funder's USDC left as they were.

mod common;

use anchor_lang::error::ErrorCode;
use common::*;
use pepper_dividends::errors::DividendsError;
use pepper_dividends::events::UsdcDistributorToppedUp;
use pepper_dividends::state::{DividendsConfig, UsdcDistributor};
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};

/// What a refused top-up leaves as it was: the vault's USDC, the month's bytes, the payer's USDC.
fn snapshot(env: &Env, month: &Month) -> (u64, Vec<u8>, u64) {
    (
        env.token_amount(&month.vault),
        env.svm.get_account(&month.distributor).unwrap().data,
        env.token_amount(&env.usdc_ata(&env.payer.pubkey())),
    )
}

/// A month of three holders the dividend payer funded `short` µUSDC under its leaves, written as a
/// funding leaves it, every holder's account open, the payer holding `in_hand` µUSDC more.
fn short_month(short: u64, in_hand: u64) -> (Env, Month) {
    let mut env = Env::new();
    env.write_config();
    let month = env.write_month(MONTH, 3);
    let mut written: UsdcDistributor = env.fetch(&month.distributor);
    written.total_amount = month.total - short;
    env.put_funded_month(&written);
    for (holder, _) in &month.holders {
        env.usdc_account(&holder.pubkey());
    }
    let payer_usdc = env.usdc_account(&env.payer.pubkey());
    if in_hand > 0 {
        env.mint_usdc(&payer_usdc, in_hand);
    }
    (env, month)
}

#[test]
fn an_under_funded_month_is_topped_up_and_its_last_holder_paid() {
    let last = USDC + 2_000;
    let (mut env, month) = short_month(last, 5 * USDC);
    assert_eq!(month.holders[2].1, last);
    let (payer, sponsor) = (env.payer.insecure_clone(), env.sponsor.insecure_clone());
    for index in 0..2 {
        let claim = env.claim_ix(&month, index);
        env.send_paid_by(&sponsor, &[claim], &[]).unwrap();
    }
    // The last leaf is short: the total is claimed in full, and nothing could fill it but a void,
    // which two claims forbid.
    let claim = env.claim_ix(&month, 2);
    expect_err(env.send_paid_by(&sponsor, &[claim.clone()], &[]), code(DividendsError::ExceedsFundedTotal));
    let admin = env.admin.insecure_clone();
    let void = env.void_ix(&admin.pubkey(), MONTH);
    expect_err(env.send_paid_by(&admin, &[void], &[]), code(DividendsError::MonthHasClaims));
    // The payer tops it up by the missing amount, the approver co-signing.
    let approver = env.approver.insecure_clone();
    let payer_usdc = env.usdc_ata(&payer.pubkey());
    let (vault_before, payer_usdc_before) = (env.token_amount(&month.vault), env.token_amount(&payer_usdc));
    let top_up = env.top_up_ix(&payer.pubkey(), MONTH, last);
    let sent = env.send_logged_paid_by(&payer, &[top_up], &[&approver]).unwrap();
    println!("top_up_distributor: {} compute units", sent.compute_units);
    assert_eq!(env.token_amount(&month.vault), vault_before + last);
    assert_eq!(env.token_amount(&payer_usdc), payer_usdc_before - last);
    let d: UsdcDistributor = env.fetch(&month.distributor);
    assert_eq!(d.total_amount, month.total);
    let event = event::<UsdcDistributorToppedUp>(&sent.logs);
    assert_eq!((event.mint, event.month, event.amount, event.total_amount), (env.mint, MONTH, last, month.total));
    // The last holder is paid, and the month is fully claimed.
    env.send_paid_by(&sponsor, &[claim], &[]).unwrap();
    assert_eq!(env.token_amount(&env.usdc_ata(&month.holders[2].0.pubkey())), last);
    let d: UsdcDistributor = env.fetch(&month.distributor);
    assert_eq!((d.claimed_amount, d.total_amount, env.token_amount(&month.vault)), (month.total, month.total, 0));
}

#[test]
fn a_top_up_needs_the_months_own_funder_and_the_configs_approver() {
    let (mut env, month) = short_month(USDC, 5 * USDC);
    let (payer, approver) = (env.payer.insecure_clone(), env.approver.insecure_clone());
    // pepper_funder, another of Fanex's funders, holding USDC of its own: not this month's funder.
    let fanex = env.pepper_funder.insecure_clone();
    let fanex_usdc = env.usdc_account(&fanex.pubkey());
    env.mint_usdc(&fanex_usdc, 5 * USDC);
    let accounts = env.top_up_accounts(&env.mint, MONTH);
    let stranger = env.funded_wallet(SOL);
    for funder in [fanex.insecure_clone(), stranger, env.admin.insecure_clone(), env.closer.insecure_clone()] {
        let before = snapshot(&env, &month);
        let ix = env.top_up_ix_with(&funder.pubkey(), &approver.pubkey(), &accounts, USDC);
        expect_err(env.send_paid_by(&funder, &[ix], &[&approver]), code(DividendsError::Unauthorized));
        assert_eq!(snapshot(&env, &month), before);
    }
    // Another key co-signing as approver, the pauser and the funder itself among them.
    for other in [env.pauser.insecure_clone(), payer.insecure_clone(), Keypair::new()] {
        let before = snapshot(&env, &month);
        let ix = env.top_up_ix_with(&payer.pubkey(), &other.pubkey(), &accounts, USDC);
        expect_err(env.send_paid_by(&payer, &[ix], &[&other]), code(DividendsError::Unauthorized));
        assert_eq!(snapshot(&env, &month), before);
    }
    // The approver named and not signing.
    let mut unsigned = env.top_up_ix(&payer.pubkey(), MONTH, USDC);
    unsigned.accounts[1].is_signer = false;
    let before = snapshot(&env, &month);
    expect_err(env.send_paid_by(&payer, &[unsigned], &[]), anchor_code(ErrorCode::AccountNotSigner));
    assert_eq!(snapshot(&env, &month), before);
}

#[test]
fn a_creators_month_is_topped_up_by_its_creator_alone() {
    let mut env = Env::new();
    env.write_config();
    let creator = env.creator.insecure_clone();
    let written = env.usdc_distributor(env.mint, MONTH, creator.pubkey(), [7; 32], 10 * USDC, 4);
    env.put_funded_month(&written);
    let creator_usdc = env.usdc_account(&creator.pubkey());
    env.mint_usdc(&creator_usdc, 3 * USDC);
    let approver = env.approver.insecure_clone();
    // The dividend payer, which paid its rents, is no funder of it.
    let payer = env.payer.insecure_clone();
    let payer_usdc = env.usdc_account(&payer.pubkey());
    env.mint_usdc(&payer_usdc, 3 * USDC);
    let accounts = env.top_up_accounts(&env.mint, MONTH);
    let by_payer = env.top_up_ix_with(&payer.pubkey(), &approver.pubkey(), &accounts, USDC);
    expect_err(env.send_paid_by(&payer, &[by_payer], &[&approver]), code(DividendsError::Unauthorized));
    let top_up = env.top_up_ix(&creator.pubkey(), MONTH, 2 * USDC);
    env.send_paid_by(&creator, &[top_up], &[&approver]).unwrap();
    assert_eq!(env.fetch::<UsdcDistributor>(&distributor_pda(&env.mint, MONTH)).total_amount, 12 * USDC);
    assert_eq!(env.token_amount(&creator_usdc), USDC);
}

#[test]
fn a_zero_amount_an_overflowing_total_and_a_paused_funding_are_refused() {
    let (mut env, month) = short_month(USDC, 5 * USDC);
    let (payer, approver) = (env.payer.insecure_clone(), env.approver.insecure_clone());
    let zero = env.top_up_ix(&payer.pubkey(), MONTH, 0);
    let before = snapshot(&env, &month);
    expect_err(env.send_paid_by(&payer, &[zero], &[&approver]), code(DividendsError::ZeroAmount));
    assert_eq!(snapshot(&env, &month), before);
    // The funding paused: a top-up is a funding.
    let paused = DividendsConfig { paused: true, ..env.dividends_config() };
    env.put_config(&paused);
    let top_up = env.top_up_ix(&payer.pubkey(), MONTH, USDC);
    expect_err(env.send_paid_by(&payer, &[top_up.clone()], &[&approver]), code(DividendsError::Paused));
    assert_eq!(snapshot(&env, &month), before);
    env.write_config();
    // A total already at the edge of a u64: refused before any USDC moves.
    let mut written: UsdcDistributor = env.fetch(&month.distributor);
    written.total_amount = u64::MAX;
    env.put_distributor(&written);
    let before = snapshot(&env, &month);
    expect_err(env.send_paid_by(&payer, &[top_up], &[&approver]), code(DividendsError::MathOverflow));
    assert_eq!(snapshot(&env, &month), before);
}

#[test]
fn another_funder_account_vault_or_mint_and_a_month_of_another_layout_are_refused() {
    let (mut env, month) = short_month(USDC, 5 * USDC);
    let (payer, approver) = (env.payer.insecure_clone(), env.approver.insecure_clone());
    let other = env.write_month(MONTH + 1, 2);
    // Another USDC account the payer owns, not the one the month records.
    let elsewhere = Pubkey::new_unique();
    let (usdc, owner) = (env.usdc.mint, payer.pubkey());
    env.put_token_account(elsewhere, usdc, owner, 5 * USDC, anchor_lang::solana_program::program_option::COption::None);
    let other_mint = env.second_mint();
    let distributors_other = env.token_ata_of(&month.distributor, &other_mint);
    let base = env.top_up_accounts(&env.mint, MONTH);
    let cases: [(TopUpAccounts, u32); 4] = [
        (TopUpAccounts { funder_usdc_account: elsewhere, ..base.clone() }, code(DividendsError::AccountMismatch)),
        (TopUpAccounts { vault: other.vault, ..base.clone() }, anchor_code(ErrorCode::ConstraintTokenOwner)),
        (TopUpAccounts { usdc_mint: other_mint, ..base.clone() }, anchor_code(ErrorCode::ConstraintAssociated)),
        (TopUpAccounts { usdc_mint: other_mint, vault: distributors_other, ..base.clone() }, code(DividendsError::AccountMismatch)),
    ];
    for (accounts, refusal) in cases {
        let before = snapshot(&env, &month);
        let ix = env.top_up_ix_with(&payer.pubkey(), &approver.pubkey(), &accounts, USDC);
        expect_err(env.send_paid_by(&payer, &[ix], &[&approver]), refusal);
        assert_eq!(snapshot(&env, &month), before);
    }
    // A later layout of the month.
    let mut later: UsdcDistributor = env.fetch(&month.distributor);
    later.version = 2;
    env.put_distributor(&later);
    let before = snapshot(&env, &month);
    let ix = env.top_up_ix(&payer.pubkey(), MONTH, USDC);
    expect_err(env.send_paid_by(&payer, &[ix], &[&approver]), code(DividendsError::UnsupportedVersion));
    assert_eq!(snapshot(&env, &month), before);
}

#[test]
fn what_a_top_up_adds_beyond_the_leaves_goes_back_to_the_funder_at_the_close() {
    // A top-up raises the total, never a leaf: a month topped up past its leaves is never fully
    // claimed, and the close returns the rest with whatever was not claimed.
    let (mut env, month) = short_month(0, 3 * USDC);
    let (payer, approver, sponsor, closer) =
        (env.payer.insecure_clone(), env.approver.insecure_clone(), env.sponsor.insecure_clone(), env.closer.insecure_clone());
    let top_up = env.top_up_ix(&payer.pubkey(), MONTH, 2 * USDC);
    env.send_paid_by(&payer, &[top_up], &[&approver]).unwrap();
    for index in 0..3 {
        let claim = env.claim_ix(&month, index);
        env.send_paid_by(&sponsor, &[claim], &[]).unwrap();
    }
    let d: UsdcDistributor = env.fetch(&month.distributor);
    assert_eq!((d.claimed_amount, d.total_amount), (month.total, month.total + 2 * USDC));
    let payer_usdc = env.usdc_ata(&payer.pubkey());
    let held = env.token_amount(&payer_usdc);
    let close = env.close_ix(&closer.pubkey(), MONTH);
    expect_err(env.send_paid_by(&closer, &[close.clone()], &[]), code(DividendsError::NotExpired));
    env.set_time(d.expires_at);
    env.send_paid_by(&closer, &[close], &[]).unwrap();
    assert_eq!(env.token_amount(&payer_usdc), held + 2 * USDC);
}

#[test]
fn a_closed_month_and_another_programs_account_are_not_topped_up() {
    let (mut env, month) = short_month(0, 3 * USDC);
    let (payer, approver, sponsor) = (env.payer.insecure_clone(), env.approver.insecure_clone(), env.sponsor.insecure_clone());
    // The month's bytes under another program's ownership, at another address.
    let lookalike = Pubkey::new_unique();
    let bytes = env.svm.get_account(&month.distributor).unwrap().data;
    env.put(lookalike, bytes, pepper_curve::ID);
    let accounts = TopUpAccounts { distributor: lookalike, ..env.top_up_accounts(&env.mint, MONTH) };
    let ix = env.top_up_ix_with(&payer.pubkey(), &approver.pubkey(), &accounts, USDC);
    expect_err(env.send_paid_by(&payer, &[ix], &[&approver]), anchor_code(ErrorCode::AccountOwnedByWrongProgram));
    // Claimed whole and closed: the distributor is gone.
    let top_up = env.top_up_ix(&payer.pubkey(), MONTH, USDC);
    for index in 0..3 {
        let claim = env.claim_ix(&month, index);
        env.send_paid_by(&sponsor, &[claim], &[]).unwrap();
    }
    let close = env.close_ix(&payer.pubkey(), MONTH);
    env.send_paid_by(&payer, &[close], &[]).unwrap();
    expect_err(env.send_paid_by(&payer, &[top_up], &[&approver]), anchor_code(ErrorCode::AccountNotInitialized));
}
