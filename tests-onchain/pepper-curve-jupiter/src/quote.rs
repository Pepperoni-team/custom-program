//! The quote: the program's own arithmetic, `pepper_curve_math`'s `quote_buy` and `quote_sell`, the
//! functions `buy_exact_in` and `sell_exact_in` call and then move exactly what they return
//! (USDC curve design §10.4). Nothing here recomputes a price: it maps the decoded accounts onto the
//! crate's `CurveState` as the program's `CurveUsdc::swap_state` does, and refuses what the program
//! would refuse, or might, from what the two accounts say.

use std::fmt;

use pepper_curve_math::{in_launch_window, quote_buy, quote_sell, CurveState, MathError};
use solana_pubkey::Pubkey;

use crate::layout::{CurveUsdc, UsdcConfig, PHASE_BONDING};

/// Which way a swap goes: USDC into tokens, or tokens into USDC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// `buy_exact_in`: USDC in, the curve's token out.
    Buy,
    /// `sell_exact_in`: the curve's token in, USDC out.
    Sell,
}

/// Why no quote: the program's refusals, or a case it may refuse on what no quote can read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// `UsdcConfig.paused`: the program refuses every swap (6000).
    ConfigPaused,
    /// `CurveUsdc.paused`: the program refuses this curve's swaps (6001).
    CurvePaused,
    /// A phase past Bonding: Graduating, Graduated or PoolCreated (6002).
    NotBonding { phase: u8 },
    /// The curve's first five minutes, where a buy must go to the signer's own associated token
    /// account and stay within 1% of the supply per wallet, a holding no quote can read.
    LaunchWindow { created_at: i64, now: i64 },
    /// The crate's own refusal, the program's error of the same name.
    Math(MathError),
    /// A buy that would deliver no token, which the program refuses (`ZeroAmount`).
    NoTokensOut,
    /// A buy owes the creator's raise share while the curve records no account for it (a migrated
    /// revenue curve may lack one): the program refuses every buy until the creator's cap fills.
    CreatorUsdcAccountUnset,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::ConfigPaused => write!(f, "the USDC side is paused"),
            Refusal::CurvePaused => write!(f, "the curve is paused"),
            Refusal::NotBonding { phase } => write!(f, "the curve is not bonding (phase {phase})"),
            Refusal::LaunchWindow { created_at, now } => write!(
                f,
                "inside the curve's launch window ({now} < {created_at} + {} s)",
                pepper_curve_math::LAUNCH_GUARD_SECONDS
            ),
            Refusal::Math(error) => write!(f, "{}", error.name()),
            Refusal::NoTokensOut => write!(f, "the buy delivers no token"),
            Refusal::CreatorUsdcAccountUnset => {
                write!(
                    f,
                    "the curve owes its creator a share but records no creator USDC account"
                )
            }
        }
    }
}

impl std::error::Error for Refusal {}

impl CurveUsdc {
    /// The terms and counters a swap reads, as the program's `CurveUsdc::swap_state` hands them to
    /// `quote_buy` and `quote_sell`: each µUSDC field its unit-free twin, the two bps widened from
    /// u16, the graduation threshold as the crate's `graduation_threshold` (never the creator target).
    pub fn swap_state(&self) -> CurveState {
        CurveState {
            virtual_reserve: self.virtual_usdc_reserves,
            virtual_token_reserves: self.virtual_token_reserves,
            real_reserve: self.real_usdc_reserves,
            real_token_reserves: self.real_token_reserves,
            creator_split_bps: u64::from(self.creator_split_bps),
            fanex_split_bps: u64::from(self.fanex_split_bps),
            creator_cap: self.creator_cap_usdc,
            fanex_cap: self.fanex_cap_usdc,
            creator_received: self.creator_received_usdc,
            fanex_received: self.fanex_received_usdc,
            graduation_threshold: self.graduation_threshold_usdc,
            created_at: self.created_at,
        }
    }

    /// Whether a buy must carry the creator's USDC account, its account 8: exactly while the curve
    /// takes a creator split and the creator's cap is open, by the counters, never by the buy's own
    /// share, which a tiny buy rounds to 0 (the program's `validation::creator_usdc_account_required`).
    pub fn creator_usdc_account_required(&self) -> bool {
        self.creator_split_bps > 0 && self.creator_received_usdc < self.creator_cap_usdc
    }
}

/// Whether the curve trades now, as the program reads it before any amount: both pauses, the phase,
/// and the launch window, where a router does not route (§10.5). `Ok` is `is_active`.
pub fn check_tradeable(curve: &CurveUsdc, config: &UsdcConfig, now: i64) -> Result<(), Refusal> {
    if config.paused {
        return Err(Refusal::ConfigPaused);
    }
    if curve.paused {
        return Err(Refusal::CurvePaused);
    }
    if curve.phase != PHASE_BONDING {
        return Err(Refusal::NotBonding { phase: curve.phase });
    }
    if in_launch_window(curve.created_at, now) {
        return Err(Refusal::LaunchWindow {
            created_at: curve.created_at,
            now,
        });
    }
    Ok(())
}

/// One exact-in swap as the program executes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CurveQuote {
    /// What leaves the source: on a buy what the curve charges, below the amount asked on a partial
    /// fill at graduation; on a sale the tokens in.
    pub in_amount: u64,
    /// What reaches the destination: the tokens bought, or the USDC proceeds net of the fee.
    pub out_amount: u64,
    /// In USDC, what does not reach the curve or the seller: on a buy the 1% and, on a revenue curve,
    /// the creator's and Fanex's raise shares; on a sale the 1% of the gross.
    pub fee_amount: u64,
    /// In USDC, what `fee_amount` is a share of: what a buy charges, or what a sale takes out of the
    /// curve's reserve.
    pub fee_base: u64,
    /// The buy takes the curve out of Bonding: its reserve reaches the threshold, or its last token
    /// goes.
    pub graduates: bool,
}

/// The quote for `amount` in, `direction`, at `now` (the chain's clock): `quote_buy` or `quote_sell`
/// on the curve's own counters, after the checks the program makes from the same two accounts.
pub fn quote(
    curve: &CurveUsdc,
    config: &UsdcConfig,
    direction: Direction,
    amount: u64,
    now: i64,
) -> Result<CurveQuote, Refusal> {
    check_tradeable(curve, config, now)?;
    match direction {
        Direction::Buy => {
            let buy = quote_buy(&curve.swap_state(), amount, now).map_err(Refusal::Math)?;
            if curve.creator_usdc_account_required()
                && curve.creator_usdc_account == Pubkey::default()
            {
                return Err(Refusal::CreatorUsdcAccountUnset);
            }
            if buy.tokens_out == 0 {
                return Err(Refusal::NoTokensOut);
            }
            // At most `charged`, which the crate computed as their sum with the curve's amount.
            let fee_amount = buy.platform_fee + buy.creator_amount + buy.fanex_amount;
            Ok(CurveQuote {
                in_amount: buy.charged,
                out_amount: buy.tokens_out,
                fee_amount,
                fee_base: buy.charged,
                graduates: buy.graduates,
            })
        }
        Direction::Sell => {
            let sale = quote_sell(&curve.swap_state(), amount).map_err(Refusal::Math)?;
            Ok(CurveQuote {
                in_amount: amount,
                out_amount: sale.proceeds,
                fee_amount: sale.platform_fee,
                fee_base: sale.reserve_out,
                graduates: false,
            })
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::layout::tests::{distinct_config, key};
    use serde_json::Value;

    const VECTORS: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../programs/pepper_curve_math/vectors/curve-v1.json"
    );

    fn u64_of(value: &Value) -> u64 {
        value
            .as_str()
            .expect("a u64 as a string")
            .parse()
            .expect("a u64")
    }

    /// A Bonding, unpaused curve holding a vector's `state`, the rest of its fields arbitrary.
    pub(crate) fn curve_from_state(state: &Value) -> CurveUsdc {
        CurveUsdc {
            bump: 255,
            usdc_vault_bump: 255,
            phase: PHASE_BONDING,
            paused: false,
            migrated: false,
            mint: key(31),
            creator: key(32),
            creator_usdc_account: key(33),
            creator_split_bps: u64_of(&state["creator_split_bps"]) as u16,
            fanex_split_bps: u64_of(&state["fanex_split_bps"]) as u16,
            creator_target_usdc: u64_of(&state["creator_cap"]) + u64_of(&state["fanex_cap"]),
            graduation_threshold_usdc: u64_of(&state["graduation_threshold"]),
            creator_cap_usdc: u64_of(&state["creator_cap"]),
            fanex_cap_usdc: u64_of(&state["fanex_cap"]),
            virtual_usdc_reserves: u64_of(&state["virtual_reserve"]),
            virtual_token_reserves: u64_of(&state["virtual_token_reserves"]),
            real_usdc_reserves: u64_of(&state["real_reserve"]),
            real_token_reserves: u64_of(&state["real_token_reserves"]),
            migration_token_reserves: 0,
            creator_received_usdc: u64_of(&state["creator_received"]),
            fanex_received_usdc: u64_of(&state["fanex_received"]),
            created_at: state["created_at"].as_i64().expect("created_at"),
            graduated_at: 0,
            migrated_at: 0,
        }
    }

    pub(crate) fn open_config() -> UsdcConfig {
        UsdcConfig {
            paused: false,
            ..distinct_config()
        }
    }

    pub(crate) fn vector_cases() -> Vec<Value> {
        let json: Value =
            serde_json::from_str(&std::fs::read_to_string(VECTORS).expect("the golden vectors"))
                .expect("the golden vectors parse");
        assert_eq!(json["version"], 1);
        json["cases"].as_array().expect("cases").clone()
    }

    /// Every golden vector through the SDK's quote: the crate answers, so these hold the mapping of
    /// the accounts onto it and what the SDK reports from its answer (§10.9: the Jupiter suite reads
    /// the vectors too).
    #[test]
    fn every_golden_vector_quotes_as_the_crate_computed_it() {
        let cases = vector_cases();
        // 21 since the internal audit (2026-10-29): a partial fill keeping the raise's shares and
        // one whose fee on the net is clamped to the fee on the input.
        assert_eq!(cases.len(), 21);
        for case in &cases {
            let name = case["name"].as_str().unwrap();
            let curve = curve_from_state(&case["state"]);
            let amount = u64_of(&case["amount"]);
            let (direction, now) = match case["op"].as_str().unwrap() {
                "buy" => (Direction::Buy, case["now"].as_i64().unwrap()),
                "sell" => (Direction::Sell, curve.created_at + 600),
                other => panic!("{name}: op {other}"),
            };
            let quoted = quote(&curve, &open_config(), direction, amount, now);
            let result = &case["result"];
            if let Some(error) = case["error"].as_str() {
                match quoted {
                    Err(Refusal::Math(e)) => assert_eq!(e.name(), error, "{name}"),
                    other => panic!("{name}: expected {error}, got {other:?}"),
                }
                continue;
            }
            if direction == Direction::Buy && result["launch_window"] == true {
                assert_eq!(
                    quoted,
                    Err(Refusal::LaunchWindow {
                        created_at: curve.created_at,
                        now
                    }),
                    "{name}"
                );
                continue;
            }
            let quoted = quoted.unwrap_or_else(|e| panic!("{name}: {e}"));
            match direction {
                Direction::Buy => {
                    assert_eq!(
                        quoted.in_amount,
                        u64_of(&result["charged"]),
                        "{name}: in_amount is what is charged"
                    );
                    assert_eq!(quoted.out_amount, u64_of(&result["tokens_out"]), "{name}");
                    let fees = u64_of(&result["platform_fee"])
                        + u64_of(&result["creator_amount"])
                        + u64_of(&result["fanex_amount"]);
                    assert_eq!(quoted.fee_amount, fees, "{name}");
                    assert_eq!(quoted.fee_base, quoted.in_amount, "{name}");
                    assert_eq!(
                        quoted.in_amount,
                        fees + u64_of(&result["curve_amount"]),
                        "{name}"
                    );
                    assert_eq!(quoted.graduates, result["graduates"] == true, "{name}");
                    assert!(quoted.in_amount <= amount, "{name}: never more than asked");
                }
                Direction::Sell => {
                    assert_eq!(quoted.in_amount, amount, "{name}");
                    assert_eq!(quoted.out_amount, u64_of(&result["proceeds"]), "{name}");
                    assert_eq!(quoted.fee_amount, u64_of(&result["platform_fee"]), "{name}");
                    assert_eq!(quoted.fee_base, u64_of(&result["reserve_out"]), "{name}");
                    assert!(!quoted.graduates);
                }
            }
        }
    }

    /// The program's own mapping (state.rs, `curve_usdc_math_tests`): every field distinct, so a
    /// field mapped onto the wrong twin shows.
    #[test]
    fn the_curve_maps_onto_the_crate_as_the_program_maps_it() {
        let curve = crate::layout::tests::distinct_curve();
        assert_eq!(
            curve.swap_state(),
            CurveState {
                virtual_reserve: 0x1111_1111_1111_1105,
                virtual_token_reserves: 0x1111_1111_1111_1106,
                real_reserve: 0x1111_1111_1111_1107,
                real_token_reserves: 0x1111_1111_1111_1108,
                creator_split_bps: 0x0102,
                fanex_split_bps: 0x0304,
                creator_cap: 0x1111_1111_1111_1103,
                fanex_cap: 0x1111_1111_1111_1104,
                creator_received: 0x1111_1111_1111_110A,
                fanex_received: 0x1111_1111_1111_110B,
                graduation_threshold: 0x1111_1111_1111_1102,
                created_at: -0x2222_2222_2222_2201,
            }
        );
    }

    fn revenue_curve() -> CurveUsdc {
        let cases = vector_cases();
        let case = cases
            .iter()
            .find(|c| c["name"] == "revenue, fresh: a buy inside both caps")
            .unwrap();
        curve_from_state(&case["state"])
    }

    #[test]
    fn account_8_is_due_exactly_while_the_creator_s_cap_is_open() {
        let curve = revenue_curve();
        assert!(curve.creator_usdc_account_required());
        let one_short = CurveUsdc {
            creator_received_usdc: curve.creator_cap_usdc - 1,
            ..curve
        };
        assert!(one_short.creator_usdc_account_required());
        let full = CurveUsdc {
            creator_received_usdc: curve.creator_cap_usdc,
            ..curve
        };
        assert!(!full.creator_usdc_account_required());
        let past = CurveUsdc {
            creator_received_usdc: curve.creator_cap_usdc + 1,
            ..curve
        };
        assert!(!past.creator_usdc_account_required());
        let equity = CurveUsdc {
            creator_split_bps: 0,
            ..curve
        };
        assert!(!equity.creator_usdc_account_required());
    }

    #[test]
    fn a_buy_owing_the_creator_with_no_account_is_refused_and_a_sale_is_not() {
        let unset = CurveUsdc {
            creator_usdc_account: Pubkey::default(),
            real_usdc_reserves: 100_000_000,
            ..revenue_curve()
        };
        let now = unset.created_at + 600;
        assert_eq!(
            quote(&unset, &open_config(), Direction::Buy, 100_000_000, now),
            Err(Refusal::CreatorUsdcAccountUnset)
        );
        assert!(quote(
            &unset,
            &open_config(),
            Direction::Sell,
            1_000_000_000_000,
            now
        )
        .is_ok());
        // Once the cap is full the account is no longer due, unset or not.
        let full = CurveUsdc {
            creator_received_usdc: unset.creator_cap_usdc,
            ..unset
        };
        assert!(quote(&full, &open_config(), Direction::Buy, 100_000_000, now).is_ok());
    }

    #[test]
    fn nothing_trades_while_paused_past_bonding_or_inside_the_launch_window() {
        let curve = revenue_curve();
        let config = open_config();
        let after = curve.created_at + 300;
        assert_eq!(check_tradeable(&curve, &config, after), Ok(()));
        assert_eq!(
            check_tradeable(
                &curve,
                &UsdcConfig {
                    paused: true,
                    ..config
                },
                after
            ),
            Err(Refusal::ConfigPaused)
        );
        assert_eq!(
            check_tradeable(
                &CurveUsdc {
                    paused: true,
                    ..curve
                },
                &config,
                after
            ),
            Err(Refusal::CurvePaused)
        );
        for phase in 1..=3 {
            assert_eq!(
                check_tradeable(&CurveUsdc { phase, ..curve }, &config, after),
                Err(Refusal::NotBonding { phase })
            );
        }
        // The window's last second, and a clock behind the launch, are inside it.
        for now in [
            curve.created_at + 299,
            curve.created_at,
            curve.created_at - 1,
        ] {
            assert_eq!(
                check_tradeable(&curve, &config, now),
                Err(Refusal::LaunchWindow {
                    created_at: curve.created_at,
                    now
                })
            );
        }
        // A quote refuses what `check_tradeable` refuses, either way.
        for direction in [Direction::Buy, Direction::Sell] {
            assert_eq!(
                quote(
                    &CurveUsdc { phase: 1, ..curve },
                    &config,
                    direction,
                    1_000_000,
                    after
                ),
                Err(Refusal::NotBonding { phase: 1 })
            );
        }
    }

    #[test]
    fn a_buy_that_delivers_no_token_is_refused() {
        // A reserve so deep that one µUSDC buys less than one base unit of the token.
        let curve = CurveUsdc {
            virtual_usdc_reserves: 1_000_000_000_000_000,
            virtual_token_reserves: 1_000,
            real_token_reserves: 1_000,
            creator_split_bps: 0,
            fanex_split_bps: 0,
            ..revenue_curve()
        };
        assert_eq!(
            quote(
                &curve,
                &open_config(),
                Direction::Buy,
                1,
                curve.created_at + 600
            ),
            Err(Refusal::NoTokensOut)
        );
    }
}
