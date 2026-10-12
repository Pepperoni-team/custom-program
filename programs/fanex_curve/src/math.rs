use crate::constants::{BPS_DENOMINATOR, PLATFORM_SWAP_FEE_BPS};
use crate::errors::FanexError;
use anchor_lang::prelude::*;

pub fn platform_swap_fee(amount: u64) -> Result<u64> {
    let fee = (amount as u128)
        .checked_mul(PLATFORM_SWAP_FEE_BPS as u128)
        .ok_or(FanexError::MathOverflow)?
        / BPS_DENOMINATOR as u128;
    Ok(fee as u64)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Split {
    pub creator: u64,
    pub fanex: u64,
    pub curve: u64,
}

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
    let dx = num
        .checked_add(den - 1)
        .ok_or(FanexError::MathOverflow)?
        / den;
    Ok(dx as u64)
}

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
    let v = (total as u128)
        .saturating_mul(elapsed as u128)
        / (duration_seconds as u128);
    v as u64
}

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

pub fn dividend_leaf(claimant: &Pubkey, amount: u64) -> [u8; 32] {
    use anchor_lang::solana_program::keccak::hashv;
    hashv(&[claimant.as_ref(), &amount.to_le_bytes()]).0
}

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
        assert_eq!(platform_swap_fee(99).unwrap(), 0);
    }

    #[test]
    fn split_basic_no_cap() {
        let s = split_with_cap(1_000_000_000, 940, 60, u64::MAX, u64::MAX, 0, 0).unwrap();
        assert_eq!(s.creator, 94_000_000);
        assert_eq!(s.fanex, 6_000_000);
        assert_eq!(s.curve, 900_000_000);
        assert_eq!(s.creator + s.fanex + s.curve, 1_000_000_000);
    }

    #[test]
    fn split_creator_cap_routes_overflow_to_curve() {
        let s = split_with_cap(1_000_000_000, 940, 60, 100, 60_000_000, 100, 0).unwrap();
        assert_eq!(s.creator, 0);
        assert_eq!(s.fanex, 6_000_000);
        assert_eq!(s.curve, 994_000_000);
    }

    #[test]
    fn split_zero_bps_routes_everything_to_curve() {
        let s = split_with_cap(1_000_000_000, 0, 0, 0, 0, 0, 0).unwrap();
        assert_eq!(s.creator, 0);
        assert_eq!(s.fanex, 0);
        assert_eq!(s.curve, 1_000_000_000);
    }

    #[test]
    fn curve_sol_for_tokens_inverts_buy_tokens_out() {
        let (vs, vt) = (30_000_000_000u64, 1_000_000_000_000u64);
        let dx = 1_000_000_000u64;
        let dy = buy_tokens_out(vs, vt, dx).unwrap();
        let needed = curve_sol_for_tokens(vs, vt, dy).unwrap();
        assert!(needed <= dx, "inverse must not exceed the original input");
        assert!(buy_tokens_out(vs, vt, needed).unwrap() >= dy);
    }

    #[test]
    fn curve_sol_for_tokens_rejects_overdraw() {
        let (vs, vt) = (30_000_000_000u64, 1_000_000_000_000u64);
        assert!(curve_sol_for_tokens(vs, vt, vt).is_err());
        assert!(curve_sol_for_tokens(vs, vt, 0).is_err());
    }

    #[test]
    fn platform_fee_on_net_matches_one_percent_of_gross() {
        assert_eq!(platform_swap_fee_on_net(990).unwrap(), 10);
        assert_eq!(platform_swap_fee_on_net(990_000_000).unwrap(), 10_000_000);
        assert_eq!(platform_swap_fee_on_net(0).unwrap(), 0);
    }

    #[test]
    fn cp_buy_sell_roundtrip_loses_to_curve() {
        let (vs, vt) = (30_000_000_000u64, 1_000_000_000_000u64);
        let out = buy_tokens_out(vs, vt, 1_000_000_000).unwrap();
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
        assert_eq!(vested_amount(1200, 0, 1200, 0, 0), 0);
        assert_eq!(vested_amount(1200, 0, 1200, 0, 1199), 0);
        assert_eq!(vested_amount(1200, 0, 1200, 0, 1200), 1200);
        assert_eq!(vested_amount(1200, 0, 1200, 0, 9999), 1200);
    }

    #[test]
    fn vesting_ramps_from_end_of_cliff_not_from_start() {
        let total = 1_000_000u64;
        let cliff = 6 * 30 * 24 * 60 * 60i64;
        let duration = 18 * 30 * 24 * 60 * 60i64;
        let nine_months = 9 * 30 * 24 * 60 * 60i64;
        let start = 0i64;

        assert_eq!(vested_amount(total, start, cliff, duration, cliff - 1), 0);
        assert_eq!(vested_amount(total, start, cliff, duration, cliff), 0);
        assert_eq!(
            vested_amount(total, start, cliff, duration, cliff + nine_months),
            total / 2
        );
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
        assert_ne!(dividend_leaf_v2(0, &claimant, 500), dividend_leaf(&claimant, 500));
    }

    #[test]
    fn a_v2_proof_verifies_only_for_its_own_index() {
        let (a, b) = (Pubkey::new_unique(), Pubkey::new_unique());
        let (la, lb) = (dividend_leaf_v2(0, &a, 10), dividend_leaf_v2(1, &b, 20));
        use anchor_lang::solana_program::keccak::hashv;
        let root = if la <= lb { hashv(&[&la, &lb]).0 } else { hashv(&[&lb, &la]).0 };
        assert!(verify_merkle_proof(&[lb], root, la));
        assert!(!verify_merkle_proof(&[lb], root, dividend_leaf_v2(1, &a, 10)));
    }
}
