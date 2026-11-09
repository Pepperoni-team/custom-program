//! The money of a raise: the split at close (spec §4.2), the 3% on every
//! monthly payment (§4.6), and where a creator's payment goes (the 2026-10-19
//! design §2: the arrears, then the guarantee, then the month). No cap
//! (amended spec D13): a payment moves whatever the month still owes, for the
//! whole term.

use anchor_lang::prelude::*;

use crate::raise::checked::{mul_div, sub};
use crate::raise::constants::*;
use crate::raise::validation::creator_part_of;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloseSplit {
    pub creator_part: u64,
    pub to_creator: u64,
    pub to_guarantee: u64,
    pub to_fanex: u64,
    /// The 10 of every 110: the exit pool's USDC (amended spec §3.1).
    pub to_pool: u64,
}

/// The split at close of what fans paid. Rounding goes to the creator.
pub fn close_split(fan_total: u64) -> Result<CloseSplit> {
    let creator_part = creator_part_of(fan_total)?;
    let to_pool = sub(fan_total, creator_part)?;
    let to_fanex = mul_div(creator_part, FANEX_RAISE_FEE_BPS, BPS)?;
    let to_guarantee = mul_div(creator_part, GUARANTEE_BPS, BPS)?;
    let to_creator = sub(sub(creator_part, to_fanex)?, to_guarantee)?;
    Ok(CloseSplit { creator_part, to_creator, to_guarantee, to_fanex, to_pool })
}

pub fn meets_minimum(creator_part: u64, min_creator_part: u64) -> bool {
    creator_part >= min_creator_part
}

/// Fanex's 3% and the fans' 97% of a gross monthly payment.
pub fn payout_split(gross: u64) -> Result<(u64, u64)> {
    let fee = mul_div(gross, PAYOUT_FEE_BPS, BPS)?;
    Ok((fee, sub(gross, fee)?))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Payment {
    pub gross: u64,
    pub fee: u64,
    pub credit: u64,
}

/// What an offered payment actually moves: never more than the month still
/// owes. `fee + credit == gross` always.
pub fn payment_plan(offered: u64, open: u64) -> Result<Payment> {
    let gross = offered.min(open);
    let (fee, credit) = payout_split(gross)?;
    Ok(Payment { gross, fee, credit })
}

/// Where a creator's payment goes while the raise is `Active` (the 2026-10-19
/// design §2), as `(to_guarantee, the month's payment)`:
/// 1. an `overdue` month takes the payment first, up to what it still owes (the
///    arrears), and what is left refills the guarantee, up to `owed` (the
///    internal audit of 2026-10-29: before it, a late payment rebuilt nothing,
///    so a creator who always paid after the due date never rebuilt it);
/// 2. otherwise the guarantee is refilled first, up to `owed`, with no fee
///    (the 3% is taken when the guarantee pays the fans);
/// 3. the rest goes to the month, up to `open`; with no month open the caller
///    passes `u64::MAX`, and it reaches the fans straight.
///
/// Whatever is left of `offered` is not taken. The caller passes `owed = 0`
/// once the raise is closed, and for an overdue month whose next month is
/// already settled (`ledger::refills_after_arrears`).
pub fn allocate_payment(offered: u64, open: u64, overdue: bool, owed: u64) -> Result<(u64, Payment)> {
    if overdue {
        let plan = payment_plan(offered, open)?;
        return Ok((sub(offered, plan.gross)?.min(owed), plan));
    }
    let to_guarantee = offered.min(owed);
    Ok((to_guarantee, payment_plan(sub(offered, to_guarantee)?, open)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spec_example_splits_90_5_5_10() {
        let s = close_split(33_000_000_000).unwrap();
        assert_eq!(
            s,
            CloseSplit {
                creator_part: 30_000_000_000,
                to_creator: 27_000_000_000,
                to_guarantee: 1_500_000_000,
                to_fanex: 1_500_000_000,
                to_pool: 3_000_000_000,
            }
        );
    }

    #[test]
    fn every_unit_lands_somewhere() {
        for fan_total in [11u64, 999, 1_000_000_000, 10_900_000_001, 33_000_000_000] {
            let s = close_split(fan_total).unwrap();
            assert_eq!(s.to_creator + s.to_guarantee + s.to_fanex + s.to_pool, fan_total);
        }
    }

    #[test]
    fn the_devnet_smoke_raise_splits_as_its_script_expects() {
        // scripts/solana/devnet-raise-smoke.ts asserts the last two numbers.
        let s = close_split(20_000_000).unwrap();
        assert_eq!((s.creator_part, s.to_creator, s.to_pool), (18_181_818, 16_363_638, 1_818_182));
    }

    #[test]
    fn a_tiny_raise_rounds_in_the_creators_favour() {
        let s = close_split(11).unwrap();
        assert_eq!((s.creator_part, s.to_pool, s.to_fanex, s.to_guarantee, s.to_creator), (10, 1, 0, 0, 10));
    }

    #[test]
    fn the_minimum_is_inclusive() {
        assert!(meets_minimum(10, 10));
        assert!(!meets_minimum(9, 10));
    }

    #[test]
    fn three_percent_to_fanex() {
        assert_eq!(payout_split(1_500_000_000).unwrap(), (45_000_000, 1_455_000_000));
    }

    #[test]
    fn a_payment_never_exceeds_what_the_month_owes() {
        assert_eq!(payment_plan(10_000, 5_000).unwrap(), Payment { gross: 5_000, fee: 150, credit: 4_850 });
    }

    #[test]
    fn a_partial_payment_moves_whole() {
        assert_eq!(payment_plan(2_000, 5_000).unwrap(), Payment { gross: 2_000, fee: 60, credit: 1_940 });
    }

    #[test]
    fn there_is_no_cap_on_a_payment() {
        // 2,000 USD on a month that owes 2,000 moves in full: under the 2026-10-01
        // design a 909 USD raise would have stopped at 1,363.64 for the fans.
        assert_eq!(
            payment_plan(2_000_000_000, 2_000_000_000).unwrap(),
            Payment { gross: 2_000_000_000, fee: 60_000_000, credit: 1_940_000_000 }
        );
    }

    #[test]
    fn nothing_moves_on_a_month_that_owes_nothing() {
        assert_eq!(payment_plan(1_000, 0).unwrap(), Payment::default());
    }

    /// The design's figures (2026-10-19 §2): a month of 200 and a guarantee owed 200.
    const OWED: u64 = 200_000_000;
    const OPEN: u64 = 200_000_000;
    /// 200 paid into a month: 6 to Fanex, 194 to the fans.
    const MONTH: Payment = Payment { gross: 200_000_000, fee: 6_000_000, credit: 194_000_000 };

    #[test]
    fn the_usual_payment_refills_the_guarantee_first() {
        assert_eq!(allocate_payment(200_000_000, OPEN, false, OWED).unwrap(), (200_000_000, Payment::default()));
    }

    #[test]
    fn paying_the_guarantee_on_top_pays_the_month_too() {
        assert_eq!(allocate_payment(400_000_000, OPEN, false, OWED).unwrap(), (200_000_000, MONTH));
    }

    #[test]
    fn an_overdue_month_is_paid_first_and_what_is_left_refills_the_guarantee() {
        // The arrears first, then the guarantee (internal audit of 2026-10-29): 200 to the month, 200 to the guarantee.
        assert_eq!(allocate_payment(400_000_000, OPEN, true, OWED).unwrap(), (200_000_000, MONTH));
        // Up to what it is owed: past that, nothing more is taken.
        assert_eq!(allocate_payment(500_000_000, OPEN, true, OWED).unwrap(), (200_000_000, MONTH));
        // A payment of the arrears alone refills nothing; the arrears always come first.
        assert_eq!(allocate_payment(200_000_000, OPEN, true, OWED).unwrap(), (0, MONTH));
        assert_eq!(
            allocate_payment(150_000_000, OPEN, true, OWED).unwrap(),
            (0, Payment { gross: 150_000_000, fee: 4_500_000, credit: 145_500_000 })
        );
        // The caller passes nothing owed when the next month is settled: the excess is not taken.
        assert_eq!(allocate_payment(400_000_000, OPEN, true, 0).unwrap(), (0, MONTH));
    }

    #[test]
    fn with_nothing_owed_a_payment_is_what_it_always_was() {
        assert_eq!(allocate_payment(250_000_000, OPEN, false, 0).unwrap(), (0, MONTH));
        assert_eq!(allocate_payment(250_000_000, OPEN, false, 0).unwrap().1, payment_plan(250_000_000, OPEN).unwrap());
    }

    #[test]
    fn a_payment_below_what_the_guarantee_is_owed_all_refills_it() {
        assert_eq!(allocate_payment(50_000_000, OPEN, false, OWED).unwrap(), (50_000_000, Payment::default()));
    }

    #[test]
    fn nothing_past_the_guarantee_and_the_month_is_taken() {
        let (to_guarantee, month) = allocate_payment(1_000_000_000, OPEN, false, OWED).unwrap();
        assert_eq!((to_guarantee, month), (200_000_000, MONTH));
        assert_eq!(to_guarantee + month.gross, 400_000_000);
    }

    #[test]
    fn with_no_month_open_the_rest_reaches_the_fans() {
        assert_eq!(
            allocate_payment(500_000_000, u64::MAX, false, OWED).unwrap(),
            (200_000_000, Payment { gross: 300_000_000, fee: 9_000_000, credit: 291_000_000 })
        );
    }
}
