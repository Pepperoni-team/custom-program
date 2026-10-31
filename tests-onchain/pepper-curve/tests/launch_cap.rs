//! The launch cap (anti-sniper), run against the built program
//! (docs/solana/PEPPER-CURVE-PROGRAM.md §5.2).
//!
//! For the first five minutes after a curve's `created_at`, a buy is refused
//! when the buyer's token account would then hold more than 1% of the supply.
//! Until 2026-10-12 a bot buying in the first blocks took as much as it wanted
//! at the lowest price, and sold it to the fans who came after.

mod common;

use common::*;
use pepper_curve::constants::{LAUNCH_GUARD_SECONDS, LAUNCH_WALLET_CAP};
use pepper_curve::errors::FanexError;
use solana_sdk::signature::{Keypair, Signer};

/// A curve launched at `OCT_5_2026`, the clock ten seconds later, and a funded buyer.
fn launched() -> (Env, Keypair) {
    let mut env = Env::new();
    env.write_tradeable_curve(OCT_5_2026);
    env.set_time(OCT_5_2026 + 10);
    let buyer = env.funded_wallet(10 * SOL);
    (env, buyer)
}

/// 0.1 SOL buys about 2% of the supply on a fresh curve; 0.03 SOL about 0.6%.
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

    // Two buys under the cap that add up over it: the holding counts, not the buy.
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
