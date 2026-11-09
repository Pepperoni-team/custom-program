//! The parity suite on mainnet's live USDC curves (USDC curve design §10.5, §10.12): four curves of the
//! program deployed on mainnet (N+2), chosen by state on 2026-11-02 and named in
//! `tests/fixtures/mainnet-pools.json`. Their accounts were snapshotted by Jupiter's kit through
//! Helius into `tests/fixtures/accounts/<curve>/` and committed, so the suite runs offline; each swap
//! is asserted by the kit and by this suite's harness, on the program the kit loads
//! (`tests/fixtures/pepper.so`): run it on mainnet's own bytes, dumped from the chain, to prove the SDK
//! against what Jupiter would route to (`scripts/ci-onchain-tests.sh` copies this tree's build there,
//! the same source).
//!
//! New snapshots: delete a pool's folder, then, from this crate's folder, only this file:
//!   RPC='https://mainnet.helius-rpc.com/?api-key=<key>' cargo test --test mainnet_pools
//! (never with REFRESH=1 and the vector pools, which are on no cluster.)

mod common;

use common::*;
use jupiter_amm_test_kit::{assert_pool_parity, PoolTest};
use pepper_curve_jupiter::layout::{CurveUsdc, PHASE_BONDING, PROGRAM_ID};
use pepper_curve_jupiter::PepperCurveAmm;
use solana_pubkey::Pubkey;

const MANIFEST: &str = "tests/fixtures/mainnet-pools.json";
const USDC: u64 = 1_000_000;
const TOKENS: u64 = 1_000_000;

struct MainnetPool {
    curve: Pubkey,
    mint: Pubkey,
    usdc_mint: Pubkey,
}

fn pool(state: &str) -> MainnetPool {
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(MANIFEST).expect("the mainnet manifest"))
            .expect("it parses");
    assert_eq!(manifest["cluster"], "mainnet");
    assert_eq!(manifest["program"], PROGRAM_ID.to_string());
    let entry = manifest["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["state"] == state)
        .unwrap_or_else(|| panic!("no mainnet pool in the state {state}"));
    let key = |field: &str| -> Pubkey { entry[field].as_str().unwrap().parse().unwrap() };
    MainnetPool {
        curve: key("curve"),
        mint: key("mint"),
        usdc_mint: manifest["usdc_mint"].as_str().unwrap().parse().unwrap(),
    }
}

/// The pool's swaps through the kit (which snapshots it first when its folder is missing and `RPC`
/// is set) and through the harness; the curve as snapshotted, and the quotes.
fn run(
    pool: &MainnetPool,
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
    assert_eq!(snapshot.owner, PROGRAM_ID, "the program's own account");
    let curve = CurveUsdc::decode(&snapshot.data).unwrap();
    assert_eq!(curve.phase, PHASE_BONDING);
    assert_eq!(curve.mint, pool.mint);
    (curve, assert_swaps_move_their_quote(&test))
}

/// A revenue curve inside its raise, the one the team trades on (CICCO1): account 8 is due, the buy
/// pays the creator and Fanex beside the 1%.
#[test]
fn mainnet_a_revenue_curve_inside_its_raise() {
    let pool = pool("revenue-inside-raise");
    let (curve, quotes) = run(&pool, &[2 * USDC, 100 * USDC], &[100_000 * TOKENS]);
    assert!(curve.creator_usdc_account_required());
    assert!(quotes[1].fee_amount > USDC, "the raise shares ride on the 1%");
    assert_eq!(quotes[0].in_amount, 2 * USDC, "no partial fill");
}

/// A revenue curve early in its raise (MON3Y1).
#[test]
fn mainnet_a_revenue_curve_early_in_its_raise() {
    let pool = pool("revenue-inside-raise-early");
    let (curve, quotes) = run(&pool, &[10 * USDC], &[100_000 * TOKENS]);
    assert!(curve.creator_usdc_account_required());
    assert!(quotes[0].fee_amount > 10 * USDC / 100, "the raise shares ride on the 1%");
}

/// An equity curve (MARCO): no raise, the 1% alone.
#[test]
fn mainnet_an_equity_curve() {
    let pool = pool("equity");
    let (curve, quotes) = run(&pool, &[10 * USDC, 100 * USDC], &[100_000 * TOKENS]);
    assert_eq!(curve.creator_split_bps, 0);
    assert_eq!(curve.fanex_split_bps, 0);
    assert_eq!(quotes[1].fee_amount, USDC, "the 1% alone");
}

/// An equity curve with a large graduation threshold (PEWDIEPIE), bought and sold in size.
#[test]
fn mainnet_an_equity_curve_with_a_large_threshold() {
    let pool = pool("equity-large-threshold");
    let (curve, quotes) = run(&pool, &[500 * USDC], &[100_000 * TOKENS]);
    assert_eq!(curve.creator_split_bps, 0);
    assert_eq!(quotes[0].in_amount, 500 * USDC, "no partial fill");
}
