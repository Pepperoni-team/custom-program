//! On-chain account state. ⚠️ A stored layout is a contract with every account
//! already on chain: new fields take bytes from `reserved`, never reorder.

use anchor_lang::prelude::*;

use crate::raise::accrual;
use crate::raise::checked::add;
use crate::errors::FanexError;
use crate::raise::pool_math::Pool;

/// Singleton config (PDA: [RAISE_CONFIG_SEED]).
#[account]
#[derive(InitSpace)]
pub struct RaiseConfig {
    /// May update the config and pause. A multisig before mainnet (spec §6.1). Named by the program's
    /// upgrade authority at `initialize_config`, then moved only in two steps (`PendingAdmin`).
    pub admin: Pubkey,
    /// Fanex's server key: co-signs launches, purchases, pool trades and declarations.
    pub attester: Pubkey,
    /// Owner of the USDC account that receives Fanex's fees.
    pub fee_wallet: Pubkey,
    /// The USDC mint. Not updatable: every vault is bound to it.
    pub usdc_mint: Pubkey,
    /// Paid by the creator at launch, USDC base units (USD 5 at launch).
    pub launch_fee: u64,
    /// Fee on every pool trade, kept in the pool for the fans (amended spec D16):
    /// one value for every raise, read at trade time, at most POOL_FEE_MAX_BPS.
    pub pool_fee_bps: u16,
    /// Stops create, buy, declare, the attester's re-declaration and the two
    /// pool trades. Never stops a refund, a payment, a settlement, a claim, a
    /// sweep, a close or the admin's own corrections. Written by the admin
    /// (`set_raise_paused`), and set to `true` by `UsdcConfig.pauser` too
    /// (`usdc_pause_raise`, 2026-11-05), which never unpauses.
    pub paused: bool,
    pub bump: u8,
    /// Room to grow without a layout break (N+3, Rick 2026-11-04): the config is born under
    /// pepper_curve at its own seed, so the room costs nothing to add now and an upgrade later.
    pub reserved: [u8; 128],
}

/// Lifecycle (spec §4.5).
#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Copy, PartialEq, Eq, Debug)]
pub enum RaiseState {
    Raising,
    Refunding,
    Active,
    Closed,
}

/// One price tier: `units` offered at `price` per whole quota, `sold` so far.
#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Tier {
    pub units: u64,
    pub price: u64,
    pub sold: u64,
}

/// One raise (PDA: [RAISE_SEED, creator, raise_id u64 LE]).
#[account]
#[derive(InitSpace)]
pub struct Raise {
    pub creator: Pubkey,
    pub raise_id: u64,
    pub state: RaiseState,

    // ── Parameters, fixed at create ──
    pub share_bps: u16,
    pub term_months: u16,
    pub deadline: i64,
    pub tier_count: u8,
    pub tiers: [Tier; 3],
    /// Quota units at a full raise.
    pub max_units: u64,
    pub min_creator_part: u64,
    /// What fans pay at a full raise (the 110).
    pub fan_max: u64,

    // ── Raising ──
    pub sold_units: u64,
    pub fan_paid: u64,
    pub refunded: u64,

    // ── Active, set at finalize ──
    pub creator_part: u64,
    /// Units that receive payouts: every sold unit (the pool's included) while
    /// active; the fans' only, once the close cancels the pool's.
    pub units_outstanding: u64,
    /// Everything monthly payments and the guarantee credited to the fans'
    /// accumulator; the close measures the rounding remainder against it.
    pub credited_total: u64,
    /// USDC owed per quota unit since the start, scaled by `accrual::ACC_SCALE`.
    pub acc_per_unit: u128,
    pub first_month: u32,
    pub term_end_month: u32,
    pub last_declared_month: u32,
    pub last_finalized_month: u32,
    /// Oldest declared month not fully paid or explicitly written off; `last_declared_month + 1` when none.
    pub oldest_open_month: u32,
    /// The guarantee's USDC as every rule sees it: a counter, not the vault
    /// balance, so USDC sent straight to the vault changes no rule (ruling P3).
    pub guarantee_usdc: u64,
    /// Drawn from the guarantee and not yet rebuilt by the creator.
    pub guarantee_owed: u64,
    pub on_time_streak: u8,
    pub guarantee_released: bool,

    // ── The exit pool (amended spec §3.1), set at finalize ──
    /// The pool's USDC as the price sees it: a counter, not the vault balance.
    pub pool_usdc: u64,
    /// Quota units the pool holds (its own `Position`, owned by this PDA).
    pub pool_units: u64,
    /// The virtual quota reserve that sets the starting price; never changes.
    pub virtual_units: u64,

    pub finalized_at: i64,
    pub closed_at: i64,

    // ── Bumps ──
    pub bump: u8,
    pub escrow_bump: u8,
    pub guarantee_bump: u8,
    pub pool_bump: u8,
    pub payouts_bump: u8,

    // ── The rent (2026-10-21): three fields taken from `reserved`, which was 128 ──
    /// The wallet that paid `create_raise`'s rent, where every account's rent of
    /// this raise goes back. `Pubkey::default()` on a raise written before
    /// 2026-10-21: its rent stays where it is (`tracks_rent`).
    pub rent_payer: Pubkey,
    /// `Position` accounts of this raise that exist, the pool's own included.
    pub open_positions: u32,
    /// `Month` accounts of this raise that exist.
    pub live_months: u16,

    /// Room to grow without a layout break.
    /// Term-month bitmap of explicit write-offs, including zero-due pointer advances (M-01).
    pub written_off_months: u64,
    pub reserved: [u8; 82],
}

impl Raise {
    /// The exit pool as `pool_math` prices it.
    pub fn pool(&self) -> Pool {
        Pool { usdc: self.pool_usdc, units: self.pool_units, virtual_units: self.virtual_units }
    }

    /// Whether this raise records who paid its rent. One written before
    /// 2026-10-21 carries the default key and counters that never counted: its
    /// counters are left alone, and neither rent instruction accepts it.
    pub fn tracks_rent(&self) -> bool {
        self.rent_payer != Pubkey::default()
    }

    /// A `Position` of this raise was created (not found by `init_if_needed`).
    pub fn count_position_opened(&mut self) -> Result<()> {
        if self.tracks_rent() {
            self.open_positions = self.open_positions.checked_add(1).ok_or_else(|| error!(FanexError::RaiseMathOverflow))?;
        }
        Ok(())
    }

    /// A `Position` of this raise was closed (`close_position`, `refund`).
    pub fn count_position_closed(&mut self) -> Result<()> {
        if self.tracks_rent() {
            self.open_positions = self.open_positions.checked_sub(1).ok_or_else(|| error!(FanexError::RaiseMathOverflow))?;
        }
        Ok(())
    }

    /// A `Month` of this raise was declared.
    pub fn count_month_opened(&mut self) -> Result<()> {
        if self.tracks_rent() {
            self.live_months = self.live_months.checked_add(1).ok_or_else(|| error!(FanexError::RaiseMathOverflow))?;
        }
        Ok(())
    }

    /// A `Month` of this raise was closed (`close_month`).
    pub fn count_month_closed(&mut self) -> Result<()> {
        if self.tracks_rent() {
            self.live_months = self.live_months.checked_sub(1).ok_or_else(|| error!(FanexError::RaiseMathOverflow))?;
        }
        Ok(())
    }
}

/// A holding in a raise (PDA: [POSITION_SEED, raise, owner]). The pool's
/// position is owned by the raise PDA itself.
#[account]
#[derive(InitSpace, Default)]
pub struct Position {
    pub raise: Pubkey,
    pub owner: Pubkey,
    pub units: u64,
    pub paid: u64,
    /// Where `units` accrue from: `accrual::entitled_ceil` when the holding
    /// changes (`set_units`), then advanced to `accrual::entitled` by every
    /// settlement that finds something owed (`accrual::settle`).
    pub debt: u128,
    pub pending: u64,
    pub claimed: u64,
    /// Gets the rent back when the position closes.
    pub rent_payer: Pubkey,
    pub bump: u8,
    /// Units of `units` bought from the exit pool and not sold back since (N+3's audit, Rick
    /// 2026-11-04): a sale sells them first, at the curve's price alone, and only the rest, bought in
    /// the raise, at the seller's floor (`pool_math::sell_position`). Taken from `reserved`, 32 then.
    pub from_pool: u64,
    pub reserved: [u8; 24],
}

impl Position {
    /// Fills an account just created by `init_if_needed`; no-op on an existing
    /// one. True when it filled it: the caller then counts a new position.
    pub fn init_if_new(&mut self, raise: Pubkey, owner: Pubkey, rent_payer: Pubkey, bump: u8) -> bool {
        if self.owner != Pubkey::default() {
            return false;
        }
        self.raise = raise;
        self.owner = owner;
        self.rent_payer = rent_payer;
        self.bump = bump;
        true
    }

    pub fn settle(&mut self, acc: u128) -> Result<()> {
        let (debt, pending) = accrual::settle(self.units, self.debt, self.pending, acc)?;
        self.debt = debt;
        self.pending = pending;
        Ok(())
    }

    /// Settles first, so a change of holding never moves what was already
    /// accrued; the new debt rounds up, so the new holding never collects a
    /// rounding unit it was not paid (`accrual::entitled_ceil`).
    pub fn set_units(&mut self, acc: u128, units: u64) -> Result<()> {
        self.settle(acc)?;
        self.units = units;
        self.debt = accrual::entitled_ceil(units, acc)?;
        Ok(())
    }

    /// Settles and hands over everything pending: the caller moves that much USDC.
    pub fn take_pending(&mut self, acc: u128) -> Result<u64> {
        self.settle(acc)?;
        let amount = self.pending;
        self.pending = 0;
        self.claimed = add(self.claimed, amount)?;
        Ok(amount)
    }
}

/// One declared month (PDA: [MONTH_SEED, raise, month u32 LE]).
#[account]
#[derive(InitSpace)]
pub struct Month {
    pub raise: Pubkey,
    pub month: u32,
    pub due: u64,
    /// The due is an estimate (YouTube disconnected, video deleted or private).
    pub estimated: bool,
    pub paid: u64,
    /// Paid by the creator before `due_ts`.
    pub paid_on_time: u64,
    pub covered: u64,
    /// Reached the fans' accumulator (after the 3%).
    pub credited: u64,
    pub due_ts: i64,
    pub finalized: bool,
    pub bump: u8,
    /// Room to grow without a layout break (internal audit of 2026-10-29): added before any `Month`
    /// existed on a cluster (devnet held none on that day, mainnet no program), so no account breaks.
    pub reserved: [u8; 32],
}

/// An admin hand-over in progress (PDA: [PENDING_ADMIN_SEED]; the internal audit of 2026-10-29):
/// `propose_admin` writes it, the proposed key's own `accept_admin` makes it the admin and closes it.
/// Its own account, not fields of `RaiseConfig`: the separate program's devnet config already held its 140 bytes.
#[account]
#[derive(InitSpace)]
pub struct PendingAdmin {
    /// The admin proposed: an account the system program owns, never yet the admin.
    pub admin: Pubkey,
    /// Paid this account's rent; gets it back when the proposal is accepted.
    pub payer: Pubkey,
    pub bump: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A change here is a layout break for every account already on chain:
    /// take the bytes from `reserved` instead. Counted by hand, field by field
    /// (Pubkey 32, u128 16, u64/i64 8, u32 4, u16 2, u8/bool/enum 1, [T; n] n x T):
    /// RaiseConfig 4x32 + 8 + 2 + 1 + 1 = 140, then `reserved` 128 (N+3); Raise 425 (297 before the rent fields, then 32 + 4 + 2 + 8 + 82); Position 177;
    /// Month 119 (87, then `reserved` 32, 2026-10-29); PendingAdmin 32 + 32 + 1 = 65.
    #[test]
    fn layouts_are_pinned() {
        assert_eq!(RaiseConfig::INIT_SPACE, 268);
        assert_eq!(Raise::INIT_SPACE, 425);
        assert_eq!(Position::INIT_SPACE, 177);
        assert_eq!(Month::INIT_SPACE, 119);
        assert_eq!(PendingAdmin::INIT_SPACE, 65);
    }

    /// `reserved` was appended after `bump`: every field before it keeps its offset (87 bytes, then 32).
    #[test]
    fn the_months_reserved_bytes_follow_its_87() {
        let month = Month {
            raise: Pubkey::new_unique(),
            month: 24_322,
            due: 7,
            estimated: true,
            paid: 1,
            paid_on_time: 1,
            covered: 0,
            credited: 1,
            due_ts: 1_798_502_400,
            finalized: false,
            bump: 254,
            reserved: [0; 32],
        };
        let mut bytes = Vec::new();
        month.serialize(&mut bytes).unwrap();
        assert_eq!(bytes.len(), 119);
        assert_eq!(bytes[86], 254, "the bump is still the 87th byte");
        assert_eq!(&bytes[87..], &[0u8; 32]);
    }

    #[test]
    fn set_units_keeps_the_accrued_amount() {
        let mut p = Position { units: 4_000_000, ..Position::default() };
        let acc = crate::raise::accrual::acc_increment(10_000_000, 10_000_000).unwrap();
        p.set_units(acc, 1_000_000).unwrap();
        assert_eq!(p.pending, 4_000_000);
        p.settle(acc).unwrap();
        assert_eq!(p.pending, 4_000_000);
    }

    #[test]
    fn take_pending_hands_over_everything_once() {
        let mut p = Position { units: 4_000_000, ..Position::default() };
        let acc = crate::raise::accrual::acc_increment(10_000_000, 10_000_000).unwrap();
        assert_eq!(p.take_pending(acc).unwrap(), 4_000_000);
        assert_eq!((p.pending, p.claimed), (0, 4_000_000));
        assert_eq!(p.take_pending(acc).unwrap(), 0);
    }

    /// An all-zero `Raise`, as the program read one written before 2026-10-21.
    fn zero_raise() -> Raise {
        AnchorDeserialize::try_from_slice(&[0u8; Raise::INIT_SPACE]).unwrap()
    }

    /// The three rent fields took their bytes from `reserved` (128 until 2026-10-21), so the size
    /// above and every account already on chain stayed as they were.
    #[test]
    fn the_rent_fields_took_their_bytes_from_reserved() {
        assert_eq!(32 + 4 + 2 + 8 + zero_raise().reserved.len(), 128);
    }

    #[test]
    fn init_if_new_says_whether_it_initialised() {
        let mut p = Position::default();
        let (raise, owner, payer) = (Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique());
        assert!(p.init_if_new(raise, owner, payer, 7));
        assert_eq!((p.raise, p.owner, p.rent_payer, p.bump), (raise, owner, payer, 7));
        // Found, not created: nothing changes, and the caller counts nothing.
        assert!(!p.init_if_new(raise, Pubkey::new_unique(), Pubkey::new_unique(), 1));
        assert_eq!((p.owner, p.rent_payer, p.bump), (owner, payer, 7));
    }

    #[test]
    fn the_counters_move_only_on_a_raise_that_records_its_rent_payer() {
        let mut old = zero_raise();
        assert!(!old.tracks_rent());
        old.count_position_opened().unwrap();
        old.count_month_opened().unwrap();
        // A close on a raise from before the upgrade never underflows: the counters are left alone.
        old.count_position_closed().unwrap();
        old.count_month_closed().unwrap();
        assert_eq!((old.open_positions, old.live_months), (0, 0));

        let mut raise = zero_raise();
        raise.rent_payer = Pubkey::new_unique();
        assert!(raise.tracks_rent());
        raise.count_position_opened().unwrap();
        raise.count_position_opened().unwrap();
        raise.count_month_opened().unwrap();
        assert_eq!((raise.open_positions, raise.live_months), (2, 1));
        raise.count_position_closed().unwrap();
        raise.count_month_closed().unwrap();
        assert_eq!((raise.open_positions, raise.live_months), (1, 0));
        // One close too many is a fault, never a wrap.
        assert!(raise.count_month_closed().is_err());
    }

    /// Review Focus 2: twelve holders and the pool pass units around between
    /// credits, as pool trades do. Whatever the order, everything they are owed
    /// plus the remainder the close shares out never exceeds what was credited.
    /// (With the new debt rounded down instead of up, 13 of these 64 seeds
    /// overpay, by up to 17 base units.)
    #[test]
    fn transfers_never_overpay() {
        use crate::raise::accrual::{acc_increment, rounding_remainder};
        const Q: u64 = 1_000_000;
        for start in 1..=64u64 {
            let mut seed = start;
            let mut next = || {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                seed >> 33
            };
            let mut holders: Vec<Position> =
                (0..12).map(|_| Position { units: 1 + next() % (50 * Q), ..Position::default() }).collect();
            let total: u64 = holders.iter().map(|p| p.units).sum();
            holders.push(Position::default()); // the pool
            let (mut acc, mut credited) = (0u128, 0u64);
            for _ in 0..80 {
                let credit = 1 + next() % 5_000_000_000;
                acc += acc_increment(credit, total).unwrap();
                credited += credit;
                for _ in 0..6 {
                    let from = (next() % 13) as usize;
                    let to = (next() % 13) as usize;
                    if from == to || holders[from].units == 0 {
                        continue;
                    }
                    let moved = 1 + next() % holders[from].units;
                    let left = holders[from].units - moved;
                    holders[from].set_units(acc, left).unwrap();
                    let held = holders[to].units + moved;
                    holders[to].set_units(acc, held).unwrap();
                }
            }
            let mut owed = rounding_remainder(credited, total, acc).unwrap() as u128;
            for p in holders.iter_mut() {
                p.settle(acc).unwrap();
                owed += p.pending as u128;
            }
            assert!(owed <= credited as u128, "seed {start}: owed {owed} > credited {credited}");
        }
    }
}
