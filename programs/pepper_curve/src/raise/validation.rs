//! Pure checks on a raise's parameters (spec §4.3, §4.4) and on the pool fee
//! (amended spec D16). The realism limit needs YouTube revenue, so it is
//! attested by Fanex's co-signature on `create_raise`, not checked here. Since
//! the internal audit of 2026-10-29 also the launch fee's ceiling, where a
//! payout may land (the beneficiary's associated account) and the ProgramData
//! reader that gates the config's birth.

use anchor_lang::prelude::*;

use crate::raise::checked::{add, mul_div};
use crate::raise::constants::*;
use crate::errors::FanexError;
use crate::raise::pricing::cost_of;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TierInput {
    pub units: u64,
    pub price: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RaiseParams {
    pub raise_id: u64,
    pub tier_count: u8,
    pub tiers: [TierInput; 3],
    pub min_creator_part: u64,
    pub share_bps: u16,
    pub term_months: u16,
    pub deadline: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValidatedRaise {
    pub max_units: u64,
    pub fan_max: u64,
    pub creator_max: u64,
}

/// The creator's part of what fans paid: 100 of every 110 (spec §4.2).
pub fn creator_part_of(fan_total: u64) -> Result<u64> {
    mul_div(fan_total, CREATOR_PART_NUM, FAN_PAYS_NUM)
}

fn bps_at_least(part: u64, whole: u64, bps: u64) -> bool {
    (part as u128) * (BPS as u128) >= (whole as u128) * (bps as u128)
}

fn bps_at_most(part: u64, whole: u64, bps: u64) -> bool {
    (part as u128) * (BPS as u128) <= (whole as u128) * (bps as u128)
}

/// Checks the tier rules and returns the quota units at a full raise.
pub fn validate_tiers(tiers: &[TierInput; 3], tier_count: u8) -> Result<u64> {
    let n = tier_count as usize;
    require!((1..=MAX_TIERS).contains(&n), FanexError::InvalidTiers);
    require!(tiers[0].price == FIRST_TIER_PRICE, FanexError::InvalidTiers);
    let mut total = 0u64;
    for tier in &tiers[..n] {
        require!(tier.units > 0, FanexError::InvalidTiers);
        total = add(total, tier.units)?;
    }
    for tier in &tiers[n..] {
        require!(tier.units == 0 && tier.price == 0, FanexError::InvalidTiers);
    }
    if n > 1 {
        for tier in &tiers[..n] {
            require!(bps_at_least(tier.units, total, TIER_MIN_SHARE_BPS), FanexError::InvalidTiers);
        }
        for i in 1..n {
            let (prev, cur) = (tiers[i - 1].price, tiers[i].price);
            require!(
                bps_at_least(cur, prev, BPS + TIER_STEP_MIN_BPS) && bps_at_most(cur, prev, BPS + TIER_STEP_MAX_BPS),
                FanexError::InvalidTiers
            );
        }
        require!(
            bps_at_most(tiers[n - 1].price, FIRST_TIER_PRICE, BPS + TIER_SPREAD_MAX_BPS),
            FanexError::InvalidTiers
        );
    }
    Ok(total)
}

pub fn validate_params(p: &RaiseParams, now: i64) -> Result<ValidatedRaise> {
    let max_units = validate_tiers(&p.tiers, p.tier_count)?;
    let mut fan_max = 0u64;
    for tier in &p.tiers[..p.tier_count as usize] {
        fan_max = add(fan_max, cost_of(tier.units, tier.price)?)?;
    }
    let creator_max = creator_part_of(fan_max)?;
    require!(creator_max > 0, FanexError::InvalidTiers);
    require!((SHARE_BPS_MIN..=SHARE_BPS_MAX).contains(&p.share_bps), FanexError::InvalidShare);
    require!((TERM_MONTHS_MIN..=TERM_MONTHS_MAX).contains(&p.term_months), FanexError::InvalidTerm);
    let duration = p.deadline.checked_sub(now).ok_or_else(|| error!(FanexError::RaiseMathOverflow))?;
    require!(
        (RAISE_DURATION_MIN_SECONDS..=RAISE_DURATION_MAX_SECONDS).contains(&duration),
        FanexError::InvalidDeadline
    );
    require!(
        p.min_creator_part <= creator_max && bps_at_least(p.min_creator_part, creator_max, MIN_PART_OF_MAX_BPS),
        FanexError::InvalidMinimum
    );
    Ok(ValidatedRaise { max_units, fan_max, creator_max })
}

/// The pool fee a config may set: at most POOL_FEE_MAX_BPS (amended spec D16).
pub fn check_pool_fee(pool_fee_bps: u16) -> Result<()> {
    require!(pool_fee_bps <= POOL_FEE_MAX_BPS, FanexError::FeeTooHigh);
    Ok(())
}

/// The launch fee a config may set: at most LAUNCH_FEE_MAX (internal audit of 2026-10-29).
pub fn check_launch_fee(launch_fee: u64) -> Result<()> {
    require!(launch_fee <= LAUNCH_FEE_MAX, FanexError::LaunchFeeTooHigh);
    Ok(())
}

/// Whether `account` is `owner`'s associated token account of `mint` under the classic token
/// program: where a claim, a refund or a close-out pays, so no caller can push someone's money into
/// an account they created for them and no wallet shows (internal audit of 2026-10-29, claim.rs:32).
pub fn is_associated_token_account(account: &Pubkey, owner: &Pubkey, mint: &Pubkey) -> bool {
    *account == anchor_spl::associated_token::get_associated_token_address(owner, mint)
}

// ── The config's birth (internal audit of 2026-10-29), copied from `pepper_dividends` ──

/// The bytes of a ProgramData before the program's ELF (`UpgradeableLoaderState::
/// size_of_programdata_metadata`): the bincode of `UpgradeableLoaderState::ProgramData`, its tag
/// 3 as a u32, `slot` as a u64, then `upgrade_authority_address` as an `Option<Pubkey>` (a 0 or 1
/// byte, then the key's 32, left zero when there is none). The same reader as `pepper_curve`'s and
/// `pepper_dividends`'.
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

/// Whether a wallet may hold `units` (10% of the quotas at a full raise).
pub fn within_wallet_limit(units: u64, max_units: u64) -> bool {
    bps_at_most(units, max_units, PER_WALLET_MAX_BPS)
}

#[cfg(test)]
mod tests {
    use super::*;

    const Q: u64 = QUOTA_UNITS;
    const NOW: i64 = 1_790_812_800;
    const DAY: i64 = 86_400;

    /// Spec §4.4: 40% / 30% / 30% at 1.00 / 1.10 / 1.20, 10,000 quotas.
    /// Fans pay at most 10,900 USD; the creator's part is 9,909.090909 USD.
    fn spec_example() -> RaiseParams {
        RaiseParams {
            raise_id: 1,
            tier_count: 3,
            tiers: [
                TierInput { units: 4_000 * Q, price: 1_000_000 },
                TierInput { units: 3_000 * Q, price: 1_100_000 },
                TierInput { units: 3_000 * Q, price: 1_200_000 },
            ],
            min_creator_part: 2_477_272_728,
            share_bps: 10_000,
            term_months: 60,
            deadline: NOW + 28 * DAY,
        }
    }

    fn err(p: RaiseParams) -> anchor_lang::error::Error {
        validate_params(&p, NOW).unwrap_err()
    }

    #[test]
    fn accepts_the_spec_example() {
        let v = validate_params(&spec_example(), NOW).unwrap();
        assert_eq!(v, ValidatedRaise { max_units: 10_000 * Q, fan_max: 10_900_000_000, creator_max: 9_909_090_909 });
    }

    #[test]
    fn accepts_a_single_tier() {
        let mut p = spec_example();
        p.tier_count = 1;
        p.tiers = [TierInput { units: 1_000 * Q, price: 1_000_000 }, TierInput::default(), TierInput::default()];
        p.min_creator_part = 227_272_728;
        assert!(validate_params(&p, NOW).is_ok());
    }

    #[test]
    fn the_first_tier_is_one_dollar() {
        let mut p = spec_example();
        p.tiers[0].price = 1_010_000;
        assert_eq!(err(p), error!(FanexError::InvalidTiers));
    }

    #[test]
    fn a_step_is_between_5_and_15_percent() {
        let mut low = spec_example();
        low.tiers[1].price = 1_040_000;
        assert_eq!(err(low), error!(FanexError::InvalidTiers));
        let mut high = spec_example();
        high.tiers[1].price = 1_160_000;
        assert_eq!(err(high), error!(FanexError::InvalidTiers));
    }

    #[test]
    fn first_to_last_is_at_most_25_percent() {
        let mut p = spec_example();
        p.tiers[1].price = 1_150_000;
        p.tiers[2].price = 1_300_000;
        assert_eq!(err(p), error!(FanexError::InvalidTiers));
    }

    #[test]
    fn every_tier_holds_at_least_20_percent() {
        let mut p = spec_example();
        p.tiers[0].units = 1_900 * Q;
        p.tiers[1].units = 4_100 * Q;
        p.tiers[2].units = 4_000 * Q;
        assert_eq!(err(p), error!(FanexError::InvalidTiers));
    }

    #[test]
    fn nothing_after_the_last_tier() {
        let mut p = spec_example();
        p.tier_count = 2;
        assert_eq!(err(p), error!(FanexError::InvalidTiers));
    }

    #[test]
    fn share_term_and_duration_bounds() {
        for (share, ok) in [(99u16, false), (100, true), (2_500, true), (10_000, true), (10_001, false)] {
            let mut p = spec_example();
            p.share_bps = share;
            assert_eq!(validate_params(&p, NOW).is_ok(), ok, "share {share}");
        }
        for (term, ok) in [(23u16, false), (24, true), (60, true), (61, false)] {
            let mut p = spec_example();
            p.term_months = term;
            assert_eq!(validate_params(&p, NOW).is_ok(), ok, "term {term}");
        }
        for (secs, ok) in [(21 * DAY - 1, false), (21 * DAY, true), (56 * DAY, true), (56 * DAY + 1, false)] {
            let mut p = spec_example();
            p.deadline = NOW + secs;
            assert_eq!(validate_params(&p, NOW).is_ok(), ok, "duration {secs}");
        }
    }

    #[test]
    fn the_minimum_is_between_25_and_100_percent_of_the_maximum() {
        let mut below = spec_example();
        below.min_creator_part = 2_477_272_727;
        assert_eq!(err(below), error!(FanexError::InvalidMinimum));
        let mut above = spec_example();
        above.min_creator_part = 9_909_090_910;
        assert_eq!(err(above), error!(FanexError::InvalidMinimum));
        let mut at_max = spec_example();
        at_max.min_creator_part = 9_909_090_909;
        assert!(validate_params(&at_max, NOW).is_ok());
    }

    #[test]
    fn the_pool_fee_is_at_most_three_percent() {
        assert!(check_pool_fee(0).is_ok());
        assert!(check_pool_fee(POOL_FEE_MAX_BPS).is_ok());
        assert_eq!(check_pool_fee(301).unwrap_err(), error!(FanexError::FeeTooHigh));
    }

    #[test]
    fn the_launch_fee_is_at_most_fifty_dollars() {
        assert!(check_launch_fee(0).is_ok());
        assert!(check_launch_fee(5_000_000).is_ok());
        assert!(check_launch_fee(LAUNCH_FEE_MAX).is_ok());
        assert_eq!(check_launch_fee(LAUNCH_FEE_MAX + 1).unwrap_err(), error!(FanexError::LaunchFeeTooHigh));
        assert_eq!(check_launch_fee(u64::MAX).unwrap_err(), error!(FanexError::LaunchFeeTooHigh));
    }

    #[test]
    fn only_the_owners_associated_account_of_the_mint_is_one() {
        let (owner, mint, other) = (Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique());
        let ata = anchor_spl::associated_token::get_associated_token_address(&owner, &mint);
        assert!(is_associated_token_account(&ata, &owner, &mint));
        // Another owner's, another mint's, and any other address: a token account the owner may
        // well hold, but not the one its wallet shows.
        assert!(!is_associated_token_account(&anchor_spl::associated_token::get_associated_token_address(&other, &mint), &owner, &mint));
        assert!(!is_associated_token_account(&anchor_spl::associated_token::get_associated_token_address(&owner, &other), &owner, &mint));
        assert!(!is_associated_token_account(&Pubkey::new_unique(), &owner, &mint));
        // An owner off the curve (a vault) has one too.
        let pda = Pubkey::find_program_address(&[b"vault"], &crate::ID).0;
        let pda_ata = anchor_spl::associated_token::get_associated_token_address(&pda, &mint);
        assert!(is_associated_token_account(&pda_ata, &pda, &mint));
    }

    #[test]
    fn a_wallet_holds_at_most_10_percent_of_the_quotas() {
        assert!(within_wallet_limit(1_000 * Q, 10_000 * Q));
        assert!(!within_wallet_limit(1_000 * Q + 1, 10_000 * Q));
    }

    #[test]
    fn the_creators_part_is_100_of_every_110() {
        assert_eq!(creator_part_of(33_000_000_000).unwrap(), 30_000_000_000);
        assert_eq!(creator_part_of(11).unwrap(), 10);
    }
}

/// `pepper_dividends`' tests of the same reader, copied with it (c11506fa2, validation.rs).
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
