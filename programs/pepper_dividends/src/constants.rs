//! Seeds and every number of the dividends program. Each value is a decision of
//! docs/superpowers/specs/2026-10-21-usdc-dividends-program-design.md or of Rick's
//! of 2026-10-23; change one only together with the canonical doc
//! (docs/solana/PEPPER-DIVIDENDS-PROGRAM.md §6).
//!
//! ⚠️ The TypeScript parity tests read this file's text: a seed stays a literal
//! `b"…"` line, a number a literal or a product of literals (`30 * 86_400`), each
//! on one line.

// ── PDA seeds ──────────────────────────────────────────────────────────────

/// `DividendsConfig`, `["config"]` (spec §3).
pub const CONFIG_SEED: &[u8] = b"config";

/// A month's distributor, `["distributor", mint, month as i64 LE]`: this program's
/// `UsdcDistributor`, and, under `config.curve_program`, the address of
/// `pepper_curve`'s v1 or v2 distributor for the same token and month, read as
/// `legacy_distributor` (spec §4.4). Byte-identical to `pepper_curve`'s
/// `DISTRIBUTOR_SEED`, held by a test below.
pub const DISTRIBUTOR_SEED: &[u8] = b"distributor";

/// A month's record, `["month", mint, month as i64 LE]`: created with the month
/// and never closed (spec §3).
pub const MONTH_SEED: &[u8] = b"month";

/// `pepper_curve`'s curve of a mint, `["curve", mint]` under
/// `config.curve_program`, read by hand for a creator funder (spec §4.4). No
/// account of this program has this seed. Byte-identical to `pepper_curve`'s
/// `CURVE_SEED`, held by a test below.
pub const CURVE_SEED: &[u8] = b"curve";

// ── The leaf (spec §5) ─────────────────────────────────────────────────────

/// Opens every leaf: `keccak256(DIVIDEND_LEAF_DOMAIN ‖ distributor ‖ usdc_mint ‖
/// index u32 LE ‖ claimant ‖ amount u64 LE)`. A tag of its own, apart from
/// `pepper_curve`'s `fanex:dividend:v2`, so no SOL month's leaf proves anything here.
pub const DIVIDEND_LEAF_DOMAIN: &[u8] = b"fanex:dividend:usdc:v1";

// ── A month ────────────────────────────────────────────────────────────────

/// The most leaves a month records: one bit each, an 8 KiB claim registry, which
/// with the distributor's 274-byte header stays inside the 10 KiB an account
/// created by a program may take (spec §3). A larger count is refused
/// (`InvalidLeafCount`), never allocated.
pub const MAX_DISTRIBUTOR_LEAVES: u32 = 65_536;

/// The soonest a month may expire, from its funding: 364 days, the 365 Fanex
/// stamps (`DISTRIBUTOR_EXPIRY_SECONDS`) less the day of slack a creator's
/// confirm allows, so no month opens with a window its holders cannot use
/// (`ExpiryTooSoon`, spec §4.4). A month lives one year (Rick, 2026-10-23).
pub const MIN_DISTRIBUTOR_EXPIRY_SECONDS: i64 = 364 * 86_400;

/// The furthest a month may expire, from its funding: 400 days, as `pepper_curve`'s
/// v2 (`ExpiryTooFar`, spec §4.4).
pub const MAX_DISTRIBUTOR_EXPIRY_SECONDS: i64 = 400 * 86_400;

/// How long a month's claims stay held after its funding: 24 hours, the funding's
/// `claims_held_until` (the internal audit of 2026-10-29). In it no claim lands, so the
/// void, the only fix for a wrong root or total, never races one: the monitor checks
/// the funding against the ledger, and the pauser holds the month on (`hold_claims`)
/// while the vault's void waits its 48 hours. Rick's to change.
pub const REVIEW_WINDOW_SECONDS: i64 = 24 * 3_600;

// ── The claims pause (spec §4.3, §4.7) ─────────────────────────────────────

/// The furthest a claims pause may end, from now: 7 days, again at each call
/// (`PauseTooLong`).
pub const MAX_CLAIMS_PAUSE_SECONDS: i64 = 7 * 86_400;

/// The soonest a claims pause or a month's hold may end, from now: an hour
/// (`PauseTooShort`, the internal audit of 2026-10-29), so only a pause that stops
/// claims for real delays an expired month's close. Rick's to change.
pub const MIN_CLAIMS_PAUSE_SECONDS: i64 = 3_600;

/// How long claims must have been open before an expired month may close:
/// `now ≥ claims_paused_until + 30 days` (`ClaimsPausedRecently`), so a pause held
/// to the expiry cannot let the funder take back what holders could not claim. A
/// guard of its own, apart from the month's one-year life: Rick's to change
/// (2026-10-23).
pub const CLAIMS_OPEN_BEFORE_CLOSE_SECONDS: i64 = 30 * 86_400;

/// The longest pauses and holds may keep an expired month from closing: 90 days
/// after its expiry, whatever pause or hold ran since (the internal audit of
/// 2026-10-29), so a pauser renewing pauses cannot keep a month open for ever. Rick's
/// to change.
pub const MAX_CLOSE_DELAY_SECONDS: i64 = 90 * 86_400;

// ── Units and layout ───────────────────────────────────────────────────────

/// USDC's decimals: every transfer is `transfer_checked` with this value.
pub const USDC_DECIMALS: u8 = 6;

/// The `version` byte every account of this program opens with (spec §3).
pub const ACCOUNT_VERSION: u8 = 1;

// ── `MonthRecord.status` (spec §3) ─────────────────────────────────────────
// 0 is only ever a record the funding has just created; none stays on chain at 0.

/// A month funded and not closed.
pub const MONTH_STATUS_OPEN: u8 = 1;

/// A month closed: its distributor and vault are gone, its record stays, and the
/// month never opens again.
pub const MONTH_STATUS_CLOSED: u8 = 2;

/// A month voided by the admin before any claim (`void_distributor`, Rick,
/// 2026-10-23): the funding accepts that month again.
pub const MONTH_STATUS_VOIDED: u8 = 3;

// ── `pepper_curve`'s curves, read by hand (spec §4.4) ──────────────────────
// This crate does not depend on `pepper_curve`'s: the LiteSVM suite, which does,
// holds these bytes against the real program.

/// `Curve`'s discriminator: the first 8 bytes of sha256("account:Curve").
pub const CURVE_DISCRIMINATOR: [u8; 8] = [191, 180, 249, 66, 180, 71, 51, 182];

/// A SOL curve's account: 8 + 188 bytes.
pub const CURVE_LEN: usize = 196;

/// Where `Curve.creator` starts: after the discriminator and the mint.
pub const CURVE_CREATOR_OFFSET: usize = 40;

/// `CurveUsdc`'s discriminator: the first 8 bytes of sha256("account:CurveUsdc").
pub const CURVE_USDC_DISCRIMINATOR: [u8; 8] = [208, 28, 161, 208, 98, 148, 66, 109];

/// A USDC curve's account, born so or migrated: 8 + 346 bytes.
pub const CURVE_USDC_LEN: usize = 354;

/// Where `CurveUsdc.creator` starts: after the discriminator, six one-byte fields
/// and the mint.
pub const CURVE_USDC_CREATOR_OFFSET: usize = 46;

/// The `CurveUsdc.version` the creator read takes, its first field (byte 8):
/// `pepper_curve`'s `USDC_ACCOUNT_VERSION`. Another is `CurveUnreadable`, so a later
/// layout that keeps 354 bytes and moves the creator is never read as this one.
pub const CURVE_USDC_VERSION: u8 = 1;

// ── `pepper_curve`'s v1 and v2 months, read by hand (spec §4.4) ────────────
// Read only for their expiry: a month there blocks the same month here until it
// expires (`legacy_month_live`).

/// `Distributor`'s discriminator: the first 8 bytes of sha256("account:Distributor").
pub const LEGACY_DISTRIBUTOR_DISCRIMINATOR: [u8; 8] = [90, 90, 217, 147, 6, 32, 135, 4];

/// `DistributorV2`'s discriminator: the first 8 bytes of sha256("account:DistributorV2").
pub const LEGACY_DISTRIBUTOR_V2_DISCRIMINATOR: [u8; 8] = [89, 141, 51, 26, 17, 255, 214, 55];

/// Where `expires_at` sits in both: v2 keeps v1's fields first, at v1's offsets
/// (the discriminator, mint, month, root, total, claimed and funder before it).
pub const LEGACY_EXPIRES_AT_OFFSET: usize = 128;

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::solana_program::hash::hash;

    const DAY: i64 = 86_400;

    #[test]
    fn seeds_are_pinned() {
        // A changed byte moves every address: the config, each month's distributor,
        // its vault and its record.
        assert_eq!(CONFIG_SEED, b"config");
        assert_eq!(DISTRIBUTOR_SEED, b"distributor");
        assert_eq!(MONTH_SEED, b"month");
        assert_eq!(CURVE_SEED, b"curve");
    }

    #[test]
    fn the_legacy_and_curve_seeds_are_pepper_curves_own() {
        // Read under `pepper_curve`, these two must be its seeds to the byte: another
        // `DISTRIBUTOR_SEED` would read an empty address, and `LegacyMonthLive`
        // would never refuse a month `pepper_curve` still holds.
        let curve = include_str!("../../pepper_curve/src/constants.rs");
        assert!(curve.contains("pub const DISTRIBUTOR_SEED: &[u8] = b\"distributor\";"));
        assert!(curve.contains("pub const CURVE_SEED: &[u8] = b\"curve\";"));
    }

    #[test]
    fn the_leaf_domain_is_its_own() {
        assert_eq!(DIVIDEND_LEAF_DOMAIN, b"fanex:dividend:usdc:v1");
        assert_ne!(DIVIDEND_LEAF_DOMAIN, b"fanex:dividend:v2");
    }

    #[test]
    fn a_month_lives_a_year() {
        assert_eq!(MIN_DISTRIBUTOR_EXPIRY_SECONDS, 364 * DAY);
        assert_eq!(MAX_DISTRIBUTOR_EXPIRY_SECONDS, 400 * DAY);
        // The window the app stamps, 365 days (`DISTRIBUTOR_EXPIRY_SECONDS` in
        // lib/solana/pepper-curve/program.ts), opens a month.
        assert!((MIN_DISTRIBUTOR_EXPIRY_SECONDS..=MAX_DISTRIBUTOR_EXPIRY_SECONDS).contains(&(365 * DAY)));
    }

    #[test]
    fn the_claims_pause_and_the_close_guard() {
        assert_eq!(MAX_CLAIMS_PAUSE_SECONDS, 7 * DAY);
        assert_eq!(CLAIMS_OPEN_BEFORE_CLOSE_SECONDS, 30 * DAY);
        // The audit's bounds (2026-10-29): a pause of an hour at least, and an expired month
        // closed at the latest 90 days after its expiry.
        assert_eq!(MIN_CLAIMS_PAUSE_SECONDS, 3_600);
        assert_eq!(MAX_CLOSE_DELAY_SECONDS, 90 * DAY);
        // A pause may run from its minimum to its maximum, and the cap leaves the 30 days of
        // open claims after a pause held to the expiry, and room for two months more.
        assert!(MIN_CLAIMS_PAUSE_SECONDS < MAX_CLAIMS_PAUSE_SECONDS);
        assert!(MAX_CLOSE_DELAY_SECONDS >= CLAIMS_OPEN_BEFORE_CLOSE_SECONDS + MAX_CLAIMS_PAUSE_SECONDS);
    }

    #[test]
    fn a_month_opens_its_claims_a_day_after_its_funding() {
        assert_eq!(REVIEW_WINDOW_SECONDS, 86_400);
        // A hold the pauser starts then may outlast the window, and the window ends long before
        // the soonest expiry, so it never reaches a close's guard.
        assert!(REVIEW_WINDOW_SECONDS > MIN_CLAIMS_PAUSE_SECONDS);
        assert!(REVIEW_WINDOW_SECONDS <= MAX_CLAIMS_PAUSE_SECONDS);
        assert!(REVIEW_WINDOW_SECONDS + CLAIMS_OPEN_BEFORE_CLOSE_SECONDS < MIN_DISTRIBUTOR_EXPIRY_SECONDS);
    }

    #[test]
    fn units_and_statuses_are_pinned() {
        assert_eq!(USDC_DECIMALS, 6);
        assert_eq!(ACCOUNT_VERSION, 1);
        assert_eq!([MONTH_STATUS_OPEN, MONTH_STATUS_CLOSED, MONTH_STATUS_VOIDED], [1, 2, 3]);
    }

    #[test]
    fn the_curve_discriminators_are_anchors_for_their_names() {
        // A rename in `pepper_curve` moves its discriminator, and the creator read
        // would refuse every curve (`CurveUnreadable`) until this file follows.
        assert_eq!(hash(b"account:Curve").to_bytes()[..8], CURVE_DISCRIMINATOR);
        assert_eq!(hash(b"account:CurveUsdc").to_bytes()[..8], CURVE_USDC_DISCRIMINATOR);
        // The same for its two months, whose expiry the legacy guard reads.
        assert_eq!(hash(b"account:Distributor").to_bytes()[..8], LEGACY_DISTRIBUTOR_DISCRIMINATOR);
        assert_eq!(hash(b"account:DistributorV2").to_bytes()[..8], LEGACY_DISTRIBUTOR_V2_DISCRIMINATOR);
        assert_ne!(LEGACY_DISTRIBUTOR_DISCRIMINATOR, LEGACY_DISTRIBUTOR_V2_DISCRIMINATOR);
    }

    #[test]
    fn the_legacy_expiry_sits_where_v1_and_v2_put_it() {
        // Counted by hand: the discriminator, mint, month i64, root, total and claimed u64,
        // funder, then `expires_at`, in `Distributor` and, at the same offsets, in `DistributorV2`.
        assert_eq!(LEGACY_EXPIRES_AT_OFFSET, 8 + 32 + 8 + 32 + 8 + 8 + 32);
        // Inside both: a v1 month is 137 bytes (its bump after the expiry), a v2 one at least 146.
        assert!(LEGACY_EXPIRES_AT_OFFSET + 8 < 137);
    }

    #[test]
    fn the_curve_usdc_version_is_pepper_curves_first_layout() {
        let curve = include_str!("../../pepper_curve/src/constants.rs");
        assert!(curve.contains("pub const USDC_ACCOUNT_VERSION: u8 = 1;"));
        assert_eq!(CURVE_USDC_VERSION, 1);
    }

    #[test]
    fn the_creator_sits_where_each_layout_puts_it() {
        // Counted by hand, field by field (Pubkey 32, u64/i64 8, u16 2, u8/bool 1).
        // `Curve`: mint, creator, phase, 13 u64, paused, created_at and
        // graduated_at, two bumps.
        assert_eq!(CURVE_LEN, 8 + 2 * 32 + 1 + 13 * 8 + 1 + 2 * 8 + 2);
        assert_eq!(CURVE_CREATOR_OFFSET, 8 + 32);
        // `CurveUsdc`: six one-byte fields, three Pubkeys (mint, creator, its USDC
        // account), two u16, 14 u64/i64, 128 reserved.
        assert_eq!(CURVE_USDC_LEN, 8 + 6 + 3 * 32 + 2 * 2 + 14 * 8 + 128);
        assert_eq!(CURVE_USDC_CREATOR_OFFSET, 8 + 6 + 32);
        // Both creators are whole inside their account, and no length or
        // discriminator is the other's.
        assert!(CURVE_CREATOR_OFFSET + 32 <= CURVE_LEN);
        assert!(CURVE_USDC_CREATOR_OFFSET + 32 <= CURVE_USDC_LEN);
        assert_ne!(CURVE_LEN, CURVE_USDC_LEN);
        assert_ne!(CURVE_DISCRIMINATOR, CURVE_USDC_DISCRIMINATOR);
    }
}
