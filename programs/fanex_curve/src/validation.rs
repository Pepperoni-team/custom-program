use anchor_lang::prelude::*;

use crate::constants::*;
use crate::errors::FanexError;

pub const MIN_VESTING_SUPPLY_BPS: u64 = 200;
pub const MAX_VESTING_SUPPLY_BPS: u64 = 1_500;

pub const MAX_VESTING_CLIFF_SECONDS: i64 = 24 * 30 * 24 * 60 * 60;
pub const MAX_VESTING_DURATION_SECONDS: i64 = 48 * 30 * 24 * 60 * 60;

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

pub fn validate_beneficiary_reassignable(current: Pubkey, placeholder: Pubkey) -> Result<()> {
    require_keys_eq!(current, placeholder, FanexError::VestingNotReassignable);
    Ok(())
}

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
        let a = Pubkey::new_unique();
        let b = Pubkey::new_unique();
        assert_ne!(transit_of(&a), transit_of(&b));
        assert!(!transit_of(&a).is_on_curve());
    }

    #[test]
    fn keeps_the_seed_the_typescript_client_derives_with() {
        assert_eq!(PAYOUT_TRANSIT_SEED, b"payout");
    }
}

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
        assert!(!may_close_distributor(TOTAL - 1, TOTAL, EXPIRES - 1, EXPIRES));
    }

    #[test]
    fn refuses_a_partially_claimed_month_before_expiry() {
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

pub fn may_fund_distributor_v1(funder: &Pubkey, fanex_fee_wallet: &Pubkey) -> bool {
    funder == fanex_fee_wallet
}

pub fn may_fund_distributor_v2(funder: &Pubkey, fanex_fee_wallet: &Pubkey, creator: &Pubkey) -> bool {
    funder == fanex_fee_wallet || *funder == DIVIDEND_PAYER || funder == creator
}

pub fn expiry_in_range(expires_at: i64, now: i64) -> bool {
    expires_at <= now.saturating_add(MAX_DISTRIBUTOR_EXPIRY_SECONDS)
}

pub fn launch_cap_allows(created_at: i64, now: i64, holding: u64, tokens_out: u64) -> bool {
    if now >= created_at.saturating_add(LAUNCH_GUARD_SECONDS) {
        return true;
    }
    holding.checked_add(tokens_out).is_some_and(|after| after <= LAUNCH_WALLET_CAP)
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
    use super::{expiry_in_range, may_fund_distributor_v1, may_fund_distributor_v2};
    use crate::constants::{DIVIDEND_PAYER, MAX_DISTRIBUTOR_EXPIRY_SECONDS};
    use anchor_lang::prelude::Pubkey;

    fn key(byte: u8) -> Pubkey {
        Pubkey::new_from_array([byte; 32])
    }

    #[test]
    fn the_dividend_payer_is_the_wallet_that_was_named() {
        assert_eq!(DIVIDEND_PAYER.to_string(), "5UZrYBpbmG1kZKobQbM1Jqn8JDCfpSaR8sTm9ApzC554");
    }

    #[test]
    fn the_fee_wallet_funds_either_version() {
        let (fee_wallet, creator) = (key(1), key(2));
        assert!(may_fund_distributor_v1(&fee_wallet, &fee_wallet));
        assert!(may_fund_distributor_v2(&fee_wallet, &fee_wallet, &creator));
    }

    #[test]
    fn the_dividend_payer_funds_only_a_v2_month() {
        let (fee_wallet, creator) = (key(1), key(2));
        assert!(!may_fund_distributor_v1(&DIVIDEND_PAYER, &fee_wallet));
        assert!(may_fund_distributor_v2(&DIVIDEND_PAYER, &fee_wallet, &creator));
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
        let (fee_wallet, creator) = (key(1), key(2));
        assert!(!may_fund_distributor_v1(&creator, &fee_wallet));
        assert!(may_fund_distributor_v2(&creator, &fee_wallet, &creator));
    }

    #[test]
    fn anyone_else_funds_nothing() {
        let (fee_wallet, creator, stranger) = (key(1), key(2), key(3));
        assert!(!may_fund_distributor_v1(&stranger, &fee_wallet));
        assert!(!may_fund_distributor_v2(&stranger, &fee_wallet, &creator));
    }
}

pub fn claim_registry_bytes(leaf_count: u32) -> usize {
    (leaf_count as usize).div_ceil(8)
}

pub fn leaf_count_in_range(leaf_count: u32) -> bool {
    (1..=MAX_DISTRIBUTOR_V2_LEAVES).contains(&leaf_count)
}

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
        assert!(validate_vesting_supply(TOTAL * 1_000 / 10_000 + 1_000).is_err());
    }

    #[test]
    fn accepts_the_lump_sum_schedule_used_today() {
        assert!(validate_vesting_schedule(12 * 30 * 24 * 60 * 60, 0).is_ok());
    }

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
        let placeholder = Pubkey::new_unique();
        let first_claimant = Pubkey::new_unique();
        assert!(validate_beneficiary_reassignable(first_claimant, placeholder).is_err());
    }

    #[test]
    fn refuses_the_default_pubkey() {
        let placeholder = Pubkey::new_unique();
        assert!(validate_beneficiary_reassignable(Pubkey::default(), placeholder).is_err());
    }

    #[test]
    fn syncs_a_placeholder_creator_to_an_already_claimed_vesting() {
        let placeholder = Pubkey::new_unique();
        let claimant = Pubkey::new_unique();
        assert!(validate_curve_creator_syncable(placeholder, claimant, placeholder).is_ok());
    }

    #[test]
    fn refuses_to_touch_a_real_creator() {
        let placeholder = Pubkey::new_unique();
        let real_creator = Pubkey::new_unique();
        let someone_else = Pubkey::new_unique();
        assert!(validate_curve_creator_syncable(real_creator, someone_else, placeholder).is_err());
        assert!(validate_curve_creator_syncable(real_creator, real_creator, placeholder).is_err());
    }

    #[test]
    fn refuses_while_the_vesting_is_still_on_the_placeholder() {
        let placeholder = Pubkey::new_unique();
        assert!(validate_curve_creator_syncable(placeholder, placeholder, placeholder).is_err());
    }

    #[test]
    fn refuses_an_all_zero_vesting_beneficiary() {
        let placeholder = Pubkey::new_unique();
        assert!(validate_curve_creator_syncable(placeholder, Pubkey::default(), placeholder).is_err());
    }

    #[test]
    fn refuses_the_placeholder_as_creator_of_a_curve_that_raises() {
        let placeholder = Pubkey::new_unique();
        assert!(validate_creator_can_receive_raise(placeholder, placeholder, 3_760).is_err());
        assert!(validate_creator_can_receive_raise(placeholder, placeholder, 1).is_err());
    }

    #[test]
    fn allows_the_placeholder_on_a_curve_with_no_raise() {
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
        let (pda, _bump) = Pubkey::find_program_address(&[FAN_LED_PLACEHOLDER_SEED], &crate::ID);
        assert_eq!(
            pda,
            crate::instructions::set_vesting_beneficiary::fan_led_placeholder()
        );
        assert_ne!(pda, Pubkey::default());
    }
}
