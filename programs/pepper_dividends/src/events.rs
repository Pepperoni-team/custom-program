//! Events, one per state change an indexer has to see (spec §6). Named apart from
//! `pepper_curve`'s, the raise's inside it included, so that no two share a discriminator:
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

// ── The reserve (the reserve's spec, 2026-11-07) ─────────────────────────────

/// `amount` µUSDC deposited into a token's reserve by its owner, its vault holding `balance`
/// after it.
#[event]
pub struct ReserveDeposited {
    pub mint: Pubkey,
    pub owner: Pubkey,
    pub amount: u64,
    pub balance: u64,
}

/// The owner asked for the reserve back: from `requested_at` the reserve pays only months
/// accrued before it, and from `withdrawable_at` the owner withdraws.
#[event]
pub struct ReserveWithdrawalRequested {
    pub mint: Pubkey,
    pub owner: Pubkey,
    pub requested_at: i64,
    pub withdrawable_at: i64,
}

/// The owner's pending withdrawal cancelled.
#[event]
pub struct ReserveWithdrawalCancelled {
    pub mint: Pubkey,
    pub owner: Pubkey,
}

/// The reserve's whole balance, `amount` µUSDC, paid back to its owner's USDC account.
#[event]
pub struct ReserveWithdrawn {
    pub mint: Pubkey,
    pub owner: Pubkey,
    pub amount: u64,
}

/// A month funded from its token's reserve: `from_reserve` and the payer's `fronted` µUSDC
/// into the vault, `total_amount` in all, then `fee_from_reserve` of Pepper's 3% to its fee
/// account and `fee_unpaid` left owed.
#[event]
pub struct UsdcDistributorFundedFromReserve {
    pub mint: Pubkey,
    pub month: i64,
    pub total_amount: u64,
    pub from_reserve: u64,
    pub fronted: u64,
    pub fee_from_reserve: u64,
    pub fee_unpaid: u64,
}

/// A void of a month its reserve funded short: `amount` µUSDC, what the payer fronted, back to
/// `to`, the rent payer's USDC account, before the rest goes to the reserve
/// (`UsdcDistributorVoided`).
#[event]
pub struct UsdcDistributorFrontedReturned {
    pub mint: Pubkey,
    pub month: i64,
    pub amount: u64,
    pub to: Pubkey,
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
        let cases: [(&str, Vec<u8>, Vec<u8>, usize); 16] = [
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
            (
                "ReserveDeposited",
                ReserveDeposited { mint, owner: other, amount: 1_000_000_000, balance: 1_500_000_000 }.data(),
                logged(
                    "ReserveDeposited",
                    &[mint.as_ref(), other.as_ref(), &1_000_000_000u64.to_le_bytes(), &1_500_000_000u64.to_le_bytes()],
                ),
                88,
            ),
            (
                "ReserveWithdrawalRequested",
                ReserveWithdrawalRequested { mint, owner: other, requested_at: 1_800_000_000, withdrawable_at: 1_803_888_000 }.data(),
                logged(
                    "ReserveWithdrawalRequested",
                    &[mint.as_ref(), other.as_ref(), &1_800_000_000i64.to_le_bytes(), &1_803_888_000i64.to_le_bytes()],
                ),
                88,
            ),
            (
                "ReserveWithdrawalCancelled",
                ReserveWithdrawalCancelled { mint, owner: other }.data(),
                logged("ReserveWithdrawalCancelled", &[mint.as_ref(), other.as_ref()]),
                72,
            ),
            (
                "ReserveWithdrawn",
                ReserveWithdrawn { mint, owner: other, amount: 7 }.data(),
                logged("ReserveWithdrawn", &[mint.as_ref(), other.as_ref(), &7u64.to_le_bytes()]),
                80,
            ),
            (
                "UsdcDistributorFundedFromReserve",
                UsdcDistributorFundedFromReserve {
                    mint,
                    month: 202_611,
                    total_amount: 97,
                    from_reserve: 90,
                    fronted: 7,
                    fee_from_reserve: 0,
                    fee_unpaid: 3,
                }
                .data(),
                logged(
                    "UsdcDistributorFundedFromReserve",
                    &[
                        mint.as_ref(),
                        &month,
                        &97u64.to_le_bytes(),
                        &90u64.to_le_bytes(),
                        &7u64.to_le_bytes(),
                        &0u64.to_le_bytes(),
                        &3u64.to_le_bytes(),
                    ],
                ),
                88,
            ),
            (
                "UsdcDistributorFrontedReturned",
                UsdcDistributorFrontedReturned { mint, month: 202_611, amount: 7, to: other }.data(),
                logged("UsdcDistributorFrontedReturned", &[mint.as_ref(), &month, &7u64.to_le_bytes(), other.as_ref()]),
                88,
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
        assert_eq!(ReserveDeposited::DISCRIMINATOR, discriminator_of("ReserveDeposited"));
        assert_eq!(ReserveWithdrawalRequested::DISCRIMINATOR, discriminator_of("ReserveWithdrawalRequested"));
        assert_eq!(ReserveWithdrawalCancelled::DISCRIMINATOR, discriminator_of("ReserveWithdrawalCancelled"));
        assert_eq!(ReserveWithdrawn::DISCRIMINATOR, discriminator_of("ReserveWithdrawn"));
        assert_eq!(UsdcDistributorFundedFromReserve::DISCRIMINATOR, discriminator_of("UsdcDistributorFundedFromReserve"));
        assert_eq!(UsdcDistributorFrontedReturned::DISCRIMINATOR, discriminator_of("UsdcDistributorFrontedReturned"));
    }

    #[test]
    fn no_event_shares_a_discriminator_with_pepper_curves_nor_its_raises() {
        let ours = event_names(include_str!("events.rs"));
        assert_eq!(ours.len(), 16, "{ours:?}");
        let curve = event_names(include_str!("../../pepper_curve/src/events.rs"));
        // The raise lives in pepper_curve since N+3, its events in their own file.
        let raise = event_names(include_str!("../../pepper_curve/src/raise/events.rs"));
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
