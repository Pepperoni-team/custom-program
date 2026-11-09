//! Checked arithmetic: every overflow is a `MathOverflow`, never a panic or a wrap.

use anchor_lang::prelude::*;

use crate::errors::FanexError;

pub fn add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b).ok_or_else(|| error!(FanexError::RaiseMathOverflow))
}

pub fn sub(a: u64, b: u64) -> Result<u64> {
    a.checked_sub(b).ok_or_else(|| error!(FanexError::RaiseMathOverflow))
}

/// floor(a * b / d) in u128.
pub fn mul_div(a: u64, b: u64, d: u64) -> Result<u64> {
    require!(d > 0, FanexError::RaiseMathOverflow);
    let product = (a as u128)
        .checked_mul(b as u128)
        .ok_or_else(|| error!(FanexError::RaiseMathOverflow))?;
    u64::try_from(product / d as u128).map_err(|_| error!(FanexError::RaiseMathOverflow))
}

/// ceil(a * b / d) in u128.
pub fn mul_div_ceil(a: u64, b: u64, d: u64) -> Result<u64> {
    require!(d > 0, FanexError::RaiseMathOverflow);
    let product = (a as u128)
        .checked_mul(b as u128)
        .ok_or_else(|| error!(FanexError::RaiseMathOverflow))?;
    let rounded_up = product
        .checked_add(d as u128 - 1)
        .ok_or_else(|| error!(FanexError::RaiseMathOverflow))?
        / d as u128;
    u64::try_from(rounded_up).map_err(|_| error!(FanexError::RaiseMathOverflow))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_sub_refuse_to_wrap() {
        assert_eq!(add(2, 3).unwrap(), 5);
        assert!(add(u64::MAX, 1).is_err());
        assert_eq!(sub(5, 3).unwrap(), 2);
        assert!(sub(3, 5).is_err());
    }

    #[test]
    fn mul_div_floors_and_mul_div_ceil_rounds_up() {
        assert_eq!(mul_div(10, 10, 11).unwrap(), 9);
        assert_eq!(mul_div_ceil(10, 10, 11).unwrap(), 10);
        assert_eq!(mul_div_ceil(11, 10, 11).unwrap(), 10);
        // u128 intermediate: no overflow on the product.
        assert_eq!(mul_div(u64::MAX, 2, 2).unwrap(), u64::MAX);
    }

    #[test]
    fn mul_div_refuses_a_zero_divisor_and_a_result_past_u64() {
        assert!(mul_div(1, 1, 0).is_err());
        assert!(mul_div(u64::MAX, 2, 1).is_err());
    }
}
