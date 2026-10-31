//! The program's rules on plain values, outside the handlers, where
//! `cargo test --lib` reaches them with no `Context` (as in `pepper_curve` and
//! `pepper_raise`). Each rule that refuses returns the error its handler
//! raises, so the tests pin the code a client sees.

use anchor_lang::prelude::*;

use crate::constants::{
    ACCOUNT_VERSION, CLAIMS_OPEN_BEFORE_CLOSE_SECONDS, CURVE_CREATOR_OFFSET, CURVE_DISCRIMINATOR, CURVE_LEN,
    CURVE_USDC_CREATOR_OFFSET, CURVE_USDC_DISCRIMINATOR, CURVE_USDC_LEN, CURVE_USDC_VERSION,
    LEGACY_DISTRIBUTOR_DISCRIMINATOR, LEGACY_DISTRIBUTOR_V2_DISCRIMINATOR, LEGACY_EXPIRES_AT_OFFSET,
    MAX_CLAIMS_PAUSE_SECONDS, MAX_CLOSE_DELAY_SECONDS, MAX_DISTRIBUTOR_EXPIRY_SECONDS, MAX_DISTRIBUTOR_LEAVES,
    MIN_CLAIMS_PAUSE_SECONDS, MIN_DISTRIBUTOR_EXPIRY_SECONDS, MONTH_STATUS_VOIDED, REVIEW_WINDOW_SECONDS,
};
use crate::errors::DividendsError;

// ── Layout versions (the internal audit, 2026-10-29) ────────────────────────

/// Whether this program reads an account of `version`: `ACCOUNT_VERSION` alone, so a binary
/// rolled back past a later layout refuses that layout's accounts (`UnsupportedVersion`) instead
/// of reading their bytes as its own. Every instruction holds the config, the distributor and the
/// record it reads to it.
pub fn known_version(version: u8) -> bool {
    version == ACCOUNT_VERSION
}

/// The record `init_distributor` meets: one it creates in that very funding reads version 0, a
/// voided one `ACCOUNT_VERSION`; anything else is `UnsupportedVersion`.
pub fn known_record_version(version: u8) -> bool {
    version == 0 || known_version(version)
}

// ── The claim registry (spec §3), copied from `pepper_curve`'s with its tests ──

/// Bytes of a month's claim registry for `leaf_count` leaves: one bit each,
/// least significant first, as `pepper_curve`'s `DistributorV2` (spec §3).
pub fn claim_registry_bytes(leaf_count: u32) -> usize {
    (leaf_count as usize).div_ceil(8)
}

/// Can a month commit to `leaf_count` leaves? At least one, at most
/// `MAX_DISTRIBUTOR_LEAVES` (`InvalidLeafCount` otherwise).
pub fn leaf_count_in_range(leaf_count: u32) -> bool {
    (1..=MAX_DISTRIBUTOR_LEAVES).contains(&leaf_count)
}

/// Where leaf `index`'s bit lives in a claim registry: (byte, mask), least
/// significant bit first, as `pepper_curve`'s `leaf_bit`, which
/// `lib/solana/distributor-status.ts` reads in the same order.
pub fn leaf_bit(index: u32) -> (usize, u8) {
    ((index / 8) as usize, 1u8 << (index % 8))
}

// ── The config's birth (spec §4.1) ──────────────────────────────────────────

/// The bytes of a ProgramData before the program's ELF (`UpgradeableLoaderState::
/// size_of_programdata_metadata`): the bincode of `UpgradeableLoaderState::ProgramData`, its tag
/// 3 as a u32, `slot` as a u64, then `upgrade_authority_address` as an `Option<Pubkey>` (a 0 or 1
/// byte, then the key's 32, left zero when there is none). Copied from track A's `pepper_curve`.
pub const PROGRAM_DATA_HEADER_LEN: usize = 45;
const PROGRAM_DATA_TAG: [u8; 4] = [3, 0, 0, 0];
const PROGRAM_DATA_AUTHORITY_TAG_OFFSET: usize = 12;

/// The upgrade authority a ProgramData's header records: `None` for an immutable program, and
/// for bytes that are not a ProgramData's header (too short, another tag of the loader's state,
/// an option byte neither 0 nor 1), so whoever signs is refused. Read by hand: Anchor's
/// `Account<ProgramData>` decodes with bincode, which made `pepper_curve` about 60 KB larger.
pub fn program_data_upgrade_authority(data: &[u8]) -> Option<Pubkey> {
    if data.len() < PROGRAM_DATA_HEADER_LEN || data[..4] != PROGRAM_DATA_TAG {
        return None;
    }
    if data[PROGRAM_DATA_AUTHORITY_TAG_OFFSET] != 1 {
        return None;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&data[PROGRAM_DATA_AUTHORITY_TAG_OFFSET + 1..PROGRAM_DATA_HEADER_LEN]);
    Some(Pubkey::new_from_array(key))
}

// ── Funding a month (spec §4.4) ─────────────────────────────────────────────

/// A month's expiry at its funding: `now + 364 days ≤ expires_at ≤ now + 400
/// days`, so no month opens with a window its holders cannot use, and none
/// stays open past 400 days.
pub fn check_expiry(expires_at: i64, now: i64) -> Result<()> {
    if expires_at < now.saturating_add(MIN_DISTRIBUTOR_EXPIRY_SECONDS) {
        return err!(DividendsError::ExpiryTooSoon);
    }
    if expires_at > now.saturating_add(MAX_DISTRIBUTOR_EXPIRY_SECONDS) {
        return err!(DividendsError::ExpiryTooFar);
    }
    Ok(())
}

/// Whether `funder` is one of Fanex's two funders the config names.
pub fn is_pepper_funder(funder: &Pubkey, pepper_funder: &Pubkey, payer_funder: &Pubkey) -> bool {
    funder == pepper_funder || funder == payer_funder
}

/// The funder gate: one of Fanex's two funders, or the curve's creator, which
/// `read_creator` reads only when the funder is neither, so a Fanex month opens
/// for a mint with no curve. An unreadable curve stays `CurveUnreadable`, never
/// «no creator»; anyone else is `NotAFunder`.
pub fn check_funder(
    funder: &Pubkey,
    pepper_funder: &Pubkey,
    payer_funder: &Pubkey,
    read_creator: impl FnOnce() -> Result<Pubkey>,
) -> Result<()> {
    if is_pepper_funder(funder, pepper_funder, payer_funder) {
        return Ok(());
    }
    if read_creator()? == *funder {
        Ok(())
    } else {
        err!(DividendsError::NotAFunder)
    }
}

/// The approver is a key of its own (Rick, 2026-10-21): never one of the two
/// funders, so one stolen key opens no month, as `pepper_curve`'s v2 rule of
/// 2026-10-08. Held at `initialize_config` and at each `update_config`.
pub fn check_approver(approver: &Pubkey, pepper_funder: &Pubkey, payer_funder: &Pubkey) -> Result<()> {
    if approver == pepper_funder || approver == payer_funder {
        err!(DividendsError::ApproverIsFunder)
    } else {
        Ok(())
    }
}

/// The creator of `pepper_curve`'s curve, read by hand from its account: owned
/// by `curve_program`, then a `Curve` (196 bytes, the creator at 40) or a
/// `CurveUsdc` (354 bytes, the creator at 46, its `version` at 8 the one this read
/// knows, `CURVE_USDC_VERSION`), each by its discriminator and its exact length.
/// Anything else is `CurveUnreadable`, never «no creator». A `Curve` carries no
/// version byte: its first field is the mint.
pub fn curve_creator(owner: &Pubkey, curve_program: &Pubkey, data: &[u8]) -> Result<Pubkey> {
    if owner != curve_program {
        return err!(DividendsError::CurveUnreadable);
    }
    let offset = match (data.get(..8), data.len()) {
        (Some(discriminator), CURVE_LEN) if discriminator == CURVE_DISCRIMINATOR => CURVE_CREATOR_OFFSET,
        (Some(discriminator), CURVE_USDC_LEN) if discriminator == CURVE_USDC_DISCRIMINATOR => {
            if data[8] != CURVE_USDC_VERSION {
                return err!(DividendsError::CurveUnreadable);
            }
            CURVE_USDC_CREATOR_OFFSET
        }
        _ => return err!(DividendsError::CurveUnreadable),
    };
    let creator: [u8; 32] =
        data[offset..offset + 32].try_into().map_err(|_| error!(DividendsError::CurveUnreadable))?;
    Ok(Pubkey::new_from_array(creator))
}

/// The expiry of `pepper_curve`'s v1 or v2 month held in `data`, read by hand: owned by
/// `curve_program`, a `Distributor` or a `DistributorV2` by its discriminator, its
/// `expires_at` at byte 128 in both (`LEGACY_EXPIRES_AT_OFFSET`). `None` for anything else.
pub fn legacy_month_expiry(owner: &Pubkey, curve_program: &Pubkey, data: &[u8]) -> Option<i64> {
    if owner != curve_program {
        return None;
    }
    let discriminator = data.get(..8)?;
    if discriminator != LEGACY_DISTRIBUTOR_DISCRIMINATOR && discriminator != LEGACY_DISTRIBUTOR_V2_DISCRIMINATOR {
        return None;
    }
    let expires_at: [u8; 8] = data.get(LEGACY_EXPIRES_AT_OFFSET..LEGACY_EXPIRES_AT_OFFSET + 8)?.try_into().ok()?;
    Some(i64::from_le_bytes(expires_at))
}

/// Whether `pepper_curve`'s month at the legacy address of the same token and month
/// blocks this one. Lamports alone, which anyone can send to the address, block
/// nothing. A v1 or v2 month of `curve_program` blocks until its expiry, closed or
/// not (the internal audit, 2026-10-29): only its funder closes one, so a month
/// opened with a root of the opener's own would otherwise block this one for as
/// long as that key chose; from its expiry on, the double-funding guard is the
/// off-chain ledger's alone, as for a SOL month already closed. Any other data
/// blocks for good, whatever its owner: nothing unreadable is taken as no month.
pub fn legacy_month_live(owner: &Pubkey, curve_program: &Pubkey, data: &[u8], now: i64) -> bool {
    if data.is_empty() {
        return false;
    }
    match legacy_month_expiry(owner, curve_program, data) {
        Some(expires_at) => now < expires_at,
        None => true,
    }
}

/// `init_distributor` refuses a month `pepper_curve` still holds (`LegacyMonthLive`).
pub fn check_no_legacy_month(owner: &Pubkey, curve_program: &Pubkey, data: &[u8], now: i64) -> Result<()> {
    if legacy_month_live(owner, curve_program, data, now) {
        err!(DividendsError::LegacyMonthLive)
    } else {
        Ok(())
    }
}

/// When a month's claims open: `REVIEW_WINDOW_SECONDS` after its funding (the internal audit,
/// 2026-10-29), the `claims_held_until` `init_distributor` writes. Until then the month's claims
/// are `ClaimsHeld` and its void needs nothing to have waited, so a wrong root or total is voided
/// before anyone claims from it.
pub fn review_window_end(now: i64) -> i64 {
    now.saturating_add(REVIEW_WINDOW_SECONDS)
}

/// A month's total after `top_up_distributor(amount)` (the internal audit, 2026-10-29): a
/// positive amount (`ZeroAmount`), the sum within a u64 (`MathOverflow`). Only the total grows:
/// the root, and so what each leaf claims, stay as funded.
pub fn topped_up_total(total_amount: u64, amount: u64) -> Result<u64> {
    require!(amount > 0, DividendsError::ZeroAmount);
    total_amount.checked_add(amount).ok_or_else(|| error!(DividendsError::MathOverflow))
}

/// What `rehome_distributor` moves to the config's dividend payer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rehome {
    /// The funder and its USDC account.
    pub funder: bool,
    /// The rent payer.
    pub rent_payer: bool,
}

/// Which of a month's payees `rehome_distributor` moves (the internal audit, 2026-10-29): the
/// funder and its USDC account only when one of Fanex's two funders funded it, never a creator's;
/// the rent payer only when the dividend payer paid its rents. A month that records neither, as
/// every month funded before 2026-10-29 reads, and a config that names no payer (all-zero: no key
/// could spend that USDC account), are `NotRehomable`.
pub fn rehome_plan(funded_by_fanex: bool, rents_by_payer: bool, payer_funder: &Pubkey) -> Result<Rehome> {
    if *payer_funder == Pubkey::default() || !(funded_by_fanex || rents_by_payer) {
        return err!(DividendsError::NotRehomable);
    }
    Ok(Rehome { funder: funded_by_fanex, rent_payer: rents_by_payer })
}

/// Whether a month's record lets `init_distributor` open the month: fresh (0,
/// as `init_if_needed` creates it) or voided. Open or closed, or any other
/// value, is `MonthAlreadyFunded`: a closed month never opens again.
pub fn check_month_record(status: u8) -> Result<()> {
    match status {
        0 | MONTH_STATUS_VOIDED => Ok(()),
        _ => err!(DividendsError::MonthAlreadyFunded),
    }
}

// ── The claims pause (spec §4.3, §4.6) ──────────────────────────────────────

/// A claim is refused before `claims_paused_until` and accepted from it on.
pub fn check_claims_open(now: i64, claims_paused_until: i64) -> Result<()> {
    if now < claims_paused_until {
        err!(DividendsError::ClaimsPaused)
    } else {
        Ok(())
    }
}

/// A claim of one month is refused before that month's `claims_held_until`
/// (`ClaimsHeld`) and accepted from it on: the hold stops that month alone, and
/// every other month and token keeps claiming (Rick, 2026-10-26).
pub fn check_month_claims_open(now: i64, claims_held_until: i64) -> Result<()> {
    if now < claims_held_until {
        err!(DividendsError::ClaimsHeld)
    } else {
        Ok(())
    }
}

/// `pause_claims(until)` only starts or extends a pause, for an hour at least:
/// `max(now + 1 hour, claims_paused_until) ≤ until ≤ now + 7 days`. A value in the
/// past, or one that would shorten a running pause, is `PauseEndTooEarly`, so no
/// value lets an expired month's close skip its 30-day guard; one in the next hour
/// is `PauseTooShort` (the internal audit, 2026-10-29: a pause that refuses no claim
/// still delayed every expired month's close by 30 days); past the 7 days it is
/// `PauseTooLong`. A month's `hold_claims(until)` takes the same rule, against that
/// month's `claims_held_until`.
pub fn check_claims_pause(until: i64, now: i64, claims_paused_until: i64) -> Result<()> {
    if until > now.saturating_add(MAX_CLAIMS_PAUSE_SECONDS) {
        return err!(DividendsError::PauseTooLong);
    }
    if until < now.max(claims_paused_until) {
        return err!(DividendsError::PauseEndTooEarly);
    }
    if until < now.saturating_add(MIN_CLAIMS_PAUSE_SECONDS) {
        return err!(DividendsError::PauseTooShort);
    }
    Ok(())
}

/// What `resume_claims()` leaves in `claims_paused_until`, and a month's
/// `release_claims()` in its `claims_held_until`: the time it runs, or the value
/// as it was when that pause already ended, so a resume after a pause ran out
/// moves no expired month's 30-day guard. It takes no argument: the admin's
/// vault executes 48 hours or more after a proposal is written, and a time fixed
/// at writing would be in the past by then.
pub fn resumed_claims_paused_until(claims_paused_until: i64, now: i64) -> i64 {
    claims_paused_until.min(now)
}

// ── Closing and voiding a month (spec §4.7, `void_distributor`) ─────────────

/// `close_distributor`: a month fully claimed closes at any time; otherwise
/// from its expiry (`now ≥ expires_at`, as `pepper_curve`'s v2
/// `may_close_distributor`, else `NotExpired`), and only once this month's
/// claims have been open for the last `CLAIMS_OPEN_BEFORE_CLOSE_SECONDS`
/// (`ClaimsPausedRecently`), counted from the later of the config's claims pause
/// and this month's hold: a pause or a hold delays a close, never a holder's
/// window to claim, so neither held to the expiry hands the funder anything its
/// holders could not claim. That delay ends `MAX_CLOSE_DELAY_SECONDS` after the
/// expiry, whatever pause or hold ran (the internal audit, 2026-10-29), so a
/// pauser renewing pauses delays a close 90 days at most, never for ever.
pub fn check_close(
    claimed_amount: u64,
    total_amount: u64,
    now: i64,
    expires_at: i64,
    claims_paused_until: i64,
    claims_held_until: i64,
) -> Result<()> {
    if claimed_amount == total_amount {
        return Ok(());
    }
    if now < expires_at {
        return err!(DividendsError::NotExpired);
    }
    let claims_last_opened = claims_paused_until.max(claims_held_until);
    let guard_ends = claims_last_opened
        .saturating_add(CLAIMS_OPEN_BEFORE_CLOSE_SECONDS)
        .min(expires_at.saturating_add(MAX_CLOSE_DELAY_SECONDS));
    if now < guard_ends {
        return err!(DividendsError::ClaimsPausedRecently);
    }
    Ok(())
}

/// `void_distributor` (Rick, 2026-10-23) takes only a month nobody has claimed
/// from: `claimed_amount` at 0 and not one bit set, since the void drops the
/// bitmap and a holder already paid could be paid again (`MonthHasClaims`).
pub fn check_void(claimed_amount: u64, claimed: &[u8]) -> Result<()> {
    if claimed_amount != 0 || claimed.iter().any(|byte| *byte != 0) {
        err!(DividendsError::MonthHasClaims)
    } else {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use anchor_lang::prelude::*;

    /// The program error a rule answered, by name, or `None` when it passed.
    pub fn refusal<T>(result: Result<T>) -> Option<String> {
        match result {
            Ok(_) => None,
            Err(anchor_lang::error::Error::AnchorError(e)) => Some(e.error_name),
            Err(other) => panic!("expected an AnchorError, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod claim_registry_tests {
    use super::*;

    #[test]
    fn one_bit_per_leaf_rounded_up_to_a_byte() {
        assert_eq!(claim_registry_bytes(1), 1);
        assert_eq!(claim_registry_bytes(8), 1);
        assert_eq!(claim_registry_bytes(9), 2);
        assert_eq!(claim_registry_bytes(14), 2);
        assert_eq!(claim_registry_bytes(MAX_DISTRIBUTOR_LEAVES), 8_192);
    }

    #[test]
    fn a_registry_needs_at_least_one_leaf_and_at_most_the_maximum() {
        assert!(!leaf_count_in_range(0));
        assert!(leaf_count_in_range(1));
        assert!(leaf_count_in_range(MAX_DISTRIBUTOR_LEAVES));
        assert!(!leaf_count_in_range(MAX_DISTRIBUTOR_LEAVES + 1));
        assert!(!leaf_count_in_range(u32::MAX));
    }

    // The same vector is pinned in pepper_curve's validation.rs and in
    // lib/solana/distributor-status.test.ts.
    #[test]
    fn bits_go_least_significant_first() {
        let mut registry = [0u8; 2];
        for index in [0u32, 7, 8, 13] {
            let (byte, mask) = leaf_bit(index);
            registry[byte] |= mask;
        }
        assert_eq!(registry, [0x81, 0x21]);
    }

    #[test]
    fn the_last_leaf_of_a_full_registry_is_its_last_bit() {
        let (byte, mask) = leaf_bit(MAX_DISTRIBUTOR_LEAVES - 1);
        assert_eq!((byte, mask), (claim_registry_bytes(MAX_DISTRIBUTOR_LEAVES) - 1, 0x80));
    }
}

#[cfg(test)]
mod program_data_tests {
    use super::*;
    use anchor_lang::solana_program::bpf_loader_upgradeable::UpgradeableLoaderState;
    use anchor_lang::{AccountDeserialize, ProgramData};

    /// A ProgramData as the loader writes it: the header, then `elf_len` bytes of program.
    fn program_data(slot: u64, authority: Option<Pubkey>, elf_len: usize) -> Vec<u8> {
        let mut data = vec![3, 0, 0, 0];
        data.extend_from_slice(&slot.to_le_bytes());
        match authority {
            Some(key) => {
                data.push(1);
                data.extend_from_slice(key.as_ref());
            }
            None => data.extend_from_slice(&[0; 33]),
        }
        data.extend(std::iter::repeat(0x7f).take(elf_len));
        data
    }

    #[test]
    fn reads_the_authority_the_loaders_own_decoding_reads() {
        // Anchor's `ProgramData` decodes with bincode into the loader's own state type: the hand
        // read must agree with it on the recorded authority, and on an immutable program's none.
        assert_eq!(UpgradeableLoaderState::size_of_programdata_metadata(), PROGRAM_DATA_HEADER_LEN);
        let key = Pubkey::new_unique();
        for recorded in [Some(key), None] {
            let data = program_data(457_292_125, recorded, 64);
            let decoded = ProgramData::try_deserialize(&mut data.as_slice()).unwrap();
            assert_eq!((decoded.slot, decoded.upgrade_authority_address), (457_292_125, recorded));
            assert_eq!(program_data_upgrade_authority(&data), recorded);
            // The header alone, with no ELF after it, reads the same.
            assert_eq!(program_data_upgrade_authority(&data[..PROGRAM_DATA_HEADER_LEN]), recorded);
        }
        // `set-upgrade-authority --final` (and `deploy --final`): the loader writes the 13 bytes of
        // the None header over one that recorded a key, so the former key's bytes stay after the
        // option byte. They record no authority.
        let mut made_final = program_data(457_292_125, Some(key), 64);
        made_final[PROGRAM_DATA_AUTHORITY_TAG_OFFSET] = 0;
        assert_eq!(&made_final[13..PROGRAM_DATA_HEADER_LEN], key.as_ref());
        let decoded = ProgramData::try_deserialize(&mut made_final.as_slice()).unwrap();
        assert_eq!(decoded.upgrade_authority_address, None);
        assert_eq!(program_data_upgrade_authority(&made_final), None);
    }

    #[test]
    fn bytes_that_are_no_program_data_header_record_no_authority() {
        let key = Pubkey::new_unique();
        let good = program_data(1, Some(key), 0);
        assert_eq!(program_data_upgrade_authority(&good[..44]), None);
        assert_eq!(program_data_upgrade_authority(&[]), None);
        // The loader's other states: Uninitialized 0, Buffer 1, Program 2.
        for tag in [0u8, 1, 2, 4] {
            let mut other = good.clone();
            other[0] = tag;
            assert_eq!(program_data_upgrade_authority(&other), None, "tag {tag}");
        }
        // An option byte bincode would refuse.
        let mut bad_option = good.clone();
        bad_option[12] = 2;
        assert_eq!(program_data_upgrade_authority(&bad_option), None);
        assert!(ProgramData::try_deserialize(&mut bad_option.as_slice()).is_err());
        assert_eq!(program_data_upgrade_authority(&good), Some(key));
    }
}

#[cfg(test)]
mod funding_tests {
    use super::test_support::refusal;
    use super::*;
    use crate::constants::{CURVE_SEED, MONTH_STATUS_CLOSED, MONTH_STATUS_OPEN};
    use std::str::FromStr;

    const DAY: i64 = 86_400;
    const NOW: i64 = 1_793_491_200;

    fn key(byte: u8) -> Pubkey {
        Pubkey::new_from_array([byte; 32])
    }

    #[test]
    fn the_expiry_is_held_between_364_and_400_days_to_the_second() {
        assert_eq!(refusal(check_expiry(NOW + 364 * DAY - 1, NOW)).as_deref(), Some("ExpiryTooSoon"));
        assert_eq!(refusal(check_expiry(NOW + 364 * DAY, NOW)), None);
        // The 365 days Fanex stamps.
        assert_eq!(refusal(check_expiry(NOW + 365 * DAY, NOW)), None);
        assert_eq!(refusal(check_expiry(NOW + 400 * DAY, NOW)), None);
        assert_eq!(refusal(check_expiry(NOW + 400 * DAY + 1, NOW)).as_deref(), Some("ExpiryTooFar"));
        // A month already expired, or expiring now, is too soon; a clock at the edge saturates.
        assert_eq!(refusal(check_expiry(NOW, NOW)).as_deref(), Some("ExpiryTooSoon"));
        assert_eq!(refusal(check_expiry(i64::MAX, NOW)).as_deref(), Some("ExpiryTooFar"));
        assert_eq!(refusal(check_expiry(i64::MAX, i64::MAX)), None);
    }

    #[test]
    fn pepper_funders_pass_without_reading_the_curve() {
        let (fanex, payer) = (key(1), key(2));
        let unread = || -> Result<Pubkey> { panic!("the curve is read only for a creator funder") };
        assert_eq!(refusal(check_funder(&fanex, &fanex, &payer, unread)), None);
        assert_eq!(refusal(check_funder(&payer, &fanex, &payer, unread)), None);
        // So a Fanex month opens for a mint with no curve, which pepper_curve's
        // v1 and v2 refuse.
        let no_curve = || curve_creator(&Pubkey::default(), &key(9), &[]);
        assert_eq!(refusal(check_funder(&payer, &fanex, &payer, no_curve)), None);
    }

    #[test]
    fn the_creator_funds_and_anyone_else_is_not_a_funder() {
        let (fanex, payer, creator, stranger) = (key(1), key(2), key(3), key(4));
        assert_eq!(refusal(check_funder(&creator, &fanex, &payer, || Ok(creator))), None);
        assert_eq!(refusal(check_funder(&stranger, &fanex, &payer, || Ok(creator))).as_deref(), Some("NotAFunder"));
        // An unreadable curve stays its own refusal, never «no creator».
        let unreadable = || curve_creator(&key(8), &key(9), &[0; 196]);
        assert_eq!(refusal(check_funder(&creator, &fanex, &payer, unreadable)).as_deref(), Some("CurveUnreadable"));
    }

    #[test]
    fn an_all_zero_pepper_funder_funds_nothing_by_itself() {
        // The proposal for the receive-only fee wallet's place: no key signs as all-zero.
        let (payer, creator) = (key(2), key(3));
        let zero = Pubkey::default();
        assert!(!is_pepper_funder(&key(4), &zero, &payer));
        assert!(is_pepper_funder(&payer, &zero, &payer));
        assert_eq!(refusal(check_funder(&key(4), &zero, &payer, || Ok(creator))).as_deref(), Some("NotAFunder"));
    }

    #[test]
    fn the_approver_is_neither_funder() {
        let (fanex, payer, approver) = (key(1), key(2), key(5));
        assert_eq!(refusal(check_approver(&approver, &fanex, &payer)), None);
        assert_eq!(refusal(check_approver(&fanex, &fanex, &payer)).as_deref(), Some("ApproverIsFunder"));
        assert_eq!(refusal(check_approver(&payer, &fanex, &payer)).as_deref(), Some("ApproverIsFunder"));
    }

    /// A `Curve` or `CurveUsdc` account's bytes: its discriminator, `creator` at
    /// its offset, a `CurveUsdc`'s layout version at 8, zeros elsewhere.
    fn curve_bytes(discriminator: [u8; 8], len: usize, creator_at: usize, creator: &Pubkey) -> Vec<u8> {
        let mut data = vec![0u8; len];
        data[..8].copy_from_slice(&discriminator);
        if discriminator == CURVE_USDC_DISCRIMINATOR && len > 8 {
            data[8] = CURVE_USDC_VERSION;
        }
        data[creator_at..creator_at + 32].copy_from_slice(creator.as_ref());
        data
    }

    #[test]
    fn a_curve_usdc_of_another_layout_version_is_unreadable() {
        // The internal audit (2026-10-29): a later layout of 354 bytes that put another key at 46
        // is never read as the creator, nor is a version the curve was never written with.
        let (program, creator) = (key(9), key(3));
        let usdc = curve_bytes(CURVE_USDC_DISCRIMINATOR, CURVE_USDC_LEN, CURVE_USDC_CREATOR_OFFSET, &creator);
        assert_eq!(usdc[8], 1);
        assert_eq!(curve_creator(&program, &program, &usdc).unwrap(), creator);
        for version in [0u8, 2, u8::MAX] {
            let mut other = usdc.clone();
            other[8] = version;
            assert_eq!(refusal(curve_creator(&program, &program, &other)).as_deref(), Some("CurveUnreadable"), "version {version}");
        }
        // A SOL `Curve` has no version byte: its first field is the mint, whatever its bytes.
        let mut sol = curve_bytes(CURVE_DISCRIMINATOR, CURVE_LEN, CURVE_CREATOR_OFFSET, &creator);
        sol[8] = 0xA7;
        assert_eq!(curve_creator(&program, &program, &sol).unwrap(), creator);
    }

    #[test]
    fn the_creator_is_read_from_a_curve_and_from_a_curve_usdc() {
        let (program, creator) = (key(9), key(3));
        let sol = curve_bytes(CURVE_DISCRIMINATOR, CURVE_LEN, CURVE_CREATOR_OFFSET, &creator);
        assert_eq!(curve_creator(&program, &program, &sol).unwrap(), creator);
        let usdc = curve_bytes(CURVE_USDC_DISCRIMINATOR, CURVE_USDC_LEN, CURVE_USDC_CREATOR_OFFSET, &creator);
        assert_eq!(curve_creator(&program, &program, &usdc).unwrap(), creator);
    }

    #[test]
    fn anything_but_a_curve_of_its_exact_length_is_unreadable() {
        let (program, creator) = (key(9), key(3));
        let unreadable = |owner: &Pubkey, data: &[u8]| refusal(curve_creator(owner, &program, data));
        let sol = curve_bytes(CURVE_DISCRIMINATOR, CURVE_LEN, CURVE_CREATOR_OFFSET, &creator);
        // Another owner, the same bytes.
        assert_eq!(unreadable(&key(8), &sol).as_deref(), Some("CurveUnreadable"));
        // A byte more or less, the discriminator unchanged.
        let mut longer = sol.clone();
        longer.push(0);
        assert_eq!(unreadable(&program, &longer).as_deref(), Some("CurveUnreadable"));
        assert_eq!(unreadable(&program, &sol[..CURVE_LEN - 1]).as_deref(), Some("CurveUnreadable"));
        // Each discriminator at the other's length.
        let crossed = curve_bytes(CURVE_DISCRIMINATOR, CURVE_USDC_LEN, CURVE_CREATOR_OFFSET, &creator);
        assert_eq!(unreadable(&program, &crossed).as_deref(), Some("CurveUnreadable"));
        let crossed = curve_bytes(CURVE_USDC_DISCRIMINATOR, CURVE_LEN, CURVE_USDC_CREATOR_OFFSET, &creator);
        assert_eq!(unreadable(&program, &crossed).as_deref(), Some("CurveUnreadable"));
        // Another account of pepper_curve at a curve's length, and nothing at all.
        let mut other = sol.clone();
        other[..8].copy_from_slice(&[90, 90, 217, 147, 6, 32, 135, 4]); // v1 `Distributor`'s discriminator
        assert_eq!(unreadable(&program, &other).as_deref(), Some("CurveUnreadable"));
        assert_eq!(unreadable(&program, &[]).as_deref(), Some("CurveUnreadable"));
        assert_eq!(unreadable(&Pubkey::default(), &[]).as_deref(), Some("CurveUnreadable"));
    }

    /// Base64 as the fixture writes it (standard alphabet, `=` padding).
    fn base64(text: &str) -> Vec<u8> {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let (mut out, mut acc, mut bits) = (Vec::new(), 0u32, 0u32);
        for c in text.bytes().filter(|c| *c != b'=') {
            acc = (acc << 6) | ALPHABET.iter().position(|a| *a == c).expect("a base64 character") as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
                acc &= (1 << bits) - 1;
            }
        }
        out
    }

    /// One account of lib/solana/__fixtures__/distributor-owners/devnet-accounts.json
    /// (read from devnet on 2026-10-22), by its `account` kind: (address, owner, data).
    fn devnet_account(kind: &str) -> (Pubkey, Pubkey, Vec<u8>) {
        let json = include_str!("../../../lib/solana/__fixtures__/distributor-owners/devnet-accounts.json");
        let marker = format!("\"account\": \"{kind}\"");
        let object = json.split('{').find(|o| o.contains(&marker)).expect("the kind in the fixture");
        let field = |name: &str| {
            let tag = format!("\"{name}\": \"");
            let start = object.find(&tag).expect(name) + tag.len();
            object[start..start + object[start..].find('"').unwrap()].to_string()
        };
        let pubkey = |text: String| Pubkey::from_str(&text).unwrap();
        (pubkey(field("address")), pubkey(field("owner")), base64(&field("dataBase64")))
    }

    #[test]
    fn the_creator_of_a_real_devnet_curve() {
        // pepper_curve's devnet curve of EPehMtVJ…, its creator as pepper_curve's IDL
        // decodes it (Anchor's coder, 2026-10-25).
        let (address, owner, data) = devnet_account("Curve");
        let curve_program = Pubkey::from_str("CDPwGWGWbAE8FL5f2oVdzfkTutrnsKJcAcgqWktC3dwZ").unwrap();
        assert_eq!((owner, data.len()), (curve_program, CURVE_LEN));
        let mint = Pubkey::from_str("EPehMtVJq63UusTv6ARvULvshdrBM3ykhMVS18rtFeLf").unwrap();
        // The address init_distributor binds as `curve`: ["curve", mint] under curve_program.
        assert_eq!(Pubkey::find_program_address(&[CURVE_SEED, mint.as_ref()], &curve_program).0, address);
        assert_eq!(
            curve_creator(&owner, &curve_program, &data).unwrap(),
            Pubkey::from_str("AXWTEvESWGV3uMFwf3kXEswv33VvMKTFFppJZFafoNjR").unwrap()
        );
    }

    /// A v2 month's bytes as `pepper_curve` writes them, `len` long: its discriminator, and
    /// `expires_at` where v1 keeps it, zeros elsewhere.
    fn v2_month(len: usize, expires_at: i64) -> Vec<u8> {
        let mut data = vec![0u8; len];
        data[..8].copy_from_slice(&LEGACY_DISTRIBUTOR_V2_DISCRIMINATOR);
        data[LEGACY_EXPIRES_AT_OFFSET..LEGACY_EXPIRES_AT_OFFSET + 8].copy_from_slice(&expires_at.to_le_bytes());
        data
    }

    #[test]
    fn a_legacy_month_blocks_until_its_expiry_and_lamports_alone_block_nothing() {
        let curve_program = Pubkey::from_str("CDPwGWGWbAE8FL5f2oVdzfkTutrnsKJcAcgqWktC3dwZ").unwrap();
        assert!(!legacy_month_live(&curve_program, &curve_program, &[], NOW));
        assert!(!legacy_month_live(&Pubkey::default(), &curve_program, &[], NOW));
        assert_eq!(refusal(check_no_legacy_month(&curve_program, &curve_program, &[], NOW)), None);
        // pepper_curve's real v1 distributor on devnet (137 bytes), its expiry read where v1 keeps it.
        let (_, owner, v1) = devnet_account("Distributor");
        assert_eq!((owner, v1.len()), (curve_program, 137));
        let expires_at = legacy_month_expiry(&owner, &curve_program, &v1).expect("a v1 month's expiry");
        // 2027-10-07 19:37:20 UTC: the month was funded on devnet with a year to run.
        assert_eq!(expires_at, 1_822_937_840);
        assert!(legacy_month_live(&owner, &curve_program, &v1, expires_at - 1));
        assert_eq!(
            refusal(check_no_legacy_month(&owner, &curve_program, &v1, expires_at - 1)).as_deref(),
            Some("LegacyMonthLive")
        );
        // From its expiry, to the second, it blocks no more, closed by its funder or not.
        assert!(!legacy_month_live(&owner, &curve_program, &v1, expires_at));
        assert_eq!(refusal(check_no_legacy_month(&owner, &curve_program, &v1, expires_at)), None);
        // A v2 month the same way, from its smallest size to a full registry.
        for len in [145 + 1, 145 + 8_192] {
            let v2 = v2_month(len, NOW + 365 * DAY);
            assert_eq!(legacy_month_expiry(&curve_program, &curve_program, &v2), Some(NOW + 365 * DAY));
            assert!(legacy_month_live(&curve_program, &curve_program, &v2, NOW + 365 * DAY - 1), "{len} bytes");
            assert!(!legacy_month_live(&curve_program, &curve_program, &v2, NOW + 365 * DAY), "{len} bytes");
        }
    }

    #[test]
    fn anything_at_the_legacy_address_but_a_month_of_pepper_curve_blocks_for_good() {
        let curve_program = key(9);
        let far = i64::MAX;
        let month = v2_month(146, NOW);
        // The same bytes under another owner, another discriminator, a month cut short of its
        // expiry, a single byte: none reads as a month, so each blocks even at the end of time.
        let mut other_discriminator = month.clone();
        other_discriminator[..8].copy_from_slice(&CURVE_DISCRIMINATOR);
        for (owner, data) in [
            (key(8), month.clone()),
            (curve_program, other_discriminator),
            (curve_program, month[..LEGACY_EXPIRES_AT_OFFSET + 7].to_vec()),
            (curve_program, vec![1]),
        ] {
            assert_eq!(legacy_month_expiry(&owner, &curve_program, &data), None);
            assert!(legacy_month_live(&owner, &curve_program, &data, far), "{} bytes", data.len());
            assert_eq!(
                refusal(check_no_legacy_month(&owner, &curve_program, &data, far)).as_deref(),
                Some("LegacyMonthLive")
            );
        }
        // The expiry's eight bytes are enough: a month whose bitmap is cut still reads.
        assert_eq!(legacy_month_expiry(&curve_program, &curve_program, &month[..LEGACY_EXPIRES_AT_OFFSET + 8]), Some(NOW));
    }

    #[test]
    fn a_months_claims_open_a_day_after_its_funding() {
        assert_eq!(review_window_end(NOW), NOW + DAY);
        // A clock at the edge saturates rather than wraps.
        assert_eq!(review_window_end(i64::MAX - 1), i64::MAX);
    }

    #[test]
    fn a_top_up_grows_the_total_by_a_positive_amount() {
        assert_eq!(topped_up_total(5_000_000, 1_000).unwrap(), 5_001_000);
        assert_eq!(refusal(topped_up_total(5_000_000, 0)).as_deref(), Some("ZeroAmount"));
        assert_eq!(topped_up_total(u64::MAX - 1, 1).unwrap(), u64::MAX);
        assert_eq!(refusal(topped_up_total(u64::MAX, 1)).as_deref(), Some("MathOverflow"));
    }

    #[test]
    fn a_rehome_moves_a_fanex_months_payees_and_only_the_rent_of_a_creators() {
        let payer = key(2);
        assert_eq!(rehome_plan(true, true, &payer).unwrap(), Rehome { funder: true, rent_payer: true });
        // A creator's month: its funder and its leftovers stay the creator's, its rents move.
        assert_eq!(rehome_plan(false, true, &payer).unwrap(), Rehome { funder: false, rent_payer: true });
        assert_eq!(rehome_plan(true, false, &payer).unwrap(), Rehome { funder: true, rent_payer: false });
        // Neither flag, as every month funded before 2026-10-29 reads: nothing to move.
        assert_eq!(refusal(rehome_plan(false, false, &payer)).as_deref(), Some("NotRehomable"));
        // A config naming no payer: never a USDC account nobody could spend.
        for (fanex, rents) in [(true, true), (false, true), (true, false), (false, false)] {
            assert_eq!(refusal(rehome_plan(fanex, rents, &Pubkey::default())).as_deref(), Some("NotRehomable"));
        }
    }

    #[test]
    fn only_the_first_layout_is_read_and_a_fresh_record_is_version_0() {
        assert!(known_version(1));
        for version in [0u8, 2, u8::MAX] {
            assert!(!known_version(version), "{version}");
        }
        assert!(known_record_version(0) && known_record_version(1));
        assert!(!known_record_version(2) && !known_record_version(u8::MAX));
    }

    #[test]
    fn a_month_opens_on_a_fresh_or_voided_record_alone() {
        assert_eq!(refusal(check_month_record(0)), None);
        assert_eq!(refusal(check_month_record(MONTH_STATUS_VOIDED)), None);
        for status in [MONTH_STATUS_OPEN, MONTH_STATUS_CLOSED, 4, u8::MAX] {
            assert_eq!(refusal(check_month_record(status)).as_deref(), Some("MonthAlreadyFunded"), "{status}");
        }
    }
}

#[cfg(test)]
mod pause_tests {
    use super::test_support::refusal;
    use super::*;

    const DAY: i64 = 86_400;
    const NOW: i64 = 1_793_491_200;

    #[test]
    fn claims_are_refused_a_second_before_the_pause_ends_and_open_at_it() {
        assert_eq!(refusal(check_claims_open(NOW - 1, NOW)).as_deref(), Some("ClaimsPaused"));
        assert_eq!(refusal(check_claims_open(NOW, NOW)), None);
        // A fresh config's 0 pauses nothing.
        assert_eq!(refusal(check_claims_open(NOW, 0)), None);
    }

    #[test]
    fn a_pause_lasts_at_most_seven_days_from_now() {
        assert_eq!(refusal(check_claims_pause(NOW + 7 * DAY, NOW, 0)), None);
        assert_eq!(refusal(check_claims_pause(NOW + 7 * DAY + 1, NOW, 0)).as_deref(), Some("PauseTooLong"));
        // Renewed at day 7 for seven more.
        let first = NOW + 7 * DAY;
        assert_eq!(refusal(check_claims_pause(first + 7 * DAY, first, first)), None);
        assert_eq!(refusal(check_claims_pause(first + 7 * DAY + 1, first, first)).as_deref(), Some("PauseTooLong"));
    }

    #[test]
    fn a_pause_lasts_an_hour_at_least_to_the_second() {
        // The internal audit (2026-10-29): a pause ending now, or within the hour, refused no
        // claim yet delayed every expired month's close by 30 days.
        assert_eq!(refusal(check_claims_pause(NOW, NOW, 0)).as_deref(), Some("PauseTooShort"));
        assert_eq!(refusal(check_claims_pause(NOW + 60, NOW, 0)).as_deref(), Some("PauseTooShort"));
        assert_eq!(refusal(check_claims_pause(NOW + 3_599, NOW, 0)).as_deref(), Some("PauseTooShort"));
        assert_eq!(refusal(check_claims_pause(NOW + 3_600, NOW, 0)), None);
        // A running pause that ends within the hour is extended past it, never kept as it is.
        let ending = NOW + 600;
        assert_eq!(refusal(check_claims_pause(ending, NOW, ending)).as_deref(), Some("PauseTooShort"));
        assert_eq!(refusal(check_claims_pause(NOW + 3_600, NOW, ending)), None);
        // A shortening stays a shortening, however short: the end before the running one's.
        assert_eq!(refusal(check_claims_pause(ending - 1, NOW, ending)).as_deref(), Some("PauseEndTooEarly"));
    }

    #[test]
    fn a_pause_only_starts_or_extends() {
        // From a fresh config's 0: an hour from now or later.
        assert_eq!(refusal(check_claims_pause(NOW + 3_600, NOW, 0)), None);
        assert_eq!(refusal(check_claims_pause(NOW + DAY, NOW, 0)), None);
        // A value in the past is refused, so no call can push the close's guard back.
        assert_eq!(refusal(check_claims_pause(NOW - 1, NOW, 0)).as_deref(), Some("PauseEndTooEarly"));
        assert_eq!(refusal(check_claims_pause(0, NOW, 0)).as_deref(), Some("PauseEndTooEarly"));
        // A running pause is kept or extended, never shortened.
        let running = NOW + 3 * DAY;
        assert_eq!(refusal(check_claims_pause(running, NOW, running)), None);
        assert_eq!(refusal(check_claims_pause(running + 1, NOW, running)), None);
        assert_eq!(refusal(check_claims_pause(running - 1, NOW, running)).as_deref(), Some("PauseEndTooEarly"));
        // A pause that already ran out is in the past like any other value.
        assert_eq!(refusal(check_claims_pause(NOW - 10, NOW, NOW - 10)).as_deref(), Some("PauseEndTooEarly"));
    }

    #[test]
    fn a_months_claims_are_refused_a_second_before_its_hold_ends_and_open_at_it() {
        assert_eq!(refusal(check_month_claims_open(NOW - 1, NOW)).as_deref(), Some("ClaimsHeld"));
        assert_eq!(refusal(check_month_claims_open(NOW, NOW)), None);
        // A fresh month's 0 holds nothing.
        assert_eq!(refusal(check_month_claims_open(NOW, 0)), None);
    }

    #[test]
    fn a_resume_ends_a_running_pause_now_and_moves_an_ended_one_nowhere() {
        // A resume built at T and executed 48 hours later leaves its own time.
        let executed = NOW + 2 * DAY + 1;
        assert_eq!(resumed_claims_paused_until(NOW + 7 * DAY, executed), executed);
        // Sent when no pause runs, the value stays as it was: no close's guard moves.
        assert_eq!(resumed_claims_paused_until(NOW, executed), NOW);
        assert_eq!(resumed_claims_paused_until(0, executed), 0);
        assert_eq!(resumed_claims_paused_until(executed, executed), executed);
    }
}

#[cfg(test)]
mod close_and_void_tests {
    use super::test_support::refusal;
    use super::*;

    const DAY: i64 = 86_400;
    const TOTAL: u64 = 369_907_555;
    const EXPIRES: i64 = 1_825_027_200;

    #[test]
    fn a_fully_claimed_month_closes_at_any_time() {
        assert_eq!(refusal(check_close(TOTAL, TOTAL, EXPIRES - 300 * DAY, EXPIRES, EXPIRES, 0)), None);
        // Even inside a claims pause: nothing is owed to anyone.
        assert_eq!(refusal(check_close(TOTAL, TOTAL, EXPIRES, EXPIRES, EXPIRES + 7 * DAY, 0)), None);
    }

    #[test]
    fn a_month_with_money_owed_waits_for_its_expiry() {
        assert_eq!(refusal(check_close(TOTAL - 1, TOTAL, EXPIRES - 1, EXPIRES, 0, 0)).as_deref(), Some("NotExpired"));
        assert_eq!(refusal(check_close(0, TOTAL, EXPIRES - 1, EXPIRES, 0, 0)).as_deref(), Some("NotExpired"));
        assert_eq!(refusal(check_close(0, TOTAL, EXPIRES, EXPIRES, 0, 0)), None);
        assert_eq!(refusal(check_close(163_931_400, TOTAL, EXPIRES + 1, EXPIRES, 0, 0)), None);
    }

    #[test]
    fn an_expired_month_closes_only_after_thirty_days_of_open_claims() {
        let resumed = EXPIRES - DAY;
        let guard = resumed + 30 * DAY;
        assert_eq!(
            refusal(check_close(0, TOTAL, guard - 1, EXPIRES, resumed, 0)).as_deref(),
            Some("ClaimsPausedRecently")
        );
        assert_eq!(refusal(check_close(0, TOTAL, guard, EXPIRES, resumed, 0)), None);
        // A pause held to the expiry and past it keeps the month open for 30 days after its end.
        let held = EXPIRES + 7 * DAY;
        assert_eq!(
            refusal(check_close(0, TOTAL, held + 30 * DAY - 1, EXPIRES, held, 0)).as_deref(),
            Some("ClaimsPausedRecently")
        );
        assert_eq!(refusal(check_close(0, TOTAL, held + 30 * DAY, EXPIRES, held, 0)), None);
        // The guard is apart from the month's one-year life: an old pause delays nothing.
        assert_eq!(refusal(check_close(0, TOTAL, EXPIRES, EXPIRES, EXPIRES - 31 * DAY, 0)), None);
    }

    #[test]
    fn a_months_hold_delays_its_close_as_a_pause_does_counted_from_the_later_of_the_two() {
        // This month held past its expiry, the config never paused: 30 days after the hold's end.
        let held = EXPIRES + 5 * DAY;
        assert_eq!(
            refusal(check_close(0, TOTAL, held + 30 * DAY - 1, EXPIRES, 0, held)).as_deref(),
            Some("ClaimsPausedRecently")
        );
        assert_eq!(refusal(check_close(0, TOTAL, held + 30 * DAY, EXPIRES, 0, held)), None);
        // A claims pause that ended after the hold counts from its own end, and so does a hold
        // that ended after the pause.
        let later = held + 2 * DAY;
        for (claims_paused_until, claims_held_until) in [(later, held), (held, later)] {
            assert_eq!(
                refusal(check_close(0, TOTAL, later + 30 * DAY - 1, EXPIRES, claims_paused_until, claims_held_until))
                    .as_deref(),
                Some("ClaimsPausedRecently")
            );
            assert_eq!(refusal(check_close(0, TOTAL, later + 30 * DAY, EXPIRES, claims_paused_until, claims_held_until)), None);
        }
        // A fully claimed month closes at once, held or not: nothing is owed to anyone.
        assert_eq!(refusal(check_close(TOTAL, TOTAL, EXPIRES - DAY, EXPIRES, 0, EXPIRES + 7 * DAY)), None);
    }

    #[test]
    fn pauses_and_holds_delay_an_expired_months_close_90_days_at_most() {
        // The internal audit (2026-10-29): a pauser renewing a pause within every 30 days kept an
        // expired month from closing for ever. From 90 days after its expiry it closes, whatever ran.
        let cap = EXPIRES + 90 * DAY;
        for (paused, held) in [(cap - DAY, 0), (0, cap - DAY), (cap + 7 * DAY, 0), (cap, cap + 3 * DAY)] {
            assert_eq!(
                refusal(check_close(0, TOTAL, cap - 1, EXPIRES, paused, held)).as_deref(),
                Some("ClaimsPausedRecently"),
                "paused {paused}, held {held}"
            );
            assert_eq!(refusal(check_close(0, TOTAL, cap, EXPIRES, paused, held)), None, "paused {paused}, held {held}");
        }
        // Until then the 30 days count as before: a pause ending 50 days after the expiry delays
        // the close to day 80, under the cap.
        let ended = EXPIRES + 50 * DAY;
        assert_eq!(
            refusal(check_close(0, TOTAL, ended + 30 * DAY - 1, EXPIRES, ended, 0)).as_deref(),
            Some("ClaimsPausedRecently")
        );
        assert_eq!(refusal(check_close(0, TOTAL, ended + 30 * DAY, EXPIRES, ended, 0)), None);
        // The cap is no shortcut before the expiry, nor for a clock at the edge, where a wrapping
        // sum would put the cap in the past: both ends saturate.
        assert_eq!(refusal(check_close(0, TOTAL, EXPIRES - 1, EXPIRES, 0, 0)).as_deref(), Some("NotExpired"));
        let edge = i64::MAX - 1;
        assert_eq!(refusal(check_close(0, TOTAL, edge, edge, edge, 0)).as_deref(), Some("ClaimsPausedRecently"));
    }

    #[test]
    fn only_a_month_nobody_claimed_from_is_voided() {
        assert_eq!(refusal(check_void(0, &[0; 8_192])), None);
        assert_eq!(refusal(check_void(0, &[])), None);
        assert_eq!(refusal(check_void(1, &[0; 2])).as_deref(), Some("MonthHasClaims"));
        // A set bit with nothing counted is refused too, wherever it sits.
        let mut registry = [0u8; 8_192];
        registry[8_191] = 0x80;
        assert_eq!(refusal(check_void(0, &registry)).as_deref(), Some("MonthHasClaims"));
    }
}
