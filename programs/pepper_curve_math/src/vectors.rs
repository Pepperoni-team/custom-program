//! The golden vectors (design §10.9): cases run through `quote_buy` and `quote_sell` and written to
//! `vectors/curve-v1.json`, which the TypeScript mirror (and, with the USDC version, the LiteSVM and
//! Jupiter suites) must reproduce. The test fails when the committed file and the functions
//! disagree; `PEPPER_CURVE_MATH_WRITE_VECTORS=1 cargo test -p pepper_curve_math` rewrites it after a
//! deliberate change. The states are illustrative, not taken from a live curve.

use super::*;

const PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/vectors/curve-v1.json");
const LAUNCH: i64 = 1_800_000_000;

enum Op {
    Buy { amount: u64, now: i64 },
    Sell { tokens: u64 },
}

struct Case {
    name: &'static str,
    state: CurveState,
    op: Op,
}

fn equity() -> CurveState {
    CurveState {
        virtual_reserve: 2_800_000_000,
        virtual_token_reserves: 107_000_000_000_000,
        real_reserve: 0,
        real_token_reserves: 76_100_000_000_000,
        creator_split_bps: 0,
        fanex_split_bps: 0,
        creator_cap: 0,
        fanex_cap: 0,
        creator_received: 0,
        fanex_received: 0,
        graduation_threshold: 6_500_000_000,
        created_at: LAUNCH,
    }
}

fn equity_near_the_end() -> CurveState {
    CurveState {
        virtual_reserve: 8_345_403_900,
        virtual_token_reserves: 35_900_000_000_000,
        real_reserve: 5_545_403_900,
        real_token_reserves: 5_000_000_000_000,
        ..equity()
    }
}

fn revenue() -> CurveState {
    CurveState {
        virtual_reserve: 1_000_000_000,
        virtual_token_reserves: 115_000_000_000_000,
        real_reserve: 0,
        real_token_reserves: 82_870_000_000_000,
        creator_split_bps: 3_760,
        fanex_split_bps: 240,
        creator_cap: 1_504_000_000,
        fanex_cap: 96_000_000,
        creator_received: 0,
        fanex_received: 0,
        graduation_threshold: 2_400_000_000,
        created_at: LAUNCH,
    }
}

/// `revenue()` with 2,000,000 tokens left and both caps open (the internal audit, 2026-10-29): the
/// curve's reserve and tokens on its constant product, 1,000 USDC of the creator's 1,504 and 60 of
/// Fanex's 96 received, so a buy past the last tokens is a partial fill that still pays both shares.
fn revenue_near_the_end() -> CurveState {
    CurveState {
        virtual_reserve: 3_369_469_000,
        virtual_token_reserves: 34_130_000_000_000,
        real_reserve: 2_369_469_000,
        real_token_reserves: 2_000_000_000_000,
        creator_received: 1_000_000_000,
        fanex_received: 60_000_000,
        ..revenue()
    }
}

fn after_a_buy(state: CurveState, amount: u64) -> (CurveState, u64) {
    let quote = quote_buy(&state, amount, LAUNCH + 600).expect("a vector's setup buy");
    (quote.after, quote.tokens_out)
}

fn cases() -> Vec<Case> {
    let later = LAUNCH + 600;
    let (bought, bought_tokens) = after_a_buy(equity(), 100_000_000);
    vec![
        Case { name: "equity, fresh: the smallest buy", state: equity(), op: Op::Buy { amount: 1, now: later } },
        Case { name: "equity, fresh: a buy under 100 base units pays no fee", state: equity(), op: Op::Buy { amount: 99, now: later } },
        Case { name: "equity, fresh: a 100 USDC buy", state: equity(), op: Op::Buy { amount: 100_000_000, now: later } },
        Case { name: "equity, fresh: the launch window's last second", state: equity(), op: Op::Buy { amount: 100_000_000, now: LAUNCH + 299 } },
        Case { name: "equity, fresh: the first second after the launch window", state: equity(), op: Op::Buy { amount: 100_000_000, now: LAUNCH + 300 } },
        Case { name: "equity, fresh: the largest buy sweeps the curve in a partial fill", state: equity(), op: Op::Buy { amount: u64::MAX, now: later } },
        Case { name: "equity, near the end: a buy reaches the threshold with tokens left", state: equity_near_the_end(), op: Op::Buy { amount: 1_000_000_000, now: later } },
        Case { name: "equity, near the end: the partial fill at graduation empties the curve", state: equity_near_the_end(), op: Op::Buy { amount: 2_000_000_000, now: later } },
        Case {
            name: "equity, near the end: a partial fill whose fee on the net is clamped to the fee on the input",
            state: equity_near_the_end(),
            op: Op::Buy { amount: 1_364_029_273, now: later },
        },
        Case {
            name: "revenue, near the end: a partial fill keeps the shares of the whole input",
            state: revenue_near_the_end(),
            op: Op::Buy { amount: 500_000_000, now: later },
        },
        Case { name: "revenue, fresh: a buy inside both caps", state: revenue(), op: Op::Buy { amount: 100_000_000, now: later } },
        Case {
            name: "revenue: a buy crossing the creator's cap",
            state: CurveState { creator_received: 1_500_000_000, fanex_received: 95_000_000, ..revenue() },
            op: Op::Buy { amount: 100_000_000, now: later },
        },
        Case {
            name: "revenue: a buy after both caps are full",
            state: CurveState { creator_received: 1_504_000_000, fanex_received: 96_000_000, ..revenue() },
            op: Op::Buy { amount: 100_000_000, now: later },
        },
        Case { name: "a buy of zero is refused", state: equity(), op: Op::Buy { amount: 0, now: later } },
        Case {
            name: "a buy on a sold-out curve is refused",
            state: CurveState { real_token_reserves: 0, ..equity() },
            op: Op::Buy { amount: 100_000_000, now: later },
        },
        Case {
            name: "a buy whose reserve counter would pass u64 is refused",
            state: CurveState {
                virtual_reserve: u64::MAX - 10,
                virtual_token_reserves: 1_000_000,
                real_token_reserves: 500_000,
                graduation_threshold: u64::MAX,
                ..equity()
            },
            op: Op::Buy { amount: 1_000_000, now: later },
        },
        Case { name: "equity, after a 100 USDC buy: a sale of one base unit pays nothing", state: bought, op: Op::Sell { tokens: 1 } },
        Case { name: "equity, after a 100 USDC buy: a sale of 1,000,000 tokens", state: bought, op: Op::Sell { tokens: 1_000_000_000_000 } },
        Case { name: "equity, after a 100 USDC buy: a sale of every token it sold", state: bought, op: Op::Sell { tokens: bought_tokens } },
        Case { name: "a sale of tokens the curve never sold is refused", state: equity(), op: Op::Sell { tokens: 1_000_000_000_000 } },
        Case { name: "a sale of zero is refused", state: equity(), op: Op::Sell { tokens: 0 } },
    ]
}

fn state_json(s: &CurveState, indent: &str) -> String {
    let fields: [(&str, String); 12] = [
        ("virtual_reserve", format!("\"{}\"", s.virtual_reserve)),
        ("virtual_token_reserves", format!("\"{}\"", s.virtual_token_reserves)),
        ("real_reserve", format!("\"{}\"", s.real_reserve)),
        ("real_token_reserves", format!("\"{}\"", s.real_token_reserves)),
        ("creator_split_bps", format!("\"{}\"", s.creator_split_bps)),
        ("fanex_split_bps", format!("\"{}\"", s.fanex_split_bps)),
        ("creator_cap", format!("\"{}\"", s.creator_cap)),
        ("fanex_cap", format!("\"{}\"", s.fanex_cap)),
        ("creator_received", format!("\"{}\"", s.creator_received)),
        ("fanex_received", format!("\"{}\"", s.fanex_received)),
        ("graduation_threshold", format!("\"{}\"", s.graduation_threshold)),
        ("created_at", s.created_at.to_string()),
    ];
    let inner = format!("{indent}  ");
    let body: Vec<String> = fields.iter().map(|(k, v)| format!("{inner}\"{k}\": {v}")).collect();
    format!("{{\n{}\n{indent}}}", body.join(",\n"))
}

fn case_json(case: &Case) -> String {
    let i = "      ";
    let mut lines = vec![format!("{i}\"name\": \"{}\"", case.name)];
    let outcome = match case.op {
        Op::Buy { amount, now } => {
            lines.push(format!("{i}\"op\": \"buy\""));
            lines.push(format!("{i}\"amount\": \"{amount}\""));
            lines.push(format!("{i}\"now\": {now}"));
            quote_buy(&case.state, amount, now).map(|q| {
                format!(
                    "{{\n{i}  \"charged\": \"{}\",\n{i}  \"tokens_out\": \"{}\",\n{i}  \"platform_fee\": \"{}\",\n{i}  \"creator_amount\": \"{}\",\n{i}  \"fanex_amount\": \"{}\",\n{i}  \"curve_amount\": \"{}\",\n{i}  \"launch_window\": {},\n{i}  \"graduates\": {},\n{i}  \"after\": {}\n{i}}}",
                    q.charged, q.tokens_out, q.platform_fee, q.creator_amount, q.fanex_amount, q.curve_amount,
                    q.launch_window, q.graduates, state_json(&q.after, &format!("{i}  "))
                )
            })
        }
        Op::Sell { tokens } => {
            lines.push(format!("{i}\"op\": \"sell\""));
            lines.push(format!("{i}\"amount\": \"{tokens}\""));
            quote_sell(&case.state, tokens).map(|q| {
                format!(
                    "{{\n{i}  \"reserve_out\": \"{}\",\n{i}  \"platform_fee\": \"{}\",\n{i}  \"proceeds\": \"{}\",\n{i}  \"after\": {}\n{i}}}",
                    q.reserve_out, q.platform_fee, q.proceeds, state_json(&q.after, &format!("{i}  "))
                )
            })
        }
    };
    lines.push(format!("{i}\"state\": {}", state_json(&case.state, i)));
    match outcome {
        Ok(result) => lines.push(format!("{i}\"result\": {result}")),
        Err(e) => lines.push(format!("{i}\"error\": \"{}\"", e.name())),
    }
    format!("    {{\n{}\n    }}", lines.join(",\n"))
}

fn render() -> String {
    let cases: Vec<String> = cases().iter().map(case_json).collect();
    format!(
        "{{\n  \"version\": 1,\n  \"source\": \"programs/pepper_curve_math, src/vectors.rs\",\n  \"cases\": [\n{}\n  ]\n}}\n",
        cases.join(",\n")
    )
}

#[test]
fn the_committed_vectors_match_the_functions() {
    let json = render();
    if std::env::var_os("PEPPER_CURVE_MATH_WRITE_VECTORS").is_some() {
        std::fs::create_dir_all(concat!(env!("CARGO_MANIFEST_DIR"), "/vectors")).unwrap();
        std::fs::write(PATH, &json).unwrap();
    }
    let committed = std::fs::read_to_string(PATH)
        .expect("vectors/curve-v1.json is missing: PEPPER_CURVE_MATH_WRITE_VECTORS=1 writes it");
    assert_eq!(
        committed.replace("\r\n", "\n"),
        json,
        "vectors/curve-v1.json and the functions disagree: a deliberate change rewrites it with PEPPER_CURVE_MATH_WRITE_VECTORS=1"
    );
}

#[test]
fn the_vectors_reach_every_path_the_design_lists() {
    let outcomes: Vec<(&str, Result<Option<BuyQuote>, MathError>)> = cases()
        .iter()
        .map(|c| match c.op {
            Op::Buy { amount, now } => (c.name, quote_buy(&c.state, amount, now).map(Some)),
            Op::Sell { tokens } => (c.name, quote_sell(&c.state, tokens).map(|_| None)),
        })
        .collect();
    let buy = |name: &str| outcomes.iter().find(|(n, _)| *n == name).unwrap().1.unwrap().unwrap();
    assert_eq!(buy("equity, fresh: the smallest buy").platform_fee, 0);
    assert_eq!(buy("equity, fresh: a buy under 100 base units pays no fee").platform_fee, 0);
    assert!(buy("equity, fresh: the launch window's last second").launch_window);
    assert!(!buy("equity, fresh: the first second after the launch window").launch_window);
    let sweep = buy("equity, fresh: the largest buy sweeps the curve in a partial fill");
    assert!(sweep.charged < u64::MAX && sweep.after.real_token_reserves == 0 && sweep.graduates);
    let threshold = buy("equity, near the end: a buy reaches the threshold with tokens left");
    assert!(threshold.graduates && threshold.after.real_token_reserves > 0);
    let partial = buy("equity, near the end: the partial fill at graduation empties the curve");
    assert!(partial.charged < 2_000_000_000 && partial.after.real_token_reserves == 0);
    // The fee clamp binds: the fee on what was swapped, ceil(net / 99), is one past the fee on the
    // input, floor(amount / 100), so the input's is charged (the internal audit, 2026-10-29).
    let clamp = buy("equity, near the end: a partial fill whose fee on the net is clamped to the fee on the input");
    assert_eq!(clamp.after.real_token_reserves, 0);
    assert_eq!(clamp.platform_fee, platform_fee(1_364_029_273).unwrap());
    assert_eq!(clamp.platform_fee, 13_640_292);
    assert_eq!(platform_fee_on_net(clamp.charged - clamp.platform_fee).unwrap(), clamp.platform_fee + 1);
    // A partial fill on a raise: the shares are the whole input's, the curve's part only the last
    // tokens' cost, the fee on what was used.
    let raise_end = buy("revenue, near the end: a partial fill keeps the shares of the whole input");
    assert_eq!(raise_end.after.real_token_reserves, 0);
    assert!(raise_end.creator_amount > 0 && raise_end.fanex_amount > 0);
    let net_in = 500_000_000 - platform_fee(500_000_000).unwrap();
    assert_eq!(raise_end.creator_amount, net_in * 3_760 / 10_000);
    assert_eq!(raise_end.fanex_amount, net_in * 240 / 10_000);
    assert!(raise_end.charged < 500_000_000);
    assert_eq!(
        raise_end.charged,
        raise_end.creator_amount + raise_end.fanex_amount + raise_end.curve_amount + raise_end.platform_fee
    );
    let inside = buy("revenue, fresh: a buy inside both caps");
    assert!(inside.creator_amount > 0 && inside.fanex_amount > 0);
    let crossing = buy("revenue: a buy crossing the creator's cap");
    assert_eq!((crossing.creator_amount, crossing.fanex_amount), (4_000_000, 1_000_000));
    let full = buy("revenue: a buy after both caps are full");
    assert_eq!((full.creator_amount, full.fanex_amount), (0, 0));
    let refused = |name: &str| outcomes.iter().find(|(n, _)| *n == name).unwrap().1.unwrap_err();
    assert_eq!(refused("a buy of zero is refused"), MathError::ZeroAmount);
    assert_eq!(refused("a buy on a sold-out curve is refused"), MathError::CurveComplete);
    assert_eq!(refused("a buy whose reserve counter would pass u64 is refused"), MathError::MathOverflow);
    assert_eq!(refused("equity, after a 100 USDC buy: a sale of one base unit pays nothing"), MathError::ZeroAmount);
    assert_eq!(refused("a sale of tokens the curve never sold is refused"), MathError::InsufficientReserves);
    assert_eq!(refused("a sale of zero is refused"), MathError::ZeroAmount);
}
