//! Emitted events. The off-chain indexer reads these (via `getTransaction`
//! logs / parsed CPI) instead of the old balance-diff heuristic — fees are now
//! structurally known, so there is no bypass to detect (retires B1).

use anchor_lang::prelude::*;

#[event]
pub struct CurveCreated {
    pub mint: Pubkey,
    pub creator: Pubkey,
    pub creator_target_lamports: u64,
    pub cpmm_seed_lamports: u64,
}

#[event]
pub struct Trade {
    pub mint: Pubkey,
    pub trader: Pubkey,
    /// true = buy (SOL in, tokens out); false = sell (tokens in, SOL out).
    pub is_buy: bool,
    pub sol_amount: u64,
    pub token_amount: u64,
    pub creator_fee: u64,
    pub fanex_fee: u64,
    /// 1% platform swap fee paid to Fanex (separate from the fundraise share).
    pub platform_fee: u64,
    pub real_sol_reserves: u64,
    pub real_token_reserves: u64,
}

#[event]
pub struct GraduationReady {
    pub mint: Pubkey,
    pub real_sol_reserves: u64,
}

#[event]
pub struct Graduated {
    pub mint: Pubkey,
    pub sol_released: u64,
    pub tokens_released: u64,
    pub keeper: Pubkey,
}

#[event]
pub struct VestingReleased {
    pub mint: Pubkey,
    pub beneficiary: Pubkey,
    pub amount: u64,
}

#[event]
pub struct DistributorFunded {
    pub mint: Pubkey,
    pub month: i64,
    pub total_amount: u64,
}

/// A v1 month's close handed over (`set_distributor_funder`, 2026-10-24): `funder`
/// closes it from now on, in `previous`'s place.
#[event]
pub struct DistributorFunderSet {
    pub mint: Pubkey,
    pub month: i64,
    pub previous: Pubkey,
    pub funder: Pubkey,
}

#[event]
pub struct DividendClaimed {
    pub mint: Pubkey,
    pub month: i64,
    pub claimant: Pubkey,
    pub amount: u64,
}

/// A fan-led vesting allocation was handed to the creator who claimed the
/// channel. Emitted at most once per curve — the placeholder guard makes a
/// second one impossible.
#[event]
pub struct ClaimStatusClosed {
    pub claim_status: Pubkey,
    pub refunded_to: Pubkey,
    pub lamports: u64,
}

#[event]
pub struct VestingBeneficiarySet {
    pub mint: Pubkey,
    pub previous: Pubkey,
    pub new_beneficiary: Pubkey,
}

/// `Curve.creator` moved off the fan-led placeholder, copied from the vesting
/// beneficiary a claim already set (`sync_curve_creator`).
#[event]
pub struct CurveCreatorSynced {
    pub mint: Pubkey,
    pub previous: Pubkey,
    pub new_creator: Pubkey,
}

// ── The USDC curve (USDC curve design §10.6) ─────────────────────────────────
//
// New events beside the SOL ones, which do not move: the indexer keeps decoding
// the 129-byte `Trade` for a SOL curve. USDC amounts in µUSDC, token amounts in
// base units. Each size, the 8-byte discriminator included, is pinned by
// `usdc_event_tests` below.

/// 88 bytes.
#[event]
pub struct CurveCreatedUsdc {
    pub mint: Pubkey,
    pub creator: Pubkey,
    pub creator_target_usdc: u64,
    pub graduation_threshold_usdc: u64,
}

/// 161 bytes. ⚠️ Neither `authority` nor `token_owner` is the end user of a
/// routed trade: in a router's shared-accounts route both are the router's
/// authority. The indexer records a trade only when `authority` signed it and
/// owns the destination (buy) or the source (sale), and drops any other.
#[event]
pub struct TradeUsdc {
    pub mint: Pubkey,
    /// Account 0 of the swap, its signer.
    pub authority: Pubkey,
    /// The owner of the destination token account (buy) or the source (sale).
    pub token_owner: Pubkey,
    /// true = buy (USDC in, tokens out); false = sell (tokens in, USDC out).
    pub is_buy: bool,
    /// On a buy everything charged; on a sale the gross, before the fee, as
    /// `Trade.sol_amount`.
    pub usdc_amount: u64,
    pub token_amount: u64,
    pub creator_fee: u64,
    pub fanex_fee: u64,
    /// The 1% platform swap fee.
    pub platform_fee: u64,
    pub real_usdc_reserves: u64,
    pub real_token_reserves: u64,
}

/// 48 bytes: the buy that froze the curve at its threshold.
#[event]
pub struct GraduationReadyUsdc {
    pub mint: Pubkey,
    pub real_usdc_reserves: u64,
}

/// 96 bytes: `graduate_usdc_pool`, the first graduation step.
#[event]
pub struct GraduationPoolCreated {
    pub mint: Pubkey,
    pub pool_state: Pubkey,
    pub usdc_seeded: u64,
    pub tokens_seeded: u64,
    /// Read from the LP account after the CPI.
    pub lp_amount: u64,
}

/// 112 bytes: `graduate_usdc_lock`, the second step.
#[event]
pub struct GraduatedUsdc {
    pub mint: Pubkey,
    pub pool_state: Pubkey,
    pub lp_locked: u64,
    pub fee_nft_mint: Pubkey,
}

/// 200 bytes: the config's keys after `initialize_usdc_config` or
/// `update_usdc_config`.
#[event]
pub struct UsdcConfigUpdated {
    pub admin: Pubkey,
    pub pauser: Pubkey,
    pub migration_authority: Pubkey,
    pub fee_usdc_account: Pubkey,
    pub lp_fee_owner: Pubkey,
    pub launcher: Pubkey,
}

/// 73 bytes.
#[event]
pub struct UsdcPauseSet {
    /// All-zero for the global pause.
    pub curve: Pubkey,
    pub paused: bool,
    pub by: Pubkey,
}

/// 200 bytes: `set_usdc_raydium`, the five addresses as set.
#[event]
pub struct UsdcRaydiumSet {
    pub raydium_cpmm_program: Pubkey,
    pub raydium_amm_config: Pubkey,
    pub raydium_create_pool_fee: Pubkey,
    pub raydium_lock_program: Pubkey,
    pub raydium_lock_authority: Pubkey,
    pub by: Pubkey,
}

#[cfg(test)]
mod usdc_event_tests {
    use super::*;
    use anchor_lang::{Discriminator, Event};

    fn key(byte: u8) -> Pubkey {
        Pubkey::new_from_array([byte; 32])
    }

    /// The event as `emit!` logs it: its discriminator, then its fields in
    /// declaration order, each in Borsh (Pubkey 32 bytes, u64 8 little-endian,
    /// bool 1). Checked against the bytes the spec's order gives, so a field
    /// moved or retyped fails here, and against the pinned size.
    fn assert_wire<E: Event + Discriminator>(event: &E, fields: &[&[u8]], size: usize) {
        let data = event.data();
        let mut expected = E::DISCRIMINATOR.to_vec();
        for field in fields {
            expected.extend_from_slice(field);
        }
        assert_eq!(data, expected);
        assert_eq!(data.len(), size);
        assert_eq!(8 + event.try_to_vec().unwrap().len(), size);
    }

    #[test]
    fn curve_created_usdc_is_88_bytes() {
        let event = CurveCreatedUsdc {
            mint: key(1),
            creator: key(2),
            creator_target_usdc: 3,
            graduation_threshold_usdc: 4,
        };
        let parts: [&[u8]; 4] = [
            &key(1).to_bytes(),
            &key(2).to_bytes(),
            &3u64.to_le_bytes(),
            &4u64.to_le_bytes(),
        ];
        assert_wire(&event, &parts, 88);
    }

    #[test]
    fn trade_usdc_is_161_bytes() {
        let event = TradeUsdc {
            mint: key(1),
            authority: key(2),
            token_owner: key(3),
            is_buy: true,
            usdc_amount: 4,
            token_amount: 5,
            creator_fee: 6,
            fanex_fee: 7,
            platform_fee: 8,
            real_usdc_reserves: 9,
            real_token_reserves: 10,
        };
        let parts: [&[u8]; 11] = [
            &key(1).to_bytes(),
            &key(2).to_bytes(),
            &key(3).to_bytes(),
            &[1],
            &4u64.to_le_bytes(),
            &5u64.to_le_bytes(),
            &6u64.to_le_bytes(),
            &7u64.to_le_bytes(),
            &8u64.to_le_bytes(),
            &9u64.to_le_bytes(),
            &10u64.to_le_bytes(),
        ];
        assert_wire(&event, &parts, 161);
    }

    #[test]
    fn graduation_ready_usdc_is_48_bytes() {
        let event = GraduationReadyUsdc {
            mint: key(1),
            real_usdc_reserves: 2,
        };
        let parts: [&[u8]; 2] = [&key(1).to_bytes(), &2u64.to_le_bytes()];
        assert_wire(&event, &parts, 48);
    }

    #[test]
    fn graduation_pool_created_is_96_bytes() {
        let event = GraduationPoolCreated {
            mint: key(1),
            pool_state: key(2),
            usdc_seeded: 3,
            tokens_seeded: 4,
            lp_amount: 5,
        };
        let parts: [&[u8]; 5] = [
            &key(1).to_bytes(),
            &key(2).to_bytes(),
            &3u64.to_le_bytes(),
            &4u64.to_le_bytes(),
            &5u64.to_le_bytes(),
        ];
        assert_wire(&event, &parts, 96);
    }

    #[test]
    fn graduated_usdc_is_112_bytes() {
        let event = GraduatedUsdc {
            mint: key(1),
            pool_state: key(2),
            lp_locked: 3,
            fee_nft_mint: key(4),
        };
        let parts: [&[u8]; 4] = [
            &key(1).to_bytes(),
            &key(2).to_bytes(),
            &3u64.to_le_bytes(),
            &key(4).to_bytes(),
        ];
        assert_wire(&event, &parts, 112);
    }

    #[test]
    fn usdc_config_updated_is_200_bytes() {
        let event = UsdcConfigUpdated {
            admin: key(1),
            pauser: key(2),
            migration_authority: key(3),
            fee_usdc_account: key(4),
            lp_fee_owner: key(5),
            launcher: key(6),
        };
        let parts: [&[u8]; 6] = [
            &key(1).to_bytes(),
            &key(2).to_bytes(),
            &key(3).to_bytes(),
            &key(4).to_bytes(),
            &key(5).to_bytes(),
            &key(6).to_bytes(),
        ];
        assert_wire(&event, &parts, 200);
    }

    #[test]
    fn usdc_pause_set_is_73_bytes() {
        let event = UsdcPauseSet {
            curve: key(1),
            paused: true,
            by: key(2),
        };
        let parts: [&[u8]; 3] = [&key(1).to_bytes(), &[1], &key(2).to_bytes()];
        assert_wire(&event, &parts, 73);
    }

    #[test]
    fn usdc_raydium_set_is_200_bytes() {
        let event = UsdcRaydiumSet {
            raydium_cpmm_program: key(1),
            raydium_amm_config: key(2),
            raydium_create_pool_fee: key(3),
            raydium_lock_program: key(4),
            raydium_lock_authority: key(5),
            by: key(6),
        };
        let parts: [&[u8]; 6] = [
            &key(1).to_bytes(),
            &key(2).to_bytes(),
            &key(3).to_bytes(),
            &key(4).to_bytes(),
            &key(5).to_bytes(),
            &key(6).to_bytes(),
        ];
        assert_wire(&event, &parts, 200);
    }

    #[test]
    fn the_usdc_events_have_their_own_discriminators() {
        // The first 8 bytes of sha256("event:<Name>"): the log decoder tells the
        // events apart by these alone, the SOL `Trade` from `TradeUsdc` included.
        let usdc: [&[u8]; 8] = [
            CurveCreatedUsdc::DISCRIMINATOR,
            TradeUsdc::DISCRIMINATOR,
            GraduationReadyUsdc::DISCRIMINATOR,
            GraduationPoolCreated::DISCRIMINATOR,
            GraduatedUsdc::DISCRIMINATOR,
            UsdcConfigUpdated::DISCRIMINATOR,
            UsdcPauseSet::DISCRIMINATOR,
            UsdcRaydiumSet::DISCRIMINATOR,
        ];
        assert_eq!(
            TradeUsdc::DISCRIMINATOR,
            &[130, 233, 100, 107, 224, 152, 155, 41]
        );
        let sol: [&[u8]; 10] = [
            CurveCreated::DISCRIMINATOR,
            Trade::DISCRIMINATOR,
            GraduationReady::DISCRIMINATOR,
            Graduated::DISCRIMINATOR,
            VestingReleased::DISCRIMINATOR,
            DistributorFunded::DISCRIMINATOR,
            DividendClaimed::DISCRIMINATOR,
            ClaimStatusClosed::DISCRIMINATOR,
            VestingBeneficiarySet::DISCRIMINATOR,
            CurveCreatorSynced::DISCRIMINATOR,
        ];
        let all: Vec<&[u8]> = usdc.iter().chain(sol.iter()).copied().collect();
        for (i, a) in all.iter().enumerate() {
            assert_eq!(a.len(), 8);
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}

// ── The USDC pause of the SOL side (USDC curve design §10.3.3) ──────────────────

/// 72 bytes: `usdc_pause_sol` or `usdc_pause_sol_curve`, the USDC pause of the SOL
/// side, signed by `UsdcConfig.pauser`. The SOL pauses of `GlobalConfig.admin`,
/// `set_paused` and `set_curve_paused`, emit nothing, so this names only the
/// pauser's. There is no `paused` field: these two only ever write `true`.
#[event]
pub struct UsdcPauseSolSet {
    /// The SOL `Curve` paused; all-zero for the whole SOL side (`GlobalConfig.paused`).
    pub curve: Pubkey,
    pub by: Pubkey,
}

#[cfg(test)]
mod usdc_pause_sol_event_tests {
    use super::*;
    use anchor_lang::{Discriminator, Event};

    #[test]
    fn usdc_pause_sol_set_is_72_bytes_with_a_discriminator_of_its_own() {
        let curve = Pubkey::new_from_array([1; 32]);
        let by = Pubkey::new_from_array([2; 32]);
        let event = UsdcPauseSolSet { curve, by };
        let mut expected = UsdcPauseSolSet::DISCRIMINATOR.to_vec();
        expected.extend_from_slice(&curve.to_bytes());
        expected.extend_from_slice(&by.to_bytes());
        assert_eq!(event.data(), expected);
        assert_eq!(event.data().len(), 72);

        let others: [&[u8]; 18] = [
            CurveCreated::DISCRIMINATOR,
            Trade::DISCRIMINATOR,
            GraduationReady::DISCRIMINATOR,
            Graduated::DISCRIMINATOR,
            VestingReleased::DISCRIMINATOR,
            DistributorFunded::DISCRIMINATOR,
            DividendClaimed::DISCRIMINATOR,
            ClaimStatusClosed::DISCRIMINATOR,
            VestingBeneficiarySet::DISCRIMINATOR,
            CurveCreatorSynced::DISCRIMINATOR,
            CurveCreatedUsdc::DISCRIMINATOR,
            TradeUsdc::DISCRIMINATOR,
            GraduationReadyUsdc::DISCRIMINATOR,
            GraduationPoolCreated::DISCRIMINATOR,
            GraduatedUsdc::DISCRIMINATOR,
            UsdcConfigUpdated::DISCRIMINATOR,
            UsdcPauseSet::DISCRIMINATOR,
            UsdcRaydiumSet::DISCRIMINATOR,
        ];
        assert_eq!(UsdcPauseSolSet::DISCRIMINATOR.len(), 8);
        for other in others {
            assert_ne!(UsdcPauseSolSet::DISCRIMINATOR, other);
        }
    }
}

// ── The internal audit's fix round (2026-10-29, docs/security/INTERNAL-AUDIT-2026-10-29.md) ──────

/// 80 bytes: `close_creator_raise_usdc`, `UsdcConfig.admin` closing a USDC curve's creator raise at
/// what it has received, so its buys no longer need the creator's USDC account (one frozen or lost
/// would stop them for good). `creator_cap_usdc` is the cap after the close, the creator's received.
#[event]
pub struct CreatorRaiseClosedUsdc {
    pub mint: Pubkey,
    pub creator_cap_usdc: u64,
    pub by: Pubkey,
}

#[cfg(test)]
mod audit_fix_event_tests {
    use super::*;
    use anchor_lang::{Discriminator, Event};

    #[test]
    fn creator_raise_closed_usdc_is_80_bytes_with_a_discriminator_of_its_own() {
        let mint = Pubkey::new_from_array([1; 32]);
        let by = Pubkey::new_from_array([2; 32]);
        let event = CreatorRaiseClosedUsdc { mint, creator_cap_usdc: 7, by };
        let mut expected = CreatorRaiseClosedUsdc::DISCRIMINATOR.to_vec();
        expected.extend_from_slice(&mint.to_bytes());
        expected.extend_from_slice(&7u64.to_le_bytes());
        expected.extend_from_slice(&by.to_bytes());
        assert_eq!(event.data(), expected);
        assert_eq!(event.data().len(), 80);
        let others: [&[u8]; 19] = [
            CurveCreated::DISCRIMINATOR,
            Trade::DISCRIMINATOR,
            GraduationReady::DISCRIMINATOR,
            Graduated::DISCRIMINATOR,
            VestingReleased::DISCRIMINATOR,
            DistributorFunded::DISCRIMINATOR,
            DividendClaimed::DISCRIMINATOR,
            ClaimStatusClosed::DISCRIMINATOR,
            VestingBeneficiarySet::DISCRIMINATOR,
            CurveCreatorSynced::DISCRIMINATOR,
            CurveCreatedUsdc::DISCRIMINATOR,
            TradeUsdc::DISCRIMINATOR,
            GraduationReadyUsdc::DISCRIMINATOR,
            GraduationPoolCreated::DISCRIMINATOR,
            GraduatedUsdc::DISCRIMINATOR,
            UsdcConfigUpdated::DISCRIMINATOR,
            UsdcPauseSet::DISCRIMINATOR,
            UsdcRaydiumSet::DISCRIMINATOR,
            UsdcPauseSolSet::DISCRIMINATOR,
        ];
        for other in others {
            assert_ne!(CreatorRaiseClosedUsdc::DISCRIMINATOR, other);
        }
    }
}
