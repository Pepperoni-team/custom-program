//! Tier pricing (spec §4.4): what a budget buys, current tier first, and what
//! units cost. A purchase that crosses a tier boundary is split across prices.

use anchor_lang::prelude::*;

use crate::raise::checked::{add, mul_div, mul_div_ceil, sub};
use crate::raise::constants::{MAX_TIERS, QUOTA_UNITS};
use crate::raise::state::Tier;

/// What `units` cost a buyer at `price` per whole quota, rounded up.
pub fn cost_of(units: u64, price: u64) -> Result<u64> {
    mul_div_ceil(units, price, QUOTA_UNITS)
}

/// What `units` pay a seller at `price` per whole quota, rounded down: the
/// program never pays out a rounding unit it does not hold.
pub fn proceeds_of(units: u64, price: u64) -> Result<u64> {
    mul_div(units, price, QUOTA_UNITS)
}

/// Quota units `amount` buys at `price`, rounded down.
pub fn units_for(amount: u64, price: u64) -> Result<u64> {
    mul_div(amount, QUOTA_UNITS, price)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fill {
    pub units: u64,
    pub cost: u64,
    pub per_tier: [u64; MAX_TIERS],
}

/// Fill a purchase of at most `budget`, from the first tier with units left.
pub fn fill_purchase(tiers: &[Tier; MAX_TIERS], tier_count: u8, budget: u64) -> Result<Fill> {
    let mut fill = Fill::default();
    let mut left = budget;
    for (i, tier) in tiers.iter().enumerate().take(tier_count as usize) {
        let available = sub(tier.units, tier.sold)?;
        if available == 0 {
            continue;
        }
        let units = units_for(left, tier.price)?.min(available);
        if units == 0 {
            break;
        }
        let cost = cost_of(units, tier.price)?;
        left = sub(left, cost)?;
        fill.units = add(fill.units, units)?;
        fill.cost = add(fill.cost, cost)?;
        fill.per_tier[i] = units;
        if units < available {
            break;
        }
    }
    Ok(fill)
}

#[cfg(test)]
mod tests {
    use super::*;

    const Q: u64 = QUOTA_UNITS;

    fn spec_tiers() -> [Tier; MAX_TIERS] {
        [
            Tier { units: 4 * Q, price: 1_000_000, sold: 0 },
            Tier { units: 3 * Q, price: 1_100_000, sold: 0 },
            Tier { units: 3 * Q, price: 1_200_000, sold: 0 },
        ]
    }

    #[test]
    fn the_first_tier_costs_one_dollar_a_quota() {
        assert_eq!(cost_of(Q, 1_000_000).unwrap(), 1_000_000);
        assert_eq!(cost_of(1, 1_100_000).unwrap(), 2);
        assert_eq!(proceeds_of(1, 1_100_000).unwrap(), 1);
        assert_eq!(units_for(1_000_000, 1_100_000).unwrap(), 909_090);
    }

    #[test]
    fn a_budget_never_buys_more_than_it_pays_for() {
        for price in [1_000_000u64, 1_050_000, 1_100_000, 1_150_000, 1_250_000] {
            for budget in [1u64, 2, 999, 1_000_001, 2_000_000, 7_777_777] {
                let units = units_for(budget, price).unwrap();
                assert!(cost_of(units, price).unwrap() <= budget);
            }
        }
    }

    #[test]
    fn fills_inside_the_first_tier() {
        let fill = fill_purchase(&spec_tiers(), 3, 2_500_000).unwrap();
        assert_eq!(fill, Fill { units: 2_500_000, cost: 2_500_000, per_tier: [2_500_000, 0, 0] });
    }

    #[test]
    fn splits_a_purchase_across_two_tiers() {
        let fill = fill_purchase(&spec_tiers(), 3, 5_000_000).unwrap();
        assert_eq!(fill.per_tier, [4 * Q, 909_090, 0]);
        assert_eq!(fill.units, 4 * Q + 909_090);
        assert_eq!(fill.cost, 4_000_000 + 999_999);
    }

    #[test]
    fn resumes_from_the_tier_in_progress() {
        let mut tiers = spec_tiers();
        tiers[0].sold = 4 * Q;
        tiers[1].sold = Q;
        let fill = fill_purchase(&tiers, 3, 1_100_000).unwrap();
        assert_eq!(fill.per_tier, [0, Q, 0]);
        assert_eq!(fill.cost, 1_100_000);
    }

    #[test]
    fn a_sold_out_raise_sells_nothing() {
        let mut tiers = spec_tiers();
        for tier in tiers.iter_mut() {
            tier.sold = tier.units;
        }
        assert_eq!(fill_purchase(&tiers, 3, 10_000_000).unwrap().units, 0);
    }

    #[test]
    fn stops_at_what_is_left() {
        let tiers = [
            Tier { units: Q, price: 1_000_000, sold: 0 },
            Tier::default(),
            Tier::default(),
        ];
        let fill = fill_purchase(&tiers, 1, 5_000_000).unwrap();
        assert_eq!((fill.units, fill.cost), (Q, 1_000_000));
    }
}
