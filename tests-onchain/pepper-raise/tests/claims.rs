mod common;

use common::*;
use pepper_curve::raise::constants::*;
use pepper_curve::errors::FanexError;
use pepper_curve::raise::state::Position;
use solana_sdk::signature::Signer;

#[test]
fn a_holder_claims_their_share() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, buyers) = env.active_raise(1);
    env.pay_november(&creator, &creator_usdc, &raise, 100 * USD);
    let (holder, holder_usdc) = &buyers[0];
    env.claim(&raise, &holder.pubkey(), holder_usdc).unwrap();
    // 100 of 1,000 quotas: a tenth of the 97 USD that reached the fans.
    assert_eq!(env.balance(holder_usdc), 9_700_000);
    assert_eq!(env.fetch::<Position>(&position_pda(&raise, &holder.pubkey())).claimed, 9_700_000);
}

#[test]
fn a_second_claim_finds_nothing() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, buyers) = env.active_raise(1);
    env.pay_november(&creator, &creator_usdc, &raise, 100 * USD);
    let (holder, holder_usdc) = &buyers[0];
    env.claim(&raise, &holder.pubkey(), holder_usdc).unwrap();
    expect_err(env.claim(&raise, &holder.pubkey(), holder_usdc), code(FanexError::NothingOwed));
}

#[test]
fn a_claim_goes_only_to_the_owner() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, buyers) = env.active_raise(1);
    env.pay_november(&creator, &creator_usdc, &raise, 100 * USD);
    let (_attacker, attacker_usdc) = env.user(0);
    let holder = buyers[0].0.pubkey();
    expect_err(env.claim(&raise, &holder, &attacker_usdc), code(FanexError::RaiseAccountMismatch));
}

#[test]
fn a_claim_under_ten_cents_waits() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, buyers) = env.active_raise(1);
    env.mint_to(&creator_usdc, USD);
    env.fund_direct(&creator, &creator_usdc, &raise, USD).unwrap();
    // 0.97 USD to the fans: 0.097 per holder, under the 0.10 minimum.
    let (holder, holder_usdc) = &buyers[0];
    expect_err(env.claim(&raise, &holder.pubkey(), holder_usdc), code(FanexError::ClaimTooSmall));
}

#[test]
fn the_pools_position_is_never_claimed() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, _) = env.active_raise(1);
    env.pay_november(&creator, &creator_usdc, &raise, 100 * USD);
    // The raise PDA owns both the pool's position and the pool vault, so a claim
    // on that position could land in the vault behind the pool's counter: refused.
    expect_err(env.claim(&raise, &raise, &vault(POOL_SEED, &raise)), code(FanexError::RaiseAccountMismatch));
}

#[test]
fn a_claim_under_ten_cents_is_paid_once_the_raise_is_closed() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, buyers) = env.active_raise(1);
    env.mint_to(&creator_usdc, 2 * USD);
    env.fund_direct(&creator, &creator_usdc, &raise, USD).unwrap();
    let (holder, holder_usdc) = &buyers[0];
    // 0.097 USD owed while the raise is active: under the 0.10 minimum.
    expect_err(env.claim(&raise, &holder.pubkey(), holder_usdc), code(FanexError::ClaimTooSmall));
    env.declare_zero_months_to_term_end(&raise);
    env.close_raise(&raise, &creator_usdc).unwrap();
    // The 0.097 and a tenth of the pool's 90.909091.
    env.claim(&raise, &holder.pubkey(), holder_usdc).unwrap();
    assert_eq!(env.balance(holder_usdc), 9_187_909);
    // A payment after the close owes the holder 0.097 again: once the raise is
    // closed there is no minimum.
    env.fund_direct(&creator, &creator_usdc, &raise, USD).unwrap();
    env.claim(&raise, &holder.pubkey(), holder_usdc).unwrap();
    assert_eq!(env.balance(holder_usdc), 9_187_909 + 97_000);
}
