//! Pure, overflow-safe math. No account access — unit-testable in isolation.
//! These mirror the off-chain `lib/solana/{buy-split,presale-math}.ts` so the
//! UI quote and the on-chain execution agree to the lamport.

use crate::constants::{BPS_DENOMINATOR, PLATFORM_SWAP_FEE_BPS};
use crate::errors::FanexError;
use anchor_lang::prelude::*;

/// Platform swap fee on `amount` lamports (`PLATFORM_SWAP_FEE_BPS`), floored.
/// Skimmed to `config.fanex_fee_wallet` on every buy/sell — independent of the
/// fundraise `split_with_cap`. Mirrors off-chain `platformSwapFee` in
/// `lib/solana/pepper-curve/curve-math.ts`.
pub fn platform_swap_fee(amount: u64) -> Result<u64> {
    let fee = (amount as u128)
        .checked_mul(PLATFORM_SWAP_FEE_BPS as u128)
        .ok_or(FanexError::MathOverflow)?
        / BPS_DENOMINATOR as u128;
    Ok(fee as u64)
}

/// Result of splitting one buy across creator / Fanex / curve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Split {
    pub creator: u64,
    pub fanex: u64,
    pub curve: u64,
}

/// Cap-aware split of `amount` lamports. Faithful port of `splitWithCap`:
/// the standard bps share goes to creator/Fanex, clamped by remaining cap
/// capacity; whatever the cap absorbs flows to the curve instead. Once a
/// destination hits its cap it receives 0 and 100% of the remainder routes to
/// the curve. This is the *enforced* version of the previously-strippable
/// prepended transfers (B1).
pub fn split_with_cap(
    amount: u64,
    creator_split_bps: u64,
    fanex_split_bps: u64,
    creator_cap: u64,
    fanex_cap: u64,
    creator_received: u64,
    fanex_received: u64,
) -> Result<Split> {
    let amount_u = amount as u128;

    let standard_creator = amount_u
        .checked_mul(creator_split_bps as u128)
        .ok_or(FanexError::MathOverflow)?
        / BPS_DENOMINATOR as u128;
    let standard_fanex = amount_u
        .checked_mul(fanex_split_bps as u128)
        .ok_or(FanexError::MathOverflow)?
        / BPS_DENOMINATOR as u128;

    let remaining_creator = (creator_cap.saturating_sub(creator_received)) as u128;
    let remaining_fanex = (fanex_cap.saturating_sub(fanex_received)) as u128;

    let creator = standard_creator.min(remaining_creator) as u64;
    let fanex = standard_fanex.min(remaining_fanex) as u64;

    let curve = amount
        .checked_sub(creator)
        .ok_or(FanexError::MathOverflow)?
        .checked_sub(fanex)
        .ok_or(FanexError::MathOverflow)?;

    Ok(Split { creator, fanex, curve })
}

/// Constant-product buy: how many tokens come out for `sol_in` lamports routed
/// into the curve. Uniswap-style delta form `dy = y·dx/(x+dx)`, floored — rounds
/// in favor of the pool and is symmetric with `sell_sol_out`, so a buy→sell
/// roundtrip can never extract value (no 1-lamport drain).
pub fn buy_tokens_out(
    virtual_sol_reserves: u64,
    virtual_token_reserves: u64,
    sol_in: u64,
) -> Result<u64> {
    require!(sol_in > 0, FanexError::ZeroAmount);
    let num = (virtual_token_reserves as u128)
        .checked_mul(sol_in as u128)
        .ok_or(FanexError::MathOverflow)?;
    let den = (virtual_sol_reserves as u128)
        .checked_add(sol_in as u128)
        .ok_or(FanexError::MathOverflow)?;
    Ok((num / den) as u64)
}

/// Inverse of [`buy_tokens_out`]: the curve SOL required to receive EXACTLY
/// `tokens_out` tokens. `dx = x·dy/(y − dy)`, rounded UP (pool-favorable, so a
/// graduation partial-fill never hands out the last tokens too cheaply).
/// Used by the `buy` handler when the requested amount over-fills the curve: it
/// clamps tokens to what remains and charges only this — refunding the rest.
/// `tokens_out` MUST be `< virtual_token_reserves`.
pub fn curve_sol_for_tokens(
    virtual_sol_reserves: u64,
    virtual_token_reserves: u64,
    tokens_out: u64,
) -> Result<u64> {
    require!(tokens_out > 0, FanexError::ZeroAmount);
    require!(virtual_token_reserves > tokens_out, FanexError::MathOverflow);
    let num = (virtual_sol_reserves as u128)
        .checked_mul(tokens_out as u128)
        .ok_or(FanexError::MathOverflow)?;
    let den = (virtual_token_reserves as u128)
        .checked_sub(tokens_out as u128)
        .ok_or(FanexError::MathOverflow)?;
    // Ceil division so the buyer pays at least the fair price for the last tokens.
    let dx = num
        .checked_add(den - 1)
        .ok_or(FanexError::MathOverflow)?
        / den;
    // Checked, never `as` (the internal audit, 2026-10-29): `buy` reaches this only where the
    // result fits, but a wrap would charge a fraction of the last tokens' price.
    u64::try_from(dx).map_err(|_| error!(FanexError::MathOverflow))
}

/// The 1% platform swap fee expressed as a fraction of the NET (post-fee)
/// amount, used when a graduation partial-fill shrinks the transacted SOL:
/// since `gross = net + fee` and `fee = bps·gross`, we have
/// `fee = net·bps/(BPS − bps)`, rounded UP. Keeps the skim consistent at 1% of
/// the amount the buyer actually swaps rather than 1% of their (larger) input.
pub fn platform_swap_fee_on_net(net: u64) -> Result<u64> {
    let num = (net as u128)
        .checked_mul(PLATFORM_SWAP_FEE_BPS as u128)
        .ok_or(FanexError::MathOverflow)?;
    let den = (BPS_DENOMINATOR as u128)
        .checked_sub(PLATFORM_SWAP_FEE_BPS as u128)
        .ok_or(FanexError::MathOverflow)?;
    let fee = num
        .checked_add(den - 1)
        .ok_or(FanexError::MathOverflow)?
        / den;
    Ok(fee as u64)
}

/// Constant-product sell: how much SOL comes out for `tokens_in` returned to the
/// curve. `dx = x·dy/(y+dy)`, floored (pool-favorable).
pub fn sell_sol_out(
    virtual_sol_reserves: u64,
    virtual_token_reserves: u64,
    tokens_in: u64,
) -> Result<u64> {
    require!(tokens_in > 0, FanexError::ZeroAmount);
    let num = (virtual_sol_reserves as u128)
        .checked_mul(tokens_in as u128)
        .ok_or(FanexError::MathOverflow)?;
    let den = (virtual_token_reserves as u128)
        .checked_add(tokens_in as u128)
        .ok_or(FanexError::MathOverflow)?;
    Ok((num / den) as u64)
}

/// Vested amount releasable at `now` for a schedule. Cliff gates the first
/// release; after `duration` the full `total` is vested. Two modes:
///   - linear: `duration > 0` → ramps `total * elapsed / duration` after the cliff.
///   - lump-sum (cliff): `duration == 0` → 0 before the cliff, full `total` at/after
///     it (the current Fanex config: ~360d cliff, duration 0).
pub fn vested_amount(
    total: u64,
    start: i64,
    cliff_seconds: i64,
    duration_seconds: i64,
    now: i64,
) -> u64 {
    if now < start.saturating_add(cliff_seconds) {
        return 0;
    }
    let elapsed = now.saturating_sub(start.saturating_add(cliff_seconds));
    if elapsed >= duration_seconds || duration_seconds == 0 {
        return total;
    }
    // total * elapsed / duration, in u128 to avoid overflow.
    let v = (total as u128)
        .saturating_mul(elapsed as u128)
        / (duration_seconds as u128);
    v as u64
}

/// Verify a sorted-pair keccak256 merkle proof of `leaf` against `root`.
pub fn verify_merkle_proof(proof: &[[u8; 32]], root: [u8; 32], leaf: [u8; 32]) -> bool {
    use anchor_lang::solana_program::keccak::hashv;
    let mut computed = leaf;
    for sibling in proof.iter() {
        computed = if computed <= *sibling {
            hashv(&[&computed, sibling]).0
        } else {
            hashv(&[sibling, &computed]).0
        };
    }
    computed == root
}

/// Leaf = keccak256(claimant_pubkey || amount_le). Bound to the wallet + amount,
/// so a holder can only claim exactly their allocated SOL once.
pub fn dividend_leaf(claimant: &Pubkey, amount: u64) -> [u8; 32] {
    use anchor_lang::solana_program::keccak::hashv;
    hashv(&[claimant.as_ref(), &amount.to_le_bytes()]).0
}

/// v2 leaf = keccak256(DIVIDEND_LEAF_V2_DOMAIN || index_le || claimant || amount_le),
/// for a `DistributorV2`. The index is the leaf's position in the tree and the
/// bit it sets in the claim registry, so binding it here is what lets a holder
/// set only their own bit.
pub fn dividend_leaf_v2(index: u32, claimant: &Pubkey, amount: u64) -> [u8; 32] {
    use anchor_lang::solana_program::keccak::hashv;
    hashv(&[
        crate::constants::DIVIDEND_LEAF_V2_DOMAIN,
        &index.to_le_bytes(),
        claimant.as_ref(),
        &amount.to_le_bytes(),
    ])
    .0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_fee_is_one_percent() {
        assert_eq!(platform_swap_fee(1_000_000_000).unwrap(), 10_000_000);
        assert_eq!(platform_swap_fee(0).unwrap(), 0);
        // sub-100-lamport trades round the fee down to 0 (floored).
        assert_eq!(platform_swap_fee(99).unwrap(), 0);
    }

    #[test]
    fn split_basic_no_cap() {
        // 1 SOL, 9.4% creator / 0.6% fanex (illustrative), caps high.
        let s = split_with_cap(1_000_000_000, 940, 60, u64::MAX, u64::MAX, 0, 0).unwrap();
        assert_eq!(s.creator, 94_000_000);
        assert_eq!(s.fanex, 6_000_000);
        assert_eq!(s.curve, 900_000_000);
        assert_eq!(s.creator + s.fanex + s.curve, 1_000_000_000);
    }

    #[test]
    fn split_creator_cap_routes_overflow_to_curve() {
        // creator already at cap → creator gets 0, all non-fanex to curve.
        let s = split_with_cap(1_000_000_000, 940, 60, 100, 60_000_000, 100, 0).unwrap();
        assert_eq!(s.creator, 0);
        assert_eq!(s.fanex, 6_000_000);
        assert_eq!(s.curve, 994_000_000);
    }

    #[test]
    fn split_zero_bps_routes_everything_to_curve() {
        // The `equity` asset type's central economic claim: no raise at all, so
        // both splits AND both caps are zero. Every lamport of every buy must
        // land in the curve vault — nothing to creator, nothing to Fanex.
        let s = split_with_cap(1_000_000_000, 0, 0, 0, 0, 0, 0).unwrap();
        assert_eq!(s.creator, 0);
        assert_eq!(s.fanex, 0);
        assert_eq!(s.curve, 1_000_000_000);
    }

    #[test]
    fn curve_sol_for_tokens_inverts_buy_tokens_out() {
        let (vs, vt) = (30_000_000_000u64, 1_000_000_000_000u64);
        // Buying with `dx` yields `dy` tokens; the inverse must require >= dx to
        // get those same dy back (ceil rounding → pool-favorable, never cheaper).
        let dx = 1_000_000_000u64;
        let dy = buy_tokens_out(vs, vt, dx).unwrap();
        let needed = curve_sol_for_tokens(vs, vt, dy).unwrap();
        assert!(needed <= dx, "inverse must not exceed the original input");
        // And buying with the inverse-derived SOL yields at least dy tokens.
        assert!(buy_tokens_out(vs, vt, needed).unwrap() >= dy);
    }

    #[test]
    fn curve_sol_for_tokens_rejects_overdraw() {
        let (vs, vt) = (30_000_000_000u64, 1_000_000_000_000u64);
        // Cannot ask for >= the entire virtual token reserve (price → ∞).
        assert!(curve_sol_for_tokens(vs, vt, vt).is_err());
        assert!(curve_sol_for_tokens(vs, vt, 0).is_err());
    }

    #[test]
    fn curve_sol_for_tokens_refuses_a_result_past_u64_instead_of_wrapping() {
        // The internal audit (2026-10-29): `(2^64 - 1) × 2 / 1` was cut to its low 64 bits.
        assert!(curve_sol_for_tokens(u64::MAX, 3, 2).is_err());
        assert_eq!(curve_sol_for_tokens(u64::MAX, 2, 1).unwrap(), u64::MAX);
    }

    #[test]
    fn platform_fee_on_net_matches_one_percent_of_gross() {
        // net = 990 → gross = 1000, fee = 10 (1% of gross). ceil keeps it exact.
        assert_eq!(platform_swap_fee_on_net(990).unwrap(), 10);
        // A gross-side fee of 1% on 1 SOL is 0.01 SOL; the net is 0.99 SOL.
        assert_eq!(platform_swap_fee_on_net(990_000_000).unwrap(), 10_000_000);
        assert_eq!(platform_swap_fee_on_net(0).unwrap(), 0);
    }

    #[test]
    fn cp_buy_sell_roundtrip_loses_to_curve() {
        let (vs, vt) = (30_000_000_000u64, 1_000_000_000_000u64);
        let out = buy_tokens_out(vs, vt, 1_000_000_000).unwrap();
        // sell the same tokens back immediately → never get more SOL than paid.
        let back = sell_sol_out(vs + 1_000_000_000, vt - out, out).unwrap();
        assert!(back <= 1_000_000_000);
    }

    #[test]
    fn vesting_linear() {
        assert_eq!(vested_amount(1200, 0, 0, 1200, 0), 0);
        assert_eq!(vested_amount(1200, 0, 0, 1200, 600), 600);
        assert_eq!(vested_amount(1200, 0, 0, 1200, 1200), 1200);
        assert_eq!(vested_amount(1200, 0, 0, 1200, 5000), 1200);
    }

    #[test]
    fn vesting_lump_sum_cliff() {
        // Current Fanex config: cliff = 1200, duration = 0 → all-or-nothing.
        assert_eq!(vested_amount(1200, 0, 1200, 0, 0), 0); // before cliff
        assert_eq!(vested_amount(1200, 0, 1200, 0, 1199), 0); // 1s before cliff
        assert_eq!(vested_amount(1200, 0, 1200, 0, 1200), 1200); // at cliff → full
        assert_eq!(vested_amount(1200, 0, 1200, 0, 9999), 1200); // after → full
    }

    #[test]
    fn vesting_ramps_from_end_of_cliff_not_from_start() {
        // Equity schedule: 6-month cliff, 18-month linear ramp. The ramp must be
        // measured from the END of the cliff, not from `start` — otherwise the
        // cliff's own elapsed time counts as already-vested and ~a third unlocks
        // in one shot the instant the cliff passes (regression for CRITICAL 1).
        let total = 1_000_000u64;
        let cliff = 6 * 30 * 24 * 60 * 60i64; // 15_552_000 (6mo)
        let duration = 18 * 30 * 24 * 60 * 60i64; // 46_656_000 (18mo)
        let nine_months = 9 * 30 * 24 * 60 * 60i64;
        let start = 0i64;

        assert_eq!(vested_amount(total, start, cliff, duration, cliff - 1), 0); // 1s before cliff
        assert_eq!(vested_amount(total, start, cliff, duration, cliff), 0); // exactly at cliff: ramp starts at 0
        // cliff + 9mo == cliff + duration/2 exactly → half vested.
        assert_eq!(
            vested_amount(total, start, cliff, duration, cliff + nine_months),
            total / 2
        );
        // cliff + 18mo == cliff + duration → fully vested.
        assert_eq!(
            vested_amount(total, start, cliff, duration, cliff + duration),
            total
        );
    }

    #[test]
    fn merkle_single_leaf() {
        let claimant = Pubkey::new_unique();
        let leaf = dividend_leaf(&claimant, 500);
        assert!(verify_merkle_proof(&[], leaf, leaf));
    }

    fn hex(bytes: [u8; 32]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    // The same two vectors are pinned in lib/solana/pepper-curve/merkle.test.ts:
    // a client that hashes a leaf differently builds trees no claim can prove.
    #[test]
    fn leaves_match_the_typescript_golden_vectors() {
        let claimant = Pubkey::new_from_array([7u8; 32]);
        assert_eq!(
            hex(dividend_leaf(&claimant, 1_000_000)),
            "fcd9ee43eb9ce64db00659565ed1186ef356c1f2431805a5106043f1d994389d"
        );
        assert_eq!(
            hex(dividend_leaf_v2(5, &claimant, 1_000_000)),
            "1268ad549425a568c5d9c291e109de5725f594e9e565d52f8e6400c610456105"
        );
    }

    #[test]
    fn a_v2_leaf_binds_its_index() {
        let claimant = Pubkey::new_unique();
        assert_ne!(dividend_leaf_v2(0, &claimant, 500), dividend_leaf_v2(1, &claimant, 500));
        // Nor does it read as the v1 leaf of the same holder and amount.
        assert_ne!(dividend_leaf_v2(0, &claimant, 500), dividend_leaf(&claimant, 500));
    }

    #[test]
    fn a_v2_proof_verifies_only_for_its_own_index() {
        let (a, b) = (Pubkey::new_unique(), Pubkey::new_unique());
        let (la, lb) = (dividend_leaf_v2(0, &a, 10), dividend_leaf_v2(1, &b, 20));
        use anchor_lang::solana_program::keccak::hashv;
        let root = if la <= lb { hashv(&[&la, &lb]).0 } else { hashv(&[&lb, &la]).0 };
        assert!(verify_merkle_proof(&[lb], root, la));
        // The same holder and amount under another index is another leaf.
        assert!(!verify_merkle_proof(&[lb], root, dividend_leaf_v2(1, &a, 10)));
    }
}
