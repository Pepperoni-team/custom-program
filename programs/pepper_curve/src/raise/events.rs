//! Events, one per state change an indexer has to see.

use anchor_lang::prelude::*;

#[event]
pub struct RaiseCreated {
    pub raise: Pubkey,
    pub creator: Pubkey,
    pub raise_id: u64,
    pub fan_max: u64,
    pub creator_max: u64,
    pub deadline: i64,
}

#[event]
pub struct RaiseCancelled {
    pub raise: Pubkey,
}

#[event]
pub struct QuotasBought {
    pub raise: Pubkey,
    pub buyer: Pubkey,
    pub units: u64,
    pub cost: u64,
}

/// On success `pool_usdc` and `virtual_units` open the exit pool; both are zero
/// when the raise turns to refunding.
#[event]
pub struct RaiseFinalized {
    pub raise: Pubkey,
    pub succeeded: bool,
    pub fan_paid: u64,
    pub creator_part: u64,
    pub pool_usdc: u64,
    pub virtual_units: u64,
}

#[event]
pub struct Refunded {
    pub raise: Pubkey,
    pub owner: Pubkey,
    pub amount: u64,
}

#[event]
pub struct MonthDeclared {
    pub raise: Pubkey,
    pub month: u32,
    pub due: u64,
    pub estimated: bool,
}

/// `month == u32::MAX` marks a `fund_direct` payment.
#[event]
pub struct MonthFunded {
    pub raise: Pubkey,
    pub month: u32,
    pub gross: u64,
    pub fee: u64,
    pub credit: u64,
}

#[event]
pub struct MonthSettled {
    pub raise: Pubkey,
    pub month: u32,
    pub covered: u64,
    pub on_time: bool,
}

#[event]
pub struct GuaranteeReplenished {
    pub raise: Pubkey,
    pub amount: u64,
}

#[event]
pub struct GuaranteeReleased {
    pub raise: Pubkey,
    pub amount: u64,
}

#[event]
pub struct Claimed {
    pub raise: Pubkey,
    pub owner: Pubkey,
    pub amount: u64,
}

/// The pool's own payouts moved into the pool; `pool_usdc` is the counter after.
#[event]
pub struct PoolEarningsSwept {
    pub raise: Pubkey,
    pub amount: u64,
    pub pool_usdc: u64,
}

/// `pool_usdc` and `pool_units` are the pool after the trade: with
/// `Raise.virtual_units` they give the new price.
#[event]
pub struct SoldToPool {
    pub raise: Pubkey,
    pub seller: Pubkey,
    pub units: u64,
    pub gross: u64,
    pub fee: u64,
    pub net: u64,
    pub pool_usdc: u64,
    pub pool_units: u64,
}

#[event]
pub struct BoughtFromPool {
    pub raise: Pubkey,
    pub buyer: Pubkey,
    pub units: u64,
    pub gross: u64,
    pub fee: u64,
    pub cost: u64,
    pub pool_usdc: u64,
    pub pool_units: u64,
}

/// `distributed` went to the quotas fans still hold (the pool's USDC and the
/// rounding remainder); `to_creator` is the same money when the pool held
/// every quota.
#[event]
pub struct RaiseClosed {
    pub raise: Pubkey,
    pub distributed: u64,
    pub to_creator: u64,
    pub guarantee_returned: u64,
}

/// A finished month's account closed; `lamports` went back to the raise's rent payer.
#[event]
pub struct MonthClosed {
    pub raise: Pubkey,
    pub month: u32,
    pub lamports: u64,
}

/// Extinguished debt, never a payment. The full financial snapshot survives the month's later close.
#[event]
pub struct MonthWrittenOff {
    pub raise: Pubkey,
    pub month: u32,
    pub due: u64,
    pub paid: u64,
    pub paid_on_time: u64,
    pub covered: u64,
    pub written_off: u64,
}

/// The `Raise` and its four vaults closed. `leftover_usdc` was in the vaults (rounding dust, USDC sent
/// to one straight) and went to the fee wallet; `lamports` is every account's rent, back to the
/// raise's rent payer.
#[event]
pub struct RaiseAccountsClosed {
    pub raise: Pubkey,
    pub leftover_usdc: u64,
    pub lamports: u64,
}
