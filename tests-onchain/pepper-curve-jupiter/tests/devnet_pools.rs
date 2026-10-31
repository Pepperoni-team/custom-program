//! The parity suite on devnet's real USDC curves (USDC curve design §10.5, §10.12 step 2): four
//! curves the program itself launched and traded into the four states of §10.5, by
//! `scripts/solana/devnet-usdc-jupiter-pools.ts`, which names them in `tests/fixtures/devnet-pools.json`.
//! Their accounts were snapshotted by Jupiter's kit through Helius into
//! `tests/fixtures/accounts/<curve>/` and committed, so the suite runs offline; each swap is asserted
//! by the kit and by this suite's harness, on the program built from this tree.
//!
//! New snapshots: rerun the script, then, from this crate's folder, only this file:
//!   RPC='https://devnet.helius-rpc.com/?api-key=<key>' REFRESH=1 cargo test --test devnet_pools
//! (REFRESH re-snapshots every pool of the binary it runs, so never with the vector pools.)

mod common;

use common::*;
use jupiter_amm_test_kit::{assert_pool_parity, PoolTest};
use pepper_curve_jupiter::layout::{CurveUsdc, PHASE_BONDING, PROGRAM_ID};
use pepper_curve_jupiter::PepperCurveAmm;
use solana_pubkey::Pubkey;

const MANIFEST: &str = "tests/fixtures/devnet-pools.json";
const USDC: u64 = 1_000_000;
const TOKENS: u64 = 1_000_000;

struct DevnetPool {
    curve: Pubkey,
    mint: Pubkey,
    usdc_mint: Pubkey,
}

fn pool(state: &str) -> DevnetPool {
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(MANIFEST).expect("the devnet manifest"))
            .expect("it parses");
    assert_eq!(manifest["cluster"], "devnet");
    assert_eq!(manifest["program"], PROGRAM_ID.to_string());
    let entry = manifest["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["state"] == state)
        .unwrap_or_else(|| panic!("no devnet pool in the state {state}"));
    let key = |field: &str| -> Pubkey { entry[field].as_str().unwrap().parse().unwrap() };
    DevnetPool {
        curve: key("curve"),
        mint: key("mint"),
        usdc_mint: manifest["usdc_mint"].as_str().unwrap().parse().unwrap(),
    }
}

/// The pool's swaps through the kit (which snapshots it first when its folder is missing and `RPC`
/// is set) and through the harness; the curve as snapshotted, and the quotes.
fn run(
    pool: &DevnetPool,
    buys: &[u64],
    sales: &[u64],
) -> (CurveUsdc, Vec<jupiter_amm_interface::Quote>) {
    let mut test = PoolTest::new(pool.curve);
    assert_eq!(test.fixtures_dir, devnet_fixtures(&pool.curve));
    for &amount in buys {
        test = test.add_swap(pool.usdc_mint, pool.mint, amount);
    }
    for &amount in sales {
        test = test.add_swap(pool.mint, pool.usdc_mint, amount);
    }
    assert_pool_parity::<PepperCurveAmm>(&test, encode);
    let accounts = load_snapshot(&test.fixtures_dir);
    let snapshot = &accounts[&pool.curve];
    assert_eq!(
        snapshot.owner, PROGRAM_ID,
        "the program's own account, written by its own instructions"
    );
    let curve = CurveUsdc::decode(&snapshot.data).unwrap();
    assert_eq!(curve.phase, PHASE_BONDING);
    assert_eq!(curve.mint, pool.mint);
    (curve, assert_swaps_move_their_quote(&test))
}

/// An equity curve after its first 100 USDC buy.
#[test]
fn devnet_a_fresh_equity_curve() {
    let pool = pool("equity-fresh");
    let (curve, quotes) = run(&pool, &[100 * USDC], &[1_000_000 * TOKENS]);
    assert_eq!(curve.creator_split_bps, 0);
    assert!(curve.real_usdc_reserves > 0);
    assert_eq!(quotes[0].in_amount, 100 * USDC, "no partial fill");
}

/// An equity curve whose last tokens a 500 USDC buy takes for less: the source keeps the change.
#[test]
fn devnet_an_equity_curve_a_buy_away_from_graduation() {
    let pool = pool("equity-near-graduation");
    let (curve, quotes) = run(&pool, &[500 * USDC], &[1_000_000 * TOKENS]);
    assert!(
        quotes[0].in_amount < 500 * USDC,
        "the partial fill charges less than it was given"
    );
    assert_eq!(
        quotes[0].out_amount, curve.real_token_reserves,
        "and takes the last token"
    );
}

/// A revenue curve inside its raise: account 8 is due, the buy pays the creator and Fanex.
#[test]
fn devnet_a_revenue_curve_inside_its_raise() {
    let pool = pool("revenue-inside-raise");
    let (curve, quotes) = run(&pool, &[100 * USDC], &[1_000_000 * TOKENS]);
    assert!(curve.creator_usdc_account_required());
    assert!(
        quotes[0].fee_amount > USDC,
        "the raise shares ride on the 1%"
    );
}

/// A revenue curve whose caps a buy filled: account 8 is left out, the 1% alone is taken.
#[test]
fn devnet_a_revenue_curve_past_the_creator_s_cap() {
    let pool = pool("revenue-past-creator-cap");
    let (curve, quotes) = run(&pool, &[100 * USDC], &[1_000_000 * TOKENS]);
    assert!(!curve.creator_usdc_account_required());
    assert_eq!(curve.creator_received_usdc, curve.creator_cap_usdc);
    assert_eq!(curve.fanex_received_usdc, curve.fanex_cap_usdc);
    assert_eq!(quotes[0].fee_amount, USDC, "the 1% alone");
}
