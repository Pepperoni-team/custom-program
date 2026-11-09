//! The bonding curve's arithmetic, with no Anchor or Solana dependency (USDC curve design §10.4,
//! decision 10). Amounts are base units: of the curve's reserve (lamports on a SOL curve, µUSDC on
//! a USDC curve) and of the token (10^-6). u64 in and out, u128 inside, every step checked.
//!
//! `quote_buy` and `quote_sell` are the point: the program's handlers call them and move exactly
//! what they return, the Jupiter SDK quotes with them, and the golden vectors
//! (`vectors/curve-v1.json`) pin them for the TypeScript mirror
//! (`lib/solana/pepper-curve/curve-math.ts`). `pepper_curve` depends on this crate (2026-10-22)
//! and maps `MathError` onto its own error codes: its USDC handlers call it (`buy_exact_in` and
//! `sell_exact_in` move what `quote_buy` and `quote_sell` return since 2026-10-23), while its SOL
//! handlers keep their own `math.rs`, untouched (Rick, 2026-10-22).

#![forbid(unsafe_code)]

#[cfg(test)]
mod vectors;
// Seeded invariant tests over random launches and trades (the internal audit, 2026-10-29).
#[cfg(test)]
mod properties;

/// Basis points in a whole.
pub const BPS_DENOMINATOR: u64 = 10_000;
/// The platform swap fee: 1% of every curve buy and sale.
pub const PLATFORM_SWAP_FEE_BPS: u64 = 100;
/// The launch window: the first five minutes after a curve's `created_at`.
pub const LAUNCH_GUARD_SECONDS: i64 = 300;
/// The most a token account may hold after a buy inside the launch window: 1% of the supply.
pub const LAUNCH_WALLET_CAP: u64 = 1_000_000_000_000;
/// A curve created from this instant (2026-11-01 00:00 UTC) releases its vesting only once graduated.
pub const VESTING_LOCKED_UNTIL_GRADUATION_FROM: i64 = 1_793_491_200;

/// Why a computation refused, named as the program refuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MathError {
    /// A result, or a counter it would leave, does not fit in a u64.
    MathOverflow,
    /// A zero amount in, or a sale that would pay nothing.
    ZeroAmount,
    /// The curve has no token left to sell.
    CurveComplete,
    /// A sale would pay out more than the curve holds.
    InsufficientReserves,
}

impl MathError {
    /// The name the program's error carries, and the vectors and the TypeScript mirror use.
    pub const fn name(self) -> &'static str {
        match self {
            MathError::MathOverflow => "MathOverflow",
            MathError::ZeroAmount => "ZeroAmount",
            MathError::CurveComplete => "CurveComplete",
            MathError::InsufficientReserves => "InsufficientReserves",
        }
    }
}

pub type MathResult<T> = Result<T, MathError>;

fn to_u64(value: u128) -> MathResult<u64> {
    u64::try_from(value).map_err(|_| MathError::MathOverflow)
}

fn add(a: u64, b: u64) -> MathResult<u64> {
    a.checked_add(b).ok_or(MathError::MathOverflow)
}

fn sub(a: u64, b: u64) -> MathResult<u64> {
    a.checked_sub(b).ok_or(MathError::MathOverflow)
}

/// The platform fee on `amount`: floor(amount × 100 / 10,000), so 0 under 100 base units.
pub fn platform_fee(amount: u64) -> MathResult<u64> {
    let fee = (amount as u128)
        .checked_mul(PLATFORM_SWAP_FEE_BPS as u128)
        .ok_or(MathError::MathOverflow)?
        / BPS_DENOMINATOR as u128;
    to_u64(fee)
}

/// The platform fee as a share of the net after it: ceil(net × 100 / 9,900). A graduation's partial
/// fill charges it on what the buyer actually swaps, never above the fee on the whole input.
pub fn platform_fee_on_net(net: u64) -> MathResult<u64> {
    let num = (net as u128)
        .checked_mul(PLATFORM_SWAP_FEE_BPS as u128)
        .ok_or(MathError::MathOverflow)?;
    let den = (BPS_DENOMINATOR - PLATFORM_SWAP_FEE_BPS) as u128;
    to_u64((num + den - 1) / den)
}

/// One buy's net, split between the creator, Fanex and the curve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Split {
    pub creator: u64,
    pub fanex: u64,
    pub curve: u64,
}

/// The raise's split of `amount`: each share floor(amount × bps / 10,000), clamped to what is left
/// of its cap; whatever a cap holds back goes to the curve.
pub fn split_with_cap(
    amount: u64,
    creator_split_bps: u64,
    fanex_split_bps: u64,
    creator_cap: u64,
    fanex_cap: u64,
    creator_received: u64,
    fanex_received: u64,
) -> MathResult<Split> {
    let share = |bps: u64| -> MathResult<u128> {
        Ok((amount as u128)
            .checked_mul(bps as u128)
            .ok_or(MathError::MathOverflow)?
            / BPS_DENOMINATOR as u128)
    };
    let creator = share(creator_split_bps)?.min(creator_cap.saturating_sub(creator_received) as u128);
    let fanex = share(fanex_split_bps)?.min(fanex_cap.saturating_sub(fanex_received) as u128);
    let creator = to_u64(creator)?;
    let fanex = to_u64(fanex)?;
    let curve = sub(sub(amount, creator)?, fanex)?;
    Ok(Split { creator, fanex, curve })
}

/// Tokens out for `reserve_in` paid into the curve: floor(y × dx / (x + dx)).
pub fn buy_tokens_out(virtual_reserve: u64, virtual_tokens: u64, reserve_in: u64) -> MathResult<u64> {
    if reserve_in == 0 {
        return Err(MathError::ZeroAmount);
    }
    let num = (virtual_tokens as u128) * reserve_in as u128;
    let den = virtual_reserve as u128 + reserve_in as u128;
    to_u64(num / den)
}

/// The reserve that buys exactly `tokens_out`: ceil(x × dy / (y − dy)), so the last tokens are
/// never sold too cheap. `tokens_out` must be below `virtual_tokens`.
pub fn reserve_for_tokens(virtual_reserve: u64, virtual_tokens: u64, tokens_out: u64) -> MathResult<u64> {
    if tokens_out == 0 {
        return Err(MathError::ZeroAmount);
    }
    if virtual_tokens <= tokens_out {
        return Err(MathError::MathOverflow);
    }
    let num = virtual_reserve as u128 * tokens_out as u128;
    let den = (virtual_tokens - tokens_out) as u128;
    to_u64((num + den - 1) / den)
}

/// The reserve out for `tokens_in` sold to the curve: floor(x × dy / (y + dy)).
pub fn sell_reserve_out(virtual_reserve: u64, virtual_tokens: u64, tokens_in: u64) -> MathResult<u64> {
    if tokens_in == 0 {
        return Err(MathError::ZeroAmount);
    }
    let num = virtual_reserve as u128 * tokens_in as u128;
    let den = virtual_tokens as u128 + tokens_in as u128;
    to_u64(num / den)
}

/// Vesting released at `now`: nothing before the cliff; then linear over `duration_seconds`
/// measured from the cliff's end, or everything at once when the duration is 0.
pub fn vested_amount(total: u64, start: i64, cliff_seconds: i64, duration_seconds: i64, now: i64) -> u64 {
    if now < start.saturating_add(cliff_seconds) {
        return 0;
    }
    let elapsed = now.saturating_sub(start.saturating_add(cliff_seconds));
    if elapsed >= duration_seconds || duration_seconds == 0 {
        return total;
    }
    ((total as u128).saturating_mul(elapsed as u128) / duration_seconds as u128) as u64
}

/// Whether `now` falls in a curve's launch window; a clock behind `created_at` does.
pub fn in_launch_window(created_at: i64, now: i64) -> bool {
    now < created_at.saturating_add(LAUNCH_GUARD_SECONDS)
}

/// Whether a buy may leave its buyer's token account holding `holding + tokens_out`: always after
/// the launch window, up to `LAUNCH_WALLET_CAP` inside it; a holding that overflows is over the cap.
pub fn launch_cap_allows(created_at: i64, now: i64, holding: u64, tokens_out: u64) -> bool {
    if !in_launch_window(created_at, now) {
        return true;
    }
    holding.checked_add(tokens_out).is_some_and(|after| after <= LAUNCH_WALLET_CAP)
}

/// Whether a curve's vesting may be claimed: one created before
/// `VESTING_LOCKED_UNTIL_GRADUATION_FROM` keeps its launch terms; a later one waits for graduation.
pub fn vesting_claim_allowed(created_at: i64, graduated: bool) -> bool {
    created_at < VESTING_LOCKED_UNTIL_GRADUATION_FROM || graduated
}

/// The part of a curve's account the swaps read and write, in the curve's own units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CurveState {
    pub virtual_reserve: u64,
    pub virtual_token_reserves: u64,
    pub real_reserve: u64,
    pub real_token_reserves: u64,
    pub creator_split_bps: u64,
    pub fanex_split_bps: u64,
    pub creator_cap: u64,
    pub fanex_cap: u64,
    pub creator_received: u64,
    pub fanex_received: u64,
    pub graduation_threshold: u64,
    pub created_at: i64,
}

/// What a buy charges, delivers and leaves behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuyQuote {
    /// What leaves the buyer: the shares, the curve amount and the fee. Below the amount asked on
    /// a graduation's partial fill.
    pub charged: u64,
    /// Tokens delivered. 0 is not refused here: the handler refuses it after the launch cap and the
    /// slippage check, in today's order.
    pub tokens_out: u64,
    pub platform_fee: u64,
    pub creator_amount: u64,
    pub fanex_amount: u64,
    pub curve_amount: u64,
    /// The buy falls in the launch window, where the handler applies the per-wallet cap.
    pub launch_window: bool,
    /// The curve leaves Bonding: its reserve reached the threshold, or its tokens ran out.
    pub graduates: bool,
    /// The curve once the buy lands.
    pub after: CurveState,
}

/// A buy of `amount_in` on a curve in Bonding (the pauses and the phase are the handler's), as the
/// program executes it (design §10.3.5, steps 1 to 10 save the transfers).
pub fn quote_buy(state: &CurveState, amount_in: u64, now: i64) -> MathResult<BuyQuote> {
    if amount_in == 0 {
        return Err(MathError::ZeroAmount);
    }
    if state.real_token_reserves == 0 {
        return Err(MathError::CurveComplete);
    }
    let mut fee = platform_fee(amount_in)?;
    let net = sub(amount_in, fee)?;
    let split = split_with_cap(
        net,
        state.creator_split_bps,
        state.fanex_split_bps,
        state.creator_cap,
        state.fanex_cap,
        state.creator_received,
        state.fanex_received,
    )?;
    let mut tokens_out = buy_tokens_out(state.virtual_reserve, state.virtual_token_reserves, split.curve)?;
    let mut curve_amount = split.curve;
    if tokens_out > state.real_token_reserves {
        // The partial fill at graduation: only the tokens left, at their exact cost, and the fee on
        // what is actually swapped. The shares are not recomputed, as today.
        tokens_out = state.real_token_reserves;
        curve_amount = reserve_for_tokens(state.virtual_reserve, state.virtual_token_reserves, tokens_out)?
            .min(split.curve);
        let net_used = add(add(split.creator, split.fanex)?, curve_amount)?;
        fee = platform_fee_on_net(net_used)?.min(fee);
    }
    let charged = add(add(add(split.creator, split.fanex)?, curve_amount)?, fee)?;
    let after = CurveState {
        virtual_reserve: add(state.virtual_reserve, curve_amount)?,
        virtual_token_reserves: sub(state.virtual_token_reserves, tokens_out)?,
        real_reserve: add(state.real_reserve, curve_amount)?,
        real_token_reserves: sub(state.real_token_reserves, tokens_out)?,
        creator_received: add(state.creator_received, split.creator)?,
        fanex_received: add(state.fanex_received, split.fanex)?,
        ..*state
    };
    Ok(BuyQuote {
        charged,
        tokens_out,
        platform_fee: fee,
        creator_amount: split.creator,
        fanex_amount: split.fanex,
        curve_amount,
        launch_window: in_launch_window(state.created_at, now),
        graduates: after.real_reserve >= state.graduation_threshold || after.real_token_reserves == 0,
        after,
    })
}

/// What a sale pays and leaves behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SellQuote {
    /// What leaves the curve's reserve: the proceeds and the fee.
    pub reserve_out: u64,
    pub platform_fee: u64,
    /// What reaches the seller; the handler checks it against the seller's minimum.
    pub proceeds: u64,
    /// The curve once the sale lands.
    pub after: CurveState,
}

/// A sale of `tokens_in` on a curve in Bonding (design §10.3.6, steps 1 to 3 and the counters). No
/// creator or Fanex share and no launch cap on a sale, as today.
pub fn quote_sell(state: &CurveState, tokens_in: u64) -> MathResult<SellQuote> {
    let reserve_out = sell_reserve_out(state.virtual_reserve, state.virtual_token_reserves, tokens_in)?;
    if reserve_out == 0 {
        return Err(MathError::ZeroAmount);
    }
    if reserve_out > state.real_reserve {
        return Err(MathError::InsufficientReserves);
    }
    let fee = platform_fee(reserve_out)?;
    let after = CurveState {
        virtual_reserve: sub(state.virtual_reserve, reserve_out)?,
        virtual_token_reserves: add(state.virtual_token_reserves, tokens_in)?,
        real_reserve: sub(state.real_reserve, reserve_out)?,
        real_token_reserves: add(state.real_token_reserves, tokens_in)?,
        ..*state
    };
    Ok(SellQuote { reserve_out, platform_fee: fee, proceeds: sub(reserve_out, fee)?, after })
}

#[cfg(test)]
mod tests {
    use super::*;

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
            created_at: 1_800_000_000,
        }
    }

    #[test]
    fn the_fee_is_one_percent_floored_and_zero_under_100() {
        assert_eq!(platform_fee(1_000_000_000).unwrap(), 10_000_000);
        assert_eq!(platform_fee(99).unwrap(), 0);
        assert_eq!(platform_fee(100).unwrap(), 1);
        assert_eq!(platform_fee(u64::MAX).unwrap(), u64::MAX / 100);
    }

    #[test]
    fn the_fee_on_the_net_is_one_percent_of_the_gross() {
        assert_eq!(platform_fee_on_net(990).unwrap(), 10);
        assert_eq!(platform_fee_on_net(990_000_000).unwrap(), 10_000_000);
        assert_eq!(platform_fee_on_net(0).unwrap(), 0);
    }

    #[test]
    fn the_split_follows_today_s_cases() {
        let s = split_with_cap(1_000_000_000, 940, 60, u64::MAX, u64::MAX, 0, 0).unwrap();
        assert_eq!((s.creator, s.fanex, s.curve), (94_000_000, 6_000_000, 900_000_000));
        let s = split_with_cap(1_000_000_000, 940, 60, 100, 60_000_000, 100, 0).unwrap();
        assert_eq!((s.creator, s.fanex, s.curve), (0, 6_000_000, 994_000_000));
        let s = split_with_cap(1_000_000_000, 0, 0, 0, 0, 0, 0).unwrap();
        assert_eq!((s.creator, s.fanex, s.curve), (0, 0, 1_000_000_000));
    }

    #[test]
    fn the_inverse_never_undercharges() {
        let (x, y) = (30_000_000_000u64, 1_000_000_000_000u64);
        let dy = buy_tokens_out(x, y, 1_000_000_000).unwrap();
        let needed = reserve_for_tokens(x, y, dy).unwrap();
        assert!(needed <= 1_000_000_000);
        assert!(buy_tokens_out(x, y, needed).unwrap() >= dy);
        assert_eq!(reserve_for_tokens(x, y, y), Err(MathError::MathOverflow));
        assert_eq!(reserve_for_tokens(x, y, 0), Err(MathError::ZeroAmount));
    }

    #[test]
    fn the_inverse_refuses_a_result_beyond_u64_where_today_s_cast_wrapped() {
        assert_eq!(reserve_for_tokens(u64::MAX, 2, 1), Ok(u64::MAX));
        assert_eq!(reserve_for_tokens(u64::MAX, 3, 2), Err(MathError::MathOverflow));
    }

    #[test]
    fn a_round_trip_never_pays_more_than_it_cost() {
        let (x, y) = (30_000_000_000u64, 1_000_000_000_000u64);
        let out = buy_tokens_out(x, y, 1_000_000_000).unwrap();
        assert!(sell_reserve_out(x + 1_000_000_000, y - out, out).unwrap() <= 1_000_000_000);
    }

    #[test]
    fn vesting_matches_today_s_schedules() {
        assert_eq!(vested_amount(1200, 0, 0, 1200, 600), 600);
        assert_eq!(vested_amount(1200, 0, 1200, 0, 1199), 0);
        assert_eq!(vested_amount(1200, 0, 1200, 0, 1200), 1200);
        let (cliff, duration) = (15_552_000i64, 46_656_000i64);
        assert_eq!(vested_amount(1_000_000, 0, cliff, duration, cliff), 0);
        assert_eq!(vested_amount(1_000_000, 0, cliff, duration, cliff + duration / 2), 500_000);
    }

    #[test]
    fn the_launch_cap_holds_for_five_minutes() {
        let at = 1_800_000_000i64;
        assert!(launch_cap_allows(at, at, 0, LAUNCH_WALLET_CAP));
        assert!(!launch_cap_allows(at, at, 0, LAUNCH_WALLET_CAP + 1));
        assert!(!launch_cap_allows(at, at + 299, LAUNCH_WALLET_CAP, 1));
        assert!(launch_cap_allows(at, at + 300, 0, LAUNCH_WALLET_CAP * 10));
        assert!(!launch_cap_allows(at, at - 1, 0, LAUNCH_WALLET_CAP + 1));
        assert!(!launch_cap_allows(at, at, u64::MAX, 1));
    }

    #[test]
    fn vesting_waits_for_graduation_from_november() {
        assert!(vesting_claim_allowed(VESTING_LOCKED_UNTIL_GRADUATION_FROM - 1, false));
        assert!(!vesting_claim_allowed(VESTING_LOCKED_UNTIL_GRADUATION_FROM, false));
        assert!(vesting_claim_allowed(VESTING_LOCKED_UNTIL_GRADUATION_FROM, true));
    }

    #[test]
    fn a_buy_charges_its_parts_and_moves_the_counters() {
        let q = quote_buy(&equity(), 100_000_000, 1_800_000_300).unwrap();
        assert_eq!(q.platform_fee, 1_000_000);
        assert_eq!(q.curve_amount, 99_000_000);
        assert_eq!(q.charged, 100_000_000);
        assert_eq!(q.after.real_reserve, 99_000_000);
        assert_eq!(q.after.real_token_reserves, 76_100_000_000_000 - q.tokens_out);
        assert!(!q.launch_window);
        assert!(!q.graduates);
    }

    #[test]
    fn a_buy_past_the_last_token_is_a_partial_fill_that_graduates() {
        let q = quote_buy(&equity(), u64::MAX / 2, 1_800_000_300).unwrap();
        assert_eq!(q.tokens_out, 76_100_000_000_000);
        assert!(q.charged < u64::MAX / 2);
        assert_eq!(q.charged, q.curve_amount + q.platform_fee);
        assert_eq!(q.after.real_token_reserves, 0);
        assert!(q.graduates);
    }

    #[test]
    fn a_sale_of_tokens_the_curve_never_sold_is_refused() {
        assert_eq!(quote_sell(&equity(), 1_000_000_000_000), Err(MathError::InsufficientReserves));
    }

    #[test]
    fn a_buy_then_its_sale_returns_the_curve_to_where_it_was_but_for_rounding() {
        let start = equity();
        let buy = quote_buy(&start, 500_000_000, 1_800_000_300).unwrap();
        let sale = quote_sell(&buy.after, buy.tokens_out).unwrap();
        assert!(sale.reserve_out <= buy.curve_amount);
        assert_eq!(sale.after.real_token_reserves, start.real_token_reserves);
        assert!(sale.after.real_reserve <= 2);
    }

    #[test]
    fn a_zero_amount_and_an_empty_curve_are_refused_first() {
        assert_eq!(quote_buy(&equity(), 0, 0), Err(MathError::ZeroAmount));
        let empty = CurveState { real_token_reserves: 0, ..equity() };
        assert_eq!(quote_buy(&empty, 1, 0), Err(MathError::CurveComplete));
        assert_eq!(quote_sell(&equity(), 0), Err(MathError::ZeroAmount));
    }
}
