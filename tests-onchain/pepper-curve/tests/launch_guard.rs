//! The launch cap's switch, run against the built program
//! (docs/solana/PEPPER-CURVE-PROGRAM.md §5.2, 2026-10-14).
//!
//! A curve launched with a waiver takes buys over the cap from its first
//! second, the launch's own first buy included. Only the launch guard authority
//! makes a waiver, and only before the curve exists: a creator cannot launch
//! without the cap alone, and nobody lifts it from a live curve.

mod common;

use common::*;
use pepper_curve::constants::LAUNCH_WALLET_CAP;
use pepper_curve::errors::FanexError;
use pepper_curve::state::{LaunchGuardConfig, LaunchGuardWaiver};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signer};

/// 0.1 SOL buys about 2% of the supply on a fresh curve.
const OVER_THE_CAP: u64 = SOL / 10;

/// An environment whose launch guard authority is `guard`, and a mint with no curve yet.
fn named() -> (Env, Keypair) {
    let mut env = Env::new();
    let guard = Keypair::new();
    let admin = env.admin.insecure_clone();
    env.set_launch_guard_authority_by(&admin, &guard.pubkey()).unwrap();
    env.mint = Pubkey::new_unique();
    (env, guard)
}

/// The launch itself, as `create_curve` leaves it, ten seconds before the buy.
fn launch(env: &mut Env) {
    env.write_tradeable_curve(OCT_5_2026);
    env.set_time(OCT_5_2026 + 10);
}

#[test]
fn a_waived_launch_takes_a_buy_over_the_cap() {
    let (mut env, guard) = named();
    let creator = env.creator.insecure_clone();
    let mint = env.mint;
    env.waive(&creator, &guard, &mint).unwrap();
    assert_eq!(env.fetch::<LaunchGuardWaiver>(&waiver_pda(&mint)).mint, mint);
    launch(&mut env);

    let buyer = env.funded_wallet(10 * SOL);
    env.buy_with_extra(&buyer, OVER_THE_CAP, &[waiver_pda(&mint)]).unwrap();
    assert!(env.token_balance(&buyer.pubkey()) > LAUNCH_WALLET_CAP);
}

#[test]
fn a_waived_curve_still_caps_a_buy_that_does_not_pass_its_waiver() {
    let (mut env, guard) = named();
    let creator = env.creator.insecure_clone();
    let mint = env.mint;
    env.waive(&creator, &guard, &mint).unwrap();
    launch(&mut env);

    let buyer = env.funded_wallet(10 * SOL);
    expect_err(env.buy(&buyer, OVER_THE_CAP), code(FanexError::LaunchWalletCapExceeded));
}

#[test]
fn another_mints_waiver_does_not_lift_the_cap() {
    let (mut env, guard) = named();
    let creator = env.creator.insecure_clone();
    let other = Pubkey::new_unique();
    env.waive(&creator, &guard, &other).unwrap();
    launch(&mut env);

    let buyer = env.funded_wallet(10 * SOL);
    expect_err(
        env.buy_with_extra(&buyer, OVER_THE_CAP, &[waiver_pda(&other)]),
        code(FanexError::LaunchWalletCapExceeded),
    );
}

#[test]
fn refuses_a_waiver_the_authority_did_not_sign() {
    let (mut env, _guard) = named();
    let creator = env.creator.insecure_clone();
    let mint = env.mint;
    // The creator signing as the authority: a launch transaction is theirs alone.
    let posing = creator.insecure_clone();
    expect_err(env.waive(&creator, &posing, &mint), code(FanexError::Unauthorized));
    assert!(!env.exists(&waiver_pda(&mint)));
}

#[test]
fn refuses_a_waiver_for_a_curve_already_launched() {
    let (mut env, guard) = named();
    launch(&mut env);
    let creator = env.creator.insecure_clone();
    let mint = env.mint;

    expect_err(env.waive(&creator, &guard, &mint), code(FanexError::LaunchGuardAfterLaunch));
    let buyer = env.funded_wallet(10 * SOL);
    expect_err(
        env.buy_with_extra(&buyer, OVER_THE_CAP, &[waiver_pda(&mint)]),
        code(FanexError::LaunchWalletCapExceeded),
    );
}

#[test]
fn refuses_a_waiver_before_an_authority_is_named() {
    let mut env = Env::new();
    env.mint = Pubkey::new_unique();
    let creator = env.creator.insecure_clone();
    let guard = Keypair::new();
    let mint = env.mint;

    expect_err(env.waive(&creator, &guard, &mint), ACCOUNT_NOT_INITIALIZED);
}

#[test]
fn only_the_admin_names_the_authority_and_all_zero_lets_nobody() {
    let (mut env, guard) = named();
    let stranger = env.funded_wallet(SOL);
    expect_err(
        env.set_launch_guard_authority_by(&stranger, &stranger.pubkey()),
        code(FanexError::Unauthorized),
    );
    assert_eq!(env.fetch::<LaunchGuardConfig>(&launch_guard_pda()).authority, guard.pubkey());

    let admin = env.admin.insecure_clone();
    env.set_launch_guard_authority_by(&admin, &Pubkey::default()).unwrap();
    let creator = env.creator.insecure_clone();
    let mint = env.mint;
    expect_err(env.waive(&creator, &guard, &mint), code(FanexError::Unauthorized));
}
