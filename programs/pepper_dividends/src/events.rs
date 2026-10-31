//! Events, one per state change an indexer has to see (spec §6). Named apart from
//! `pepper_curve`'s and `pepper_raise`'s, so that no two share a discriminator:
//! `DistributorFunded` and `DividendClaimed` are `pepper_curve`'s own, and the
//! tests below hold every name of both programs off ours. Their sizes, the
//! 8-byte discriminator included, and their field order are pinned too, since
//! the app decodes them by offset.

use anchor_lang::prelude::*;

/// A month funded: `total_amount` µUSDC moved from the funder into the vault.
#[event]
pub struct UsdcDistributorFunded {
    pub mint: Pubkey,
    pub month: i64,
    pub total_amount: u64,
    pub funder: Pubkey,
}

/// Leaf `index` paid `amount` µUSDC into the claimant's own USDC account.
#[event]
pub struct UsdcDividendClaimed {
    pub mint: Pubkey,
    pub month: i64,
    pub claimant: Pubkey,
    pub index: u32,
    pub amount: u64,
}

/// A month closed: `returned` µUSDC, the vault's whole balance, went to `to`, the
/// funder's recorded USDC account.
#[event]
pub struct UsdcDistributorClosed {
    pub mint: Pubkey,
    pub month: i64,
    pub returned: u64,
    pub to: Pubkey,
}

/// A month voided by the admin before any claim: the same money move as a close,
/// and the month may be funded again.
#[event]
pub struct UsdcDistributorVoided {
    pub mint: Pubkey,
    pub month: i64,
    pub returned: u64,
    pub to: Pubkey,
}

/// `claims_paused_until` as `pause_claims` or `resume_claims` left it, and who signed.
#[event]
pub struct ClaimsPaused {
    pub until: i64,
    pub by: Pubkey,
}

/// One month's `claims_held_until` as `hold_claims` or `release_claims` left it, and who
/// signed (Rick, 2026-10-26).
#[event]
pub struct ClaimsHeld {
    pub mint: Pubkey,
    pub month: i64,
    pub until: i64,
    pub by: Pubkey,
}

/// A month's payees re-homed by the admin to the config's dividend payer (the internal audit,
/// 2026-10-29): the three accounts it records after the write, the close's and the void's.
#[event]
pub struct UsdcDistributorRehomed {
    pub mint: Pubkey,
    pub month: i64,
    pub funder: Pubkey,
    pub funder_usdc_account: Pubkey,
    pub rent_payer: Pubkey,
}

/// A month topped up by its funder with the approver's signature (the internal audit,
/// 2026-10-29): `amount` µUSDC more in the vault, `total_amount` as it now stands.
#[event]
pub struct UsdcDistributorToppedUp {
    pub mint: Pubkey,
    pub month: i64,
    pub amount: u64,
    pub total_amount: u64,
}

/// The funding pause set or cleared, and who signed.
#[event]
pub struct FundingPauseSet {
    pub paused: bool,
    pub by: Pubkey,
}

/// Every key of the config after `initialize_config` or `update_config`.
#[event]
pub struct ConfigUpdated {
    pub admin: Pubkey,
    pub pauser: Pubkey,
    pub approver: Pubkey,
    pub closer: Pubkey,
    pub pepper_funder: Pubkey,
    pub payer_funder: Pubkey,
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::solana_program::hash::hash;
    use anchor_lang::{Discriminator, Event};

    fn key(byte: u8) -> Pubkey {
        Pubkey::new_from_array([byte; 32])
    }

    /// The discriminator `#[event]` gives a struct named `name`.
    fn discriminator_of(name: &str) -> [u8; 8] {
        hash(format!("event:{name}").as_bytes()).to_bytes()[..8].try_into().unwrap()
    }

    /// Every event `#[event]` declares in a program's `events.rs`, by name: the
    /// file read up to its tests, whose text names the attribute too.
    fn event_names(source: &str) -> Vec<String> {
        const ATTRIBUTE: &str = concat!("#[", "event]");
        let mut names = Vec::new();
        let mut rest = source.split(concat!("#[cfg(", "test)]")).next().unwrap();
        while let Some(at) = rest.find(ATTRIBUTE) {
            rest = &rest[at + ATTRIBUTE.len()..];
            let declaration = rest.trim_start().strip_prefix("pub struct ").expect("the attribute, then pub struct");
            let name: String = declaration.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            names.push(name);
        }
        names
    }

    /// The bytes an event logs: its discriminator, then each field in order.
    fn logged(name: &str, fields: &[&[u8]]) -> Vec<u8> {
        let mut bytes = discriminator_of(name).to_vec();
        for field in fields {
            bytes.extend_from_slice(field);
        }
        bytes
    }

    #[test]
    fn every_event_logs_its_fields_in_order_at_its_size() {
        let (mint, other) = (key(1), key(2));
        let month = 202_611i64.to_le_bytes();
        let cases: [(&str, Vec<u8>, Vec<u8>, usize); 10] = [
            (
                "UsdcDistributorFunded",
                UsdcDistributorFunded { mint, month: 202_611, total_amount: 5_000_000, funder: other }.data(),
                logged("UsdcDistributorFunded", &[mint.as_ref(), &month, &5_000_000u64.to_le_bytes(), other.as_ref()]),
                88,
            ),
            (
                "UsdcDividendClaimed",
                UsdcDividendClaimed { mint, month: 202_611, claimant: other, index: 7, amount: 1_000_000 }.data(),
                logged(
                    "UsdcDividendClaimed",
                    &[mint.as_ref(), &month, other.as_ref(), &7u32.to_le_bytes(), &1_000_000u64.to_le_bytes()],
                ),
                92,
            ),
            (
                "UsdcDistributorClosed",
                UsdcDistributorClosed { mint, month: 202_611, returned: 12, to: other }.data(),
                logged("UsdcDistributorClosed", &[mint.as_ref(), &month, &12u64.to_le_bytes(), other.as_ref()]),
                88,
            ),
            (
                "UsdcDistributorVoided",
                UsdcDistributorVoided { mint, month: 202_611, returned: 12, to: other }.data(),
                logged("UsdcDistributorVoided", &[mint.as_ref(), &month, &12u64.to_le_bytes(), other.as_ref()]),
                88,
            ),
            (
                "ClaimsPaused",
                ClaimsPaused { until: 1_800_000_000, by: other }.data(),
                logged("ClaimsPaused", &[&1_800_000_000i64.to_le_bytes(), other.as_ref()]),
                48,
            ),
            (
                "ClaimsHeld",
                ClaimsHeld { mint, month: 202_611, until: 1_800_000_000, by: other }.data(),
                logged("ClaimsHeld", &[mint.as_ref(), &month, &1_800_000_000i64.to_le_bytes(), other.as_ref()]),
                88,
            ),
            (
                "UsdcDistributorRehomed",
                UsdcDistributorRehomed { mint, month: 202_611, funder: key(3), funder_usdc_account: key(4), rent_payer: key(5) }.data(),
                logged(
                    "UsdcDistributorRehomed",
                    &[mint.as_ref(), &month, key(3).as_ref(), key(4).as_ref(), key(5).as_ref()],
                ),
                144,
            ),
            (
                "UsdcDistributorToppedUp",
                UsdcDistributorToppedUp { mint, month: 202_611, amount: 1_000, total_amount: 5_001_000 }.data(),
                logged(
                    "UsdcDistributorToppedUp",
                    &[mint.as_ref(), &month, &1_000u64.to_le_bytes(), &5_001_000u64.to_le_bytes()],
                ),
                64,
            ),
            (
                "FundingPauseSet",
                FundingPauseSet { paused: true, by: other }.data(),
                logged("FundingPauseSet", &[&[1], other.as_ref()]),
                41,
            ),
            (
                "ConfigUpdated",
                ConfigUpdated {
                    admin: key(11),
                    pauser: key(12),
                    approver: key(13),
                    closer: key(14),
                    pepper_funder: key(15),
                    payer_funder: key(16),
                }
                .data(),
                logged(
                    "ConfigUpdated",
                    &[
                        key(11).as_ref(),
                        key(12).as_ref(),
                        key(13).as_ref(),
                        key(14).as_ref(),
                        key(15).as_ref(),
                        key(16).as_ref(),
                    ],
                ),
                200,
            ),
        ];
        for (name, data, expected, size) in cases {
            assert_eq!(data, expected, "{name}");
            assert_eq!(data.len(), size, "{name}");
        }
    }

    #[test]
    fn each_discriminator_is_anchors_for_its_name() {
        assert_eq!(UsdcDistributorFunded::DISCRIMINATOR, discriminator_of("UsdcDistributorFunded"));
        assert_eq!(UsdcDividendClaimed::DISCRIMINATOR, discriminator_of("UsdcDividendClaimed"));
        assert_eq!(UsdcDistributorClosed::DISCRIMINATOR, discriminator_of("UsdcDistributorClosed"));
        assert_eq!(UsdcDistributorVoided::DISCRIMINATOR, discriminator_of("UsdcDistributorVoided"));
        assert_eq!(ClaimsPaused::DISCRIMINATOR, discriminator_of("ClaimsPaused"));
        assert_eq!(ClaimsHeld::DISCRIMINATOR, discriminator_of("ClaimsHeld"));
        assert_eq!(UsdcDistributorRehomed::DISCRIMINATOR, discriminator_of("UsdcDistributorRehomed"));
        assert_eq!(UsdcDistributorToppedUp::DISCRIMINATOR, discriminator_of("UsdcDistributorToppedUp"));
        assert_eq!(FundingPauseSet::DISCRIMINATOR, discriminator_of("FundingPauseSet"));
        assert_eq!(ConfigUpdated::DISCRIMINATOR, discriminator_of("ConfigUpdated"));
    }

    #[test]
    fn no_event_shares_a_discriminator_with_pepper_curves_or_pepper_raises() {
        let ours = event_names(include_str!("events.rs"));
        assert_eq!(ours.len(), 10, "{ours:?}");
        let curve = event_names(include_str!("../../pepper_curve/src/events.rs"));
        let raise = event_names(include_str!("../../pepper_raise/src/events.rs"));
        // The reader is proven on names each file is known to hold.
        assert!(curve.iter().any(|n| n == "DistributorFunded") && curve.iter().any(|n| n == "DividendClaimed"));
        assert!(raise.iter().any(|n| n == "Claimed"));
        for theirs in curve.iter().chain(raise.iter()) {
            for name in &ours {
                assert_ne!(discriminator_of(name), discriminator_of(theirs), "{name} against {theirs}");
            }
        }
        // Nor do two of ours share one.
        for (i, a) in ours.iter().enumerate() {
            for b in &ours[i + 1..] {
                assert_ne!(discriminator_of(a), discriminator_of(b), "{a} against {b}");
            }
        }
    }
}
