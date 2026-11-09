//! Who may open a month's distributor, run against the built program
//! (docs/solana/SOLANA-DIVIDEND-DISTRIBUTION.md §2.2, §6.1).
//!
//! A creator signs their own funding in the browser, so the program cannot
//! rely on that transaction being the one the prepare built, and it takes the
//! root, the total and the expiry from whoever funds. Until 2026-10-05 a
//! creator could open a month under a root of their own: holders could never
//! prove a claim, the creator could claim or close the SOL back, and the PDA,
//! `init`-once per (mint, month), kept the month from ever being funded again.
//! Now a creator opens a month only in v2 and only with Fanex's signature as
//! its approver, given by the server on the transaction it prepared: the
//! graduation authority's since the fee-wallet split (2026-10-24), the fee
//! wallet's until then.

mod common;

use common::*;
use pepper_curve::constants::{DIVIDEND_PAYER, MAX_DISTRIBUTOR_EXPIRY_SECONDS};
use pepper_curve::errors::FanexError;
use pepper_curve::state::{Distributor, DistributorV2};
use solana_sdk::signature::Signer;

#[test]
fn a_creator_funds_a_v2_month_with_the_fanex_approval() {
    let mut env = Env::new();
    let month = env.v2_month(2);
    let creator = env.creator.insecure_clone();

    env.init_v2(&creator, month.tree.root, month.total, OCT_5_2026 + 365 * DAY, 2).unwrap();

    let d: DistributorV2 = env.fetch(&distributor_pda(&env.mint, MONTH));
    assert_eq!(d.funder, creator.pubkey());
    assert_eq!(d.merkle_root, month.tree.root);
}

#[test]
fn a_creator_cannot_approve_their_own_month() {
    let mut env = Env::new();
    let creator = env.creator.insecure_clone();
    let own_root = [7u8; 32];

    let ix = env.init_v2_ix(&creator, &creator.pubkey(), own_root, SOL, OCT_5_2026 + 365 * DAY, 2);
    expect_err(env.send_paid_by(&creator, &[ix], &[]), code(FanexError::Unauthorized));
    assert!(!env.exists(&distributor_pda(&env.mint, MONTH)));
}

#[test]
fn a_creator_cannot_name_fanex_as_approver_without_its_signature() {
    let mut env = Env::new();
    let creator = env.creator.insecure_clone();
    let fanex = env.graduation_authority.pubkey();

    let mut ix = env.init_v2_ix(&creator, &fanex, [7u8; 32], SOL, OCT_5_2026, 2);
    for meta in ix.accounts.iter_mut().filter(|m| m.pubkey == fanex) {
        meta.is_signer = false;
    }
    // Paid by the creator: Fanex signs nothing in this transaction.
    expect_err(env.send_paid_by(&creator, &[ix], &[]), ACCOUNT_NOT_SIGNER);
    assert!(!env.exists(&distributor_pda(&env.mint, MONTH)));
}

#[test]
fn a_creator_can_no_longer_open_a_v1_month() {
    // v1 has no approver, and v1 and v2 share the month's PDA, so a v1 month a
    // creator opened would squat the v2 one too.
    let mut env = Env::new();
    let creator = env.creator.insecure_clone();

    expect_err(env.init_v1(&creator, [7u8; 32], SOL), code(FanexError::Unauthorized));
    assert!(!env.exists(&distributor_pda(&env.mint, MONTH)));
}

// The dividend payer's key, stolen, must open no month alone, and no month may
// stay blocked past 400 days (2026-10-08, security card point 5): v1 has no
// approver, so v1 takes one config key alone (the graduation authority since
// 2026-10-24, the fee wallet until then); v2 takes the payer with Fanex's
// approval. The suite holds no key for the payer, a fixed address, so these
// run with signature verification off (`Env::unverified`).

#[test]
fn the_dividend_payer_cannot_open_a_v1_month() {
    let mut env = Env::unverified();
    env.svm.airdrop(&DIVIDEND_PAYER, 10 * SOL).unwrap();

    let ix = env.init_v1_ix_by(&DIVIDEND_PAYER, [7u8; 32], SOL, OCT_5_2026 + 365 * DAY);
    expect_err(env.send_unverified(&[ix], &[]), code(FanexError::Unauthorized));
    assert!(!env.exists(&distributor_pda(&env.mint, MONTH)));
}

#[test]
fn the_dividend_payer_funds_a_v2_month_with_the_fanex_approval() {
    let mut env = Env::unverified();
    env.svm.airdrop(&DIVIDEND_PAYER, 10 * SOL).unwrap();
    let month = env.v2_month(2);

    let approver = env.graduation_authority.insecure_clone();
    let ix = env.init_v2_ix_by(&DIVIDEND_PAYER, &approver.pubkey(), month.tree.root, month.total, OCT_5_2026 + 365 * DAY, 2);
    env.send_unverified(&[ix], &[&approver]).unwrap();

    let d: DistributorV2 = env.fetch(&distributor_pda(&env.mint, MONTH));
    assert_eq!(d.funder, DIVIDEND_PAYER);
    assert_eq!(d.merkle_root, month.tree.root);
}

#[test]
fn no_month_expires_more_than_400_days_out() {
    // Whoever funds and in either version: one second past the cap is refused,
    // the cap itself is accepted.
    let mut env = Env::new();
    let authority = env.graduation_authority.insecure_clone();
    let creator = env.creator.insecure_clone();
    let cap = OCT_5_2026 + MAX_DISTRIBUTOR_EXPIRY_SECONDS;

    expect_err(env.init_v1_expiring(&authority, [7u8; 32], SOL, cap + 1), code(FanexError::ExpiryTooFar));
    let month = env.v2_month(2);
    expect_err(
        env.init_v2(&creator, month.tree.root, month.total, cap + 1, 2),
        code(FanexError::ExpiryTooFar),
    );
    assert!(!env.exists(&distributor_pda(&env.mint, MONTH)));

    env.init_v1_expiring(&authority, [7u8; 32], SOL, cap).unwrap();
    let d: Distributor = env.fetch(&distributor_pda(&env.mint, MONTH));
    assert_eq!(d.expires_at, cap);
}

// The fee-wallet split (2026-10-24): one config key funds v1, the server's
// graduation authority; none funds v2, so the approver's key alone, stolen,
// opens no month there.

#[test]
fn the_graduation_authority_funds_a_v1_month() {
    let mut env = Env::new();
    let authority = env.graduation_authority.insecure_clone();

    env.init_v1(&authority, [7u8; 32], SOL).unwrap();

    let d: Distributor = env.fetch(&distributor_pda(&env.mint, MONTH));
    assert_eq!(d.funder, authority.pubkey());
}

#[test]
fn no_config_key_funds_a_v2_month() {
    let mut env = Env::new();
    let month = env.v2_month(2);
    let keys = [env.graduation_authority.insecure_clone(), env.fee_wallet.insecure_clone(), env.admin.insecure_clone()];

    for funder in &keys {
        expect_err(
            env.init_v2(funder, month.tree.root, month.total, OCT_5_2026 + 365 * DAY, 2),
            code(FanexError::Unauthorized),
        );
    }
    assert!(!env.exists(&distributor_pda(&env.mint, MONTH)));
}

#[test]
fn neither_the_fee_wallet_nor_the_admin_funds_a_v1_month() {
    let mut env = Env::new();
    let fee_wallet = env.fee_wallet.insecure_clone();
    let admin = env.admin.insecure_clone();

    expect_err(env.init_v1(&fee_wallet, [7u8; 32], SOL / 10), code(FanexError::Unauthorized));
    expect_err(env.init_v1(&admin, [7u8; 32], SOL), code(FanexError::Unauthorized));
    assert!(!env.exists(&distributor_pda(&env.mint, MONTH)));
}

#[test]
fn neither_the_fee_wallet_nor_the_admin_approves_a_v2_month() {
    // Each signs as the approver: the address constraint refuses it.
    let mut env = Env::new();
    let creator = env.creator.insecure_clone();
    let month = env.v2_month(2);

    for approver in [env.fee_wallet.insecure_clone(), env.admin.insecure_clone()] {
        let ix = env.init_v2_ix(&creator, &approver.pubkey(), month.tree.root, month.total, OCT_5_2026 + 365 * DAY, 2);
        expect_err(env.send(&[ix], &[&creator, &approver]), code(FanexError::Unauthorized));
    }
    assert!(!env.exists(&distributor_pda(&env.mint, MONTH)));
}
