//! The exit pool (amended spec §3): a constant-product pool that holds only
//! real USDC, priced against a virtual quota reserve fixed at `finalize`. Every
//! rounding and the fee favour the pool, so its product never falls through a
//! trade, a round trip never pays, and a sale never empties it. Both trades the
//! program runs are held to the fans' share of the pool, the purchase from above
//! (`buy_pro_rata`) and the sale from below (`sell_pro_rata`, N+3): there a sale may pay more
//! than the constant product, so the product may fall, never a remaining fan's share.

use anchor_lang::prelude::*;

use crate::raise::checked::{add, mul_div, mul_div_ceil, sub};
use crate::raise::constants::{BPS, QUOTA_UNITS};
use crate::errors::FanexError;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pool {
    /// `Raise.pool_usdc`: the pool's USDC counter, not its vault balance.
    pub usdc: u64,
    /// `Raise.pool_units`: quota units the pool holds.
    pub units: u64,
    /// `Raise.virtual_units`: fixed at finalize.
    pub virtual_units: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sale {
    pub gross: u64,
    pub fee: u64,
    /// What the seller receives: `gross - fee`.
    pub net: u64,
    pub after: Pool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Purchase {
    pub gross: u64,
    pub fee: u64,
    /// What the buyer pays: `gross + fee`.
    pub cost: u64,
    pub after: Pool,
}

/// The virtual quotas that start the pool at the average price fans paid,
/// rounded up so the start is never above it.
pub fn virtual_units_for(pool_usdc: u64, sold_units: u64, fan_paid: u64) -> Result<u64> {
    mul_div_ceil(pool_usdc, sold_units, fan_paid)
}

/// The pool's fee on a trade's gross amount, rounded up; it stays in the pool.
pub fn fee_of(gross: u64, fee_bps: u16) -> Result<u64> {
    mul_div_ceil(gross, fee_bps as u64, BPS)
}

/// USDC per whole quota at the pool's reserves, rounded down.
pub fn spot_price(pool: Pool) -> Result<u64> {
    mul_div(pool.usdc, QUOTA_UNITS, add(pool.units, pool.virtual_units)?)
}

/// A sale of `units` to the pool: the gross rounds down, the fee up.
pub fn sell(pool: Pool, units: u64, fee_bps: u16) -> Result<Sale> {
    require!(units > 0, FanexError::RaiseZeroAmount);
    let depth = add(add(pool.units, pool.virtual_units)?, units)?;
    let gross = mul_div(pool.usdc, units, depth)?;
    let fee = fee_of(gross, fee_bps)?;
    let net = sub(gross, fee)?;
    require!(net > 0, FanexError::RaiseZeroAmount);
    let after = Pool { usdc: sub(pool.usdc, net)?, units: add(pool.units, units)?, virtual_units: pool.virtual_units };
    Ok(Sale { gross, fee, net, after })
}

/// A purchase of `units` from the pool, at most what it holds: the gross
/// rounds up, and so does the fee.
pub fn buy(pool: Pool, units: u64, fee_bps: u16) -> Result<Purchase> {
    require!(units > 0, FanexError::RaiseZeroAmount);
    require!(units <= pool.units, FanexError::InsufficientUnits);
    let depth = sub(add(pool.units, pool.virtual_units)?, units)?;
    let gross = mul_div_ceil(pool.usdc, units, depth)?;
    require!(gross > 0, FanexError::RaiseZeroAmount);
    let fee = fee_of(gross, fee_bps)?;
    let cost = add(gross, fee)?;
    let after = Pool { usdc: add(pool.usdc, cost)?, units: sub(pool.units, units)?, virtual_units: pool.virtual_units };
    Ok(Purchase { gross, fee, cost, after })
}

/// What `buy_from_pool` charges: `buy`, the constant-product price, and only while that gross is at
/// least the pool's USDC per unit the fans hold, `ceil(usdc x units / fan_units)`; under it the pool
/// does not sell (`PoolPriceUnderShare`), and with no fan unit left it sells nothing
/// (`PoolHoldsEveryQuota`). At close the pool's USDC, its own payouts swept in, is shared over every unit
/// the fans hold, units bought from the pool included (close.rs). A unit bought under that share took
/// part of it from the fans who stayed: one base unit for two took a whole pool from the creator (the
/// internal audit of 2026-10-29), and, priced up to the share as it was from then until N+3's second
/// audit round, a purchase just before a monthly credit, held to close, still took the credit's share
/// of the pool's payouts from them (RM-1: USD 4.84 on 2.96 at 90% sold). Rick's call of 2026-11-05
/// («Niente acquisti al floor») closes the pool's buy side where the share binds: late in a raise, once
/// the fans hold less than the pool's units and virtual units together (about 55% of the quotas),
/// where every capture lives. Bought at the curve, a unit pays at least its share, so what a purchase
/// still gains from the credits that land while it is held is the timing capture accepted on
/// 2026-10-05, priced by the curve.
pub fn buy_pro_rata(pool: Pool, units: u64, fee_bps: u16, fan_units: u64) -> Result<Purchase> {
    let purchase = buy(pool, units, fee_bps)?;
    require!(fan_units > 0, FanexError::PoolHoldsEveryQuota);
    let floor = mul_div_ceil(pool.usdc, units, fan_units)?;
    require!(purchase.gross >= floor, FanexError::PoolPriceUnderShare);
    Ok(purchase)
}

/// What `sell_to_pool` pays (N+3, Rick 2026-11-04, option B): `sell`, but never a gross under the
/// seller's share of the pool's USDC, `floor(usdc x units / fan_units)`, the units the fans hold the
/// seller's included, the fee then taken as on any sale. That share is what the close would pay those
/// units if the pool closed at once, so a seller never gets less than staying would have paid. Where
/// the floor pays, every unit left keeps at least its share: it rounds down and the fee stays in the
/// pool. The curve pays more early in a run of sales (the pool opens at the average price paid, so an
/// early seller takes more than its share, as before) and falls under the floor late; the last fan's
/// whole holding takes the whole counter but the fee, so a pool never empties while `pool_fee_bps` is
/// above zero. Every unit floored: `sell_position` with none bought from the pool.
pub fn sell_pro_rata(pool: Pool, units: u64, fee_bps: u16, fan_units: u64) -> Result<Sale> {
    sell_position(pool, units, 0, fee_bps, fan_units)
}

/// What `sell_to_pool` pays a position (N+3's audit, Rick 2026-11-04): the units it bought from the
/// pool and still holds, `from_pool`, are sold first, at the curve alone, and only the rest, bought in
/// the raise, at `sell_pro_rata`'s floor, priced on the pool the first leg leaves. Bought from the pool
/// at the fans' share and sold back at the share a monthly credit just raised, a unit would take part
/// of the pool's own earnings from the fans who stay, with no risk: the audit's capture. At the curve's
/// price, late in a raise far under the share, that trip loses. Each leg's fee rounds up on its own
/// gross; together they pay at least one base unit (`RaiseZeroAmount` otherwise).
pub fn sell_position(pool: Pool, units: u64, from_pool: u64, fee_bps: u16, fan_units: u64) -> Result<Sale> {
    require!(units > 0, FanexError::RaiseZeroAmount);
    require!(units <= fan_units, FanexError::InsufficientUnits);
    let at_curve = units.min(from_pool);
    let floored = sub(units, at_curve)?;
    // 1. The units bought from the pool, at the curve alone.
    let gross_curve = if at_curve == 0 {
        0
    } else {
        mul_div(pool.usdc, at_curve, add(add(pool.units, pool.virtual_units)?, at_curve)?)?
    };
    let fee_curve = fee_of(gross_curve, fee_bps)?;
    let usdc = sub(pool.usdc, sub(gross_curve, fee_curve)?)?;
    let pool_units = add(pool.units, at_curve)?;
    // 2. The rest at the curve or the seller's share of what the first leg left, whichever pays more.
    let gross_floor = if floored == 0 {
        0
    } else {
        let curve = mul_div(usdc, floored, add(add(pool_units, pool.virtual_units)?, floored)?)?;
        curve.max(mul_div(usdc, floored, sub(fan_units, at_curve)?)?)
    };
    let fee_floor = fee_of(gross_floor, fee_bps)?;
    let gross = add(gross_curve, gross_floor)?;
    let fee = add(fee_curve, fee_floor)?;
    let net = sub(gross, fee)?;
    require!(net > 0, FanexError::RaiseZeroAmount);
    let after = Pool { usdc: sub(pool.usdc, net)?, units: add(pool.units, units)?, virtual_units: pool.virtual_units };
    Ok(Sale { gross, fee, net, after })
}

#[cfg(test)]
mod tests {
    use super::*;

    const Q: u64 = QUOTA_UNITS;
    const FEE: u16 = 100;

    /// Amended spec §3.4: fans paid 33,000 USD for 30,275 quotas (1.0900083 on
    /// average); the pool opens with the 3,000 of the 10 in every 110.
    /// virtual = ceil(3,000,000,000 x 30,275,000,000 / 33,000,000,000)
    ///         = ceil(2,752,272,727.27) = 2,752,272,728 units (2,752.27 quotas).
    fn spec_pool() -> Pool {
        Pool { usdc: 3_000_000_000, units: 0, virtual_units: 2_752_272_728 }
    }

    fn product(p: Pool) -> u128 {
        p.usdc as u128 * (p.units as u128 + p.virtual_units as u128)
    }

    /// Deterministic pool states shaped like real raises: a virtual reserve
    /// between a twentieth and a quarter of the quotas sold, the pool holding
    /// anything from none to all of them, with a sale size and a purchase size.
    fn cases() -> Vec<(Pool, u64, u64)> {
        let mut seed: u64 = 7;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            seed >> 16
        };
        (0..3_000)
            .map(|_| {
                let sold = Q + next() % (100_000 * Q);
                let virtual_units = 1 + sold / 20 + next() % (sold / 5);
                let units = next() % (sold + 1);
                let usdc = 1 + next() % 1_000_000_000_000;
                let sell_units = 1 + next() % sold;
                let buy_units = 1 + next() % units.max(1);
                (Pool { usdc, units, virtual_units }, sell_units, buy_units)
            })
            .collect()
    }

    #[test]
    fn the_spec_pool_starts_at_the_average_price_paid() {
        assert_eq!(virtual_units_for(3_000_000_000, 30_275 * Q, 33_000_000_000).unwrap(), 2_752_272_728);
        // 3,000,000,000 x 1,000,000 / 2,752,272,728 = 1,090,008.26: 1.090008 a
        // quota, the average paid (1.0900083) at the same precision.
        assert_eq!(spot_price(spec_pool()).unwrap(), 1_090_008);
    }

    #[test]
    fn a_sale_of_one_percent_of_the_quotas() {
        // 1% of 30,275 quotas = 302,750,000 units.
        // gross = floor(3,000,000,000 x 302,750,000 / (0 + 2,752,272,728 + 302,750,000))
        //       = floor(908,250,000,000,000,000 / 3,055,022,728) = 297,297,297
        // fee   = ceil(297,297,297 x 100 / 10,000) = ceil(2,972,972.97) = 2,972,973
        // net   = 294,324,324: 0.972 a quota, about 10% under the starting 1.090.
        let sale = sell(spec_pool(), 302_750_000, FEE).unwrap();
        assert_eq!((sale.gross, sale.fee, sale.net), (297_297_297, 2_972_973, 294_324_324));
        assert_eq!(sale.after, Pool { usdc: 2_705_675_676, units: 302_750_000, virtual_units: 2_752_272_728 });
        // 2,705,675,676 x 1,000,000 / 3,055,022,728 = 885,648.30: the price falls to 0.885648.
        assert_eq!(spot_price(sale.after).unwrap(), 885_648);
    }

    #[test]
    fn buying_back_the_same_quotas_lifts_the_price_back() {
        let after_sale = sell(spec_pool(), 302_750_000, FEE).unwrap().after;
        // gross = ceil(2,705,675,676 x 302,750,000 / (302,750,000 + 2,752,272,728 - 302,750,000))
        //       = ceil(819,143,310,909,000,000 / 2,752,272,728) = ceil(297,624,324.28) = 297,624,325
        // fee   = ceil(2,976,243.25) = 2,976,244; cost = 300,600,569
        let purchase = buy(after_sale, 302_750_000, FEE).unwrap();
        assert_eq!((purchase.gross, purchase.fee, purchase.cost), (297_624_325, 2_976_244, 300_600_569));
        assert_eq!(purchase.after, Pool { usdc: 3_006_276_245, units: 0, virtual_units: 2_752_272_728 });
        // 3,006,276,245 x 1,000,000 / 2,752,272,728 = 1,092,288.64: back above the
        // start by the two fees the pool kept.
        assert_eq!(spot_price(purchase.after).unwrap(), 1_092_288);
    }

    #[test]
    fn selling_every_quota_pays_out_about_91_percent_of_the_pool() {
        // gross = floor(3,000,000,000 x 30,275,000,000 / (2,752,272,728 + 30,275,000,000))
        //       = floor(90,825,000,000,000,000,000 / 33,027,272,728) = 2,749,999,999 (91.67%)
        // fee   = ceil(27,499,999.99) = 27,500,000; net = 2,722,499,999 (90.75% of the pool).
        let sale = sell(spec_pool(), 30_275 * Q, FEE).unwrap();
        assert_eq!((sale.gross, sale.fee, sale.net), (2_749_999_999, 27_500_000, 2_722_499_999));
        assert_eq!(sale.after.usdc, 277_500_001);
    }

    #[test]
    fn the_devnet_smoke_round_trip_is_what_its_script_expects() {
        // scripts/solana/devnet-raise-smoke.ts: 20 quotas sold for 20 USD open a
        // pool of 1,818,182 with as many virtual units; one quota is sold, then
        // bought back.
        assert_eq!(virtual_units_for(1_818_182, 20 * Q, 20_000_000).unwrap(), 1_818_182);
        let pool = Pool { usdc: 1_818_182, units: 0, virtual_units: 1_818_182 };
        let sale = sell(pool, Q, FEE).unwrap();
        assert_eq!((sale.gross, sale.fee, sale.net), (645_161, 6_452, 638_709));
        let back = buy(sale.after, Q, FEE).unwrap();
        assert_eq!((back.gross, back.fee, back.cost), (648_711, 6_488, 655_199));
        assert_eq!(back.after, Pool { usdc: 1_834_672, units: 0, virtual_units: 1_818_182 });
    }

    #[test]
    fn a_trade_never_lowers_the_pools_product() {
        for fee in [0u16, 100, 300] {
            for (pool, sell_units, buy_units) in cases() {
                match sell(pool, sell_units, fee) {
                    Ok(sale) => assert!(product(sale.after) >= product(pool), "sale {pool:?} {sell_units}"),
                    Err(e) => assert_eq!(e, error!(FanexError::RaiseZeroAmount)),
                }
                if buy_units <= pool.units {
                    let purchase = buy(pool, buy_units, fee).unwrap();
                    assert!(product(purchase.after) >= product(pool), "purchase {pool:?} {buy_units}");
                }
            }
        }
    }

    #[test]
    fn a_round_trip_never_pays() {
        for fee in [0u16, 100, 300] {
            for (pool, sell_units, buy_units) in cases() {
                // A sale bought back at once costs at least what the sale paid.
                if let Ok(sale) = sell(pool, sell_units, fee) {
                    let back = buy(sale.after, sell_units, fee).unwrap();
                    assert!(back.cost >= sale.net, "sell then buy {pool:?} {sell_units}");
                }
                // A purchase sold back at once returns at most what it cost.
                if buy_units <= pool.units {
                    let purchase = buy(pool, buy_units, fee).unwrap();
                    match sell(purchase.after, buy_units, fee) {
                        Ok(sale) => assert!(sale.net <= purchase.cost, "buy then sell {pool:?} {buy_units}"),
                        Err(e) => assert_eq!(e, error!(FanexError::RaiseZeroAmount)),
                    }
                }
            }
        }
    }

    #[test]
    fn a_sale_never_empties_the_pool() {
        for (pool, sell_units, _) in cases() {
            // Even with no fee at all.
            if let Ok(sale) = sell(pool, sell_units, 0) {
                assert!(sale.net < pool.usdc && sale.after.usdc > 0, "{pool:?} {sell_units}");
            }
        }
        // Not even a sale of more units than could ever exist.
        assert!(sell(spec_pool(), u64::MAX / 2, FEE).unwrap().after.usdc > 0);
    }

    #[test]
    fn a_purchase_above_the_pools_quotas_is_refused() {
        let pool = Pool { usdc: 1_000_000, units: 10 * Q, virtual_units: 100 * Q };
        assert_eq!(buy(pool, 10 * Q + 1, FEE).unwrap_err(), error!(FanexError::InsufficientUnits));
        assert!(buy(pool, 10 * Q, FEE).is_ok());
        let empty = Pool { units: 0, ..pool };
        assert_eq!(buy(empty, 1, FEE).unwrap_err(), error!(FanexError::InsufficientUnits));
        for (pool, _, _) in cases() {
            assert_eq!(buy(pool, pool.units + 1, FEE).unwrap_err(), error!(FanexError::InsufficientUnits));
        }
    }

    #[test]
    fn zero_units_and_a_sale_worth_nothing_are_refused() {
        assert_eq!(sell(spec_pool(), 0, FEE).unwrap_err(), error!(FanexError::RaiseZeroAmount));
        assert_eq!(buy(Pool { units: Q, ..spec_pool() }, 0, FEE).unwrap_err(), error!(FanexError::RaiseZeroAmount));
        // One unit is worth floor(3,000,000,000 / 2,752,272,729) = 1 base unit; the
        // 1% fee rounds up to 1, so the seller would receive nothing.
        assert_eq!(sell(spec_pool(), 1, FEE).unwrap_err(), error!(FanexError::RaiseZeroAmount));
        // A pool with no USDC would hand quotas out for nothing.
        let dry = Pool { usdc: 0, units: Q, virtual_units: Q };
        assert_eq!(buy(dry, 1, FEE).unwrap_err(), error!(FanexError::RaiseZeroAmount));
    }

    #[test]
    fn the_fee_rounds_up_and_stays_in_the_pool() {
        assert_eq!(fee_of(0, 100).unwrap(), 0);
        assert_eq!(fee_of(1, 100).unwrap(), 1);
        assert_eq!(fee_of(100, 100).unwrap(), 1);
        assert_eq!(fee_of(101, 100).unwrap(), 2);
        assert_eq!(fee_of(1_000_000, 300).unwrap(), 30_000);
        // The counter falls by the net only: the fee stays in the fans' pool.
        let sale = sell(spec_pool(), 302_750_000, FEE).unwrap();
        assert_eq!(spec_pool().usdc - sale.after.usdc, sale.gross - sale.fee);
    }

    // ── The pro-rata purchase (internal audit of 2026-10-29) ──

    /// The spec pool once every quota was sold to it: the audit's own vector.
    fn every_quota_sold() -> Pool {
        sell(spec_pool(), 30_275 * Q, FEE).unwrap().after
    }

    #[test]
    fn one_base_unit_no_longer_buys_a_pool_that_holds_every_quota() {
        let pool = every_quota_sold();
        assert_eq!(pool, Pool { usdc: 277_500_001, units: 30_275 * Q, virtual_units: 2_752_272_728 });
        // The constant-product price alone: one base unit for 2 (gross 1, fee 1), and at close it
        // would have been the only fan unit, taking the whole pool from the creator.
        let cheap = buy(pool, 1, FEE).unwrap();
        assert_eq!((cheap.gross, cheap.fee, cheap.cost), (1, 1, 2));
        // With no fan unit left nothing is sold, whatever the size.
        for units in [1, Q, 30_275 * Q] {
            assert_eq!(buy_pro_rata(pool, units, FEE, 0).unwrap_err(), error!(FanexError::PoolHoldsEveryQuota));
        }
    }

    #[test]
    fn a_quota_under_the_last_fans_share_is_not_sold() {
        // One fan kept one quota; the pool holds the other 30,274.
        let pool = Pool { usdc: 277_500_001, units: 30_274 * Q, virtual_units: 2_752_272_728 };
        let amm = buy(pool, Q, FEE).unwrap();
        // ceil(277,500,001 x 1,000,000 / 33,025,272,728) = 8,403: 0.0084 a quota, against the fan's share
        // ceil(277,500,001 x 1,000,000 / 1,000,000) = 277,500,001. Until N+3's second round the purchase
        // was charged that share; since Rick's call of 2026-11-05 the pool does not sell under it.
        assert_eq!(amm.gross, 8_403);
        assert_eq!(buy_pro_rata(pool, Q, FEE, Q).unwrap_err(), error!(FanexError::PoolPriceUnderShare));
        // Not even one base unit: ceil(277,500,001 x 1 / 1,000,000) = 278 against a gross of 1.
        assert_eq!(buy_pro_rata(pool, 1, FEE, Q).unwrap_err(), error!(FanexError::PoolPriceUnderShare));
    }

    #[test]
    fn the_share_closes_the_pools_buy_side_exactly_where_it_passes_the_curve() {
        // `active_raise(1)`'s pool after n fans sold their 100 quotas each: a 10-quota purchase is sold at
        // the curve while the curve is at or above the fans' share, refused once the share passes it.
        for n in 0..=9u64 {
            let (pool, fans) = after_sales(n);
            if pool.units < 10 * Q {
                continue;
            }
            let amm = buy(pool, 10 * Q, FEE).unwrap();
            let share = (pool.usdc as u128 * (10 * Q) as u128).div_ceil(fans as u128) as u64;
            match buy_pro_rata(pool, 10 * Q, FEE, fans) {
                Ok(p) => assert!(amm.gross >= share && p == amm, "{n}"),
                Err(e) => assert!(amm.gross < share && e == error!(FanexError::PoolPriceUnderShare), "{n}"),
            }
        }
        // On that raise the buy side is open after one sale of 100 (fans hold 90%) and shut after five
        // (fans hold 50%, under the ~55% where the share passes the curve).
        assert!(buy_pro_rata(after_sales(1).0, 10 * Q, FEE, after_sales(1).1).is_ok());
        assert_eq!(buy_pro_rata(after_sales(5).0, 10 * Q, FEE, after_sales(5).1).unwrap_err(), error!(FanexError::PoolPriceUnderShare));
    }

    #[test]
    fn while_fans_hold_most_quotas_the_constant_product_price_stands() {
        // After a 1% sale the fans hold 99%: the floor is far below the curve.
        let after_sale = sell(spec_pool(), 302_750_000, FEE).unwrap().after;
        let fans = 30_275 * Q - 302_750_000;
        assert_eq!(buy_pro_rata(after_sale, 302_750_000, FEE, fans).unwrap(), buy(after_sale, 302_750_000, FEE).unwrap());
    }

    #[test]
    fn a_purchase_the_pool_sells_never_gets_back_more_than_it_paid_nor_dilutes_the_fans() {
        for fee in [0u16, 100, 300] {
            for (pool, _, buy_units) in cases() {
                if buy_units > pool.units {
                    continue;
                }
                // The fans hold anything from one unit to many times the pool's units.
                for fans in [1u64, Q, pool.units / 3 + 1, pool.units + 1, 4 * pool.units + 1] {
                    let p = match buy_pro_rata(pool, buy_units, fee, fans) {
                        Ok(p) => p,
                        // Refused only where the share prices it above the curve, or the cost overflows.
                        Err(e) => {
                            let amm = buy(pool, buy_units, fee);
                            let share = (pool.usdc as u128 * buy_units as u128).div_ceil(fans as u128);
                            assert!(
                                e == error!(FanexError::RaiseMathOverflow)
                                    || (e == error!(FanexError::PoolPriceUnderShare) && (amm.unwrap().gross as u128) < share),
                                "{pool:?} {buy_units} {fans}: {e:?}"
                            );
                            continue;
                        }
                    };
                    // Always the curve's own price.
                    assert_eq!(p, buy(pool, buy_units, fee).unwrap());
                    let (usdc, units, fans, paid) = (p.after.usdc as u128, buy_units as u128, fans as u128, p.cost as u128);
                    // What the units would take at close, if the pool closed at once.
                    assert!(usdc * units <= paid * (fans + units), "{pool:?} {buy_units} {fans}");
                    // Each fan unit's share of the pool does not fall.
                    assert!(usdc * fans >= pool.usdc as u128 * (fans + units), "{pool:?} {buy_units} {fans}");
                    // Never under the constant-product price, so the product never falls either.
                    let amm = buy(pool, buy_units, fee).unwrap();
                    assert!(p.gross >= amm.gross && product(p.after) >= product(pool));
                }
            }
        }
    }

    // ── The seller's floor (N+3, option B) ──

    #[test]
    fn while_fans_hold_most_quotas_a_sale_is_priced_by_the_curve() {
        // A 1% sale of the spec pool: the share, floor(3,000,000,000 x 302,750,000 / 30,275,000,000) =
        // 30,000,000, is far under the curve's 297,297,297.
        let fans = 30_275 * Q;
        assert_eq!(sell_pro_rata(spec_pool(), 302_750_000, FEE, fans).unwrap(), sell(spec_pool(), 302_750_000, FEE).unwrap());
    }

    #[test]
    fn the_smoke_raise_s_twelfth_quota_is_paid_its_share() {
        // The devnet smoke's raise (20 quotas for 20 USD, a pool of 1,818,182 against as many virtual
        // units), its fans selling one quota after another: what each is paid, in thousandths of a
        // dollar rounded, by the curve alone and with the floor, as the canvas's table showed (5% sold
        // 0.639 both ways; 60%: 0.019 by the curve alone, 0.028 with the floor).
        let start = Pool { usdc: 1_818_182, units: 0, virtual_units: 1_818_182 };
        let (mut curve, mut floored, mut fans) = (start, start, 20 * Q);
        let (mut by_curve, mut with_floor) = (Vec::new(), Vec::new());
        for _ in 0..12 {
            let a = sell(curve, Q, FEE).unwrap();
            let b = sell_pro_rata(floored, Q, FEE, fans).unwrap();
            by_curve.push((a.net + 500) / 1_000);
            with_floor.push((b.net + 500) / 1_000);
            (curve, floored, fans) = (a.after, b.after, fans - Q);
        }
        assert_eq!((by_curve[0], by_curve[11]), (639, 19));
        assert_eq!((with_floor[0], with_floor[11]), (639, 28));
        // The floor first pays where the curve falls under the seller's share, and from then on.
        let first = with_floor.iter().zip(&by_curve).position(|(b, a)| b > a).unwrap();
        assert!(with_floor[first..].iter().zip(&by_curve[first..]).all(|(b, a)| b > a));
        assert_eq!(with_floor[..first], by_curve[..first]);
    }

    #[test]
    fn the_last_fan_takes_the_pool_but_its_fee() {
        let pool = Pool { usdc: 250_000, units: 19 * Q, virtual_units: 1_818_182 };
        let sale = sell_pro_rata(pool, Q, FEE, Q).unwrap();
        // gross 250,000, the whole counter; fee ceil(2,500) = 2,500 stays.
        assert_eq!((sale.gross, sale.fee, sale.net, sale.after.usdc), (250_000, 2_500, 247_500, 2_500));
        // With no fee, the whole counter: the close would have paid that unit no less.
        assert_eq!(sell_pro_rata(pool, Q, 0, Q).unwrap().after.usdc, 0);
    }

    #[test]
    fn a_sale_above_the_fans_units_or_of_nothing_is_refused() {
        let pool = Pool { usdc: 1_000_000, units: 10 * Q, virtual_units: 100 * Q };
        assert_eq!(sell_pro_rata(pool, Q + 1, FEE, Q).unwrap_err(), error!(FanexError::InsufficientUnits));
        assert_eq!(sell_pro_rata(pool, 0, FEE, Q).unwrap_err(), error!(FanexError::RaiseZeroAmount));
        assert_eq!(sell_pro_rata(pool, 1, FEE, 0).unwrap_err(), error!(FanexError::InsufficientUnits));
    }

    #[test]
    fn a_floored_sale_never_lowers_a_remaining_fans_share_nor_pays_on_a_round_trip() {
        for fee in [0u16, 100, 300] {
            for (pool, sell_units, _) in cases() {
                // The fans hold anything from the sale itself to many times it.
                for fans in [sell_units, sell_units + 1, 2 * sell_units, 10 * sell_units + 7] {
                    let Ok(sale) = sell_pro_rata(pool, sell_units, fee, fans) else { continue };
                    // Never under the curve, nor under the seller's share.
                    let amm = sell(pool, sell_units, fee).map(|s| s.gross).unwrap_or(0);
                    let share = (pool.usdc as u128 * sell_units as u128 / fans as u128) as u64;
                    assert!(sale.gross >= amm && sale.gross >= share, "{pool:?} {sell_units} {fans}");
                    // Where the floor pays, each unit left keeps at least its share: usdc' x fans >=
                    // usdc x (fans - units). (Where the curve pays more, an early seller takes more
                    // than its share, as before N+3: the pool opens at the average price paid.)
                    let left = fans - sell_units;
                    if sale.gross > amm {
                        assert!(
                            sale.after.usdc as u128 * fans as u128 >= pool.usdc as u128 * left as u128,
                            "{pool:?} {sell_units} {fans}"
                        );
                    }
                    // Bought straight back, as `buy_from_pool` prices it, the units cost at least the sale
                    // paid, or the pool does not sell them (under the share), or the cost would pass u64.
                    if left > 0 {
                        match buy_pro_rata(sale.after, sell_units, fee, left) {
                            Ok(back) => assert!(back.cost >= sale.net, "round trip {pool:?} {sell_units} {fans}"),
                            Err(e) => assert!(
                                e == error!(FanexError::RaiseMathOverflow) || e == error!(FanexError::PoolPriceUnderShare),
                                "{e:?}"
                            ),
                        }
                    }
                    // With a fee, the pool keeps something.
                    if fee > 0 {
                        assert!(sale.after.usdc > 0, "{pool:?} {sell_units} {fans}");
                    }
                }
            }
        }
    }

    // ── Units bought from the pool (N+3's audit, Rick 2026-11-04) ──

    /// The audit's pool: `active_raise(1)`'s 1,000 quotas for USD 1,000 after nine fans sold their 100
    /// quotas each at the floor (the residual 2,925,804 of the LiteSVM suite's end.rs replay).
    fn late_pool() -> (Pool, u64) {
        after_sales(9)
    }

    /// `active_raise(1)`'s pool after `n` fans sold their 100 quotas each, floored.
    fn after_sales(n: u64) -> (Pool, u64) {
        let mut pool = Pool { usdc: 90_909_091, units: 0, virtual_units: 90_909_091 };
        let mut fans = 1_000 * Q;
        for _ in 0..n {
            pool = sell_pro_rata(pool, 100 * Q, FEE, fans).unwrap().after;
            fans -= 100 * Q;
        }
        (pool, fans)
    }

    #[test]
    fn units_bought_from_the_pool_sell_at_the_curve_alone() {
        for (pool, sell_units, _) in cases() {
            let fans = 2 * sell_units;
            match (sell_position(pool, sell_units, sell_units, FEE, fans), sell(pool, sell_units, FEE)) {
                (Ok(a), Ok(b)) => assert_eq!(a, b, "{pool:?} {sell_units}"),
                (Err(a), Err(b)) => assert_eq!(a, b),
                (a, b) => panic!("{pool:?} {sell_units}: {a:?} against {b:?}"),
            }
        }
    }

    #[test]
    fn a_mixed_sale_sells_the_pool_s_units_first_then_floors_the_rest() {
        // Five fans sold: the floor pays a sale of the 500 quotas left.
        let (pool, fans) = after_sales(5);
        let sale = sell_position(pool, 100 * Q, 40 * Q, FEE, fans).unwrap();
        let first = sell(pool, 40 * Q, FEE).unwrap();
        let rest = sell_pro_rata(first.after, 60 * Q, FEE, fans - 40 * Q).unwrap();
        assert_eq!(sale.gross, first.gross + rest.gross);
        assert_eq!(sale.fee, first.fee + rest.fee);
        assert_eq!(sale.after, rest.after);
        // More than all of it at the curve, less than all of it floored.
        assert!(sale.net > sell(pool, 100 * Q, FEE).unwrap().net);
        assert!(sale.net < sell_pro_rata(pool, 100 * Q, FEE, fans).unwrap().net);
        // `from_pool` above the sale: every unit at the curve.
        assert_eq!(sell_position(pool, 10 * Q, 50 * Q, FEE, fans).unwrap(), sell(pool, 10 * Q, FEE).unwrap());
    }

    #[test]
    fn the_audits_captures_around_a_monthly_credit_no_longer_start() {
        // Round 1 and round 2 of N+3's audit: a newcomer bought the 100 quotas the fans' share priced (cost
        // 2,955,063), a month credited 9,700,000 over the 1,000 quotas outstanding (970,000 to its 100,
        // 7,760,000 to the pool's 800, swept into the pool), and it either sold the 100 straight back,
        // floored (round 1: USD 4.77 gained), or held them to `close_raise`, which shares the pool over every
        // fan unit (round 2: USD 4.84 gained). Both start with that purchase, and the pool no longer sells
        // under the fans' share: the curve prices the 100 at 0.33 USD against their share of 2.93.
        let (pool, fans) = late_pool();
        assert_eq!((pool.usdc, pool.units, fans), (2_925_804, 900 * Q, 100 * Q));
        assert!(buy(pool, 100 * Q, FEE).unwrap().gross < 2_925_804);
        assert_eq!(buy_pro_rata(pool, 100 * Q, FEE, fans).unwrap_err(), error!(FanexError::PoolPriceUnderShare));
        // Round 1's trip on the old floored purchase, kept as the measure: still a gain floored, a loss
        // at the curve, as units bought from the pool are sold since N+3's first fix.
        let bought_cost = 2_955_063;
        let credited = Pool { usdc: pool.usdc + bought_cost + 7_760_000, units: pool.units - 100 * Q, ..pool };
        let payout = 970_000;
        let floored = sell_pro_rata(credited, 100 * Q, FEE, fans + 100 * Q).unwrap();
        assert!(floored.net + payout > bought_cost + 4_700_000, "{floored:?}");
        let fixed = sell_position(credited, 100 * Q, 100 * Q, FEE, fans + 100 * Q).unwrap();
        assert!(fixed.net + payout < bought_cost, "{fixed:?}");
    }

    #[test]
    fn a_unit_bought_from_the_pool_and_sold_straight_back_never_pays() {
        for fee in [0u16, 100, 300] {
            for (pool, _, buy_units) in cases() {
                if buy_units > pool.units {
                    continue;
                }
                for fans in [Q, pool.units / 3 + 1, 4 * pool.units + 1] {
                    // Refused under the fans' share: no trip to make.
                    let Ok(bought) = buy_pro_rata(pool, buy_units, fee, fans) else { continue };
                    match sell_position(bought.after, buy_units, buy_units, fee, fans + buy_units) {
                        Ok(back) => assert!(back.net <= bought.cost, "{pool:?} {buy_units} {fans}"),
                        Err(e) => assert_eq!(e, error!(FanexError::RaiseZeroAmount)),
                    }
                }
            }
        }
    }

    #[test]
    fn a_raise_holding_split_across_the_crossover_is_paid_a_little_more() {
        // The audit's measure, kept as a known effect (PEPPER-RAISE-PROGRAM.md §4): the floor is taken per
        // sale, so after four fans sold 100 quotas, the fifth's 100 pay 2,883,686 in one sale and 2,982,448
        // in two of 50, a few percent the 500 quotas that stay and the creator's leftover give up.
        let (pool, fans) = after_sales(4);
        let whole = sell_pro_rata(pool, 100 * Q, FEE, fans).unwrap().net;
        let first = sell_pro_rata(pool, 50 * Q, FEE, fans).unwrap();
        let second = sell_pro_rata(first.after, 50 * Q, FEE, fans - 50 * Q).unwrap();
        assert_eq!((whole, first.net + second.net), (2_883_686, 2_982_448));
    }
}
