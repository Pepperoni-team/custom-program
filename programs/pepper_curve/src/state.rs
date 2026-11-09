//! On-chain account state.

use anchor_lang::prelude::*;

/// Bonding-curve lifecycle. Mirrors `solana_curves.current_phase` off-chain.
#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Copy, PartialEq, Eq, Debug)]
pub enum CurvePhase {
    /// Accepting buys/sells on the bonding curve.
    Bonding,
    /// Threshold reached; awaiting the keeper to release liquidity into Raydium
    /// CPMM. Trading on our program is frozen (read-only) in this state.
    Graduating,
    /// Liquidity handed off to Raydium CPMM. Our program no longer trades.
    Graduated,
}

/// Singleton program config (PDA: [CONFIG_SEED]).
#[account]
#[derive(InitSpace)]
pub struct GlobalConfig {
    /// Admin authority allowed to update config + toggle the global pause; since
    /// 2026-10-24 (the fee-wallet split) also the Metaplex update authority of
    /// every SOL token launched (`create_curve`).
    pub admin: Pubkey,
    /// Where `buy` and `sell` pay Fanex: the 1% platform swap fee on every trade
    /// and a buy's share of the raise. Read by those two alone since 2026-10-24
    /// (the fee-wallet split): receive-only, it signs and authorizes nothing.
    pub fanex_fee_wallet: Pubkey,
    /// Keeper authority permitted to call `graduate` (release liquidity); since
    /// 2026-10-24 also the approver of every v2 dividend month and the only
    /// funder of a v1 one.
    pub graduation_authority: Pubkey,
    /// On-chain hard kill-switch. When true, create/buy/sell/distribute halt.
    pub paused: bool,
    pub bump: u8,
}

/// Per-token bonding curve (PDA: [CURVE_SEED, mint]).
#[account]
#[derive(InitSpace)]
pub struct Curve {
    /// The SPL mint this curve trades.
    pub mint: Pubkey,
    /// Creator wallet — receives the 94% atomic split + vesting.
    pub creator: Pubkey,
    pub phase: CurvePhase,

    // ── Economics (immutable after create) ───────────────────────────────
    /// Gross creator target in lamports (includes the 6% Fanex fee). >= 16 SOL.
    pub creator_target_lamports: u64,
    /// SOL routed into the curve that triggers graduation + seeds the CPMM.
    pub cpmm_seed_lamports: u64,
    /// Per-buy split, applied to the buyer's SOL until the caps below fill.
    pub creator_split_bps: u64,
    pub fanex_split_bps: u64,
    /// Hard caps: total creator/Fanex SOL ever taken (0.94×target / 0.06×target).
    pub creator_cap_lamports: u64,
    pub fanex_cap_lamports: u64,

    // ── Constant-product bonding curve reserves (base units) ──────────────
    pub virtual_sol_reserves: u64,
    pub virtual_token_reserves: u64,
    /// SOL actually collected into the curve (withdrawable at graduation).
    pub real_sol_reserves: u64,
    /// Tokens remaining for sale on the curve.
    pub real_token_reserves: u64,
    /// Tokens reserved to seed the Raydium CPMM at graduation.
    pub migration_token_reserves: u64,

    // ── Running totals (for caps + telemetry) ─────────────────────────────
    pub creator_received_lamports: u64,
    pub fanex_received_lamports: u64,

    /// Per-curve pause (independent of the global kill-switch).
    pub paused: bool,

    pub created_at: i64,
    pub graduated_at: i64,

    // ── Bumps for the curve's owned PDAs ──────────────────────────────────
    pub bump: u8,
    pub sol_vault_bump: u8,
}

/// Who may launch a curve without the launch cap (PDA: [LAUNCH_GUARD_SEED],
/// 2026-10-14), set by `config.admin` through `set_launch_guard_authority`.
/// An all-zero `authority` lets nobody: no key signs for it.
#[account]
#[derive(InitSpace)]
pub struct LaunchGuardConfig {
    pub authority: Pubkey,
    pub bump: u8,
}

/// A curve launched without the launch cap (PDA: [LAUNCH_GUARD_WAIVER_SEED,
/// mint], 2026-10-14). Only `waive_launch_guard` makes one, signed by
/// `LaunchGuardConfig.authority`, before the curve exists; `buy` skips the cap
/// when it is passed as an extra account (`validation::launch_guard_waived`).
/// Never closed.
#[account]
#[derive(InitSpace)]
pub struct LaunchGuardWaiver {
    pub mint: Pubkey,
    pub bump: u8,
}

/// Program-owned lamport vault for a curve's SOL reserves
/// (PDA: [SOL_VAULT_SEED, mint]). Holds rent + `real_sol_reserves`.
#[account]
#[derive(InitSpace)]
pub struct SolVault {
    pub mint: Pubkey,
    pub bump: u8,
}

/// Creator vesting schedule (PDA: [VESTING_SEED, mint]).
#[account]
#[derive(InitSpace)]
pub struct Vesting {
    pub mint: Pubkey,
    pub beneficiary: Pubkey,
    pub total_amount: u64,
    pub released_amount: u64,
    pub start_time: i64,
    pub cliff_seconds: i64,
    pub duration_seconds: i64,
    pub bump: u8,
}

/// A funded monthly dividend distribution (PDA: [DISTRIBUTOR_SEED, mint, month]).
/// Holds the SOL to be claimed; `merkle_root` commits to (claimant, amount).
#[account]
#[derive(InitSpace)]
pub struct Distributor {
    pub mint: Pubkey,
    /// Off-chain period key (e.g. YYYYMM as a little-endian u32 in an i64).
    pub month: i64,
    pub merkle_root: [u8; 32],
    pub total_amount: u64,
    pub claimed_amount: u64,
    /// Who funded it (allowed to reclaim leftovers after expiry).
    pub funder: Pubkey,
    /// Unix time after which the funder may reclaim unclaimed SOL.
    pub expires_at: i64,
    pub bump: u8,
}

/// A funded monthly dividend distribution that records its claims itself
/// (PDA: [DISTRIBUTOR_SEED, mint, month], the same address a v1 `Distributor`
/// would take, so a month has one distributor whichever its version).
///
/// One bit per Merkle leaf, set by `claim_dividend_v2`: a claim creates no
/// account, so it costs only the network fee. The funder pays the bitmap once,
/// inside this account's rent, and gets it back at close. The v1 fields come
/// first, at the v1 offsets, so an off-chain reader that needs only totals,
/// funder and expiry decodes both versions alike; the discriminator tells them
/// apart, and every instruction refuses the other version's account.
#[account]
pub struct DistributorV2 {
    pub mint: Pubkey,
    /// Off-chain period key (YYYYMM), as in `Distributor`.
    pub month: i64,
    /// Root over v2 leaves (`math::dividend_leaf_v2`): each leaf binds its index.
    pub merkle_root: [u8; 32],
    pub total_amount: u64,
    pub claimed_amount: u64,
    /// Who funded it (allowed to close it, as in `Distributor`).
    pub funder: Pubkey,
    pub expires_at: i64,
    pub bump: u8,
    /// Leaves the root commits to; `claimed` holds one bit for each.
    pub leaf_count: u32,
    /// Bit `index % 8` of byte `index / 8` is set once leaf `index` has claimed
    /// (`validation::leaf_bit`). Exactly `ceil(leaf_count / 8)` bytes.
    pub claimed: Vec<u8>,
}

impl DistributorV2 {
    /// Discriminator, the v1 fields, `leaf_count`, and the bitmap's 4-byte length.
    pub const HEADER_SPACE: usize = 8 + 32 + 8 + 32 + 8 + 8 + 32 + 8 + 1 + 4 + 4;

    /// The account's size for `leaf_count` leaves, discriminator included. Clamped
    /// to `MAX_DISTRIBUTOR_V2_LEAVES` so an out-of-range count reaches the
    /// handler's `InvalidLeafCount` instead of failing the allocation first.
    pub fn space(leaf_count: u32) -> usize {
        let leaves = leaf_count.min(crate::constants::MAX_DISTRIBUTOR_V2_LEAVES);
        Self::HEADER_SPACE + crate::validation::claim_registry_bytes(leaves)
    }
}

/// Marks a single (distributor, claimant) as claimed
/// (PDA: [CLAIM_SEED, distributor, claimant]). Existence == claimed.
///
/// ⚠️ Two formats exist on chain. Accounts created before the A+D upgrade are
/// 56 bytes and carry no `rent_payer` — their rent was paid by the claimant.
/// `close_claim_status` refunds by format: new accounts to the recorded
/// `rent_payer`, legacy ones to the `claimant`. `Account<ClaimStatus>` only
/// deserializes the new format, which is fine everywhere except that close
/// path — it parses manually for exactly this reason.
#[account]
#[derive(InitSpace)]
pub struct ClaimStatus {
    pub claimant: Pubkey,
    pub amount: u64,
    pub claimed_at: i64,
    /// Who funded this account's rent-exemption (the Fanex sponsor wallet in
    /// the ordinary flow) — where the rent returns on close.
    pub rent_payer: Pubkey,
}

// ── The USDC curve (USDC curve design §10.2) ─────────────────────────────────
//
// Appended below the SOL accounts, which do not move. Each account starts with
// a `version` byte and ends with `reserved` bytes: a new field takes its bytes
// from `reserved`, never a new place, so every account already on chain keeps
// its layout. The offsets are pinned by `usdc_layout_tests` below. No
// `#[derive(Default)]`: a `reserved` array past 32 bytes has none.

/// The USDC curve's config (PDA: [USDC_CONFIG_SEED]), 491 bytes on chain
/// (8 + 483). `GlobalConfig` has no spare byte, hence a second config; the USDC
/// side reads `GlobalConfig` for a key only, never for money, pauses or fees.
#[account]
#[derive(InitSpace)]
pub struct UsdcConfig {
    /// Layout version: 1.
    pub version: u8,
    pub bump: u8,
    /// The USDC side's kill-switch: stops `create_curve_usdc`, both swaps and
    /// both graduation steps, never the vesting or the admin instructions.
    pub paused: bool,
    /// Changes the config and unpauses (the Squad's vault once handed over);
    /// the Metaplex update authority of every token launched on the USDC curve.
    pub admin: Pubkey,
    /// The USDC pause's one key: may only pause, the USDC side and, through
    /// `usdc_pause_sol` and `usdc_pause_sol_curve`, the SOL side, and through
    /// `usdc_pause_raise` the raise; a hot key, so a pause stays fast.
    pub pauser: Pubkey,
    /// Signs the migration permits of the SOL curves; all-zero, nobody.
    pub migration_authority: Pubkey,
    /// Set once, at `initialize_usdc_config`.
    pub usdc_mint: Pubkey,
    /// The fee wallet's USDC account: every fee and Fanex share lands there.
    /// Its associated account, not frozen, closable only by this config's PDA
    /// (the internal audit, 2026-10-29: `validation::fee_usdc_account_valid`), so
    /// the fee wallet sweeps it and never closes it under the swaps.
    pub fee_usdc_account: Pubkey,
    /// Receives a graduated pool's Fee Key, so it harvests the LP fees.
    pub lp_fee_owner: Pubkey,
    // The five Raydium fields: written by the program's upgrade authority alone
    // (`set_usdc_raydium`), all-zero until then, and no curve graduates while
    // they are (`RaydiumNotSet`).
    pub raydium_cpmm_program: Pubkey,
    /// The 1% fee tier the graduation seeds its pool on.
    pub raydium_amm_config: Pubkey,
    /// The account Raydium's `initialize` takes its pool-creation fee into.
    pub raydium_create_pool_fee: Pubkey,
    pub raydium_lock_program: Pubkey,
    /// The LP lock's authority PDA.
    pub raydium_lock_authority: Pubkey,
    /// The only key `create_curve_usdc` takes as its payer: the sponsor.
    pub launcher: Pubkey,
    /// Approves a creator's own claim of a fan-led USDC market (`claim_fan_led_usdc`, Rick,
    /// 2026-11-05): a server key, set by the admin (`set_claim_approver`); all-zero, nobody, which is
    /// what every config written before N+3 reads here (these 32 bytes were reserved zeros).
    pub claim_approver: Pubkey,
    /// 128 until `launcher` took 32 of them (2026-10-22), 96 until `claim_approver` took 32 more
    /// (2026-11-05).
    pub reserved: [u8; 64],
}

impl UsdcConfig {
    /// The five Raydium fields in their order, for `raydium::raydium_addresses_set`: the launch and
    /// both graduation steps refuse while any is all-zero (`RaydiumNotSet`).
    pub fn raydium_addresses(&self) -> [Pubkey; 5] {
        [
            self.raydium_cpmm_program,
            self.raydium_amm_config,
            self.raydium_create_pool_fee,
            self.raydium_lock_program,
            self.raydium_lock_authority,
        ]
    }
}

/// A curve priced in USDC (PDA: [CURVE_SEED, mint], the address a SOL `Curve`
/// would take: a mint has one or the other), 354 bytes on chain (8 + 346).
/// Another account type, so every SOL instruction refuses it by its
/// discriminator (3002) and every USDC instruction refuses a `Curve` alike.
/// USDC amounts in µUSDC, token amounts in base units; each field keeps its
/// SOL twin's meaning. Unlike `Curve`, the bumps come first.
#[account]
#[derive(InitSpace)]
pub struct CurveUsdc {
    /// Layout version: 1.
    pub version: u8,
    pub bump: u8,
    /// The bump of the curve's USDC vault, [USDC_VAULT_SEED, mint].
    pub usdc_vault_bump: u8,
    /// A raw u8 read through `constants::PHASE_*` (0 Bonding, 1 Graduating,
    /// 2 Graduated, 3 PoolCreated), never `CurvePhase`, which stays the SOL
    /// enum as it is.
    pub phase: u8,
    /// Per-curve pause: stops this curve's two swaps and its graduation.
    pub paused: bool,
    /// A curve converted from SOL (the migration, USDC curve design §10.10).
    pub migrated: bool,
    pub mint: Pubkey,
    /// As `Curve.creator`: the fee beneficiary, the fan-led placeholder until
    /// the claim.
    pub creator: Pubkey,
    /// Where the creator's raise split is paid; all-zero when
    /// `creator_split_bps` is 0.
    pub creator_usdc_account: Pubkey,
    /// Narrowed from `Curve`'s u64: validation keeps each under 10 000.
    pub creator_split_bps: u16,
    pub fanex_split_bps: u16,
    pub creator_target_usdc: u64,
    /// The pool seed that triggers the graduation: `Curve.cpmm_seed_lamports`.
    pub graduation_threshold_usdc: u64,
    pub creator_cap_usdc: u64,
    pub fanex_cap_usdc: u64,
    pub virtual_usdc_reserves: u64,
    pub virtual_token_reserves: u64,
    /// A counter: the vault's balance is never read for a price, so a donation
    /// to the vault moves nothing.
    pub real_usdc_reserves: u64,
    pub real_token_reserves: u64,
    pub migration_token_reserves: u64,
    pub creator_received_usdc: u64,
    pub fanex_received_usdc: u64,
    /// A migrated curve keeps its SOL birth.
    pub created_at: i64,
    pub graduated_at: i64,
    /// 0 for a curve born in USDC.
    pub migrated_at: i64,
    /// When `claim_fan_led_usdc` handed the market to its creator (N+3, 2026-11-05): the mark
    /// `revoke_fan_led_usdc` needs to undo that claim. 0 for every other curve, the ones the admin's
    /// two steps handed over included, and after an undo; every curve written before N+3 reads 0 here
    /// (these 8 bytes were reserved zeros).
    pub claimed_at: i64,
    /// 128 until `claimed_at` took 8 of them (2026-11-09).
    pub reserved: [u8; 120],
}

#[cfg(test)]
mod usdc_layout_tests {
    use super::*;
    use crate::constants::PHASE_POOL_CREATED;

    fn key(byte: u8) -> Pubkey {
        Pubkey::new_from_array([byte; 32])
    }

    fn account_bytes<T: AccountSerialize>(account: &T) -> Vec<u8> {
        let mut out = Vec::new();
        account.try_serialize(&mut out).unwrap();
        out
    }

    /// Every field at its offset, read back from `try_serialize`: the fields
    /// follow one another from byte 8, right after the discriminator, with no
    /// gap, and the last one ends where the account does.
    fn assert_fields_at(bytes: &[u8], fields: &[(&str, usize, Vec<u8>)]) {
        let mut next = 8;
        for (name, offset, value) in fields {
            assert_eq!(
                *offset, next,
                "{name} starts where the field before it ends"
            );
            assert_eq!(&bytes[*offset..*offset + value.len()], &value[..], "{name}");
            next = offset + value.len();
        }
        assert_eq!(next, bytes.len(), "the last field ends the account");
    }

    /// A change here is a layout break for every account already on chain:
    /// take the bytes from `reserved` instead. Counted by hand, field by field
    /// (Pubkey 32, u64/i64 8, u16 2, u8/bool 1, [u8; n] n):
    /// UsdcConfig 3 x 1 + 13 x 32 + 64 = 483; CurveUsdc 6 x 1 + 3 x 32 + 2 x 2
    /// + 15 x 8 + 120 = 346. On chain, the 8-byte discriminator first.
    #[test]
    fn the_usdc_layouts_are_pinned() {
        assert_eq!(UsdcConfig::INIT_SPACE, 483);
        assert_eq!(3 + 13 * 32 + 64, 483);
        assert_eq!(CurveUsdc::INIT_SPACE, 346);
        assert_eq!(6 + 3 * 32 + 2 * 2 + 15 * 8 + 120, 346);
        assert_eq!(8 + UsdcConfig::INIT_SPACE, 491);
        assert_eq!(8 + CurveUsdc::INIT_SPACE, 354);
    }

    #[test]
    fn the_usdc_discriminators_are_pinned() {
        // The first 8 bytes of sha256("account:<Name>"): a rename moves them,
        // and every account of that type already on chain stops decoding.
        assert_eq!(
            UsdcConfig::DISCRIMINATOR,
            &[48, 197, 208, 34, 24, 38, 56, 253]
        );
        assert_eq!(
            CurveUsdc::DISCRIMINATOR,
            &[208, 28, 161, 208, 98, 148, 66, 109]
        );
        assert_ne!(CurveUsdc::DISCRIMINATOR, Curve::DISCRIMINATOR);
    }

    fn sample_usdc_config() -> UsdcConfig {
        UsdcConfig {
            version: 1,
            bump: 254,
            paused: true,
            admin: key(1),
            pauser: key(2),
            migration_authority: key(3),
            usdc_mint: key(4),
            fee_usdc_account: key(5),
            lp_fee_owner: key(6),
            raydium_cpmm_program: key(7),
            raydium_amm_config: key(8),
            raydium_create_pool_fee: key(9),
            raydium_lock_program: key(10),
            raydium_lock_authority: key(11),
            launcher: key(12),
            claim_approver: key(13),
            reserved: [0xAB; 64],
        }
    }

    #[test]
    fn every_usdc_config_field_sits_at_its_offset() {
        let bytes = account_bytes(&sample_usdc_config());
        assert_eq!(bytes.len(), 491);
        assert_eq!(&bytes[..8], UsdcConfig::DISCRIMINATOR);
        let k = |byte: u8| key(byte).to_bytes().to_vec();
        assert_fields_at(
            &bytes,
            &[
                ("version", 8, vec![1]),
                ("bump", 9, vec![254]),
                ("paused", 10, vec![1]),
                ("admin", 11, k(1)),
                ("pauser", 43, k(2)),
                ("migration_authority", 75, k(3)),
                ("usdc_mint", 107, k(4)),
                ("fee_usdc_account", 139, k(5)),
                ("lp_fee_owner", 171, k(6)),
                ("raydium_cpmm_program", 203, k(7)),
                ("raydium_amm_config", 235, k(8)),
                ("raydium_create_pool_fee", 267, k(9)),
                ("raydium_lock_program", 299, k(10)),
                ("raydium_lock_authority", 331, k(11)),
                ("launcher", 363, k(12)),
                ("claim_approver", 395, k(13)),
                ("reserved", 427, vec![0xAB; 64]),
            ],
        );
    }

    fn sample_curve_usdc() -> CurveUsdc {
        CurveUsdc {
            version: 1,
            bump: 253,
            usdc_vault_bump: 252,
            phase: PHASE_POOL_CREATED,
            paused: true,
            migrated: true,
            mint: key(21),
            creator: key(22),
            creator_usdc_account: key(23),
            creator_split_bps: 0x0102,
            fanex_split_bps: 0x0304,
            creator_target_usdc: 0x1111_1111_1111_1101,
            graduation_threshold_usdc: 0x1111_1111_1111_1102,
            creator_cap_usdc: 0x1111_1111_1111_1103,
            fanex_cap_usdc: 0x1111_1111_1111_1104,
            virtual_usdc_reserves: 0x1111_1111_1111_1105,
            virtual_token_reserves: 0x1111_1111_1111_1106,
            real_usdc_reserves: 0x1111_1111_1111_1107,
            real_token_reserves: 0x1111_1111_1111_1108,
            migration_token_reserves: 0x1111_1111_1111_1109,
            creator_received_usdc: 0x1111_1111_1111_110A,
            fanex_received_usdc: 0x1111_1111_1111_110B,
            created_at: -0x2222_2222_2222_2201,
            graduated_at: 0x2222_2222_2222_2202,
            migrated_at: 0x2222_2222_2222_2203,
            claimed_at: 0x2222_2222_2222_2204,
            reserved: [0xCD; 120],
        }
    }

    #[test]
    fn every_curve_usdc_field_sits_at_its_offset() {
        let bytes = account_bytes(&sample_curve_usdc());
        assert_eq!(bytes.len(), 354);
        assert_eq!(&bytes[..8], CurveUsdc::DISCRIMINATOR);
        let k = |byte: u8| key(byte).to_bytes().to_vec();
        let u = |v: u64| v.to_le_bytes().to_vec();
        let i = |v: i64| v.to_le_bytes().to_vec();
        assert_fields_at(
            &bytes,
            &[
                ("version", 8, vec![1]),
                ("bump", 9, vec![253]),
                ("usdc_vault_bump", 10, vec![252]),
                ("phase", 11, vec![3]),
                ("paused", 12, vec![1]),
                ("migrated", 13, vec![1]),
                ("mint", 14, k(21)),
                ("creator", 46, k(22)),
                ("creator_usdc_account", 78, k(23)),
                ("creator_split_bps", 110, 0x0102u16.to_le_bytes().to_vec()),
                ("fanex_split_bps", 112, 0x0304u16.to_le_bytes().to_vec()),
                ("creator_target_usdc", 114, u(0x1111_1111_1111_1101)),
                ("graduation_threshold_usdc", 122, u(0x1111_1111_1111_1102)),
                ("creator_cap_usdc", 130, u(0x1111_1111_1111_1103)),
                ("fanex_cap_usdc", 138, u(0x1111_1111_1111_1104)),
                ("virtual_usdc_reserves", 146, u(0x1111_1111_1111_1105)),
                ("virtual_token_reserves", 154, u(0x1111_1111_1111_1106)),
                ("real_usdc_reserves", 162, u(0x1111_1111_1111_1107)),
                ("real_token_reserves", 170, u(0x1111_1111_1111_1108)),
                ("migration_token_reserves", 178, u(0x1111_1111_1111_1109)),
                ("creator_received_usdc", 186, u(0x1111_1111_1111_110A)),
                ("fanex_received_usdc", 194, u(0x1111_1111_1111_110B)),
                ("created_at", 202, i(-0x2222_2222_2222_2201)),
                ("graduated_at", 210, i(0x2222_2222_2222_2202)),
                ("migrated_at", 218, i(0x2222_2222_2222_2203)),
                ("claimed_at", 226, i(0x2222_2222_2222_2204)),
                ("reserved", 234, vec![0xCD; 120]),
            ],
        );
    }

    #[test]
    fn an_all_zero_account_decodes_with_every_reserved_byte() {
        // As `initialize_usdc_config` and `create_curve_usdc` will find the
        // account: zeroed, `reserved` included. A config written before N+3
        // held zeros where `claim_approver` now sits, so it reads as nobody's.
        let config = UsdcConfig::try_from_slice(&[0u8; UsdcConfig::INIT_SPACE]).unwrap();
        assert_eq!(config.reserved.len(), 64);
        assert_eq!(config.launcher, Pubkey::default());
        assert_eq!(config.claim_approver, Pubkey::default());
        assert_eq!(32 + 32 + config.reserved.len(), 128);
        let curve = CurveUsdc::try_from_slice(&[0u8; CurveUsdc::INIT_SPACE]).unwrap();
        assert_eq!(curve.reserved.len(), 120);
        assert_eq!(curve.claimed_at, 0, "a curve written before N+3 reads as never self-claimed");
        assert_eq!(curve.phase, crate::constants::PHASE_BONDING);
    }

    /// What `Account::<T>::try_from` answers for one program-owned account
    /// holding `bytes`: Ok, or the Anchor error's number.
    fn load_as<T>(mut bytes: Vec<u8>) -> std::result::Result<(), u32>
    where
        T: AccountSerialize + AccountDeserialize + Owner + Clone,
    {
        let key = Pubkey::new_unique();
        let owner = crate::ID;
        let mut lamports = 1_000_000u64;
        let info = AccountInfo::new(
            &key,
            false,
            true,
            &mut lamports,
            &mut bytes,
            &owner,
            false,
            0,
        );
        match Account::<T>::try_from(&info) {
            Ok(_) => Ok(()),
            Err(anchor_lang::error::Error::AnchorError(e)) => Err(e.error_code_number),
            Err(other) => panic!("expected an AnchorError, got {other:?}"),
        }
    }

    fn sol_curve_bytes() -> Vec<u8> {
        let curve = Curve::try_from_slice(&[0u8; Curve::INIT_SPACE]).unwrap();
        account_bytes(&curve)
    }

    #[test]
    fn a_sol_curve_is_refused_as_a_usdc_curve_and_back() {
        let mismatch = u32::from(anchor_lang::error::ErrorCode::AccountDiscriminatorMismatch);
        assert_eq!(mismatch, 3002);
        // Each loads as its own type, so the refusals below are the discriminator's alone.
        assert_eq!(load_as::<Curve>(sol_curve_bytes()), Ok(()));
        assert_eq!(
            load_as::<CurveUsdc>(account_bytes(&sample_curve_usdc())),
            Ok(())
        );
        assert_eq!(load_as::<CurveUsdc>(sol_curve_bytes()), Err(mismatch));
        assert_eq!(
            load_as::<Curve>(account_bytes(&sample_curve_usdc())),
            Err(mismatch)
        );
        // Nor does either config read as the other.
        assert_eq!(
            load_as::<UsdcConfig>(account_bytes(&sample_curve_usdc())),
            Err(mismatch)
        );
        assert_eq!(
            load_as::<CurveUsdc>(account_bytes(&sample_usdc_config())),
            Err(mismatch)
        );
    }
}

// ── A USDC curve in the math crate's shape (USDC curve design §10.4) ─────────
//
// The swaps quote with `pepper_curve_math` and write back what it computed: these two are the only
// place a `CurveUsdc` field meets its crate twin, so a counter read from or written to the wrong one
// shows in `curve_usdc_math_tests` below.

impl CurveUsdc {
    /// The terms and counters a swap reads, as `quote_buy` and `quote_sell` take them: each µUSDC
    /// field its unit-free twin, the two bps widened from u16, the graduation threshold as the
    /// crate's `graduation_threshold` (never the creator target).
    pub fn swap_state(&self) -> pepper_curve_math::CurveState {
        pepper_curve_math::CurveState {
            virtual_reserve: self.virtual_usdc_reserves,
            virtual_token_reserves: self.virtual_token_reserves,
            real_reserve: self.real_usdc_reserves,
            real_token_reserves: self.real_token_reserves,
            creator_split_bps: u64::from(self.creator_split_bps),
            fanex_split_bps: u64::from(self.fanex_split_bps),
            creator_cap: self.creator_cap_usdc,
            fanex_cap: self.fanex_cap_usdc,
            creator_received: self.creator_received_usdc,
            fanex_received: self.fanex_received_usdc,
            graduation_threshold: self.graduation_threshold_usdc,
            created_at: self.created_at,
        }
    }

    /// Writes back the six counters a swap moves, from the crate's `after`: the virtual and real
    /// reserves of both sides and the two received counters. The launch's terms (the bps, the caps,
    /// the threshold, `created_at`) are the curve's own and never taken from a quote.
    pub fn set_swap_counters(&mut self, after: &pepper_curve_math::CurveState) {
        self.virtual_usdc_reserves = after.virtual_reserve;
        self.virtual_token_reserves = after.virtual_token_reserves;
        self.real_usdc_reserves = after.real_reserve;
        self.real_token_reserves = after.real_token_reserves;
        self.creator_received_usdc = after.creator_received;
        self.fanex_received_usdc = after.fanex_received;
    }
}

#[cfg(test)]
mod curve_usdc_math_tests {
    use super::*;
    use pepper_curve_math::{quote_buy, CurveState};

    /// Every field a value of its own, so a field mapped to the wrong twin reads another number.
    fn curve() -> CurveUsdc {
        CurveUsdc {
            version: 1,
            bump: 255,
            usdc_vault_bump: 254,
            phase: crate::constants::PHASE_BONDING,
            paused: false,
            migrated: false,
            mint: Pubkey::new_from_array([1; 32]),
            creator: Pubkey::new_from_array([2; 32]),
            creator_usdc_account: Pubkey::new_from_array([3; 32]),
            creator_split_bps: 3_760,
            fanex_split_bps: 240,
            creator_target_usdc: 1_600_000_001,
            graduation_threshold_usdc: 2_400_000_002,
            creator_cap_usdc: 1_504_000_003,
            fanex_cap_usdc: 96_000_004,
            virtual_usdc_reserves: 1_000_000_005,
            virtual_token_reserves: 115_000_000_000_006,
            real_usdc_reserves: 7,
            real_token_reserves: 82_870_000_000_008,
            migration_token_reserves: 14_000_000_000_009,
            creator_received_usdc: 10,
            fanex_received_usdc: 11,
            created_at: 1_800_000_012,
            graduated_at: 13,
            migrated_at: 14,
            claimed_at: 0,
            reserved: [0; 120],
        }
    }

    fn bytes(curve: &CurveUsdc) -> Vec<u8> {
        let mut out = Vec::new();
        curve.try_serialize(&mut out).unwrap();
        out
    }

    #[test]
    fn a_usdc_curve_reads_into_the_crate_s_shape_field_by_field() {
        assert_eq!(
            curve().swap_state(),
            CurveState {
                virtual_reserve: 1_000_000_005,
                virtual_token_reserves: 115_000_000_000_006,
                real_reserve: 7,
                real_token_reserves: 82_870_000_000_008,
                creator_split_bps: 3_760,
                fanex_split_bps: 240,
                creator_cap: 1_504_000_003,
                fanex_cap: 96_000_004,
                creator_received: 10,
                fanex_received: 11,
                // The threshold, not the creator target: the crate graduates on it.
                graduation_threshold: 2_400_000_002,
                created_at: 1_800_000_012,
            }
        );
    }

    #[test]
    fn the_bps_widen_from_u16_whole() {
        let mut wide = curve();
        wide.creator_split_bps = u16::MAX;
        wide.fanex_split_bps = 0x0100;
        let state = wide.swap_state();
        assert_eq!(
            (state.creator_split_bps, state.fanex_split_bps),
            (65_535, 256)
        );
    }

    #[test]
    fn writing_back_its_own_state_leaves_every_byte() {
        let mut same = curve();
        same.set_swap_counters(&curve().swap_state());
        assert_eq!(bytes(&same), bytes(&curve()));
    }

    #[test]
    fn a_quote_s_after_moves_the_six_counters_and_no_term() {
        let start = curve();
        let quote = quote_buy(&start.swap_state(), 100_000_000, 1_800_000_600).unwrap();
        // Shares inside both caps: every counter the buy moves changes.
        assert!(quote.creator_amount > 0 && quote.fanex_amount > 0 && quote.tokens_out > 0);
        let mut after = start.clone();
        after.set_swap_counters(&quote.after);

        assert_eq!(after.swap_state(), quote.after);
        assert_eq!(
            after.virtual_usdc_reserves,
            start.virtual_usdc_reserves + quote.curve_amount
        );
        assert_eq!(
            after.real_usdc_reserves,
            start.real_usdc_reserves + quote.curve_amount
        );
        assert_eq!(
            after.virtual_token_reserves,
            start.virtual_token_reserves - quote.tokens_out
        );
        assert_eq!(
            after.real_token_reserves,
            start.real_token_reserves - quote.tokens_out
        );
        assert_eq!(
            after.creator_received_usdc,
            start.creator_received_usdc + quote.creator_amount
        );
        assert_eq!(
            after.fanex_received_usdc,
            start.fanex_received_usdc + quote.fanex_amount
        );

        // A quote's terms written back would be the crate's copy of them: changed here, they must
        // stay the curve's own.
        let foreign = CurveState {
            creator_split_bps: 1,
            fanex_split_bps: 2,
            creator_cap: 3,
            fanex_cap: 4,
            graduation_threshold: 5,
            created_at: 6,
            ..quote.after
        };
        let mut kept = start.clone();
        kept.set_swap_counters(&foreign);
        let mut expected = start.clone();
        expected.set_swap_counters(&quote.after);
        assert_eq!(bytes(&kept), bytes(&expected));
        assert_eq!(
            kept.migration_token_reserves,
            start.migration_token_reserves
        );
        assert_eq!(
            (kept.phase, kept.graduated_at),
            (start.phase, start.graduated_at)
        );
    }
}
