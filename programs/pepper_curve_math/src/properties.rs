//! Seeded invariant tests (the internal audit, 2026-10-29): random curves in the launch builder's
//! shape, random buys and sales through `quote_buy` and `quote_sell`, and after every step the
//! invariants the program and its mirrors rely on. A fixed seed and no dependency, so a failure
//! reproduces as it is:
//! - a buy charges at most what it was given, exactly its parts, and never past either cap;
//! - a sale pays at most the real reserve, its proceeds and its fee summing to what leaves it;
//! - the product of the virtual reserves never falls, and each virtual reserve stays its real one
//!   plus the gap it launched with, which no trade moves;
//! - whatever was bought, selling it all back, at once or in chunks, fits in the real reserve.

use super::*;

/// xorshift64*, from a fixed seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `lo..=hi`.
    fn between(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }
}

const USDC: u64 = 1_000_000;
/// The fixed supply, 100,000,000 tokens of 6 decimals.
const SUPPLY: u64 = 100_000_000_000_000;
const LAUNCH: i64 = 1_800_000_000;

/// A curve as the launch builder shapes one (`usdc-launch-params.ts`): the supply split of its kind,
/// `K_RATIO` 0.2234 over the threshold and the curve's tokens, a revenue raise's 94/6 caps and their
/// splits over `target + threshold`, or an equity curve's band and no raise.
fn launch(rng: &mut Rng) -> CurveState {
    let revenue = rng.next() % 2 == 0;
    let curve_tokens = if revenue { SUPPLY / 10_000 * 8_287 } else { SUPPLY / 10_000 * 7_610 };
    let (threshold, creator_cap, fanex_cap, creator_bps, fanex_bps) = if revenue {
        let target = rng.between(1_600 * USDC, 100_000 * USDC);
        let threshold = (target * 3 / 2).clamp(2_400 * USDC, 80_000 * USDC);
        let creator_cap = target * 94 / 100;
        let fanex_cap = target - creator_cap;
        let total = (target + threshold) as f64;
        let bps = |cap: u64| ((cap as f64) * 10_000.0 / total).round() as u64;
        (threshold, creator_cap, fanex_cap, bps(creator_cap), bps(fanex_cap))
    } else {
        let bands = [6_900 * USDC, 13_000 * USDC, 25_000 * USDC, 42_000 * USDC];
        (bands[(rng.next() % 4) as usize], 0, 0, 0, 0)
    };
    CurveState {
        virtual_reserve: (threshold as f64 * 0.2234).round() as u64,
        virtual_token_reserves: curve_tokens / 1_000_000 * 1_223_400,
        real_reserve: 0,
        real_token_reserves: curve_tokens,
        creator_split_bps: creator_bps,
        fanex_split_bps: fanex_bps,
        creator_cap,
        fanex_cap,
        creator_received: 0,
        fanex_received: 0,
        graduation_threshold: threshold,
        created_at: LAUNCH,
    }
}

fn k(s: &CurveState) -> u128 {
    u128::from(s.virtual_reserve) * u128::from(s.virtual_token_reserves)
}

fn gaps(s: &CurveState) -> (u64, u64) {
    (s.virtual_reserve - s.real_reserve, s.virtual_token_reserves - s.real_token_reserves)
}

/// Sells `held` tokens back in `chunks` parts: each sale pays at most the reserve, a part too small
/// to pay anything is carried into the next, and the last sale takes whatever is left.
fn sell_back(mut state: CurveState, held: u64, chunks: u64) {
    let mut left = held;
    let mut carried = 0;
    for i in 0..chunks {
        let part = if i + 1 == chunks { left } else { left / (chunks - i) };
        left -= part;
        let tokens = part + carried;
        if tokens == 0 {
            continue;
        }
        match quote_sell(&state, tokens) {
            Ok(q) => {
                assert!(q.reserve_out <= state.real_reserve);
                state = q.after;
                carried = 0;
            }
            Err(MathError::ZeroAmount) => carried = tokens,
            Err(e) => panic!("selling back {tokens} of {held}: {e:?} on {state:?}"),
        }
    }
}

#[test]
fn random_trades_on_launch_shaped_curves_keep_every_invariant() {
    let mut rng = Rng(0x5EED_2026_1029_0001);
    let mut steps = 0u32;
    for _ in 0..1_000 {
        let mut state = launch(&mut rng);
        let start_gaps = gaps(&state);
        let mut held = 0u64;
        for _ in 0..60 {
            let before = state;
            if held == 0 || rng.next() % 3 != 0 {
                // A buy, from dust to past the whole curve.
                let amount = match rng.next() % 20 {
                    0..=7 => rng.between(1, 10_000),
                    8..=15 => rng.between(1, 100 * USDC),
                    16..=18 => rng.between(1, 5_000 * USDC),
                    _ => rng.between(1, 200_000 * USDC),
                };
                let Ok(q) = quote_buy(&state, amount, LAUNCH + 600) else { continue };
                assert!(q.charged <= amount, "charged {} of {amount}", q.charged);
                assert_eq!(q.charged, q.creator_amount + q.fanex_amount + q.curve_amount + q.platform_fee);
                assert!(q.tokens_out <= before.real_token_reserves);
                assert!(q.after.creator_received <= q.after.creator_cap);
                assert!(q.after.fanex_received <= q.after.fanex_cap);
                assert_eq!(q.after.real_reserve - before.real_reserve, q.curve_amount);
                state = q.after;
                held += q.tokens_out;
                if q.graduates {
                    assert!(state.real_reserve >= state.graduation_threshold || state.real_token_reserves == 0);
                }
            } else {
                let tokens = rng.between(1, held);
                let Ok(q) = quote_sell(&state, tokens) else { continue };
                assert!(q.reserve_out <= before.real_reserve);
                assert_eq!(q.proceeds + q.platform_fee, q.reserve_out);
                state = q.after;
                held -= tokens;
            }
            assert!(k(&state) >= k(&before), "the product fell: {before:?} -> {state:?}");
            assert_eq!(gaps(&state), start_gaps);
            steps += 1;
            if state.real_token_reserves == 0 || state.real_reserve >= state.graduation_threshold {
                break;
            }
        }
        sell_back(state, held, 1);
        sell_back(state, held, rng.between(2, 25));
    }
    assert!(steps > 20_000, "{steps} steps");
}
