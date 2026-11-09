//! Payouts without snapshots (spec §6.5). One accumulator per raise, "USDC owed
//! per quota unit since the start", scaled by ACC_SCALE. A position is owed
//! what its units accrued since its last settlement. Entitlements round down
//! and the debt a holding starts from rounds up, so no position ever collects a
//! rounding unit the accumulator did not pay, however often units change hands
//! (every pool trade moves some): the payouts vault always holds at least what
//! every position can claim.

use anchor_lang::prelude::*;

use crate::raise::checked::add;
use crate::errors::FanexError;

pub const ACC_SCALE: u128 = 1_000_000_000_000;

/// How much a credit of `amount` raises the accumulator.
pub fn acc_increment(amount: u64, units_outstanding: u64) -> Result<u128> {
    require!(units_outstanding > 0, FanexError::RaiseZeroAmount);
    let scaled = (amount as u128)
        .checked_mul(ACC_SCALE)
        .ok_or_else(|| error!(FanexError::RaiseMathOverflow))?;
    Ok(scaled / units_outstanding as u128)
}

/// What `units` are entitled to at accumulator `acc`, in USDC base units,
/// rounded down.
pub fn entitled(units: u64, acc: u128) -> Result<u128> {
    let product = (units as u128)
        .checked_mul(acc)
        .ok_or_else(|| error!(FanexError::RaiseMathOverflow))?;
    Ok(product / ACC_SCALE)
}

/// The same, rounded up: the debt of a holding that starts accruing at `acc`.
pub fn entitled_ceil(units: u64, acc: u128) -> Result<u128> {
    let product = (units as u128)
        .checked_mul(acc)
        .ok_or_else(|| error!(FanexError::RaiseMathOverflow))?;
    let rounded_up = product
        .checked_add(ACC_SCALE - 1)
        .ok_or_else(|| error!(FanexError::RaiseMathOverflow))?;
    Ok(rounded_up / ACC_SCALE)
}

/// Moves what `units` accrued since `debt` into `pending`. Right after a change
/// of holding the debt can sit one unit above the entitlement: nothing is owed
/// until the accumulator passes it.
pub fn settle(units: u64, debt: u128, pending: u64, acc: u128) -> Result<(u128, u64)> {
    let now_entitled = entitled(units, acc)?;
    if now_entitled <= debt {
        return Ok((debt, pending));
    }
    let owed = u64::try_from(now_entitled - debt).map_err(|_| error!(FanexError::RaiseMathOverflow))?;
    Ok((now_entitled, add(pending, owed)?))
}

/// What the accumulator's own floors kept back of everything credited to
/// `units`: no position can ever claim it, so the close shares it out
/// (amended spec §4). Zero when nothing is left over.
pub fn rounding_remainder(credited_total: u64, units: u64, acc: u128) -> Result<u64> {
    let promised = entitled_ceil(units, acc)?;
    let left = (credited_total as u128).saturating_sub(promised);
    u64::try_from(left).map_err(|_| error!(FanexError::RaiseMathOverflow))
}

#[cfg(test)]
mod tests {
    use super::*;

    const Q: u64 = 1_000_000;

    #[test]
    fn two_holders_split_a_credit_pro_rata() {
        let acc = acc_increment(4_000_000, 4 * Q).unwrap();
        assert_eq!(entitled(3 * Q, acc).unwrap(), 3_000_000);
        assert_eq!(entitled(Q, acc).unwrap(), 1_000_000);
    }

    #[test]
    fn settling_twice_owes_nothing_the_second_time() {
        let acc = acc_increment(97_000_000, 1_000 * Q).unwrap();
        let (debt, pending) = settle(100 * Q, 0, 0, acc).unwrap();
        assert_eq!(pending, 9_700_000);
        assert_eq!(settle(100 * Q, debt, pending, acc).unwrap(), (debt, pending));
    }

    #[test]
    fn a_transfer_keeps_what_the_seller_accrued() {
        let mut acc = acc_increment(10_000_000, 10 * Q).unwrap();
        // Seller holds 4 quotas, settles, then gives 3 away.
        let (_, pending) = settle(4 * Q, 0, 0, acc).unwrap();
        assert_eq!(pending, 4_000_000);
        let debt_after = entitled_ceil(Q, acc).unwrap();
        // A new credit after the transfer pays only the quota still held.
        acc += acc_increment(10_000_000, 10 * Q).unwrap();
        let (_, pending_after) = settle(Q, debt_after, pending, acc).unwrap();
        assert_eq!(pending_after, 4_000_000 + 1_000_000);
    }

    #[test]
    fn a_debt_rounded_up_waits_for_the_next_credit() {
        // 5 base units over 3 quotas: 1,666,666 per unit, scaled.
        let acc = acc_increment(5, 3 * Q).unwrap();
        assert_eq!(acc, 1_666_666);
        // 2 quotas are entitled to 3.333332 base units: 3 rounded down, 4 up.
        assert_eq!(entitled(2 * Q, acc).unwrap(), 3);
        assert_eq!(entitled_ceil(2 * Q, acc).unwrap(), 4);
        // A holding that starts here owes nothing yet, and settling it does not underflow.
        assert_eq!(settle(2 * Q, 4, 0, acc).unwrap(), (4, 0));
        // After another 5: entitled to 6 (6.666664), so 2 more, never more than
        // the 3.33 an exact share of that credit would be.
        let acc = acc + acc_increment(5, 3 * Q).unwrap();
        assert_eq!(settle(2 * Q, 4, 0, acc).unwrap(), (6, 2));
    }

    #[test]
    fn floors_never_overpay() {
        // Deterministic pseudo-random holders and credits.
        let mut seed: u64 = 42;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            seed >> 33
        };
        let holders: Vec<u64> = (0..37).map(|_| 1 + next() % (50 * Q)).collect();
        let total: u64 = holders.iter().sum();
        let mut acc: u128 = 0;
        let mut credited: u128 = 0;
        for _ in 0..60 {
            let credit = 1 + next() % 5_000_000_000;
            acc += acc_increment(credit, total).unwrap();
            credited += credit as u128;
        }
        let owed: u128 = holders.iter().map(|u| entitled(*u, acc).unwrap()).sum();
        // What the close shares out on top must not be owed to anyone either.
        let remainder = rounding_remainder(u64::try_from(credited).unwrap(), total, acc).unwrap() as u128;
        assert!(owed + remainder <= credited, "owed {owed} + remainder {remainder} > credited {credited}");
    }

    #[test]
    fn the_remainder_is_what_the_floors_kept_back() {
        // 666,666,666,667 units: each 1-unit credit adds floor(1.4999999999) = 1 to
        // the accumulator, so six of them promise ceil(666,666,666,667 x 6 / 10^12)
        // = ceil(4.000000000002) = 5 of the 6 credited.
        let units = 666_666_666_667;
        let mut acc = 0u128;
        for _ in 0..6 {
            acc += acc_increment(1, units).unwrap();
        }
        assert_eq!(acc, 6);
        assert_eq!(rounding_remainder(6, units, acc).unwrap(), 1);
        // Nothing is left over when the credit divides evenly, and never less than nothing.
        let even = acc_increment(97_000_000, 1_000 * Q).unwrap();
        assert_eq!(rounding_remainder(97_000_000, 1_000 * Q, even).unwrap(), 0);
        assert_eq!(rounding_remainder(0, 1_000 * Q, even).unwrap(), 0);
    }

    #[test]
    fn nobody_to_credit_is_an_error() {
        assert!(acc_increment(1, 0).is_err());
    }
}
