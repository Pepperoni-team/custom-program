//! Seeds and every product number of the raise. Each value is a decision
//! recorded in docs/superpowers/specs/2026-10-01-revenue-raise-design.md as
//! amended by docs/superpowers/specs/2026-10-02-revenue-raise-term-and-pool-design.md;
//! change one only together with the canonical doc
//! (docs/solana/PEPPER-RAISE-PROGRAM.md).

// ── PDA seeds. ⚠️ Byte-identical to `SEED.x` in lib/solana/pepper-raise/program.ts ──
pub const RAISE_CONFIG_SEED: &[u8] = b"raise_config";
pub const RAISE_SEED: &[u8] = b"raise";
pub const POSITION_SEED: &[u8] = b"position";
pub const MONTH_SEED: &[u8] = b"month";
pub const ESCROW_SEED: &[u8] = b"escrow";
pub const GUARANTEE_SEED: &[u8] = b"guarantee";
pub const POOL_SEED: &[u8] = b"pool";
pub const PAYOUTS_SEED: &[u8] = b"payouts";
/// The admin hand-over in progress (`PendingAdmin`, internal audit of 2026-10-29).
pub const PENDING_ADMIN_SEED: &[u8] = b"pending_admin";

// ── Units ──
/// USDC decimals; every transfer is `transfer_checked` with this value.
pub const USDC_DECIMALS: u8 = 6;
/// Units in one quota. Fans buy fractions of a quota.
pub const QUOTA_UNITS: u64 = 1_000_000;
/// The first tier always costs USD 1.00 per quota (spec §4.4).
pub const FIRST_TIER_PRICE: u64 = 1_000_000;
pub const BPS: u64 = 10_000;

// ── The split at close (spec §4.2): fans pay 110 for every 100 of the creator's part ──
pub const FAN_PAYS_NUM: u64 = 11;
pub const CREATOR_PART_NUM: u64 = 10;
pub const FANEX_RAISE_FEE_BPS: u64 = 500;
pub const GUARANTEE_BPS: u64 = 500;

// ── Monthly payouts (spec §4.6) ──
pub const PAYOUT_FEE_BPS: u64 = 300;
/// Smallest manual claim, USD 0.10 (any amount once the raise is closed).
pub const MIN_CLAIM: u64 = 100_000;

// ── Parameters (spec §4.3) ──
/// 1%, lowered from 25% in N+3 before it reached mainnet (2026-11-05): the program only stores the share
/// (the attester declares each month's due), so each kind of raise takes its floor from the server, 25%
/// for videos (`VIDEO_SHARE_BPS_MIN`), and a channel's or a business's smaller one needs no upgrade.
pub const SHARE_BPS_MIN: u16 = 100;
pub const SHARE_BPS_MAX: u16 = 10_000;
pub const TERM_MONTHS_MIN: u16 = 24;
pub const TERM_MONTHS_MAX: u16 = 60;
pub const RAISE_DURATION_MIN_SECONDS: i64 = 21 * 86_400;
pub const RAISE_DURATION_MAX_SECONDS: i64 = 56 * 86_400;
pub const MIN_PART_OF_MAX_BPS: u64 = 2_500;
pub const MAX_TIERS: usize = 3;
pub const TIER_MIN_SHARE_BPS: u64 = 2_000;
pub const TIER_STEP_MIN_BPS: u64 = 500;
pub const TIER_STEP_MAX_BPS: u64 = 1_500;
pub const TIER_SPREAD_MAX_BPS: u64 = 2_500;
pub const PER_WALLET_MAX_BPS: u64 = 1_000;
/// Smallest purchase, USD 2 (the *Saldo in USDC* rule), except the one that sells out.
pub const MIN_PURCHASE: u64 = 2_000_000;

// ── Guarantee (spec §4.7) and exit pool (amended spec §3, D16) ──
pub const GUARANTEE_RELEASE_STREAK: u8 = 12;
/// Ceiling of `RaiseConfig.pool_fee_bps`: the program refuses a pool fee above 3%.
pub const POOL_FEE_MAX_BPS: u16 = 300;

// ── The launch fee (spec D8: USD 5) ──
/// Ceiling of `RaiseConfig.launch_fee`, USD 50: ten times the spec's USD 5, so the fee can be revised
/// without an upgrade while a slip or a hostile admin cannot take a creator's whole balance (internal
/// audit of 2026-10-29). Refused above it on both config paths.
pub const LAUNCH_FEE_MAX: u64 = 50_000_000;

// ── Calendar (spec §4.6, D11) ──
pub const DECLARE_DAY_OF_NEXT_MONTH: i64 = 15;
pub const DUE_DAY_OF_SECOND_MONTH: i64 = 6;
/// A raise becomes closable this many days after its last month's due date even
/// if some months were never declared, so the pool and the guarantee never wait
/// on the attester (`ledger::close_fallback_ts`, controller ruling P1).
pub const CLOSE_FALLBACK_DAYS: i64 = 90;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_are_pinned() {
        assert_eq!(RAISE_CONFIG_SEED, b"raise_config");
        assert_eq!(RAISE_SEED, b"raise");
        assert_eq!(POSITION_SEED, b"position");
        assert_eq!(MONTH_SEED, b"month");
        assert_eq!(ESCROW_SEED, b"escrow");
        assert_eq!(GUARANTEE_SEED, b"guarantee");
        assert_eq!(POOL_SEED, b"pool");
        assert_eq!(PAYOUTS_SEED, b"payouts");
        assert_eq!(PENDING_ADMIN_SEED, b"pending_admin");
    }

    #[test]
    fn the_launch_fee_ceiling_is_ten_times_the_spec_fee() {
        assert_eq!(LAUNCH_FEE_MAX, 10 * 5 * FIRST_TIER_PRICE);
    }

    #[test]
    fn the_split_of_the_creators_part_is_90_5_5() {
        assert_eq!(BPS - FANEX_RAISE_FEE_BPS - GUARANTEE_BPS, 9_000);
        assert_eq!(FAN_PAYS_NUM - CREATOR_PART_NUM, 1);
    }

    #[test]
    fn the_tier_arrays_have_three_slots() {
        // `RaiseParams.tiers` and `Raise.tiers` are written as `[_; 3]` so the IDL
        // carries a literal length; this pins that literal to MAX_TIERS.
        assert_eq!(MAX_TIERS, 3);
    }
}
