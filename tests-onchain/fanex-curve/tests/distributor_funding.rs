mod common;

use common::*;
use fanex_curve::constants::{DIVIDEND_PAYER, MAX_DISTRIBUTOR_EXPIRY_SECONDS};
use fanex_curve::errors::FanexError;
use fanex_curve::state::{Distributor, DistributorV2};
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
    let fanex = env.admin.pubkey();

    let mut ix = env.init_v2_ix(&creator, &fanex, [7u8; 32], SOL, OCT_5_2026, 2);
    for meta in ix.accounts.iter_mut().filter(|m| m.pubkey == fanex) {
        meta.is_signer = false;
    }
    expect_err(env.send_paid_by(&creator, &[ix], &[]), ACCOUNT_NOT_SIGNER);
    assert!(!env.exists(&distributor_pda(&env.mint, MONTH)));
}

#[test]
fn a_creator_can_no_longer_open_a_v1_month() {
    let mut env = Env::new();
    let creator = env.creator.insecure_clone();

    expect_err(env.init_v1(&creator, [7u8; 32], SOL), code(FanexError::Unauthorized));
    assert!(!env.exists(&distributor_pda(&env.mint, MONTH)));
}

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

    let approver = env.admin.pubkey();
    let ix = env.init_v2_ix_by(&DIVIDEND_PAYER, &approver, month.tree.root, month.total, OCT_5_2026 + 365 * DAY, 2);
    env.send_unverified(&[ix], &[]).unwrap();

    let d: DistributorV2 = env.fetch(&distributor_pda(&env.mint, MONTH));
    assert_eq!(d.funder, DIVIDEND_PAYER);
    assert_eq!(d.merkle_root, month.tree.root);
}

#[test]
fn no_month_expires_more_than_400_days_out() {
    let mut env = Env::new();
    let admin = env.admin.insecure_clone();
    let cap = OCT_5_2026 + MAX_DISTRIBUTOR_EXPIRY_SECONDS;

    expect_err(env.init_v1_expiring(&admin, [7u8; 32], SOL, cap + 1), code(FanexError::ExpiryTooFar));
    let month = env.v2_month(2);
    expect_err(
        env.init_v2(&admin, month.tree.root, month.total, cap + 1, 2),
        code(FanexError::ExpiryTooFar),
    );
    assert!(!env.exists(&distributor_pda(&env.mint, MONTH)));

    env.init_v1_expiring(&admin, [7u8; 32], SOL, cap).unwrap();
    let d: Distributor = env.fetch(&distributor_pda(&env.mint, MONTH));
    assert_eq!(d.expires_at, cap);
}

#[test]
fn fanex_still_funds_both_versions() {
    let mut env = Env::new();
    let admin = env.admin.insecure_clone();
    env.init_v1(&admin, [7u8; 32], SOL).unwrap();

    let mut other = Env::new();
    let month = other.v2_month(2);
    other.fund_v2(&month);
    let d: DistributorV2 = other.fetch(&distributor_pda(&other.mint, MONTH));
    assert_eq!(d.funder, other.admin.pubkey());
}
