//! Pure launch-parameter validation, extracted from `create_curve`.
//!
//! These rules used to be inline `require!`s inside the instruction handler,
//! where they could not be reached by `cargo test --lib` (a handler needs a
//! Context). They are the rules that decide whether a launch is well-formed, so
//! they are exactly the ones worth testing.

use anchor_lang::prelude::*;
use anchor_spl::associated_token::get_associated_token_address;
use anchor_spl::token::spl_token::solana_program::program_option::COption;
use anchor_spl::token::spl_token::state::Account as SplTokenAccount;

use crate::constants::*;
use crate::errors::FanexError;
use crate::state::LaunchGuardWaiver;

/// Creator vesting bounds. 2% is the `revenue` type's fixed allocation and the
/// floor; 15% is the ceiling. Widening the range does NOT touch live curves:
/// each `Vesting` account stores its own schedule, stamped at creation.
pub const MIN_VESTING_SUPPLY_BPS: u64 = 200;
pub const MAX_VESTING_SUPPLY_BPS: u64 = 1_500;

/// Schedule bounds. `duration = 0` means lump-sum at the cliff (today's
/// `revenue` behaviour); anything larger releases linearly after it.
pub const MAX_VESTING_CLIFF_SECONDS: i64 = 24 * 30 * 24 * 60 * 60;
pub const MAX_VESTING_DURATION_SECONDS: i64 = 48 * 30 * 24 * 60 * 60;

/// The vesting allocation must be a whole number of basis points inside the
/// permitted range. Deriving the bps from the supplied amount keeps this a
/// single instruction argument instead of two that could disagree.
pub fn validate_vesting_supply(vesting_token_supply: u64) -> Result<()> {
    let bps = ((vesting_token_supply as u128) * (BPS_DENOMINATOR as u128)
        / (TOKEN_TOTAL_SUPPLY as u128)) as u64;
    require!(
        bps >= MIN_VESTING_SUPPLY_BPS && bps <= MAX_VESTING_SUPPLY_BPS,
        FanexError::InvalidVestingAllocation
    );
    let expected =
        ((TOKEN_TOTAL_SUPPLY as u128) * (bps as u128) / (BPS_DENOMINATOR as u128)) as u64;
    require!(
        vesting_token_supply.abs_diff(expected) <= 1,
        FanexError::InvalidVestingAllocation
    );
    Ok(())
}

pub fn validate_vesting_schedule(cliff_seconds: i64, duration_seconds: i64) -> Result<()> {
    require!(
        (0..=MAX_VESTING_CLIFF_SECONDS).contains(&cliff_seconds),
        FanexError::InvalidVestingAllocation
    );
    require!(
        (0..=MAX_VESTING_DURATION_SECONDS).contains(&duration_seconds),
        FanexError::InvalidVestingAllocation
    );
    Ok(())
}

/// The creator-target floor exists to guarantee a minimum fundraise. A launch
/// that raises nothing (`equity`: both splits zero, every lamport of a buy goes
/// to the curve) has no target to floor, and forcing it to declare a fake one
/// just to pass a check would put a lie in on-chain state.
pub fn validate_creator_target(creator_target_lamports: u64, split_sum_bps: u64) -> Result<()> {
    if split_sum_bps == 0 {
        return Ok(());
    }
    require!(
        creator_target_lamports >= MIN_CREATOR_TARGET_LAMPORTS,
        FanexError::CreatorTargetTooLow
    );
    Ok(())
}

/// A `Vesting` account may have its beneficiary reassigned **only** while it
/// still points at the fan-led placeholder. Once a real creator has claimed it
/// the allocation is theirs and no authority can move it — which is what stops
/// `set_vesting_beneficiary` from ever becoming a way to seize a creator's
/// tokens, even with the admin key.
pub fn validate_beneficiary_reassignable(current: Pubkey, placeholder: Pubkey) -> Result<()> {
    require_keys_eq!(current, placeholder, FanexError::VestingNotReassignable);
    Ok(())
}

/// `Curve.creator` may be copied from the vesting beneficiary **only** while it
/// still holds the fan-led placeholder AND the vesting has already been handed
/// to a real wallet by `set_vesting_beneficiary`.
///
/// This is what keeps `sync_curve_creator` from widening anything: it takes no
/// address, so the only value it can ever write is one that the placeholder-only
/// `set_vesting_beneficiary` already wrote, and once `Curve.creator` has left the
/// placeholder no authority can move it again.
pub fn validate_curve_creator_syncable(
    current_creator: Pubkey,
    vesting_beneficiary: Pubkey,
    placeholder: Pubkey,
) -> Result<()> {
    require_keys_eq!(current_creator, placeholder, FanexError::CurveCreatorNotSyncable);
    require_keys_neq!(vesting_beneficiary, placeholder, FanexError::CurveCreatorNotSyncable);
    require_keys_neq!(vesting_beneficiary, Pubkey::default(), FanexError::CurveCreatorNotSyncable);
    Ok(())
}

/// A curve that routes a fundraise split to its creator must not name the
/// keyless placeholder as that creator: `buy` transfers the split to
/// `Curve.creator` inside every trade, and SOL sent to a PDA no instruction
/// signs for is gone. A curve with a zero creator split (`equity`) never pays
/// `Curve.creator` anything on a trade, so the placeholder stays legal there,
/// which is exactly the fan-led launch.
pub fn validate_creator_can_receive_raise(
    creator: Pubkey,
    placeholder: Pubkey,
    creator_split_bps: u64,
) -> Result<()> {
    if creator_split_bps > 0 {
        require_keys_neq!(creator, placeholder, FanexError::PlaceholderCannotRaise);
    }
    Ok(())
}

/// The extra accounts a caller passed after a payout instruction's own list
/// (`sell`, `claim_dividend`) must be exactly the payout transit the program
/// derived for that source: one account, that address.
///
/// The caller decides the route by passing the transit or not; an EMPTY list is
/// the direct move and never reaches this check. Anything else refuses: a
/// transit is the `from` of a transfer the program signs, so the only address
/// it may ever be is the one the program derived itself.
pub fn validate_payout_transit(extra_accounts: &[Pubkey], expected_transit: &Pubkey) -> Result<()> {
    require!(extra_accounts.len() == 1, FanexError::AccountMismatch);
    require_keys_eq!(extra_accounts[0], *expected_transit, FanexError::AccountMismatch);
    Ok(())
}

#[cfg(test)]
mod payout_transit_tests {
    use super::validate_payout_transit;
    use crate::constants::PAYOUT_TRANSIT_SEED;
    use anchor_lang::prelude::Pubkey;

    fn transit_of(source: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[PAYOUT_TRANSIT_SEED, source.as_ref()], &crate::ID).0
    }

    #[test]
    fn accepts_exactly_the_transit_derived_for_the_source() {
        let vault = Pubkey::new_unique();
        assert!(validate_payout_transit(&[transit_of(&vault)], &transit_of(&vault)).is_ok());
    }

    #[test]
    fn refuses_any_other_address_as_the_transit() {
        // A client that could name the transit could name any account as the
        // `from` of a transfer the program signs for.
        let vault = Pubkey::new_unique();
        let expected = transit_of(&vault);
        assert!(validate_payout_transit(&[Pubkey::new_unique()], &expected).is_err());
        assert!(validate_payout_transit(&[vault], &expected).is_err());
        assert!(validate_payout_transit(&[transit_of(&Pubkey::new_unique())], &expected).is_err());
    }

    #[test]
    fn refuses_a_list_that_is_not_one_account() {
        let vault = Pubkey::new_unique();
        let expected = transit_of(&vault);
        assert!(validate_payout_transit(&[], &expected).is_err());
        assert!(validate_payout_transit(&[expected, expected], &expected).is_err());
        assert!(validate_payout_transit(&[expected, Pubkey::new_unique()], &expected).is_err());
    }

    #[test]
    fn gives_each_source_its_own_off_curve_transit() {
        // Per source: a sell never shares a write lock with another curve's
        // sells, a claim never with another month's claims. Off-curve: no
        // private key exists, so only this program can sign for it.
        let a = Pubkey::new_unique();
        let b = Pubkey::new_unique();
        assert_ne!(transit_of(&a), transit_of(&b));
        assert!(!transit_of(&a).is_on_curve());
    }

    #[test]
    fn keeps_the_seed_the_typescript_client_derives_with() {
        // Mirrored by `SEED.payoutTransit` in lib/solana/pepper-curve/program.ts,
        // held from that side by payout-transit.test.ts, which reads this file.
        assert_eq!(PAYOUT_TRANSIT_SEED, b"payout");
    }
}

/// Where a `ClaimStatus`'s rent returns on close, by on-chain FORMAT.
///
/// 56 bytes = pre-A+D account: the claimant paid their own rent, it goes back
/// to the `claimant` field (bytes 8..40). 88 bytes = current account: the
/// recorded `rent_payer` (bytes 56..88) paid — the Fanex sponsor in the
/// ordinary flow. Any other length is not a ClaimStatus this program wrote.
pub fn claim_status_refund_range(data_len: usize) -> Option<core::ops::Range<usize>> {
    match data_len {
        56 => Some(8..40),
        88 => Some(56..88),
        _ => None,
    }
}

#[cfg(test)]
mod close_claim_status_tests {
    use super::claim_status_refund_range;

    #[test]
    fn legacy_accounts_refund_the_claimant_who_paid_their_own_rent() {
        assert_eq!(claim_status_refund_range(56), Some(8..40));
    }

    #[test]
    fn sponsored_accounts_refund_the_recorded_rent_payer() {
        assert_eq!(claim_status_refund_range(88), Some(56..88));
    }

    #[test]
    fn any_other_size_is_not_ours_and_never_pays_out() {
        for len in [0, 8, 40, 55, 57, 87, 89, 1024] {
            assert_eq!(claim_status_refund_range(len), None, "len {len}");
        }
    }
}

/// May the funder close a distributor and sweep its remaining lamports? The
/// same rule for both versions (`close_distributor`, `close_distributor_v2`).
///
/// `close = funder` returns the WHOLE balance — the rent-exemption reserve AND
/// any unclaimed remainder — so this is the rule standing between a funder and
/// money that still belongs to holders.
///
/// Two ways in:
///
/// 1. **Fully claimed.** Nothing is owed, so the only lamports left are the rent.
///    Making the funder wait out the expiry to recover ~0.0018 SOL is waste.
///    ⚠️ Note a fully-claimed distributor is NOT at zero balance — it sits at
///    exactly that reserve — so a keeper looking for "balance == 0" finds none.
/// 2. **Expired.** The escape hatch for a month holders never fully claimed.
///
/// ⚠️ The expiry cannot be dropped in favour of (1): without a time bound a
/// funder could close a PARTIALLY claimed month and take back dividends still
/// owed. The disjunction is what makes that impossible.
pub fn may_close_distributor(
    claimed_amount: u64,
    total_amount: u64,
    now: i64,
    expires_at: i64,
) -> bool {
    claimed_amount == total_amount || now >= expires_at
}

#[cfg(test)]
mod close_distributor_tests {
    use super::may_close_distributor;

    const TOTAL: u64 = 369_907_555;
    const EXPIRES: i64 = 1_796_000_000;

    #[test]
    fn allows_an_early_close_once_every_holder_has_claimed() {
        assert!(may_close_distributor(TOTAL, TOTAL, EXPIRES - 1, EXPIRES));
    }

    #[test]
    fn refuses_while_a_single_lamport_is_still_owed() {
        // The whole point: an early close must not sweep money holders can claim.
        assert!(!may_close_distributor(TOTAL - 1, TOTAL, EXPIRES - 1, EXPIRES));
    }

    #[test]
    fn refuses_a_partially_claimed_month_before_expiry() {
        // The live MARCO1 shape on 2026-09-02: 44% claimed, 90 days to run.
        assert!(!may_close_distributor(163_931_400, TOTAL, EXPIRES - 1, EXPIRES));
    }

    #[test]
    fn allows_the_expiry_escape_hatch_even_with_funds_unclaimed() {
        assert!(may_close_distributor(0, TOTAL, EXPIRES, EXPIRES));
        assert!(may_close_distributor(0, TOTAL, EXPIRES + 1, EXPIRES));
    }

    #[test]
    fn treats_an_empty_distributor_as_closeable() {
        assert!(may_close_distributor(0, 0, 0, EXPIRES));
    }
}

/// Who may fund a dividend month on v1, used only above `MAX_DISTRIBUTOR_V2_LEAVES`
/// payable wallets. The distributor PDA is `init`-once per (mint, month), so whoever
/// may fund a month may also squat it: commit a root of their own and keep the month
/// from ever being funded again. v1 has no approver, so one config key alone:
/// `config.graduation_authority`, the server's, since 2026-10-24 (the fee-wallet
/// split; the fee wallet until then, now receive-only). ⚠️ Not the dividend payer
/// since 2026-10-08 (security card point 5): with its key stolen, a v1 month needed
/// nothing else, and the thief could squat the future months of every token.
pub fn may_fund_distributor_v1(funder: &Pubkey, graduation_authority: &Pubkey) -> bool {
    funder == graduation_authority
}

/// Who may fund a dividend month on v2: the dividend payer `DIVIDEND_PAYER`
/// (2026-10-07) and the curve's creator, each only with the graduation authority's
/// approval (`InitDistributorV2::approver`). No config key funds one since
/// 2026-10-24 (the fee-wallet split): the fee wallet is receive-only, and the
/// approver's key alone must open no month.
pub fn may_fund_distributor_v2(funder: &Pubkey, creator: &Pubkey) -> bool {
    *funder == DIVIDEND_PAYER || funder == creator
}

/// Whom `set_distributor_funder` may hand a v1 month's close to: the dividend payer
/// alone (2026-10-24), whose key closes Fanex's months on the server, so the months
/// the hot wallet funded no longer need its key.
pub fn may_rehome_distributor_funder(new_funder: &Pubkey) -> bool {
    *new_funder == DIVIDEND_PAYER
}

/// Whether a distributor's `expires_at` is within `MAX_DISTRIBUTOR_EXPIRY_SECONDS`
/// of `now` (2026-10-08, security card point 5): a month a funder squats stays
/// blocked at most that long, whoever held the key, since anyone may close a month
/// once it has expired (`close_expired_distributor`, `close_expired_distributor_v2`,
/// the internal audit, 2026-10-29; before them only its funder could, so a month it
/// never closed blocked its address for good).
pub fn expiry_in_range(expires_at: i64, now: i64) -> bool {
    expires_at <= now.saturating_add(MAX_DISTRIBUTOR_EXPIRY_SECONDS)
}

/// Whether a buy may leave its buyer's token account holding `holding +
/// tokens_out` (the launch cap, anti-sniper, 2026-10-12): always once the
/// launch guard is over, and up to `LAUNCH_WALLET_CAP` inside it. The guard runs
/// for `LAUNCH_GUARD_SECONDS` from `created_at`; a clock behind `created_at` is
/// inside it, and a holding that overflows is over the cap.
pub fn launch_cap_allows(created_at: i64, now: i64, holding: u64, tokens_out: u64) -> bool {
    if now >= created_at.saturating_add(LAUNCH_GUARD_SECONDS) {
        return true;
    }
    holding.checked_add(tokens_out).is_some_and(|after| after <= LAUNCH_WALLET_CAP)
}

/// Whether `claim_vesting` may release anything for a curve (2026-10-15): one
/// created before `VESTING_LOCKED_UNTIL_GRADUATION_FROM` keeps the terms it
/// launched with; a later one releases only once `Graduated`, so its vested
/// tokens never reach the reserve that backs the holders' sales.
pub fn vesting_claim_allowed(created_at: i64, phase: crate::state::CurvePhase) -> bool {
    created_at < VESTING_LOCKED_UNTIL_GRADUATION_FROM || phase == crate::state::CurvePhase::Graduated
}

/// Whether `accounts`, a buy's extra accounts, hold the `LaunchGuardWaiver` of
/// `mint` (the launch cap's switch, 2026-10-14): an account this program owns,
/// with the waiver's discriminator and that mint, at the mint's waiver PDA. Only
/// `waive_launch_guard` makes one, signed by the launch guard authority before
/// the curve exists; anything else passed here is ignored, and the cap applies.
pub fn launch_guard_waived(accounts: &[AccountInfo], mint: &Pubkey) -> bool {
    accounts.iter().any(|info| {
        if info.owner != &crate::ID || info.lamports() == 0 {
            return false;
        }
        let Ok(bytes) = info.try_borrow_data() else {
            return false;
        };
        // The discriminator first: another account type of this program never reads as a waiver.
        let Ok(waiver) = LaunchGuardWaiver::try_deserialize(&mut &bytes[..]) else {
            return false;
        };
        waiver.mint == *mint
            && Pubkey::create_program_address(
                &[LAUNCH_GUARD_WAIVER_SEED, mint.as_ref(), &[waiver.bump]],
                &crate::ID,
            )
            .is_ok_and(|address| address == *info.key)
    })
}

#[cfg(test)]
mod launch_guard_waiver_tests {
    use super::launch_guard_waived;
    use crate::constants::LAUNCH_GUARD_WAIVER_SEED;
    use crate::state::LaunchGuardWaiver;
    use anchor_lang::prelude::*;

    /// A waiver as `waive_launch_guard` writes it: the discriminator, the mint, the bump.
    fn data(mint: Pubkey, bump: u8) -> Vec<u8> {
        let mut out = Vec::new();
        LaunchGuardWaiver { mint, bump }.try_serialize(&mut out).unwrap();
        out
    }

    fn pda(mint: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[LAUNCH_GUARD_WAIVER_SEED, mint.as_ref()], &crate::ID)
    }

    /// Whether one account, of these parts, reads as `mint`'s waiver.
    fn waived(key: Pubkey, owner: Pubkey, lamports: u64, mut bytes: Vec<u8>, mint: &Pubkey) -> bool {
        let mut lamports = lamports;
        let info = AccountInfo::new(&key, false, false, &mut lamports, &mut bytes, &owner, false, 0);
        launch_guard_waived(&[info], mint)
    }

    #[test]
    fn the_mints_own_waiver_lifts_the_cap() {
        let mint = Pubkey::new_unique();
        let (key, bump) = pda(&mint);
        assert!(waived(key, crate::ID, 900_000, data(mint, bump), &mint));
    }

    #[test]
    fn another_mints_waiver_does_not() {
        let mint = Pubkey::new_unique();
        let other = Pubkey::new_unique();
        let (key, bump) = pda(&other);
        assert!(!waived(key, crate::ID, 900_000, data(other, bump), &mint));
    }

    #[test]
    fn a_waiver_this_program_does_not_own_does_not() {
        let mint = Pubkey::new_unique();
        let (key, bump) = pda(&mint);
        assert!(!waived(key, Pubkey::new_unique(), 900_000, data(mint, bump), &mint));
    }

    #[test]
    fn a_waiver_away_from_its_pda_does_not() {
        let mint = Pubkey::new_unique();
        let (_, bump) = pda(&mint);
        assert!(!waived(Pubkey::new_unique(), crate::ID, 900_000, data(mint, bump), &mint));
    }

    #[test]
    fn an_empty_or_unrelated_account_does_not() {
        let mint = Pubkey::new_unique();
        let (key, bump) = pda(&mint);
        // No lamports: not initialized.
        assert!(!waived(key, crate::ID, 0, data(mint, bump), &mint));
        // Another account's bytes under the waiver's address.
        assert!(!waived(key, crate::ID, 900_000, vec![0; 41], &mint));
        assert!(!launch_guard_waived(&[], &mint));
    }
}

#[cfg(test)]
mod launch_cap_tests {
    use super::launch_cap_allows;
    use crate::constants::{LAUNCH_GUARD_SECONDS, LAUNCH_WALLET_CAP};

    const LAUNCH: i64 = 1_791_158_400;

    #[test]
    fn inside_the_guard_a_wallet_holds_at_most_the_cap() {
        assert!(launch_cap_allows(LAUNCH, LAUNCH, 0, LAUNCH_WALLET_CAP));
        assert!(!launch_cap_allows(LAUNCH, LAUNCH, 0, LAUNCH_WALLET_CAP + 1));
        assert!(launch_cap_allows(LAUNCH, LAUNCH + 10, 0, 1));
    }

    #[test]
    fn the_holding_counts_so_buys_add_up() {
        let half = LAUNCH_WALLET_CAP / 2;
        assert!(launch_cap_allows(LAUNCH, LAUNCH + 10, half, half));
        assert!(!launch_cap_allows(LAUNCH, LAUNCH + 10, half, half + 1));
        assert!(!launch_cap_allows(LAUNCH, LAUNCH + 10, LAUNCH_WALLET_CAP, 1));
    }

    #[test]
    fn the_guard_lasts_five_minutes_from_the_launch() {
        let over = LAUNCH_WALLET_CAP * 10;
        assert!(!launch_cap_allows(LAUNCH, LAUNCH + LAUNCH_GUARD_SECONDS - 1, 0, over));
        assert!(launch_cap_allows(LAUNCH, LAUNCH + LAUNCH_GUARD_SECONDS, 0, over));
        assert!(launch_cap_allows(LAUNCH, LAUNCH + 365 * 24 * 60 * 60, LAUNCH_WALLET_CAP, over));
    }

    #[test]
    fn a_clock_behind_the_launch_is_inside_the_guard() {
        assert!(!launch_cap_allows(LAUNCH, LAUNCH - 5, 0, LAUNCH_WALLET_CAP + 1));
    }

    #[test]
    fn a_holding_that_overflows_is_over_the_cap() {
        assert!(!launch_cap_allows(LAUNCH, LAUNCH, u64::MAX, 1));
        assert!(launch_cap_allows(i64::MAX, i64::MAX, u64::MAX, 1));
    }
}

#[cfg(test)]
mod distributor_funder_tests {
    use super::{
        expiry_in_range, may_fund_distributor_v1, may_fund_distributor_v2,
        may_rehome_distributor_funder,
    };
    use crate::constants::{DIVIDEND_PAYER, MAX_DISTRIBUTOR_EXPIRY_SECONDS};
    use anchor_lang::prelude::Pubkey;

    fn key(byte: u8) -> Pubkey {
        Pubkey::new_from_array([byte; 32])
    }

    #[test]
    fn the_dividend_payer_is_the_wallet_that_was_named() {
        assert_eq!(DIVIDEND_PAYER.to_string(), "5UZrYBpbmG1kZKobQbM1Jqn8JDCfpSaR8sTm9ApzC554");
    }

    // The fee-wallet split (2026-10-24): one config key funds v1, none funds v2.
    #[test]
    fn the_graduation_authority_funds_only_a_v1_month() {
        let (graduation_authority, creator) = (key(1), key(2));
        assert!(may_fund_distributor_v1(&graduation_authority, &graduation_authority));
        assert!(!may_fund_distributor_v2(&graduation_authority, &creator));
    }

    // Security card point 5 (2026-10-08): a v1 month needed the payer's key
    // alone, so a stolen key could squat every token's future months.
    #[test]
    fn the_dividend_payer_funds_only_a_v2_month() {
        let (graduation_authority, creator) = (key(1), key(2));
        assert!(!may_fund_distributor_v1(&DIVIDEND_PAYER, &graduation_authority));
        assert!(may_fund_distributor_v2(&DIVIDEND_PAYER, &creator));
    }

    #[test]
    fn a_distributor_expires_within_400_days() {
        let now = 1_791_000_000;
        assert!(expiry_in_range(now + 365 * 24 * 60 * 60, now));
        assert!(expiry_in_range(now + MAX_DISTRIBUTOR_EXPIRY_SECONDS, now));
        assert!(!expiry_in_range(now + MAX_DISTRIBUTOR_EXPIRY_SECONDS + 1, now));
        assert!(!expiry_in_range(i64::MAX, now));
        assert!(expiry_in_range(now, i64::MAX));
    }

    #[test]
    fn the_creator_funds_only_a_v2_month() {
        let (graduation_authority, creator) = (key(1), key(2));
        assert!(!may_fund_distributor_v1(&creator, &graduation_authority));
        assert!(may_fund_distributor_v2(&creator, &creator));
    }

    #[test]
    fn anyone_else_funds_nothing() {
        let (graduation_authority, creator, stranger) = (key(1), key(2), key(3));
        assert!(!may_fund_distributor_v1(&stranger, &graduation_authority));
        assert!(!may_fund_distributor_v2(&stranger, &creator));
    }

    #[test]
    fn a_v1_month_is_handed_over_to_the_dividend_payer_alone() {
        assert!(may_rehome_distributor_funder(&DIVIDEND_PAYER));
        for other in [key(1), key(2), Pubkey::default()] {
            assert!(!may_rehome_distributor_funder(&other));
        }
    }
}

/// Bytes of a `DistributorV2` claim registry for `leaf_count` leaves: one bit each.
pub fn claim_registry_bytes(leaf_count: u32) -> usize {
    (leaf_count as usize).div_ceil(8)
}

/// Can a `DistributorV2` commit to `leaf_count` leaves? At least one, at most
/// `MAX_DISTRIBUTOR_V2_LEAVES`.
pub fn leaf_count_in_range(leaf_count: u32) -> bool {
    (1..=MAX_DISTRIBUTOR_V2_LEAVES).contains(&leaf_count)
}

/// Where leaf `index`'s bit lives in a claim registry: (byte, mask), least
/// significant bit first. `lib/solana/distributor-status.ts` reads it with the
/// same order (`isLeafClaimed`), pinned by the same vector in both test suites.
pub fn leaf_bit(index: u32) -> (usize, u8) {
    ((index / 8) as usize, 1u8 << (index % 8))
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
        assert_eq!(claim_registry_bytes(MAX_DISTRIBUTOR_V2_LEAVES), 8_192);
    }

    #[test]
    fn a_registry_needs_at_least_one_leaf_and_at_most_the_maximum() {
        assert!(!leaf_count_in_range(0));
        assert!(leaf_count_in_range(1));
        assert!(leaf_count_in_range(MAX_DISTRIBUTOR_V2_LEAVES));
        assert!(!leaf_count_in_range(MAX_DISTRIBUTOR_V2_LEAVES + 1));
        assert!(!leaf_count_in_range(u32::MAX));
    }

    // The same vector is pinned in lib/solana/distributor-status.test.ts.
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
    fn the_space_clamps_an_absurd_leaf_count_so_the_handler_can_refuse_it() {
        use crate::state::DistributorV2;
        assert_eq!(DistributorV2::HEADER_SPACE, 145);
        assert_eq!(DistributorV2::space(14), 147);
        assert_eq!(DistributorV2::space(u32::MAX), DistributorV2::space(MAX_DISTRIBUTOR_V2_LEAVES));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_curve_created_before_the_threshold_keeps_its_terms_in_any_phase() {
        use crate::state::CurvePhase;
        let before = VESTING_LOCKED_UNTIL_GRADUATION_FROM - 1;
        assert!(vesting_claim_allowed(before, CurvePhase::Bonding));
        assert!(vesting_claim_allowed(before, CurvePhase::Graduating));
        assert!(vesting_claim_allowed(before, CurvePhase::Graduated));
    }

    #[test]
    fn a_curve_created_from_the_threshold_releases_only_once_graduated() {
        use crate::state::CurvePhase;
        for created_at in [VESTING_LOCKED_UNTIL_GRADUATION_FROM, VESTING_LOCKED_UNTIL_GRADUATION_FROM + 86_400] {
            assert!(!vesting_claim_allowed(created_at, CurvePhase::Bonding));
            assert!(!vesting_claim_allowed(created_at, CurvePhase::Graduating));
            assert!(vesting_claim_allowed(created_at, CurvePhase::Graduated));
        }
    }

    #[test]
    fn the_threshold_is_2026_11_01_utc() {
        assert_eq!(VESTING_LOCKED_UNTIL_GRADUATION_FROM, 1_793_491_200);
    }

    const TOTAL: u64 = 100_000_000 * 1_000_000;

    #[test]
    fn accepts_two_percent_the_revenue_default() {
        assert!(validate_vesting_supply(TOTAL * 200 / 10_000).is_ok());
    }

    #[test]
    fn accepts_ten_percent_for_equity() {
        assert!(validate_vesting_supply(TOTAL * 1_000 / 10_000).is_ok());
    }

    #[test]
    fn rejects_one_percent_below_the_floor() {
        assert!(validate_vesting_supply(TOTAL * 100 / 10_000).is_err());
    }

    #[test]
    fn rejects_sixteen_percent_above_the_ceiling() {
        assert!(validate_vesting_supply(TOTAL * 1_600 / 10_000).is_err());
    }

    #[test]
    fn rejects_a_supply_that_is_not_a_whole_bps() {
        // 10% plus a large sliver: in range by ratio, but not an exact bps.
        assert!(validate_vesting_supply(TOTAL * 1_000 / 10_000 + 1_000).is_err());
    }

    #[test]
    fn accepts_the_lump_sum_schedule_used_today() {
        assert!(validate_vesting_schedule(12 * 30 * 24 * 60 * 60, 0).is_ok());
    }

    // The app passes this from TypeScript; the program only bounds it. Shortened
    // from 6 + 18 on 2026-09-08, and both ends still sit well inside the bounds,
    // which is why that change needed no redeploy.
    #[test]
    fn accepts_the_equity_schedule_three_month_cliff_nine_month_linear() {
        assert!(validate_vesting_schedule(3 * 30 * 24 * 60 * 60, 9 * 30 * 24 * 60 * 60).is_ok());
    }

    #[test]
    fn rejects_a_negative_cliff() {
        assert!(validate_vesting_schedule(-1, 0).is_err());
    }

    #[test]
    fn rejects_a_cliff_beyond_twenty_four_months() {
        assert!(validate_vesting_schedule(25 * 30 * 24 * 60 * 60, 0).is_err());
    }

    #[test]
    fn rejects_a_duration_beyond_forty_eight_months() {
        assert!(validate_vesting_schedule(0, 49 * 30 * 24 * 60 * 60).is_err());
    }

    #[test]
    fn target_floor_applies_when_there_is_a_raise() {
        assert!(validate_creator_target(15 * LAMPORTS_PER_SOL, 7_043).is_err());
        assert!(validate_creator_target(16 * LAMPORTS_PER_SOL, 7_043).is_ok());
    }

    #[test]
    fn target_floor_is_skipped_when_there_is_no_raise() {
        // Equity: both splits are zero, so no lamport is ever routed to a target.
        assert!(validate_creator_target(0, 0).is_ok());
    }
    #[test]
    fn allows_reassigning_a_vesting_still_on_the_placeholder() {
        let placeholder = Pubkey::new_unique();
        assert!(validate_beneficiary_reassignable(placeholder, placeholder).is_ok());
    }

    #[test]
    fn refuses_reassigning_a_real_creators_vesting() {
        let placeholder = Pubkey::new_unique();
        let a_real_creator = Pubkey::new_unique();
        assert!(validate_beneficiary_reassignable(a_real_creator, placeholder).is_err());
    }

    #[test]
    fn refuses_a_vesting_already_reassigned_once() {
        // The second attempt on the same curve must not be able to take the
        // allocation back from the creator who already claimed it.
        let placeholder = Pubkey::new_unique();
        let first_claimant = Pubkey::new_unique();
        assert!(validate_beneficiary_reassignable(first_claimant, placeholder).is_err());
    }

    #[test]
    fn refuses_the_default_pubkey() {
        // All-zero bytes must not be mistaken for "unset, therefore free".
        let placeholder = Pubkey::new_unique();
        assert!(validate_beneficiary_reassignable(Pubkey::default(), placeholder).is_err());
    }
    // ── sync_curve_creator ─────────────────────────────────────────────────

    #[test]
    fn syncs_a_placeholder_creator_to_an_already_claimed_vesting() {
        let placeholder = Pubkey::new_unique();
        let claimant = Pubkey::new_unique();
        assert!(validate_curve_creator_syncable(placeholder, claimant, placeholder).is_ok());
    }

    #[test]
    fn refuses_to_touch_a_real_creator() {
        // The property the instruction rests on: a curve launched by a real
        // creator, or one already synced once, is unreachable from here.
        let placeholder = Pubkey::new_unique();
        let real_creator = Pubkey::new_unique();
        let someone_else = Pubkey::new_unique();
        assert!(validate_curve_creator_syncable(real_creator, someone_else, placeholder).is_err());
        assert!(validate_curve_creator_syncable(real_creator, real_creator, placeholder).is_err());
    }

    #[test]
    fn refuses_while_the_vesting_is_still_on_the_placeholder() {
        // Nothing has been claimed: syncing would be a no-op at best, and the
        // instruction must never be a way to "claim" by itself.
        let placeholder = Pubkey::new_unique();
        assert!(validate_curve_creator_syncable(placeholder, placeholder, placeholder).is_err());
    }

    #[test]
    fn refuses_an_all_zero_vesting_beneficiary() {
        let placeholder = Pubkey::new_unique();
        assert!(validate_curve_creator_syncable(placeholder, Pubkey::default(), placeholder).is_err());
    }

    // ── create_curve: a raise can never pay the placeholder ────────────────

    #[test]
    fn refuses_the_placeholder_as_creator_of_a_curve_that_raises() {
        // A revenue launch: every buy sends the creator split to Curve.creator.
        let placeholder = Pubkey::new_unique();
        assert!(validate_creator_can_receive_raise(placeholder, placeholder, 3_760).is_err());
        assert!(validate_creator_can_receive_raise(placeholder, placeholder, 1).is_err());
    }

    #[test]
    fn allows_the_placeholder_on_a_curve_with_no_raise() {
        // Equity: a zero creator split, so Curve.creator is paid nothing on a
        // trade. This is the fan-led launch and must keep working.
        let placeholder = Pubkey::new_unique();
        assert!(validate_creator_can_receive_raise(placeholder, placeholder, 0).is_ok());
    }

    #[test]
    fn allows_a_real_creator_with_a_raise() {
        let placeholder = Pubkey::new_unique();
        let creator = Pubkey::new_unique();
        assert!(validate_creator_can_receive_raise(creator, placeholder, 3_760).is_ok());
    }

    #[test]
    fn fan_led_placeholder_is_stable_and_off_curve() {
        // find_program_address only ever returns off-curve addresses, so a PDA
        // provably has no private key. Pinning it here turns an accidental seed
        // change into a failing test rather than a set of stranded tokens.
        let (pda, _bump) = Pubkey::find_program_address(&[FAN_LED_PLACEHOLDER_SEED], &crate::ID);
        assert_eq!(
            pda,
            crate::instructions::set_vesting_beneficiary::fan_led_placeholder()
        );
        assert_ne!(pda, Pubkey::default());
    }
}

// ── The USDC curve (design §10) ─────────────────────────────────────────────
//
// The rules the USDC handlers check, on plain values so they are tested here with no AccountInfo.
// Amounts in µUSDC. The SOL rules above stay as they are: `create_curve` keeps its inline checks and
// `validate_creator_target` its SOL floor (§10.1).

/// The two raise splits of a USDC launch, summed (§10.3.4). `CurveUsdc` keeps each as a u16, so the
/// sum is taken in u64 and cannot overflow: 65,535 + 65,535 is refused as a split
/// (`InvalidSplitBps`), never as `MathOverflow`. Under 10,000, as today, so the curve keeps a share.
/// Returns the sum, which the creator target's floor reads.
pub fn usdc_split_sum_bps(creator_split_bps: u16, fanex_split_bps: u16) -> Result<u64> {
    let sum = u64::from(creator_split_bps) + u64::from(fanex_split_bps);
    require!(sum < BPS_DENOMINATOR, FanexError::InvalidSplitBps);
    Ok(sum)
}

/// `validate_creator_target` with its floor a parameter: `MIN_CREATOR_TARGET_USDC` on a USDC launch.
/// A launch that raises nothing (both splits zero) has no target to floor.
pub fn validate_creator_target_floor(
    creator_target: u64,
    split_sum_bps: u64,
    floor: u64,
) -> Result<()> {
    if split_sum_bps == 0 {
        return Ok(());
    }
    require!(creator_target >= floor, FanexError::CreatorTargetTooLow);
    Ok(())
}

/// The graduation threshold of a USDC launch within its bounds (§10.8): today's `cpmm_seed` bounds
/// in µUSDC, refused with today's `CpmmSeedOutOfRange`.
pub fn validate_graduation_threshold_usdc(graduation_threshold_usdc: u64) -> Result<()> {
    require!(
        (MIN_GRADUATION_THRESHOLD_USDC..=CAP_GRADUATION_THRESHOLD_USDC)
            .contains(&graduation_threshold_usdc),
        FanexError::CpmmSeedOutOfRange
    );
    Ok(())
}

/// The creator's and Fanex's caps sum to the creator target within 1 base unit, as today's
/// `create_curve` checks inline. A sum past u64 is refused as caps (`InvalidCaps`), where today's
/// handler refuses it as `MathOverflow`.
pub fn validate_caps_sum(creator_cap: u64, fanex_cap: u64, creator_target: u64) -> Result<()> {
    let sum = creator_cap
        .checked_add(fanex_cap)
        .ok_or(FanexError::InvalidCaps)?;
    require!(sum.abs_diff(creator_target) <= 1, FanexError::InvalidCaps);
    Ok(())
}

/// The token supply split between the curve, the migration reserve and the vesting adds up to
/// `TOKEN_TOTAL_SUPPLY` exactly, as today's `create_curve` checks inline, an overflowing sum
/// refused as `MathOverflow` as there (§10.3.4: «exactly as today»). Since the internal audit
/// (2026-10-29) neither the curve's part nor the pool's may be 0 (`InvalidSupplyAllocation`): with
/// no migration tokens a sold-out curve seeds a pool with no token side, which CPMM refuses, and the
/// curve waits in Graduating, its holders' USDC with it, until an upgrade; with no curve tokens it
/// never trades.
pub fn validate_supply_allocation(
    curve_supply: u64,
    migration_supply: u64,
    vesting_supply: u64,
) -> Result<()> {
    let sum = curve_supply
        .checked_add(migration_supply)
        .ok_or(FanexError::MathOverflow)?
        .checked_add(vesting_supply)
        .ok_or(FanexError::MathOverflow)?;
    require!(
        sum == TOKEN_TOTAL_SUPPLY,
        FanexError::InvalidSupplyAllocation
    );
    require!(
        curve_supply > 0 && migration_supply > 0,
        FanexError::InvalidSupplyAllocation
    );
    Ok(())
}

/// A revenue launch's graduation threshold, µUSDC, from its creator target (ECONOMIC-MODEL §3.1, the
/// seed's formula at the USDC bounds): `max(MIN, min(CAP, ⌊target × 3 / 2⌋))`, computed in u128, so
/// no target overflows it. The launch builder computes the same, in integers.
pub fn revenue_graduation_threshold_usdc(creator_target_usdc: u64) -> u64 {
    let linear = u128::from(creator_target_usdc) * u128::from(REVENUE_THRESHOLD_SLOPE_NUMERATOR)
        / u128::from(REVENUE_THRESHOLD_SLOPE_DENOMINATOR);
    let capped = linear.min(u128::from(CAP_GRADUATION_THRESHOLD_USDC)) as u64;
    capped.max(MIN_GRADUATION_THRESHOLD_USDC)
}

/// Whether `value` is within one base unit of `share_pct`% of `whole`: |100 × value − share_pct ×
/// whole| ≤ 100, in u128.
fn within_one_unit_of_share(value: u64, whole: u64, share_pct: u64) -> bool {
    let lhs = u128::from(value) * 100;
    let rhs = u128::from(whole) * u128::from(share_pct);
    lhs.abs_diff(rhs) <= 100
}

/// Whether a split of `bps` is within one basis point of `cap / total`: |bps × total − 10,000 ×
/// cap| ≤ total, in u128. The builder rounds the exact rational to the nearest bp (half a bp off at
/// most, a float's error aside).
fn within_one_bp_of(bps: u16, cap: u64, total: u64) -> bool {
    let lhs = u128::from(bps) * u128::from(total);
    let rhs = u128::from(cap) * u128::from(BPS_DENOMINATOR);
    lhs.abs_diff(rhs) <= u128::from(total)
}

/// The raise terms of a USDC launch held to the economic model's relation (the internal audit,
/// 2026-10-29; ECONOMIC-MODEL §4.2), `LaunchTermsOutOfBounds` otherwise, so the launcher's key alone
/// cannot launch a curve that pays its creator more of a buy than the model's highest row:
/// - **no raise** (both splits 0, an `equity` launch): no target and no caps either, so no target
///   the raise scale would read as a raise never made;
/// - **a raise**: a target at most `MAX_CREATOR_TARGET_USDC`; the threshold exactly
///   `revenue_graduation_threshold_usdc(target)`; the creator's cap within a base unit of 94% of the
///   target and Fanex's of 6%; each split within a basis point of its cap over `target +
///   threshold`, the whole raise. The floor (`CreatorTargetTooLow`), the threshold's bounds and the
///   caps' sum keep their own checks, run before this one.
pub fn validate_usdc_raise_terms(
    creator_target_usdc: u64,
    graduation_threshold_usdc: u64,
    creator_split_bps: u16,
    fanex_split_bps: u16,
    creator_cap_usdc: u64,
    fanex_cap_usdc: u64,
) -> Result<()> {
    if creator_split_bps == 0 && fanex_split_bps == 0 {
        require!(
            creator_target_usdc == 0 && creator_cap_usdc == 0 && fanex_cap_usdc == 0,
            FanexError::LaunchTermsOutOfBounds
        );
        return Ok(());
    }
    require!(
        creator_target_usdc <= MAX_CREATOR_TARGET_USDC,
        FanexError::LaunchTermsOutOfBounds
    );
    require!(
        graduation_threshold_usdc == revenue_graduation_threshold_usdc(creator_target_usdc),
        FanexError::LaunchTermsOutOfBounds
    );
    require!(
        within_one_unit_of_share(creator_cap_usdc, creator_target_usdc, CREATOR_TARGET_SHARE_PCT)
            && within_one_unit_of_share(fanex_cap_usdc, creator_target_usdc, FANEX_TARGET_SHARE_PCT),
        FanexError::LaunchTermsOutOfBounds
    );
    let total_raise = creator_target_usdc
        .checked_add(graduation_threshold_usdc)
        .ok_or(FanexError::MathOverflow)?;
    require!(
        within_one_bp_of(creator_split_bps, creator_cap_usdc, total_raise)
            && within_one_bp_of(fanex_split_bps, fanex_cap_usdc, total_raise),
        FanexError::LaunchTermsOutOfBounds
    );
    Ok(())
}

/// Whether `account`, found at `address`, may be the USDC side's fee account (the internal audit,
/// 2026-10-29): the associated token account of `fee_wallet` (`GlobalConfig.fanex_fee_wallet`) and
/// `usdc_mint` under the classic Token program, so anyone can open it again at that address; of that
/// mint and still owned by the fee wallet; not frozen; and closable by `close_authority` alone, the
/// `UsdcConfig` PDA, which no instruction ever signs for. Every buy and every sale names this account,
/// so a close by its owner (a stolen key, a wallet's «reclaim rent») would stop every USDC trade until
/// a 48-hour proposal; with its close authority a PDA, the owner can sweep it but never close it.
pub fn fee_usdc_account_valid(
    address: &Pubkey,
    account: &SplTokenAccount,
    fee_wallet: &Pubkey,
    usdc_mint: &Pubkey,
    close_authority: &Pubkey,
) -> bool {
    *address == get_associated_token_address(fee_wallet, usdc_mint)
        && account.mint == *usdc_mint
        && account.owner == *fee_wallet
        && !account.is_frozen()
        && account.close_authority == COption::Some(*close_authority)
}

/// Where a USDC buy may deliver its tokens (§10.3.5 step 6): anywhere once the launch window is over
/// (`LAUNCH_GUARD_SECONDS` from `created_at`; a clock behind `created_at` is inside it, as
/// `launch_cap_allows` reads it), and inside it only into the signer's own associated token account
/// of the mint. Bound by its mint alone, a fresh token account per buy would hold 0 each time and turn
/// the per-wallet cap into a per-transaction one. Routers do not route inside the window.
pub fn launch_destination_allowed(
    created_at: i64,
    now: i64,
    destination: &Pubkey,
    authority: &Pubkey,
    mint: &Pubkey,
) -> bool {
    if now >= created_at.saturating_add(LAUNCH_GUARD_SECONDS) {
        return true;
    }
    *destination == get_associated_token_address(authority, mint)
}

/// Whether a USDC buy must carry the creator's USDC account, its account 8 (§10.3.5): exactly while
/// the curve takes a creator split and the creator's cap is not full, read from the curve's counters,
/// never from this buy's own split, which a tiny buy can round to 0 while the cap is still open. The
/// Jupiter SDK builds the account list by the same rule. When not required, a passed account is still
/// address-checked and paid only a non-zero amount, so a closed or frozen account can stop buys only
/// until the creator's cap fills.
pub fn creator_usdc_account_required(
    creator_split_bps: u16,
    creator_received_usdc: u64,
    creator_cap_usdc: u64,
) -> bool {
    creator_split_bps > 0 && creator_received_usdc < creator_cap_usdc
}

/// Whether `account`, found at `address`, is the creator USDC account a USDC launch with a creator
/// split names (§10.3.4, trap 7): the associated token account of `creator` and `usdc_mint` under
/// the classic Token program, of that mint and still owned by `creator` (classic SPL Token lets an
/// owner hand an ATA to another key, its address unchanged), not frozen, and closable by the
/// launch's `payer` alone, so the creator cannot close it under their own token. An ATA because
/// anyone can open one again: an account closed or lost refuses buys until someone does, never stops
/// the token until an upgrade, as any other address would.
pub fn creator_usdc_account_valid(
    address: &Pubkey,
    account: &SplTokenAccount,
    creator: &Pubkey,
    usdc_mint: &Pubkey,
    payer: &Pubkey,
) -> bool {
    *address == get_associated_token_address(creator, usdc_mint)
        && account.mint == *usdc_mint
        && account.owner == *creator
        && !account.is_frozen()
        && account.close_authority == COption::Some(*payer)
}

/// Whether `claim_vesting_usdc` may release anything (§10.3.7): a curve created before
/// `VESTING_LOCKED_UNTIL_GRADUATION_FROM` keeps the terms it launched with, as on the SOL side; a
/// later one once its pool exists, PoolCreated as well as Graduated (Rick, 2026-10-22): the
/// holders' reserve is already in the pool, so a lock step that cannot run (a Raydium change,
/// `RaydiumNotSet`, a pause) never shuts the creator's vested tokens until an upgrade. A migrated
/// curve waits for its pool too, whenever it launched (the internal audit, 2026-10-29): its
/// restatement guarantees the reserve pays back the supply the curve sold, never a vesting sold into
/// it before graduation, which the last holders' sales would pay for. For a curve not migrated, the
/// same rule as `pepper_curve_math::vesting_claim_allowed(created_at, phase == 2 || phase == 3)`;
/// an unknown phase number unlocks nothing.
pub fn usdc_vesting_claim_allowed(created_at: i64, phase: u8, migrated: bool) -> bool {
    (!migrated && created_at < VESTING_LOCKED_UNTIL_GRADUATION_FROM)
        || phase == PHASE_GRADUATED
        || phase == PHASE_POOL_CREATED
}

/// Whether `signer` may write `paused` into a USDC pause flag, the config's or a curve's (§10.3.3):
/// `UsdcConfig.pauser` or `UsdcConfig.admin` may pause, only the admin may unpause (`NotPauser`
/// otherwise). The value asked decides, not the one stored: the pauser writing `false` is refused
/// even onto a flag already `false`, so a hot key never learns to unpause. An all-zero pauser lets
/// only the admin pause, since no key signs for it.
pub fn may_set_usdc_pause(signer: &Pubkey, admin: &Pubkey, pauser: &Pubkey, paused: bool) -> bool {
    *signer == *admin || (paused && *signer == *pauser)
}

#[cfg(test)]
mod usdc_tests {
    use super::*;
    use anchor_spl::token::spl_token::state::AccountState;

    /// µUSDC in one USDC.
    const USDC: u64 = 1_000_000;

    // ── The launch's numbers ──────────────────────────────────────────────

    #[test]
    fn the_split_sum_is_taken_in_u64_and_kept_under_10_000() {
        assert_eq!(usdc_split_sum_bps(0, 0).unwrap(), 0);
        assert_eq!(usdc_split_sum_bps(9_400, 500).unwrap(), 9_900);
        assert_eq!(usdc_split_sum_bps(9_999, 0).unwrap(), 9_999);
        assert_eq!(
            usdc_split_sum_bps(5_000, 5_000).unwrap_err(),
            error!(FanexError::InvalidSplitBps)
        );
        assert_eq!(
            usdc_split_sum_bps(0, 10_000).unwrap_err(),
            error!(FanexError::InvalidSplitBps)
        );
    }

    #[test]
    fn two_u16_maxima_are_a_refused_split_not_an_overflow() {
        // In u16 the sum would wrap or overflow; widened, it is 131,070 and simply too much.
        assert_eq!(
            usdc_split_sum_bps(u16::MAX, u16::MAX).unwrap_err(),
            error!(FanexError::InvalidSplitBps)
        );
    }

    #[test]
    fn the_target_floor_is_the_one_passed_and_applies_only_to_a_raise() {
        let floor = MIN_CREATOR_TARGET_USDC;
        assert_eq!(
            validate_creator_target_floor(floor - 1, 9_900, floor).unwrap_err(),
            error!(FanexError::CreatorTargetTooLow)
        );
        assert!(validate_creator_target_floor(floor, 9_900, floor).is_ok());
        assert!(validate_creator_target_floor(floor, 1, floor).is_ok());
        // Equity: no split, no target to floor.
        assert!(validate_creator_target_floor(0, 0, floor).is_ok());
    }

    #[test]
    fn with_the_sol_floor_it_rules_as_validate_creator_target() {
        for target in [
            0,
            15 * LAMPORTS_PER_SOL,
            MIN_CREATOR_TARGET_LAMPORTS - 1,
            MIN_CREATOR_TARGET_LAMPORTS,
            u64::MAX,
        ] {
            for split in [0, 1, 7_043, 9_999] {
                assert_eq!(
                    validate_creator_target_floor(target, split, MIN_CREATOR_TARGET_LAMPORTS)
                        .is_ok(),
                    validate_creator_target(target, split).is_ok(),
                    "target {target}, split {split}"
                );
            }
        }
    }

    #[test]
    fn the_threshold_stays_within_2_400_and_80_000_usdc() {
        let refused = error!(FanexError::CpmmSeedOutOfRange);
        assert_eq!(validate_graduation_threshold_usdc(0).unwrap_err(), refused);
        assert_eq!(
            validate_graduation_threshold_usdc(2_400 * USDC - 1).unwrap_err(),
            refused
        );
        assert!(validate_graduation_threshold_usdc(2_400 * USDC).is_ok());
        assert!(validate_graduation_threshold_usdc(6_900 * USDC).is_ok());
        assert!(validate_graduation_threshold_usdc(80_000 * USDC).is_ok());
        assert_eq!(
            validate_graduation_threshold_usdc(80_000 * USDC + 1).unwrap_err(),
            refused
        );
        assert_eq!(
            validate_graduation_threshold_usdc(u64::MAX).unwrap_err(),
            refused
        );
    }

    #[test]
    fn the_caps_sum_to_the_target_within_one_microdollar() {
        let target = 1_600 * USDC;
        assert!(validate_caps_sum(1_504 * USDC, 96 * USDC, target).is_ok());
        assert!(validate_caps_sum(1_504 * USDC, 96 * USDC - 1, target).is_ok());
        assert!(validate_caps_sum(1_504 * USDC, 96 * USDC + 1, target).is_ok());
        let refused = error!(FanexError::InvalidCaps);
        assert_eq!(
            validate_caps_sum(1_504 * USDC, 96 * USDC - 2, target).unwrap_err(),
            refused
        );
        assert_eq!(
            validate_caps_sum(1_504 * USDC, 96 * USDC + 2, target).unwrap_err(),
            refused
        );
        // Equity: no raise, no caps, no target.
        assert!(validate_caps_sum(0, 0, 0).is_ok());
    }

    #[test]
    fn caps_past_u64_are_refused_as_caps() {
        assert_eq!(
            validate_caps_sum(u64::MAX, 1, u64::MAX).unwrap_err(),
            error!(FanexError::InvalidCaps)
        );
        assert_eq!(
            validate_caps_sum(u64::MAX, u64::MAX, u64::MAX).unwrap_err(),
            error!(FanexError::InvalidCaps)
        );
        assert!(validate_caps_sum(u64::MAX - 1, 1, u64::MAX).is_ok());
    }

    #[test]
    fn the_supply_adds_up_to_the_fixed_total_exactly() {
        let vesting = TOKEN_TOTAL_SUPPLY / 50; // 2%
        let migration = TOKEN_TOTAL_SUPPLY / 5;
        let curve = TOKEN_TOTAL_SUPPLY - migration - vesting;
        assert!(validate_supply_allocation(curve, migration, vesting).is_ok());
        let refused = error!(FanexError::InvalidSupplyAllocation);
        assert_eq!(
            validate_supply_allocation(curve - 1, migration, vesting).unwrap_err(),
            refused
        );
        assert_eq!(
            validate_supply_allocation(curve + 1, migration, vesting).unwrap_err(),
            refused
        );
        assert_eq!(
            validate_supply_allocation(u64::MAX, 1, 0).unwrap_err(),
            error!(FanexError::MathOverflow)
        );
        assert_eq!(
            validate_supply_allocation(u64::MAX - 1, 1, 1).unwrap_err(),
            error!(FanexError::MathOverflow)
        );
    }

    #[test]
    fn neither_the_curve_s_part_nor_the_pool_s_may_be_zero() {
        // The internal audit (2026-10-29): with no migration tokens a sold-out curve seeds a pool
        // with no token side, which CPMM refuses, so it waits in Graduating for good.
        let vesting = TOKEN_TOTAL_SUPPLY / 50;
        let refused = error!(FanexError::InvalidSupplyAllocation);
        assert_eq!(
            validate_supply_allocation(TOKEN_TOTAL_SUPPLY - vesting, 0, vesting).unwrap_err(),
            refused
        );
        assert_eq!(
            validate_supply_allocation(0, TOKEN_TOTAL_SUPPLY - vesting, vesting).unwrap_err(),
            refused
        );
        assert!(validate_supply_allocation(1, TOKEN_TOTAL_SUPPLY - vesting - 1, vesting).is_ok());
        assert!(validate_supply_allocation(TOKEN_TOTAL_SUPPLY - vesting - 1, 1, vesting).is_ok());
    }

    // ── The raise terms (the internal audit, 2026-10-29) ──────────────────

    /// A revenue launch's terms as the launch builder derives them from a target
    /// (`lib/solana/pepper-curve/usdc-launch-params.ts`, phase 5.1's P20): the threshold
    /// `max(2,400, min(80,000, 1.5 × target))` USDC, the creator's cap ⌊94% × target⌋, Fanex's the
    /// rest, each split its cap over `target + threshold` in bps, rounded to the nearest.
    fn builder_terms(target: u64) -> (u64, u64, u16, u16, u64, u64) {
        let threshold = revenue_graduation_threshold_usdc(target);
        let creator_cap = target * 94 / 100;
        let fanex_cap = target - creator_cap;
        let total = (target + threshold) as f64;
        let bps = |cap: u64| ((cap as f64) * 10_000.0 / total).round() as u16;
        (target, threshold, bps(creator_cap), bps(fanex_cap), creator_cap, fanex_cap)
    }

    fn terms(t: (u64, u64, u16, u16, u64, u64)) -> Result<()> {
        validate_usdc_raise_terms(t.0, t.1, t.2, t.3, t.4, t.5)
    }

    #[test]
    fn the_threshold_is_the_seed_s_formula_at_the_usdc_bounds() {
        assert_eq!(revenue_graduation_threshold_usdc(1_600 * USDC), 2_400 * USDC);
        assert_eq!(revenue_graduation_threshold_usdc(2_000 * USDC), 3_000 * USDC);
        assert_eq!(revenue_graduation_threshold_usdc(2_000 * USDC + 1), 3_000 * USDC + 1);
        assert_eq!(revenue_graduation_threshold_usdc(53_333 * USDC), 79_999_500_000);
        assert_eq!(revenue_graduation_threshold_usdc(100_000 * USDC), 80_000 * USDC);
        assert_eq!(revenue_graduation_threshold_usdc(0), 2_400 * USDC);
        assert_eq!(revenue_graduation_threshold_usdc(u64::MAX), 80_000 * USDC);
    }

    #[test]
    fn every_launch_the_builder_derives_passes() {
        // The app's own launches, from the floor to the ceiling: the economic model's rows (1,600,
        // ~53,333 and 100,000 USDC, at 37.60%, 47.00% and 52.22% to the creator) and a sweep.
        let mut targets = vec![MIN_CREATOR_TARGET_USDC, 53_333_333_333, 80_000 * USDC, MAX_CREATOR_TARGET_USDC];
        let mut t = MIN_CREATOR_TARGET_USDC;
        while t <= MAX_CREATOR_TARGET_USDC {
            targets.push(t);
            targets.push(t + 1);
            t += 997_654_321;
        }
        for target in targets {
            let derived = builder_terms(target);
            assert!(terms(derived).is_ok(), "target {target}: {derived:?}");
        }
        assert_eq!(builder_terms(MIN_CREATOR_TARGET_USDC).2, 3_760);
        assert_eq!(builder_terms(MAX_CREATOR_TARGET_USDC).2, 5_222);
    }

    #[test]
    fn a_raise_off_the_relation_or_past_the_ceiling_is_refused() {
        let refused = error!(FanexError::LaunchTermsOutOfBounds);
        // The audit's take: 99.99% to the creator, a target of 10^12 USDC.
        let take = (1_000_000_000_000_000_000, 80_000 * USDC, 9_999, 0, 1_000_000_000_000_000_000, 0);
        assert_eq!(terms(take).unwrap_err(), refused);
        // One µUSDC past the ceiling.
        assert_eq!(terms(builder_terms(MAX_CREATOR_TARGET_USDC + 1)).unwrap_err(), refused);
        let good = builder_terms(10_000 * USDC);
        // A threshold one µUSDC off the formula.
        assert_eq!(terms((good.0, good.1 + 1, good.2, good.3, good.4, good.5)).unwrap_err(), refused);
        // A split one bp past the nearest, either way, and the two swapped.
        assert_eq!(terms((good.0, good.1, good.2 + 2, good.3, good.4, good.5)).unwrap_err(), refused);
        assert_eq!(terms((good.0, good.1, good.2, good.3 + 2, good.4, good.5)).unwrap_err(), refused);
        assert_eq!(terms((good.0, good.1, good.3, good.2, good.4, good.5)).unwrap_err(), refused);
        // Caps off 94/6 by more than a base unit, their sum kept.
        assert_eq!(terms((good.0, good.1, good.2, good.3, good.4 - 2, good.5 + 2)).unwrap_err(), refused);
        // The rounding the builder needs, a bp either side of the exact share, passes.
        assert!(terms((good.0, good.1, good.2 + 1, good.3, good.4, good.5)).is_ok());
        assert!(terms((good.0, good.1, good.2 - 1, good.3, good.4, good.5)).is_ok());
    }

    #[test]
    fn a_launch_that_raises_nothing_names_no_target_and_no_cap() {
        assert!(validate_usdc_raise_terms(0, 6_900 * USDC, 0, 0, 0, 0).is_ok());
        let refused = error!(FanexError::LaunchTermsOutOfBounds);
        // A target with no split would read as a raise never made: the holders' raise scale 0.
        assert_eq!(validate_usdc_raise_terms(1_600 * USDC, 6_900 * USDC, 0, 0, 0, 0).unwrap_err(), refused);
        assert_eq!(validate_usdc_raise_terms(0, 6_900 * USDC, 0, 0, 1, 0).unwrap_err(), refused);
        assert_eq!(validate_usdc_raise_terms(0, 6_900 * USDC, 0, 0, 0, 1).unwrap_err(), refused);
    }

    // ── The fee account (the internal audit, 2026-10-29) ──────────────────

    #[test]
    fn the_fee_account_is_the_fee_wallet_s_hardened_ata() {
        let (wallet, mint, config) = (Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique());
        let address = get_associated_token_address(&wallet, &mint);
        let hardened = SplTokenAccount {
            mint,
            owner: wallet,
            amount: 0,
            delegate: COption::None,
            state: AccountState::Initialized,
            is_native: COption::None,
            delegated_amount: 0,
            close_authority: COption::Some(config),
        };
        assert!(fee_usdc_account_valid(&address, &hardened, &wallet, &mint, &config));
        // Not the ATA: only its keypair's holder could open it again.
        assert!(!fee_usdc_account_valid(&Pubkey::new_unique(), &hardened, &wallet, &mint, &config));
        // Closable by its owner: no close authority, or the owner, or any other key.
        for close_authority in [COption::None, COption::Some(wallet), COption::Some(Pubkey::new_unique())] {
            let open = SplTokenAccount { close_authority, ..hardened };
            assert!(!fee_usdc_account_valid(&address, &open, &wallet, &mint, &config));
        }
        let frozen = SplTokenAccount { state: AccountState::Frozen, ..hardened };
        assert!(!fee_usdc_account_valid(&address, &frozen, &wallet, &mint, &config));
        // Handed to another owner, or of another mint.
        let handed = SplTokenAccount { owner: Pubkey::new_unique(), ..hardened };
        assert!(!fee_usdc_account_valid(&address, &handed, &wallet, &mint, &config));
        let other_mint = SplTokenAccount { mint: Pubkey::new_unique(), ..hardened };
        assert!(!fee_usdc_account_valid(&address, &other_mint, &wallet, &mint, &config));
    }

    // ── The launch window's destination ───────────────────────────────────

    const LAUNCH: i64 = 1_793_491_200;

    /// The associated token account of `wallet` and `mint` under `token_program`, derived by hand.
    fn ata(wallet: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(
            &[wallet.as_ref(), token_program.as_ref(), mint.as_ref()],
            &anchor_spl::associated_token::ID,
        )
        .0
    }

    #[test]
    fn inside_the_window_only_the_signers_own_ata_of_the_mint_takes_the_tokens() {
        let (authority, mint) = (Pubkey::new_unique(), Pubkey::new_unique());
        let own = ata(&authority, &mint, &anchor_spl::token::ID);
        assert!(launch_destination_allowed(
            LAUNCH, LAUNCH, &own, &authority, &mint
        ));
        // A fresh token account, someone else's ATA, the signer's ATA of another mint, the
        // signer's ATA under Token-2022: each would read an empty holding against the cap.
        let others = [
            Pubkey::new_unique(),
            ata(&Pubkey::new_unique(), &mint, &anchor_spl::token::ID),
            ata(&authority, &Pubkey::new_unique(), &anchor_spl::token::ID),
            ata(&authority, &mint, &anchor_spl::token_2022::ID),
        ];
        for destination in others {
            assert!(!launch_destination_allowed(
                LAUNCH,
                LAUNCH + 10,
                &destination,
                &authority,
                &mint
            ));
        }
    }

    #[test]
    fn the_window_lasts_five_minutes_and_a_clock_behind_the_launch_is_inside_it() {
        let (authority, mint, elsewhere) = (
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        );
        assert!(!launch_destination_allowed(
            LAUNCH,
            LAUNCH - 5,
            &elsewhere,
            &authority,
            &mint
        ));
        assert!(!launch_destination_allowed(
            LAUNCH,
            LAUNCH + LAUNCH_GUARD_SECONDS - 1,
            &elsewhere,
            &authority,
            &mint
        ));
        // After it, any destination: a router's authority need not own the account it fills.
        assert!(launch_destination_allowed(
            LAUNCH,
            LAUNCH + LAUNCH_GUARD_SECONDS,
            &elsewhere,
            &authority,
            &mint
        ));
        assert!(launch_destination_allowed(
            i64::MAX,
            i64::MAX,
            &elsewhere,
            &authority,
            &mint
        ));
    }

    // ── Account 8 of a buy ────────────────────────────────────────────────

    #[test]
    fn account_8_is_required_while_a_creator_split_runs_and_its_cap_is_open() {
        let cap = 1_504 * USDC;
        assert!(creator_usdc_account_required(9_400, 0, cap));
        // One µUSDC short of the cap: a tiny buy may split 0 to the creator, the account is still due.
        assert!(creator_usdc_account_required(9_400, cap - 1, cap));
        assert!(creator_usdc_account_required(1, 0, 1));
    }

    #[test]
    fn account_8_is_not_required_once_the_cap_is_full_or_with_no_creator_split() {
        let cap = 1_504 * USDC;
        assert!(!creator_usdc_account_required(9_400, cap, cap));
        assert!(!creator_usdc_account_required(9_400, cap + 1, cap));
        assert!(!creator_usdc_account_required(0, 0, cap));
        assert!(!creator_usdc_account_required(0, 0, 0));
    }

    // ── The creator's USDC account at launch ──────────────────────────────

    struct Launch {
        creator: Pubkey,
        usdc_mint: Pubkey,
        payer: Pubkey,
    }

    impl Launch {
        fn new() -> Self {
            Self {
                creator: Pubkey::new_unique(),
                usdc_mint: Pubkey::new_unique(),
                payer: Pubkey::new_unique(),
            }
        }

        /// The creator's ATA as the launch transaction leaves it: open, the payer its close authority.
        fn hardened(&self) -> (Pubkey, SplTokenAccount) {
            let account = SplTokenAccount {
                mint: self.usdc_mint,
                owner: self.creator,
                amount: 0,
                delegate: COption::None,
                state: AccountState::Initialized,
                is_native: COption::None,
                delegated_amount: 0,
                close_authority: COption::Some(self.payer),
            };
            (
                ata(&self.creator, &self.usdc_mint, &anchor_spl::token::ID),
                account,
            )
        }

        fn valid(&self, address: &Pubkey, account: &SplTokenAccount) -> bool {
            creator_usdc_account_valid(
                address,
                account,
                &self.creator,
                &self.usdc_mint,
                &self.payer,
            )
        }
    }

    #[test]
    fn the_creators_hardened_ata_is_valid() {
        let launch = Launch::new();
        let (address, account) = launch.hardened();
        assert!(launch.valid(&address, &account));
        // A balance or a delegate does not matter: only where it is, whose, and who may close it.
        let funded = SplTokenAccount {
            amount: 42 * USDC,
            delegate: COption::Some(Pubkey::new_unique()),
            delegated_amount: 1,
            ..account
        };
        assert!(launch.valid(&address, &funded));
    }

    #[test]
    fn an_account_away_from_the_creators_ata_is_not() {
        let launch = Launch::new();
        let (_, account) = launch.hardened();
        for address in [
            Pubkey::new_unique(),
            ata(
                &Pubkey::new_unique(),
                &launch.usdc_mint,
                &anchor_spl::token::ID,
            ),
            ata(&launch.payer, &launch.usdc_mint, &anchor_spl::token::ID),
            ata(
                &launch.creator,
                &Pubkey::new_unique(),
                &anchor_spl::token::ID,
            ),
            ata(
                &launch.creator,
                &launch.usdc_mint,
                &anchor_spl::token_2022::ID,
            ),
        ] {
            assert!(!launch.valid(&address, &account));
        }
    }

    #[test]
    fn an_ata_handed_to_another_owner_or_of_another_mint_is_not() {
        let launch = Launch::new();
        let (address, account) = launch.hardened();
        let handed_over = SplTokenAccount {
            owner: Pubkey::new_unique(),
            ..account
        };
        assert!(!launch.valid(&address, &handed_over));
        let other_mint = SplTokenAccount {
            mint: Pubkey::new_unique(),
            ..account
        };
        assert!(!launch.valid(&address, &other_mint));
    }

    #[test]
    fn a_frozen_ata_is_not() {
        let launch = Launch::new();
        let (address, account) = launch.hardened();
        let frozen = SplTokenAccount {
            state: AccountState::Frozen,
            ..account
        };
        assert!(!launch.valid(&address, &frozen));
    }

    #[test]
    fn an_ata_the_payer_cannot_close_is_not() {
        // No close authority: the owner, the creator, could close it under their own token.
        let launch = Launch::new();
        let (address, account) = launch.hardened();
        for close_authority in [
            COption::None,
            COption::Some(launch.creator),
            COption::Some(Pubkey::new_unique()),
        ] {
            let other = SplTokenAccount {
                close_authority,
                ..account
            };
            assert!(!launch.valid(&address, &other));
        }
    }

    // ── The vesting lock ──────────────────────────────────────────────────

    #[test]
    fn a_usdc_curve_created_before_the_lock_releases_in_any_phase() {
        let before = VESTING_LOCKED_UNTIL_GRADUATION_FROM - 1;
        for phase in [
            PHASE_BONDING,
            PHASE_GRADUATING,
            PHASE_GRADUATED,
            PHASE_POOL_CREATED,
            4,
            u8::MAX,
        ] {
            assert!(usdc_vesting_claim_allowed(before, phase, false), "phase {phase}");
        }
    }

    #[test]
    fn a_later_usdc_curve_releases_once_its_pool_exists() {
        for created_at in [
            VESTING_LOCKED_UNTIL_GRADUATION_FROM,
            VESTING_LOCKED_UNTIL_GRADUATION_FROM + 86_400,
        ] {
            assert!(!usdc_vesting_claim_allowed(created_at, PHASE_BONDING, false));
            assert!(!usdc_vesting_claim_allowed(created_at, PHASE_GRADUATING, false));
            assert!(usdc_vesting_claim_allowed(created_at, PHASE_POOL_CREATED, false));
            assert!(usdc_vesting_claim_allowed(created_at, PHASE_GRADUATED, false));
            // A phase number the program never writes unlocks nothing.
            assert!(!usdc_vesting_claim_allowed(created_at, 4, false));
            assert!(!usdc_vesting_claim_allowed(created_at, u8::MAX, false));
        }
    }

    #[test]
    fn a_migrated_curve_releases_only_once_its_pool_exists_whenever_it_launched() {
        // The internal audit (2026-10-29): a migrated curve keeps its SOL `created_at`, before the
        // lock, and its restatement pays back only the supply the curve sold, never a vesting sold
        // into it in Bonding.
        for created_at in [
            0,
            VESTING_LOCKED_UNTIL_GRADUATION_FROM - 1,
            VESTING_LOCKED_UNTIL_GRADUATION_FROM,
        ] {
            assert!(!usdc_vesting_claim_allowed(created_at, PHASE_BONDING, true));
            assert!(!usdc_vesting_claim_allowed(created_at, PHASE_GRADUATING, true));
            assert!(usdc_vesting_claim_allowed(created_at, PHASE_POOL_CREATED, true));
            assert!(usdc_vesting_claim_allowed(created_at, PHASE_GRADUATED, true));
            assert!(!usdc_vesting_claim_allowed(created_at, 4, true));
        }
        // Not migrated, before the lock: any phase, as it launched.
        assert!(usdc_vesting_claim_allowed(0, PHASE_BONDING, false));
    }

    #[test]
    fn on_the_three_shared_phases_it_rules_as_the_sol_lock() {
        use crate::state::CurvePhase;
        let shared = [
            (CurvePhase::Bonding, PHASE_BONDING),
            (CurvePhase::Graduating, PHASE_GRADUATING),
            (CurvePhase::Graduated, PHASE_GRADUATED),
        ];
        for created_at in [
            0,
            VESTING_LOCKED_UNTIL_GRADUATION_FROM - 1,
            VESTING_LOCKED_UNTIL_GRADUATION_FROM,
        ] {
            for (sol, usdc) in shared {
                assert_eq!(
                    usdc_vesting_claim_allowed(created_at, usdc, false),
                    vesting_claim_allowed(created_at, sol)
                );
            }
        }
    }

    // ── The pauses ────────────────────────────────────────────────────────

    #[test]
    fn the_pauser_pauses_and_only_the_admin_unpauses() {
        let admin = Pubkey::new_unique();
        let pauser = Pubkey::new_unique();
        let stranger = Pubkey::new_unique();
        assert!(may_set_usdc_pause(&pauser, &admin, &pauser, true));
        assert!(!may_set_usdc_pause(&pauser, &admin, &pauser, false));
        assert!(may_set_usdc_pause(&admin, &admin, &pauser, true));
        assert!(may_set_usdc_pause(&admin, &admin, &pauser, false));
        assert!(!may_set_usdc_pause(&stranger, &admin, &pauser, true));
        assert!(!may_set_usdc_pause(&stranger, &admin, &pauser, false));
    }

    #[test]
    fn a_pauser_who_is_the_admin_unpauses_as_the_admin() {
        let admin = Pubkey::new_unique();
        assert!(may_set_usdc_pause(&admin, &admin, &admin, false));
        // No pauser named: the admin alone, and nobody signs as the all-zero key.
        let nobody = Pubkey::default();
        assert!(may_set_usdc_pause(&admin, &admin, &nobody, true));
        assert!(!may_set_usdc_pause(&Pubkey::new_unique(), &admin, &nobody, true));
    }
}
