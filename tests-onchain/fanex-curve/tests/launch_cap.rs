mod common;

use common::*;
use fanex_curve::constants::{LAUNCH_GUARD_SECONDS, LAUNCH_WALLET_CAP};
use fanex_curve::errors::FanexError;
use solana_sdk::signature::{Keypair, Signer};

fn launched() -> (Env, Keypair) {
    let mut env = Env::new();
    env.write_tradeable_curve(OCT_5_2026);
    env.set_time(OCT_5_2026 + 10);
    let buyer = env.funded_wallet(10 * SOL);
    (env, buyer)
}

const OVER_THE_CAP: u64 = SOL / 10;
const UNDER_THE_CAP: u64 = 3 * SOL / 100;

#[test]
fn refuses_a_buy_over_the_cap_in_the_first_five_minutes() {
    let (mut env, buyer) = launched();

    expect_err(env.buy(&buyer, OVER_THE_CAP), code(FanexError::LaunchWalletCapExceeded));
    assert_eq!(env.token_balance(&buyer.pubkey()), 0);
}

#[test]
fn takes_a_buy_under_the_cap() {
    let (mut env, buyer) = launched();

    env.buy(&buyer, UNDER_THE_CAP).unwrap();
    let held = env.token_balance(&buyer.pubkey());
    assert!(held > 0 && held <= LAUNCH_WALLET_CAP, "held {held}");
}

#[test]
fn counts_what_the_wallet_already_holds() {
    let (mut env, buyer) = launched();
    env.buy(&buyer, UNDER_THE_CAP).unwrap();
    let held = env.token_balance(&buyer.pubkey());

    expect_err(env.buy(&buyer, UNDER_THE_CAP), code(FanexError::LaunchWalletCapExceeded));
    assert_eq!(env.token_balance(&buyer.pubkey()), held);
}

#[test]
fn gives_each_wallet_its_own_cap() {
    let (mut env, first) = launched();
    let second = env.funded_wallet(10 * SOL);

    env.buy(&first, UNDER_THE_CAP).unwrap();
    env.buy(&second, UNDER_THE_CAP).unwrap();
    assert!(env.token_balance(&second.pubkey()) > 0);
}

#[test]
fn binds_the_creator_and_the_launch_first_buy_too() {
    let (mut env, _) = launched();
    env.set_time(OCT_5_2026);
    let creator = env.creator.insecure_clone();

    expect_err(env.buy(&creator, OVER_THE_CAP), code(FanexError::LaunchWalletCapExceeded));
}

#[test]
fn lifts_the_cap_five_minutes_after_the_launch() {
    let (mut env, buyer) = launched();
    env.set_time(OCT_5_2026 + LAUNCH_GUARD_SECONDS - 1);
    expect_err(env.buy(&buyer, OVER_THE_CAP), code(FanexError::LaunchWalletCapExceeded));

    env.set_time(OCT_5_2026 + LAUNCH_GUARD_SECONDS);
    env.buy(&buyer, OVER_THE_CAP).unwrap();
    assert!(env.token_balance(&buyer.pubkey()) > LAUNCH_WALLET_CAP);
}
