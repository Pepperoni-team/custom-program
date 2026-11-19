//! Where a payout lands (the internal audit of 2026-10-29, claim.rs:32). Every instruction that pays
//! someone is permissionless: anyone sends a claim, a refund, a position's close-out, the creator's
//! split, the guarantee's release or the close. Before, the money could land in any USDC account its
//! beneficiary owned, and anyone can open one for anyone, so a griefer could push a fan's payout into
//! an account no wallet shows. Now it lands only in the beneficiary's associated account; any caller
//! still sends it there.

mod common;

use common::*;
use pepper_curve::raise::calendar::{declare_open_ts, due_ts};
use pepper_curve::raise::constants::*;
use pepper_curve::errors::FanexError;
use pepper_curve::raise::state::Raise;
use solana_sdk::signature::Signer;

#[test]
fn a_claim_lands_only_in_the_owners_associated_account() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, buyers) = env.active_raise(1);
    env.pay_november(&creator, &creator_usdc, &raise, 100 * USD);
    let (holder, holder_usdc) = &buyers[0];
    let elsewhere = env.token_account_elsewhere(&holder.pubkey());
    expect_err(env.claim(&raise, &holder.pubkey(), &elsewhere), code(FanexError::RaiseAccountMismatch));
    assert_eq!(env.balance(&elsewhere), 0);
    // Anyone still sends it, into the account the holder's wallet shows.
    env.claim(&raise, &holder.pubkey(), holder_usdc).unwrap();
    assert_eq!(env.balance(holder_usdc), 9_700_000);
}

#[test]
fn a_refund_lands_only_in_the_buyers_associated_account() {
    let mut env = Env::new();
    let (creator, _, raise) = env.launch(1);
    let (buyer, buyer_usdc) = env.user(10 * USD);
    env.buy(&raise, &buyer, &buyer_usdc, 10 * USD).unwrap();
    env.cancel(&creator, &raise).unwrap();
    let elsewhere = env.token_account_elsewhere(&buyer.pubkey());
    expect_err(env.refund(&raise, &buyer.pubkey(), &elsewhere), code(FanexError::RaiseAccountMismatch));
    env.refund(&raise, &buyer.pubkey(), &buyer_usdc).unwrap();
    assert_eq!(env.balance(&buyer_usdc), 10 * USD);
}

#[test]
fn a_close_out_lands_only_in_the_holders_associated_account() {
    let mut env = Env::new();
    let (_, creator_usdc, raise, buyers) = env.active_raise(1);
    env.declare_zero_months_to_term_end(&raise);
    env.close_raise(&raise, &creator_usdc).unwrap();
    let (holder, holder_usdc) = &buyers[0];
    let elsewhere = env.token_account_elsewhere(&holder.pubkey());
    expect_err(env.close_position(&raise, &holder.pubkey(), &elsewhere), code(FanexError::RaiseAccountMismatch));
    let before = env.balance(holder_usdc);
    env.close_position(&raise, &holder.pubkey(), holder_usdc).unwrap();
    // A tenth of the pool's 90,909,091.
    assert_eq!(env.balance(holder_usdc) - before, 9_090_909);
}

#[test]
fn the_creators_split_and_close_land_only_in_its_associated_account() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise) = env.launch(1);
    env.sell_out(&raise);
    let elsewhere = env.token_account_elsewhere(&creator.pubkey());
    expect_err(env.finalize(&raise, &elsewhere), code(FanexError::RaiseAccountMismatch));
    env.finalize(&raise, &creator_usdc).unwrap();
    env.declare_zero_months_to_term_end(&raise);
    expect_err(env.close_raise(&raise, &elsewhere), code(FanexError::RaiseAccountMismatch));
    env.close_raise(&raise, &creator_usdc).unwrap();
    assert_eq!(env.balance(&elsewhere), 0);
}

#[test]
fn the_guarantees_release_lands_only_in_the_creators_associated_account() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, _) = env.active_raise(1);
    env.mint_to(&creator_usdc, 120 * USD);
    for i in 0..12u32 {
        let month = NOVEMBER_2026 + i;
        env.set_time(declare_open_ts(month));
        env.declare(&raise, month, 10 * USD).unwrap();
        env.fund_month(&creator, &creator_usdc, &raise, month, 10 * USD).unwrap();
    }
    env.set_time(due_ts(NOVEMBER_2026 + 11));
    for i in 0..12u32 {
        env.settle(&raise, NOVEMBER_2026 + i).unwrap();
    }
    let elsewhere = env.token_account_elsewhere(&creator.pubkey());
    expect_err(env.release(&raise, &elsewhere), code(FanexError::RaiseAccountMismatch));
    env.release(&raise, &creator_usdc).unwrap();
    assert_eq!(env.fetch::<Raise>(&raise).guarantee_usdc, 0);
    assert_eq!(env.balance(&vault(GUARANTEE_SEED, &raise)), 0);
}

#[test]
fn the_pools_own_position_still_closes_into_its_vault() {
    let mut env = Env::new();
    let (_, creator_usdc, raise, buyers) = env.active_raise(1);
    env.declare_zero_months_to_term_end(&raise);
    env.close_raise(&raise, &creator_usdc).unwrap();
    for (holder, holder_usdc) in &buyers {
        env.close_position(&raise, &holder.pubkey(), holder_usdc).unwrap();
    }
    // The raise PDA owns the pool's position and holds no associated account: the vault it names
    // is the pool vault, as `closePositionIx` builds it.
    env.close_position(&raise, &raise, &vault(POOL_SEED, &raise)).unwrap();
    assert!(!env.exists(&pool_position_pda(&raise)));
}
