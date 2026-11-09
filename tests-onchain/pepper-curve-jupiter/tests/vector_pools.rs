//! The parity suite on curves built by hand from the golden vectors (USDC curve design §10.5, §10.9),
//! before any deploy: one `PoolTest` per state, each swap asserted by Jupiter's kit
//! (`assert_pool_parity`, the quote's `out_amount` against the program's delivery) and by this
//! suite's harness (the source's change against `in_amount`, the fees, the vaults, the counters),
//! on the program built from this tree. Every swap a vector holds is checked against the vector too,
//! so the vector, the SDK's quote and the program's execution are one number.
//!
//! The four states §10.5 names, a buy and a sale each: a fresh equity curve (after its first buy, so
//! a sale has a reserve to take from), a revenue curve inside its raise, one past the creator's cap,
//! and one a buy away from graduation. Beside them, the vectors' remaining buys, each on its own
//! state: the smallest, one under 100 µUSDC, the largest (a sweep partly filled), the first second
//! after the launch window, a revenue curve fresh, crossing the creator's cap and past both caps.
//!
//!   cargo test --test vector_pools        (tests/fixtures/pepper.so in place)

mod common;

use common::*;
use jupiter_amm_test_kit::assert_pool_parity;
use pepper_curve_jupiter::PepperCurveAmm;
use serde_json::Value;

const LAUNCH: i64 = 1_800_000_000;
const LATER: i64 = LAUNCH + 600;

/// The vectors' swap amounts and what each case says comes of it, against the SDK's quote.
fn assert_quotes_match_vectors(quotes: &[jupiter_amm_interface::Quote], cases: &[Value]) {
    assert_eq!(quotes.len(), cases.len());
    for (quote, case) in quotes.iter().zip(cases) {
        let name = case["name"].as_str().unwrap();
        let result = &case["result"];
        match case["op"].as_str().unwrap() {
            "buy" => {
                assert_eq!(quote.in_amount, u64_of(&result["charged"]), "{name}");
                assert_eq!(quote.out_amount, u64_of(&result["tokens_out"]), "{name}");
                let fees = u64_of(&result["platform_fee"])
                    + u64_of(&result["creator_amount"])
                    + u64_of(&result["fanex_amount"]);
                assert_eq!(quote.fee_amount, fees, "{name}");
            }
            _ => {
                assert_eq!(quote.in_amount, u64_of(&case["amount"]), "{name}");
                assert_eq!(quote.out_amount, u64_of(&result["proceeds"]), "{name}");
                assert_eq!(quote.fee_amount, u64_of(&result["platform_fee"]), "{name}");
            }
        }
    }
}

/// A pool's swaps asserted by the kit and by the harness; the quotes, in the order given.
fn run(pool: &VectorPool, swaps: &[(Side, u64)]) -> Vec<jupiter_amm_interface::Quote> {
    let mint = pool.curve.mint;
    let mut test = pool.pool_test();
    for &(side, amount) in swaps {
        test = match side {
            Side::Buy => test.add_swap(USDC_MINT, mint, amount),
            Side::Sell => test.add_swap(mint, USDC_MINT, amount),
        };
    }
    assert_pool_parity::<PepperCurveAmm>(&test, encode);
    let quotes = assert_swaps_move_their_quote(&test);
    std::fs::remove_dir_all(&test.fixtures_dir).ok();
    quotes
}

#[derive(Clone, Copy)]
enum Side {
    Buy,
    Sell,
}

fn vector_swaps(cases: &[Value]) -> Vec<(Side, u64)> {
    cases
        .iter()
        .map(|c| {
            (
                if c["op"] == "buy" {
                    Side::Buy
                } else {
                    Side::Sell
                },
                u64_of(&c["amount"]),
            )
        })
        .collect()
}

// ── The four states of §10.5, a buy and a sale each ─────────────────────────

/// A fresh equity curve after its first 100 USDC buy: the vectors' two sales that pay, then a buy.
#[test]
fn a_fresh_equity_curve() {
    let sales = [
        vector("equity, after a 100 USDC buy: a sale of 1,000,000 tokens"),
        vector("equity, after a 100 USDC buy: a sale of every token it sold"),
    ];
    let pool = VectorPool::new(7, &sales[0]["state"], LATER);
    let mut swaps = vector_swaps(&sales);
    swaps.push((Side::Buy, 100_000_000));
    let quotes = run(&pool, &swaps);
    assert_quotes_match_vectors(&quotes[..2], &sales);
}

/// A revenue curve inside its raise (after the vectors' buy inside both caps): account 8 is due, the
/// creator's and Fanex's shares paid by the buy; then a sale of half what that buy bought.
#[test]
fn a_revenue_curve_inside_its_raise() {
    let first = vector("revenue, fresh: a buy inside both caps");
    let pool = VectorPool::new(8, &first["result"]["after"], LATER);
    assert!(pool.curve.creator_usdc_account_required());
    let bought = u64_of(&first["result"]["tokens_out"]);
    let quotes = run(&pool, &[(Side::Buy, 100_000_000), (Side::Sell, bought / 2)]);
    assert!(
        quotes[0].fee_amount > quotes[0].in_amount / 100,
        "the raise shares ride on the fee"
    );
}

/// A revenue curve whose creator cap the vectors' crossing buy filled: account 8 is left out, the buy
/// pays Fanex nothing more either; then a sale.
#[test]
fn a_revenue_curve_past_the_creator_s_cap() {
    let crossing = vector("revenue: a buy crossing the creator's cap");
    let pool = VectorPool::new(9, &crossing["result"]["after"], LATER);
    assert!(!pool.curve.creator_usdc_account_required());
    assert_eq!(
        pool.curve.creator_received_usdc,
        pool.curve.creator_cap_usdc
    );
    let bought = u64_of(&crossing["result"]["tokens_out"]);
    let quotes = run(&pool, &[(Side::Buy, 100_000_000), (Side::Sell, bought / 2)]);
    assert_eq!(
        quotes[0].fee_amount, 1_000_000,
        "the 1% alone once both caps are full"
    );
}

/// An equity curve a buy away from graduation: the vectors' buy that reaches the threshold with
/// tokens left, the one partly filled that empties it (charged less than given: the source keeps the
/// change), then a sale.
#[test]
fn an_equity_curve_a_buy_away_from_graduation() {
    let buys = [
        vector("equity, near the end: a buy reaches the threshold with tokens left"),
        vector("equity, near the end: the partial fill at graduation empties the curve"),
    ];
    let pool = VectorPool::new(3, &buys[0]["state"], LATER);
    let mut swaps = vector_swaps(&buys);
    swaps.push((Side::Sell, 1_000_000_000_000));
    let quotes = run(&pool, &swaps);
    assert_quotes_match_vectors(&quotes[..2], &buys);
    assert!(
        quotes[1].in_amount < 2_000_000_000,
        "the partial fill charges less than it was given"
    );
}

// ── The vectors' other buys, each on its own state ──────────────────────────

#[test]
fn the_vectors_buys_on_a_fresh_equity_curve() {
    let buys = [
        vector("equity, fresh: the smallest buy"),
        vector("equity, fresh: a buy under 100 base units pays no fee"),
        vector("equity, fresh: a 100 USDC buy"),
        vector("equity, fresh: the largest buy sweeps the curve in a partial fill"),
    ];
    let pool = VectorPool::new(1, &buys[0]["state"], LATER);
    let quotes = run(&pool, &vector_swaps(&buys));
    assert_quotes_match_vectors(&quotes, &buys);
    assert!(
        quotes[3].in_amount < u64::MAX,
        "the sweep charges a fraction of what it was given"
    );
}

#[test]
fn the_first_second_after_the_launch_window() {
    let case = vector("equity, fresh: the first second after the launch window");
    assert_eq!(case["now"].as_i64().unwrap(), LAUNCH + 300);
    let pool = VectorPool::new(2, &case["state"], LAUNCH + 300);
    let quotes = run(&pool, &vector_swaps(std::slice::from_ref(&case)));
    assert_quotes_match_vectors(&quotes, &[case]);
}

#[test]
fn the_vectors_revenue_buys() {
    for (id, name) in [
        (4, "revenue, fresh: a buy inside both caps"),
        (5, "revenue: a buy crossing the creator's cap"),
        (6, "revenue: a buy after both caps are full"),
    ] {
        let case = vector(name);
        let pool = VectorPool::new(id, &case["state"], LATER);
        let quotes = run(&pool, &vector_swaps(std::slice::from_ref(&case)));
        assert_quotes_match_vectors(&quotes, &[case]);
    }
}
