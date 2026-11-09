//! What the program reads of Raydium's accounts, by hand (USDC curve design §10.3.9, §10.3.10):
//! Raydium's CPI crates pin Anchor 1.0.2 and this program is on 0.31.1, so each layout is copied
//! from the Raydium SDK the keeper calls today (`@raydium-io/raydium-sdk-v2`,
//! `src/raydium/cpmm/layout.ts`) and pinned by the tests below. Pure: bytes and keys in, values or
//! a refusal out, so every rule is tested here with no AccountInfo.
//!
//! `set_usdc_raydium` reads the CPMM's `AmmConfig` (the fee tier a graduation seeds its pool on)
//! through `check_amm_config`, and `graduate_usdc_pool` reads it again by the same function, since
//! Raydium's admin can change a tier's rate, or stop its pool creation, after it was set. The
//! graduation's CPI into CPMM's `initialize` is encoded here too (`cpmm_initialize_data`,
//! `CpmmInitializeAccounts`), with CPMM's seeds, the rents it charges the pool creator and its
//! `PoolState` (`parse_pool_state`), whose own records `graduate_usdc_lock` binds to
//! (`pool_vaults_by_role`); and the second step's CPI into the LP lock's `lock_cp_liquidity`
//! (`lock_cp_liquidity_data`, `LockCpLiquidityAccounts`), with the rents the lock charges
//! (`lock_funding`).

use anchor_lang::prelude::*;

use crate::constants::RAYDIUM_TRADE_FEE_RATE_1PCT;
use crate::errors::FanexError;

/// The first 8 bytes of sha256("account:AmmConfig"), Anchor's discriminator of the CPMM's fee tier.
pub const AMM_CONFIG_DISCRIMINATOR: [u8; 8] = [218, 244, 33, 104, 203, 203, 43, 111];

/// An `AmmConfig` on chain, its discriminator included: 236 bytes (`CpmmConfigInfoLayout`: the
/// discriminator, `bump` u8, `disable_create_pool` bool, `index` u16, the four u64 rates and fee,
/// `protocol_owner` and `fund_owner`, `creator_fee_rate` u64, then 15 u64 of padding).
pub const AMM_CONFIG_LEN: usize = 236;

/// Byte offsets in an `AmmConfig`, the discriminator counted.
pub const AMM_CONFIG_DISABLE_CREATE_POOL_OFFSET: usize = 9;
pub const AMM_CONFIG_INDEX_OFFSET: usize = 10;
/// The trade fee, in Raydium's millionths: 10,000 is 1% (`RAYDIUM_TRADE_FEE_RATE_1PCT`).
pub const AMM_CONFIG_TRADE_FEE_RATE_OFFSET: usize = 12;
/// The lamports CPMM's `initialize` charges to create a pool on this tier (0.15 SOL on mainnet's
/// index 1, 2026-10-22), which `graduate_usdc_pool` funds the pool creator with (§10.3.9 step 3).
pub const AMM_CONFIG_CREATE_POOL_FEE_OFFSET: usize = 36;

/// The fields of an `AmmConfig` the program reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AmmConfig {
    /// Raydium's switch that stops `initialize` on this tier.
    pub disable_create_pool: bool,
    /// The tier's index, a seed of its address (`["amm_config", index as u16 big-endian]`).
    pub index: u16,
    pub trade_fee_rate: u64,
    pub create_pool_fee: u64,
}

impl AmmConfig {
    /// Whether the tier still charges 1%, the tier today's keeper seeds on: an address pinned in
    /// `UsdcConfig` cannot see Raydium's admin change its rate (`GraduationFeeTierChanged`).
    pub fn charges_one_percent(&self) -> bool {
        self.trade_fee_rate == RAYDIUM_TRADE_FEE_RATE_1PCT
    }
}

fn u64_at(data: &[u8], offset: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&data[offset..offset + 8]);
    u64::from_le_bytes(bytes)
}

/// The `AmmConfig` in `data`, or `None` when `data` is not one: shorter than `AMM_CONFIG_LEN`,
/// another discriminator, or a `disable_create_pool` byte Borsh would refuse as a bool (neither 0
/// nor 1). A longer account is read as Borsh reads one, its prefix: Raydium grows an account into
/// its padding, and a length pinned here would refuse every graduation after such an upgrade until
/// one of ours.
pub fn parse_amm_config(data: &[u8]) -> Option<AmmConfig> {
    if data.len() < AMM_CONFIG_LEN || data[..8] != AMM_CONFIG_DISCRIMINATOR {
        return None;
    }
    let disable_create_pool = match data[AMM_CONFIG_DISABLE_CREATE_POOL_OFFSET] {
        0 => false,
        1 => true,
        _ => return None,
    };
    Some(AmmConfig {
        disable_create_pool,
        index: u16::from_le_bytes([
            data[AMM_CONFIG_INDEX_OFFSET],
            data[AMM_CONFIG_INDEX_OFFSET + 1],
        ]),
        trade_fee_rate: u64_at(data, AMM_CONFIG_TRADE_FEE_RATE_OFFSET),
        create_pool_fee: u64_at(data, AMM_CONFIG_CREATE_POOL_FEE_OFFSET),
    })
}

/// The account found as the `amm_config`, checked in this order: at `expected_address`
/// (`AccountMismatch`), owned by `expected_owner`, the CPMM program (`AccountMismatch`), an
/// `AmmConfig` (`AccountMismatch`), charging 1% (`GraduationFeeTierChanged`), still creating pools
/// (`PoolCreationDisabled`). Only the CPMM program writes the accounts it owns, so the owner and
/// the discriminator together say the bytes are Raydium's own record of the tier.
///
/// `disable_create_pool` set makes CPMM's own `initialize` refuse (its `NotApproved`, measured on
/// the mainnet binary by LiteSVM `usdc_graduation.rs`, 2026-10-23), after the graduation has moved
/// the reserves to the pool creator: refused here instead, `set_usdc_raydium` never names such a
/// tier and the graduation stops before any move, the curve left in Graduating until
/// `set_usdc_raydium` names another tier (the default of 2026-10-23, for Rick to confirm).
pub fn check_amm_config(
    address: &Pubkey,
    owner: &Pubkey,
    data: &[u8],
    expected_address: &Pubkey,
    expected_owner: &Pubkey,
) -> Result<AmmConfig> {
    require_keys_eq!(*address, *expected_address, FanexError::AccountMismatch);
    require_keys_eq!(*owner, *expected_owner, FanexError::AccountMismatch);
    let config = parse_amm_config(data).ok_or(FanexError::AccountMismatch)?;
    require!(
        config.charges_one_percent(),
        FanexError::GraduationFeeTierChanged
    );
    require!(
        !config.disable_create_pool,
        FanexError::PoolCreationDisabled
    );
    Ok(config)
}

/// Whether every Raydium address is set: none all-zero (`RaydiumNotSet` otherwise). All-zero is
/// what `initialize_usdc_config` writes, and no curve graduates while any of them is (§10.3.9).
pub fn raydium_addresses_set(addresses: &[Pubkey]) -> bool {
    addresses
        .iter()
        .all(|address| *address != Pubkey::default())
}

// ── CPMM `initialize`, encoded by hand (USDC curve design §10.3.9, 2026-10-23) ──────────────────
//
// `graduate_usdc_pool` creates the token/USDC pool by CPI into CPMM's `initialize`, as the Raydium
// SDK the keeper calls builds it (`makeCreateCpmmPoolInInstruction`, `src/raydium/cpmm/
// instruction.ts`): its discriminator, three u64 arguments and its 20 accounts in the SDK's order.

/// The first 8 bytes of sha256("global:initialize"), Anchor's discriminator of CPMM's `initialize`.
pub const CPMM_INITIALIZE_DISCRIMINATOR: [u8; 8] = [175, 175, 109, 31, 13, 152, 155, 237];

/// `initialize`'s data: the discriminator, then `init_amount_0`, `init_amount_1` and `open_time`,
/// each a u64 little-endian (Borsh), 32 bytes in all.
pub fn cpmm_initialize_data(init_amount_0: u64, init_amount_1: u64, open_time: u64) -> [u8; 32] {
    let mut data = [0u8; 32];
    data[..8].copy_from_slice(&CPMM_INITIALIZE_DISCRIMINATOR);
    data[8..16].copy_from_slice(&init_amount_0.to_le_bytes());
    data[16..24].copy_from_slice(&init_amount_1.to_le_bytes());
    data[24..32].copy_from_slice(&open_time.to_le_bytes());
    data
}

/// The 20 accounts of CPMM's `initialize`, named as the SDK names them, token 0 the lower mint
/// address (`cpmm_token_0_first`).
pub struct CpmmInitializeAccounts {
    pub creator: Pubkey,
    pub amm_config: Pubkey,
    pub authority: Pubkey,
    pub pool_state: Pubkey,
    pub token_0_mint: Pubkey,
    pub token_1_mint: Pubkey,
    pub lp_mint: Pubkey,
    pub creator_token_0: Pubkey,
    pub creator_token_1: Pubkey,
    pub creator_lp_token: Pubkey,
    pub token_0_vault: Pubkey,
    pub token_1_vault: Pubkey,
    pub create_pool_fee: Pubkey,
    pub observation: Pubkey,
    pub token_program: Pubkey,
    pub token_0_program: Pubkey,
    pub token_1_program: Pubkey,
    pub associated_token_program: Pubkey,
    pub system_program: Pubkey,
    pub rent: Pubkey,
}

impl CpmmInitializeAccounts {
    /// The metas in the SDK's order, two flags apart from it: `creator` is writable, since the pool
    /// creator pays CPMM's rents and fee from its own lamports where the SDK leans on a fee payer
    /// writable anyway; and `pool_state` always signs, since ours sits at our own PDA, never at
    /// CPMM's canonical `["pool", amm_config, mint_0, mint_1]`, which alone would not sign.
    pub fn to_account_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.creator, true),
            AccountMeta::new_readonly(self.amm_config, false),
            AccountMeta::new_readonly(self.authority, false),
            AccountMeta::new(self.pool_state, true),
            AccountMeta::new_readonly(self.token_0_mint, false),
            AccountMeta::new_readonly(self.token_1_mint, false),
            AccountMeta::new(self.lp_mint, false),
            AccountMeta::new(self.creator_token_0, false),
            AccountMeta::new(self.creator_token_1, false),
            AccountMeta::new(self.creator_lp_token, false),
            AccountMeta::new(self.token_0_vault, false),
            AccountMeta::new(self.token_1_vault, false),
            AccountMeta::new(self.create_pool_fee, false),
            AccountMeta::new(self.observation, false),
            AccountMeta::new_readonly(self.token_program, false),
            AccountMeta::new_readonly(self.token_0_program, false),
            AccountMeta::new_readonly(self.token_1_program, false),
            AccountMeta::new_readonly(self.associated_token_program, false),
            AccountMeta::new_readonly(self.system_program, false),
            AccountMeta::new_readonly(self.rent, false),
        ]
    }
}

/// Whether `mint` is the pool's token 0 against `other`: CPMM takes its two mints in address order,
/// `token_0_mint < token_1_mint` compared as bytes (its own constraint), so which of USDC and the
/// curve's token is token 0 depends on the token's random address.
pub fn cpmm_token_0_first(mint: &Pubkey, other: &Pubkey) -> bool {
    mint < other
}

/// CPMM's seeds, as the SDK's `cpmm/pda.ts` derives them under the CPMM program: the authority of
/// every pool's vaults and LP mint, then a pool's LP mint, vaults and observation account.
pub const CPMM_AUTH_SEED: &[u8] = b"vault_and_lp_mint_auth_seed";
pub const CPMM_POOL_LP_MINT_SEED: &[u8] = b"pool_lp_mint";
pub const CPMM_POOL_VAULT_SEED: &[u8] = b"pool_vault";
pub const CPMM_OBSERVATION_SEED: &[u8] = b"observation";

/// What CPMM's `initialize` opens at the pool creator's cost, each by its size: the `PoolState`,
/// the LP mint, the creator's LP account and the two vaults, the observation account (sizes read
/// on the mainnet binary in LiteSVM, `usdc_graduation.rs`, 2026-10-23).
pub const CPMM_POOL_STATE_LEN: usize = 637;
pub const CPMM_OBSERVATION_LEN: usize = 4_075;
const SPL_MINT_LEN: usize = 82;
const SPL_TOKEN_ACCOUNT_LEN: usize = 165;
pub const CPMM_INITIALIZE_OPENS: [usize; 6] = [
    CPMM_POOL_STATE_LEN,
    SPL_MINT_LEN,
    SPL_TOKEN_ACCOUNT_LEN,
    SPL_TOKEN_ACCOUNT_LEN,
    SPL_TOKEN_ACCOUNT_LEN,
    CPMM_OBSERVATION_LEN,
];

/// Lamports given to the pool creator past the fee and the rents, and refunded with what is left:
/// room for Raydium growing an account by about 1,968 bytes at mainnet's rent (5,080 lamports a
/// byte, 2026-10-23; 1,436 at LiteSVM's default 6,960) before a graduation runs short.
pub const POOL_CREATOR_SLACK_LAMPORTS: u64 = 10_000_000;

/// The most a caller may add to either step's funding (2026-10-24): 0.1 SOL, room for about 19,685
/// more bytes at mainnet's rent, should Raydium grow its accounts past the slack, so a graduation
/// needs no upgrade to go on. Bounded because the pool creator signs Raydium's CPI, which could take
/// whatever the pool creator holds: with `MAX_CREATE_POOL_FEE_LAMPORTS` it caps what an upgraded
/// Raydium could keep of the caller's lamports. Refunded with the rest.
pub const MAX_GRADUATION_EXTRA_LAMPORTS: u64 = 100_000_000;

/// The most `graduate_usdc_pool` lets the tier's `create_pool_fee` charge its caller (the internal
/// audit, 2026-10-29): 0.5 SOL, past both clusters' 0.15 SOL (mainnet's index 1 and devnet's index 3,
/// read on 2026-10-29). Raydium's admin sets that fee with one signature and no time lock, and the
/// pool creator, funded by the caller, pays it to CPMM: unbounded, a raise drains the sponsor at
/// every graduation. Past it the step is refused (`CreatePoolFeeTooHigh`) before anything moves, so a
/// real repricing waits for an upgrade that moves this bound, as `PoolCreationDisabled` waits for a
/// tier change. ⚠️ 0.5 SOL is an assumption for Rick to confirm.
pub const MAX_CREATE_POOL_FEE_LAMPORTS: u64 = 500_000_000;

/// What the caller hands the pool creator for `initialize` (§10.3.9 step 3): the tier's
/// `create_pool_fee`, the rent of each account CPMM opens, the slack and the caller's `extra`;
/// `None` past u64. The bound on `extra` is the handler's.
pub fn pool_creator_funding(create_pool_fee: u64, rent: &Rent, extra: u64) -> Option<u64> {
    CPMM_INITIALIZE_OPENS
        .iter()
        .try_fold(create_pool_fee, |sum, len| {
            sum.checked_add(rent.minimum_balance(*len))
        })?
        .checked_add(POOL_CREATOR_SLACK_LAMPORTS)?
        .checked_add(extra)
}

// ── CPMM's `PoolState`, read by hand (`CpmmPoolInfoLayout`, `src/raydium/cpmm/layout.ts`) ─────

/// The first 8 bytes of sha256("account:PoolState").
pub const POOL_STATE_DISCRIMINATOR: [u8; 8] = [247, 237, 227, 245, 215, 195, 222, 70];

/// Byte offsets in a `PoolState`, the discriminator counted (a packed layout).
pub const POOL_STATE_AMM_CONFIG_OFFSET: usize = 8;
pub const POOL_STATE_POOL_CREATOR_OFFSET: usize = 40;
pub const POOL_STATE_TOKEN_0_VAULT_OFFSET: usize = 72;
pub const POOL_STATE_TOKEN_1_VAULT_OFFSET: usize = 104;
pub const POOL_STATE_LP_MINT_OFFSET: usize = 136;
pub const POOL_STATE_TOKEN_0_MINT_OFFSET: usize = 168;
pub const POOL_STATE_TOKEN_1_MINT_OFFSET: usize = 200;
pub const POOL_STATE_TOKEN_0_PROGRAM_OFFSET: usize = 232;
pub const POOL_STATE_TOKEN_1_PROGRAM_OFFSET: usize = 264;
pub const POOL_STATE_OBSERVATION_OFFSET: usize = 296;
pub const POOL_STATE_STATUS_OFFSET: usize = 329;
pub const POOL_STATE_LP_SUPPLY_OFFSET: usize = 333;
pub const POOL_STATE_OPEN_TIME_OFFSET: usize = 373;

/// The fields of a `PoolState` the graduation reads: what the pool records of its config, its
/// creator, its two sides and its LP mint, which `graduate_usdc_lock` binds its accounts to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolState {
    pub amm_config: Pubkey,
    pub pool_creator: Pubkey,
    pub token_0_vault: Pubkey,
    pub token_1_vault: Pubkey,
    pub lp_mint: Pubkey,
    pub token_0_mint: Pubkey,
    pub token_1_mint: Pubkey,
    pub token_0_program: Pubkey,
    pub token_1_program: Pubkey,
    pub observation: Pubkey,
    pub status: u8,
    pub lp_supply: u64,
    pub open_time: u64,
}

fn pubkey_at(data: &[u8], offset: usize) -> Pubkey {
    let mut key = [0u8; 32];
    key.copy_from_slice(&data[offset..offset + 32]);
    Pubkey::new_from_array(key)
}

/// The `PoolState` in `data`, or `None` when `data` is not one: shorter than `CPMM_POOL_STATE_LEN`
/// or another discriminator. A longer account is read by its prefix, as `parse_amm_config` reads.
pub fn parse_pool_state(data: &[u8]) -> Option<PoolState> {
    if data.len() < CPMM_POOL_STATE_LEN || data[..8] != POOL_STATE_DISCRIMINATOR {
        return None;
    }
    Some(PoolState {
        amm_config: pubkey_at(data, POOL_STATE_AMM_CONFIG_OFFSET),
        pool_creator: pubkey_at(data, POOL_STATE_POOL_CREATOR_OFFSET),
        token_0_vault: pubkey_at(data, POOL_STATE_TOKEN_0_VAULT_OFFSET),
        token_1_vault: pubkey_at(data, POOL_STATE_TOKEN_1_VAULT_OFFSET),
        lp_mint: pubkey_at(data, POOL_STATE_LP_MINT_OFFSET),
        token_0_mint: pubkey_at(data, POOL_STATE_TOKEN_0_MINT_OFFSET),
        token_1_mint: pubkey_at(data, POOL_STATE_TOKEN_1_MINT_OFFSET),
        token_0_program: pubkey_at(data, POOL_STATE_TOKEN_0_PROGRAM_OFFSET),
        token_1_program: pubkey_at(data, POOL_STATE_TOKEN_1_PROGRAM_OFFSET),
        observation: pubkey_at(data, POOL_STATE_OBSERVATION_OFFSET),
        status: data[POOL_STATE_STATUS_OFFSET],
        lp_supply: u64_at(data, POOL_STATE_LP_SUPPLY_OFFSET),
        open_time: u64_at(data, POOL_STATE_OPEN_TIME_OFFSET),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `AmmConfig` as Raydium stores one, built by hand from the SDK's layout: mainnet's index 1
    /// as read on 2026-10-22 (bump 254, the 1% tier, protocol 12%, fund 4% of the fee, 0.15 SOL to
    /// create a pool, creator fee 500), every byte past the fields it names at 0xEE, so a field read
    /// at the wrong offset shows.
    fn amm_config_bytes(
        trade_fee_rate: u64,
        create_pool_fee: u64,
        disable_create_pool: u8,
    ) -> Vec<u8> {
        let mut data = vec![0xEE; AMM_CONFIG_LEN];
        data[..8].copy_from_slice(&AMM_CONFIG_DISCRIMINATOR);
        data[8] = 254;
        data[9] = disable_create_pool;
        data[10..12].copy_from_slice(&1u16.to_le_bytes());
        data[12..20].copy_from_slice(&trade_fee_rate.to_le_bytes());
        data[20..28].copy_from_slice(&120_000u64.to_le_bytes());
        data[28..36].copy_from_slice(&40_000u64.to_le_bytes());
        data[36..44].copy_from_slice(&create_pool_fee.to_le_bytes());
        data[108..116].copy_from_slice(&500u64.to_le_bytes());
        data
    }

    const POOL_FEE: u64 = 150_000_000;

    #[test]
    fn the_discriminator_is_anchors_for_amm_config() {
        let hash = anchor_lang::solana_program::hash::hash(b"account:AmmConfig");
        assert_eq!(hash.to_bytes()[..8], AMM_CONFIG_DISCRIMINATOR);
        assert_eq!(
            AMM_CONFIG_DISCRIMINATOR,
            [218, 244, 33, 104, 203, 203, 43, 111]
        );
    }

    #[test]
    fn the_layout_adds_up_to_236_bytes() {
        // discriminator, bump, disable_create_pool, index, four u64, two Pubkeys, one u64, 15 u64.
        assert_eq!(8 + 1 + 1 + 2 + 4 * 8 + 2 * 32 + 8 + 15 * 8, AMM_CONFIG_LEN);
        assert_eq!(AMM_CONFIG_DISABLE_CREATE_POOL_OFFSET, 8 + 1);
        assert_eq!(AMM_CONFIG_INDEX_OFFSET, 8 + 1 + 1);
        assert_eq!(AMM_CONFIG_TRADE_FEE_RATE_OFFSET, 8 + 1 + 1 + 2);
        assert_eq!(AMM_CONFIG_CREATE_POOL_FEE_OFFSET, 8 + 1 + 1 + 2 + 3 * 8);
    }

    #[test]
    fn reads_each_field_at_its_offset() {
        let config = parse_amm_config(&amm_config_bytes(10_000, POOL_FEE, 0)).unwrap();
        assert_eq!(
            config,
            AmmConfig {
                disable_create_pool: false,
                index: 1,
                trade_fee_rate: 10_000,
                create_pool_fee: POOL_FEE,
            }
        );
        assert!(config.charges_one_percent());

        let disabled = parse_amm_config(&amm_config_bytes(2_500, 1, 1)).unwrap();
        assert!(disabled.disable_create_pool);
        assert_eq!(
            (disabled.trade_fee_rate, disabled.create_pool_fee),
            (2_500, 1)
        );
        assert!(!disabled.charges_one_percent());
    }

    #[test]
    fn only_one_percent_is_the_tier() {
        for rate in [0, 100, 2_500, 9_999, 10_001, 20_000, 40_000, u64::MAX] {
            let config = parse_amm_config(&amm_config_bytes(rate, POOL_FEE, 0)).unwrap();
            assert!(!config.charges_one_percent(), "{rate}");
        }
    }

    #[test]
    fn refuses_bytes_that_are_not_an_amm_config() {
        let good = amm_config_bytes(10_000, POOL_FEE, 0);
        // Short by one byte, or empty.
        assert_eq!(parse_amm_config(&good[..AMM_CONFIG_LEN - 1]), None);
        assert_eq!(parse_amm_config(&[]), None);
        // Another account type of the CPMM: `PoolState`'s discriminator.
        let mut pool_state = good.clone();
        pool_state[..8].copy_from_slice(&[247, 237, 227, 245, 215, 195, 222, 70]);
        assert_eq!(parse_amm_config(&pool_state), None);
        // A bool Borsh would refuse.
        assert_eq!(
            parse_amm_config(&amm_config_bytes(10_000, POOL_FEE, 2)),
            None
        );
        // Longer is read by its prefix, as Borsh reads it.
        let mut longer = good.clone();
        longer.extend_from_slice(&[0xAB; 64]);
        assert_eq!(parse_amm_config(&longer), parse_amm_config(&good));
    }

    fn code(result: Result<AmmConfig>) -> u32 {
        match result {
            Err(anchor_lang::error::Error::AnchorError(e)) => e.error_code_number,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn checks_the_address_then_the_owner_then_the_bytes_then_the_tier() {
        let address = Pubkey::new_unique();
        let cpmm = Pubkey::new_unique();
        let other = Pubkey::new_unique();
        let good = amm_config_bytes(10_000, POOL_FEE, 0);
        let quarter = amm_config_bytes(2_500, POOL_FEE, 0);
        let mismatch = u32::from(FanexError::AccountMismatch);
        let tier = u32::from(FanexError::GraduationFeeTierChanged);

        let config = check_amm_config(&address, &cpmm, &good, &address, &cpmm).unwrap();
        assert_eq!(config.create_pool_fee, POOL_FEE);

        assert_eq!(
            code(check_amm_config(&other, &cpmm, &good, &address, &cpmm)),
            mismatch
        );
        assert_eq!(
            code(check_amm_config(&address, &other, &good, &address, &cpmm)),
            mismatch
        );
        assert_eq!(
            code(check_amm_config(
                &address,
                &cpmm,
                &good[..100],
                &address,
                &cpmm
            )),
            mismatch
        );
        assert_eq!(
            code(check_amm_config(&address, &cpmm, &quarter, &address, &cpmm)),
            tier
        );
        // The order: a wrong address or owner refuses before the tier is read.
        assert_eq!(
            code(check_amm_config(&other, &cpmm, &quarter, &address, &cpmm)),
            mismatch
        );
        assert_eq!(
            code(check_amm_config(
                &address, &other, &quarter, &address, &cpmm
            )),
            mismatch
        );
        // A 1% tier that stops pool creation is refused here, before any move: CPMM's own
        // `initialize` would refuse it only after the graduation moved the reserves (2026-10-23).
        let disabled = u32::from(FanexError::PoolCreationDisabled);
        assert_eq!(
            code(check_amm_config(
                &address,
                &cpmm,
                &amm_config_bytes(10_000, POOL_FEE, 1),
                &address,
                &cpmm
            )),
            disabled
        );
        // The rate is read first: a disabled tier at another rate is a changed tier.
        assert_eq!(
            code(check_amm_config(
                &address,
                &cpmm,
                &amm_config_bytes(2_500, POOL_FEE, 1),
                &address,
                &cpmm
            )),
            tier
        );
    }

    #[test]
    fn every_address_must_be_set() {
        let set = [
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        ];
        assert!(raydium_addresses_set(&set));
        for i in 0..set.len() {
            let mut one_unset = set;
            one_unset[i] = Pubkey::default();
            assert!(!raydium_addresses_set(&one_unset), "address {i}");
        }
        assert!(!raydium_addresses_set(&[Pubkey::default(); 5]));
        // The all-zero key is the system program's: no Raydium account is ever there.
        assert_eq!(Pubkey::default(), anchor_lang::system_program::ID);
    }
}

#[cfg(test)]
mod cpmm_tests {
    use super::*;

    #[test]
    fn the_discriminators_are_anchors() {
        let hash = |s: &str| anchor_lang::solana_program::hash::hash(s.as_bytes()).to_bytes();
        assert_eq!(
            hash("global:initialize")[..8],
            CPMM_INITIALIZE_DISCRIMINATOR
        );
        assert_eq!(hash("account:PoolState")[..8], POOL_STATE_DISCRIMINATOR);
        // As the SDK writes them (`anchorDataBuf.initialize`, the pool layout's blob).
        assert_eq!(
            CPMM_INITIALIZE_DISCRIMINATOR,
            [175, 175, 109, 31, 13, 152, 155, 237]
        );
        assert_eq!(
            POOL_STATE_DISCRIMINATOR,
            [247, 237, 227, 245, 215, 195, 222, 70]
        );
    }

    #[test]
    fn initialize_s_data_is_the_discriminator_then_three_u64_le() {
        let data = cpmm_initialize_data(0x0102_0304_0506_0708, 2_400_000_000, 1_791_158_400);
        assert_eq!(data.len(), 32);
        assert_eq!(data[..8], CPMM_INITIALIZE_DISCRIMINATOR);
        assert_eq!(data[8..16], [8, 7, 6, 5, 4, 3, 2, 1]);
        assert_eq!(data[16..24], 2_400_000_000u64.to_le_bytes());
        assert_eq!(data[24..32], 1_791_158_400u64.to_le_bytes());
        // Borsh of the three arguments in order, as Anchor's client would encode them.
        let mut borsh = Vec::new();
        for value in [0x0102_0304_0506_0708u64, 2_400_000_000, 1_791_158_400] {
            value.serialize(&mut borsh).unwrap();
        }
        assert_eq!(&data[8..], borsh.as_slice());
    }

    /// Every account a key of its own, so a meta out of place names another key.
    fn accounts() -> (CpmmInitializeAccounts, [Pubkey; 20]) {
        let k: [Pubkey; 20] = std::array::from_fn(|_| Pubkey::new_unique());
        let accounts = CpmmInitializeAccounts {
            creator: k[0],
            amm_config: k[1],
            authority: k[2],
            pool_state: k[3],
            token_0_mint: k[4],
            token_1_mint: k[5],
            lp_mint: k[6],
            creator_token_0: k[7],
            creator_token_1: k[8],
            creator_lp_token: k[9],
            token_0_vault: k[10],
            token_1_vault: k[11],
            create_pool_fee: k[12],
            observation: k[13],
            token_program: k[14],
            token_0_program: k[15],
            token_1_program: k[16],
            associated_token_program: k[17],
            system_program: k[18],
            rent: k[19],
        };
        (accounts, k)
    }

    /// The SDK's 20 metas (`makeCreateCpmmPoolInInstruction`), in its order, with its flags:
    /// (name, signer, writable). Its `creator` is a read-only signer and its `pool_state` signs
    /// only away from the canonical PDA.
    const SDK_METAS: [(&str, bool, bool); 20] = [
        ("creator", true, false),
        ("amm_config", false, false),
        ("authority", false, false),
        ("pool_state", true, true),
        ("token_0_mint", false, false),
        ("token_1_mint", false, false),
        ("lp_mint", false, true),
        ("creator_token_0", false, true),
        ("creator_token_1", false, true),
        ("creator_lp_token", false, true),
        ("token_0_vault", false, true),
        ("token_1_vault", false, true),
        ("create_pool_fee", false, true),
        ("observation", false, true),
        ("token_program", false, false),
        ("token_0_program", false, false),
        ("token_1_program", false, false),
        ("associated_token_program", false, false),
        ("system_program", false, false),
        ("rent", false, false),
    ];

    #[test]
    fn the_metas_are_the_sdk_s_twenty_in_its_order() {
        let (accounts, keys) = accounts();
        let metas = accounts.to_account_metas();
        assert_eq!(metas.len(), 20);
        assert_eq!(metas.iter().map(|m| m.pubkey).collect::<Vec<_>>(), keys);
        for (i, (name, signer, writable)) in SDK_METAS.iter().enumerate() {
            assert_eq!(metas[i].is_signer, *signer, "{name}: signer");
            // The one flag apart from the SDK: the pool creator pays, so it is writable.
            let writable = *writable || *name == "creator";
            assert_eq!(metas[i].is_writable, writable, "{name}: writable");
        }
        let signers: Vec<usize> = (0..20).filter(|&i| metas[i].is_signer).collect();
        assert_eq!(
            signers,
            [0, 3],
            "the pool creator and the pool state, our two PDAs"
        );
    }

    #[test]
    fn token_0_is_the_lower_address() {
        let low = Pubkey::new_from_array([1; 32]);
        let mut high_bytes = [1u8; 32];
        high_bytes[31] = 2;
        let high = Pubkey::new_from_array(high_bytes);
        assert!(cpmm_token_0_first(&low, &high));
        assert!(!cpmm_token_0_first(&high, &low));
        // Bytes compared from the first: a higher first byte outranks every later one.
        let mut first = [0u8; 32];
        first[0] = 2;
        let mut later = [0xFFu8; 32];
        later[0] = 1;
        assert!(cpmm_token_0_first(
            &Pubkey::new_from_array(later),
            &Pubkey::new_from_array(first)
        ));
        // A mint is never its own pair.
        assert!(!cpmm_token_0_first(&low, &low));
        // Mainnet USDC against a token below and above it.
        let usdc: Pubkey = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
            .parse()
            .unwrap();
        assert!(cpmm_token_0_first(&Pubkey::new_from_array([0; 32]), &usdc));
        assert!(cpmm_token_0_first(
            &usdc,
            &Pubkey::new_from_array([0xFF; 32])
        ));
    }

    #[test]
    fn the_cpmm_seeds_are_the_sdk_s() {
        assert_eq!(CPMM_AUTH_SEED, b"vault_and_lp_mint_auth_seed");
        assert_eq!(CPMM_POOL_LP_MINT_SEED, b"pool_lp_mint");
        assert_eq!(CPMM_POOL_VAULT_SEED, b"pool_vault");
        assert_eq!(CPMM_OBSERVATION_SEED, b"observation");
    }

    /// A `PoolState` as CPMM stores one, built by hand from the SDK's layout: each key a value of
    /// its own, every other byte 0xEE, so a field read at the wrong offset shows.
    fn pool_state_bytes(
        keys: &[Pubkey; 10],
        status: u8,
        lp_supply: u64,
        open_time: u64,
    ) -> Vec<u8> {
        let mut data = vec![0xEE; CPMM_POOL_STATE_LEN];
        data[..8].copy_from_slice(&POOL_STATE_DISCRIMINATOR);
        for (i, key) in keys.iter().enumerate() {
            data[8 + 32 * i..8 + 32 * (i + 1)].copy_from_slice(key.as_ref());
        }
        data[329] = status;
        data[333..341].copy_from_slice(&lp_supply.to_le_bytes());
        data[373..381].copy_from_slice(&open_time.to_le_bytes());
        data
    }

    #[test]
    fn the_pool_state_layout_adds_up_to_637_bytes() {
        // The discriminator, ten keys, five u8, seven u64, the creator fee's byte, bool and six
        // of padding, two u64, then 28 u64 of padding (`CpmmPoolInfoLayout`).
        assert_eq!(
            8 + 10 * 32 + 5 + 7 * 8 + 8 + 2 * 8 + 28 * 8,
            CPMM_POOL_STATE_LEN
        );
        let offsets = [
            POOL_STATE_AMM_CONFIG_OFFSET,
            POOL_STATE_POOL_CREATOR_OFFSET,
            POOL_STATE_TOKEN_0_VAULT_OFFSET,
            POOL_STATE_TOKEN_1_VAULT_OFFSET,
            POOL_STATE_LP_MINT_OFFSET,
            POOL_STATE_TOKEN_0_MINT_OFFSET,
            POOL_STATE_TOKEN_1_MINT_OFFSET,
            POOL_STATE_TOKEN_0_PROGRAM_OFFSET,
            POOL_STATE_TOKEN_1_PROGRAM_OFFSET,
            POOL_STATE_OBSERVATION_OFFSET,
        ];
        for (i, offset) in offsets.iter().enumerate() {
            assert_eq!(*offset, 8 + 32 * i);
        }
        // After the keys: auth_bump, status, the LP's and the mints' decimals, then the u64s.
        assert_eq!(POOL_STATE_STATUS_OFFSET, 8 + 10 * 32 + 1);
        assert_eq!(POOL_STATE_LP_SUPPLY_OFFSET, 8 + 10 * 32 + 5);
        // lp_supply, the four protocol and fund fees, then open_time.
        assert_eq!(
            POOL_STATE_OPEN_TIME_OFFSET,
            POOL_STATE_LP_SUPPLY_OFFSET + 5 * 8
        );
        // The graduation's lock binds the LP mint and the two vaults by these three (§10.3.9).
        assert_eq!(
            (
                POOL_STATE_LP_MINT_OFFSET,
                POOL_STATE_TOKEN_0_VAULT_OFFSET,
                POOL_STATE_TOKEN_1_VAULT_OFFSET
            ),
            (136, 72, 104)
        );
    }

    #[test]
    fn reads_each_pool_field_at_its_offset() {
        let keys: [Pubkey; 10] = std::array::from_fn(|_| Pubkey::new_unique());
        let data = pool_state_bytes(&keys, 4, 1_099_511_627_776, 1_791_158_401);
        assert_eq!(
            parse_pool_state(&data).unwrap(),
            PoolState {
                amm_config: keys[0],
                pool_creator: keys[1],
                token_0_vault: keys[2],
                token_1_vault: keys[3],
                lp_mint: keys[4],
                token_0_mint: keys[5],
                token_1_mint: keys[6],
                token_0_program: keys[7],
                token_1_program: keys[8],
                observation: keys[9],
                status: 4,
                lp_supply: 1_099_511_627_776,
                open_time: 1_791_158_401,
            }
        );
    }

    #[test]
    fn refuses_bytes_that_are_not_a_pool_state() {
        let keys: [Pubkey; 10] = std::array::from_fn(|_| Pubkey::new_unique());
        let good = pool_state_bytes(&keys, 0, 1, 2);
        assert_eq!(parse_pool_state(&good[..CPMM_POOL_STATE_LEN - 1]), None);
        assert_eq!(parse_pool_state(&[]), None);
        let mut amm_config = good.clone();
        amm_config[..8].copy_from_slice(&AMM_CONFIG_DISCRIMINATOR);
        assert_eq!(parse_pool_state(&amm_config), None);
        let mut longer = good.clone();
        longer.extend_from_slice(&[0xAB; 32]);
        assert_eq!(parse_pool_state(&longer), parse_pool_state(&good));
    }

    #[test]
    fn the_pool_creator_gets_the_fee_the_rents_and_the_slack() {
        let rent = Rent::default();
        let fee = 150_000_000;
        let rents: u64 = [637usize, 82, 165, 165, 165, CPMM_OBSERVATION_LEN]
            .iter()
            .map(|len| rent.minimum_balance(*len))
            .sum();
        assert_eq!(
            pool_creator_funding(fee, &rent, 0),
            Some(fee + rents + POOL_CREATOR_SLACK_LAMPORTS)
        );
        // Each account's rent apart, never one rent for their summed size.
        assert!(rents > rent.minimum_balance(CPMM_INITIALIZE_OPENS.iter().sum()));
        assert_eq!(pool_creator_funding(u64::MAX, &rent, 0), None);
        // The caller's extra on top, refunded with the rest; past u64, nothing.
        assert_eq!(
            pool_creator_funding(fee, &rent, MAX_GRADUATION_EXTRA_LAMPORTS),
            Some(fee + rents + POOL_CREATOR_SLACK_LAMPORTS + MAX_GRADUATION_EXTRA_LAMPORTS)
        );
        assert_eq!(pool_creator_funding(fee, &rent, u64::MAX), None);
        // About 0.2 SOL in all on mainnet's tier (§10.3.9), what CPMM does not spend refunded.
        let total = pool_creator_funding(fee, &rent, 0).unwrap();
        assert!(total > 190_000_000 && total < 210_000_000, "{total}");
    }

    #[test]
    fn the_pool_creation_fee_s_bound_is_past_both_clusters_and_caps_the_caller() {
        // Both clusters' 1% tiers charge 0.15 SOL (2026-10-29), so the bound refuses neither.
        let today = 150_000_000;
        assert!(MAX_CREATE_POOL_FEE_LAMPORTS >= 3 * today);
        assert_eq!(MAX_CREATE_POOL_FEE_LAMPORTS, 500_000_000);
        // What a caller can lose to one graduation's pool step, at the most: the bound, the rents,
        // the slack and the extra, under 0.7 SOL even at the default rent, above mainnet's.
        let most = pool_creator_funding(MAX_CREATE_POOL_FEE_LAMPORTS, &Rent::default(), MAX_GRADUATION_EXTRA_LAMPORTS)
            .unwrap();
        assert!(most < 700_000_000, "{most}");
    }

    /// Mainnet's rent as read on 2026-10-23: 5,080 lamports a byte and no doubling, 650,240 for an
    /// empty account and 1,488,440 for a token account's 165 bytes.
    fn mainnet_rent() -> Rent {
        Rent {
            lamports_per_byte_year: 5_080,
            exemption_threshold: 1.0,
            ..Rent::default()
        }
    }

    #[test]
    fn the_pool_step_costs_its_caller_0_184_sol_on_mainnet() {
        let rent = mainnet_rent();
        assert_eq!(
            (rent.minimum_balance(0), rent.minimum_balance(165)),
            (650_240, 1_488_440)
        );
        // CPMM's six accounts 30,769,560, handed with Raydium's 0.15 SOL and the slack.
        let fee = 150_000_000;
        let cpmm_rents = 30_769_560;
        assert_eq!(
            pool_creator_funding(fee, &rent, 0),
            Some(fee + cpmm_rents + POOL_CREATOR_SLACK_LAMPORTS)
        );
        // What it spends: those, and the pool creator's two ATAs, which the caller opens itself.
        assert_eq!(fee + cpmm_rents + 2 * 1_488_440, 183_746_440);
        // The slack, at this rent, covers about 1,968 bytes of growth.
        assert_eq!(POOL_CREATOR_SLACK_LAMPORTS / 5_080, 1_968);
    }
}

// ── The LP lock's `lock_cp_liquidity`, encoded by hand (USDC curve design §10.3.9, 2026-10-23) ──
//
// `graduate_usdc_lock` locks the pool creator's whole LP by CPI into Raydium's LP lock, a closed
// source program, as the Raydium SDK the keeper calls builds the instruction
// (`cpmmLockPositionInstruction`, `src/raydium/cpmm/instruction.ts`): its discriminator,
// `lp_amount` and `with_metadata`, and its 19 accounts in the SDK's order with the SDK's flags.
// The pool's LP mint and vaults come from the pool's own `PoolState` (`pool_vaults_by_role`).

/// The first 8 bytes of sha256("global:lock_cp_liquidity"), Anchor's discriminator of the lock's
/// `lock_cp_liquidity`.
pub const LOCK_CP_LIQUIDITY_DISCRIMINATOR: [u8; 8] = [216, 157, 29, 78, 38, 51, 31, 26];

/// `lock_cp_liquidity`'s data: the discriminator, then `lp_amount` (a u64 little-endian) and
/// `with_metadata` (a bool, one byte), Borsh, 17 bytes in all.
pub fn lock_cp_liquidity_data(lp_amount: u64, with_metadata: bool) -> [u8; 17] {
    let mut data = [0u8; 17];
    data[..8].copy_from_slice(&LOCK_CP_LIQUIDITY_DISCRIMINATOR);
    data[8..16].copy_from_slice(&lp_amount.to_le_bytes());
    data[16] = u8::from(with_metadata);
    data
}

/// The seed of a lock's record under the lock program, `["locked_liquidity", fee_nft_mint]` (the
/// SDK's `getCpLockPda`).
pub const LOCKED_LIQUIDITY_SEED: &[u8] = b"locked_liquidity";

/// The 19 accounts of `lock_cp_liquidity`, named as the SDK names them.
pub struct LockCpLiquidityAccounts {
    pub authority: Pubkey,
    pub payer: Pubkey,
    pub liquidity_owner: Pubkey,
    pub fee_nft_owner: Pubkey,
    pub fee_nft_mint: Pubkey,
    pub fee_nft_account: Pubkey,
    pub pool_state: Pubkey,
    pub locked_liquidity: Pubkey,
    pub lp_mint: Pubkey,
    pub liquidity_owner_lp: Pubkey,
    pub locked_lp_vault: Pubkey,
    pub token_0_vault: Pubkey,
    pub token_1_vault: Pubkey,
    pub metadata_account: Pubkey,
    pub rent: Pubkey,
    pub system_program: Pubkey,
    pub token_program: Pubkey,
    pub associated_token_program: Pubkey,
    pub metadata_program: Pubkey,
}

impl LockCpLiquidityAccounts {
    /// The metas in the SDK's order with the SDK's flags: the payer a writable signer, the
    /// liquidity owner a signer, the Fee Key's mint a writable signer (a fresh keypair in the SDK,
    /// the program's PDA here, which signs the same), the pool's two vaults writable. The pool
    /// creator fills both the payer's slot and the owner's: the runtime merges the two metas of one
    /// account, so it is a writable signer once.
    pub fn to_account_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new_readonly(self.authority, false),
            AccountMeta::new(self.payer, true),
            AccountMeta::new_readonly(self.liquidity_owner, true),
            AccountMeta::new_readonly(self.fee_nft_owner, false),
            AccountMeta::new(self.fee_nft_mint, true),
            AccountMeta::new(self.fee_nft_account, false),
            AccountMeta::new_readonly(self.pool_state, false),
            AccountMeta::new(self.locked_liquidity, false),
            AccountMeta::new_readonly(self.lp_mint, false),
            AccountMeta::new(self.liquidity_owner_lp, false),
            AccountMeta::new(self.locked_lp_vault, false),
            AccountMeta::new(self.token_0_vault, false),
            AccountMeta::new(self.token_1_vault, false),
            AccountMeta::new(self.metadata_account, false),
            AccountMeta::new_readonly(self.rent, false),
            AccountMeta::new_readonly(self.system_program, false),
            AccountMeta::new_readonly(self.token_program, false),
            AccountMeta::new_readonly(self.associated_token_program, false),
            AccountMeta::new_readonly(self.metadata_program, false),
        ]
    }
}

/// The pool's two vaults by role, USDC's then the token's, as the pool's own `PoolState` records
/// them, never as the config names anything (§10.3.9, §10.3.10): a curve in PoolCreated keeps its
/// pool whatever `set_usdc_raydium` names after it. `AccountMismatch` unless the pool is the
/// curve's: its creator `pool_creator`, its two mints `usdc_mint` and `token_mint`, in either order.
pub fn pool_vaults_by_role(
    pool: &PoolState,
    pool_creator: &Pubkey,
    usdc_mint: &Pubkey,
    token_mint: &Pubkey,
) -> Result<(Pubkey, Pubkey)> {
    require_keys_eq!(
        pool.pool_creator,
        *pool_creator,
        FanexError::AccountMismatch
    );
    if pool.token_0_mint == *usdc_mint && pool.token_1_mint == *token_mint {
        Ok((pool.token_0_vault, pool.token_1_vault))
    } else if pool.token_0_mint == *token_mint && pool.token_1_mint == *usdc_mint {
        Ok((pool.token_1_vault, pool.token_0_vault))
    } else {
        err!(FanexError::AccountMismatch)
    }
}

/// The lock's record, `LockedCpLiquidityState`, 256 bytes (read on the mainnet binary in LiteSVM,
/// `usdc_graduation.rs`, 2026-10-23; the spec's 288 was an estimate).
pub const LOCKED_LIQUIDITY_LEN: usize = 256;

/// What the lock opens at its payer's cost, each by its size: the Fee Key's mint, the Fee Key's
/// token account, the lock's record, and its LP vault, the lock authority's ATA of the LP mint,
/// which it opens only when missing (read on the mainnet binary in LiteSVM, 2026-10-23).
pub const LOCK_OPENS: [usize; 4] = [
    SPL_MINT_LEN,
    SPL_TOKEN_ACCOUNT_LEN,
    LOCKED_LIQUIDITY_LEN,
    SPL_TOKEN_ACCOUNT_LEN,
];

/// What the caller hands the pool creator for the lock (§10.3.9 step 2): the rent of each account
/// the lock opens, the pool's slack and the caller's `extra`; `None` past u64. What the lock does
/// not spend goes back.
pub fn lock_funding(rent: &Rent, extra: u64) -> Option<u64> {
    LOCK_OPENS
        .iter()
        .try_fold(POOL_CREATOR_SLACK_LAMPORTS, |sum, len| {
            sum.checked_add(rent.minimum_balance(*len))
        })?
        .checked_add(extra)
}

#[cfg(test)]
mod lock_tests {
    use super::*;

    #[test]
    fn the_lock_s_discriminator_is_anchors() {
        let hash = anchor_lang::solana_program::hash::hash(b"global:lock_cp_liquidity");
        assert_eq!(hash.to_bytes()[..8], LOCK_CP_LIQUIDITY_DISCRIMINATOR);
        // As the SDK writes it (`anchorDataBuf.lockCpLiquidity`).
        assert_eq!(
            LOCK_CP_LIQUIDITY_DISCRIMINATOR,
            [216, 157, 29, 78, 38, 51, 31, 26]
        );
    }

    #[test]
    fn the_lock_s_data_is_the_discriminator_then_a_u64_and_a_bool() {
        let data = lock_cp_liquidity_data(0x0102_0304_0506_0708, false);
        assert_eq!(data.len(), 17);
        assert_eq!(data[..8], LOCK_CP_LIQUIDITY_DISCRIMINATOR);
        assert_eq!(data[8..16], [8, 7, 6, 5, 4, 3, 2, 1]);
        assert_eq!(data[16], 0);
        assert_eq!(lock_cp_liquidity_data(1, true)[16], 1);
        // Borsh of the two arguments in order, as Anchor's client would encode them.
        let mut borsh = Vec::new();
        424_242_424_242u64.serialize(&mut borsh).unwrap();
        false.serialize(&mut borsh).unwrap();
        assert_eq!(
            &lock_cp_liquidity_data(424_242_424_242, false)[8..],
            borsh.as_slice()
        );
    }

    #[test]
    fn the_lock_s_seed_is_the_sdk_s() {
        assert_eq!(LOCKED_LIQUIDITY_SEED, b"locked_liquidity");
    }

    /// Every account a key of its own, so a meta out of place names another key.
    fn accounts() -> (LockCpLiquidityAccounts, [Pubkey; 19]) {
        let k: [Pubkey; 19] = std::array::from_fn(|_| Pubkey::new_unique());
        let accounts = LockCpLiquidityAccounts {
            authority: k[0],
            payer: k[1],
            liquidity_owner: k[2],
            fee_nft_owner: k[3],
            fee_nft_mint: k[4],
            fee_nft_account: k[5],
            pool_state: k[6],
            locked_liquidity: k[7],
            lp_mint: k[8],
            liquidity_owner_lp: k[9],
            locked_lp_vault: k[10],
            token_0_vault: k[11],
            token_1_vault: k[12],
            metadata_account: k[13],
            rent: k[14],
            system_program: k[15],
            token_program: k[16],
            associated_token_program: k[17],
            metadata_program: k[18],
        };
        (accounts, k)
    }

    /// The SDK's 19 metas (`cpmmLockPositionInstruction`), in its order, with its flags:
    /// (name, signer, writable).
    const SDK_METAS: [(&str, bool, bool); 19] = [
        ("authority", false, false),
        ("payer", true, true),
        ("liquidity_owner", true, false),
        ("fee_nft_owner", false, false),
        ("fee_nft_mint", true, true),
        ("fee_nft_account", false, true),
        ("pool_state", false, false),
        ("locked_liquidity", false, true),
        ("lp_mint", false, false),
        ("liquidity_owner_lp", false, true),
        ("locked_lp_vault", false, true),
        ("token_0_vault", false, true),
        ("token_1_vault", false, true),
        ("metadata_account", false, true),
        ("rent", false, false),
        ("system_program", false, false),
        ("token_program", false, false),
        ("associated_token_program", false, false),
        ("metadata_program", false, false),
    ];

    #[test]
    fn the_metas_are_the_sdk_s_nineteen_in_its_order_with_its_flags() {
        let (accounts, keys) = accounts();
        let metas = accounts.to_account_metas();
        assert_eq!(metas.len(), 19);
        assert_eq!(metas.iter().map(|m| m.pubkey).collect::<Vec<_>>(), keys);
        for (i, (name, signer, writable)) in SDK_METAS.iter().enumerate() {
            assert_eq!(metas[i].is_signer, *signer, "{name}: signer");
            assert_eq!(metas[i].is_writable, *writable, "{name}: writable");
        }
        let signers: Vec<usize> = (0..19).filter(|&i| metas[i].is_signer).collect();
        assert_eq!(
            signers,
            [1, 2, 4],
            "the payer and the liquidity owner, both the pool creator, and the Fee Key's mint"
        );
    }

    fn pool(token_0_mint: Pubkey, token_1_mint: Pubkey, creator: Pubkey) -> PoolState {
        PoolState {
            amm_config: Pubkey::new_unique(),
            pool_creator: creator,
            token_0_vault: Pubkey::new_unique(),
            token_1_vault: Pubkey::new_unique(),
            lp_mint: Pubkey::new_unique(),
            token_0_mint,
            token_1_mint,
            token_0_program: Pubkey::new_unique(),
            token_1_program: Pubkey::new_unique(),
            observation: Pubkey::new_unique(),
            status: 0,
            lp_supply: 1,
            open_time: 2,
        }
    }

    fn code(result: Result<(Pubkey, Pubkey)>) -> u32 {
        match result {
            Err(anchor_lang::error::Error::AnchorError(e)) => e.error_code_number,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn the_vaults_are_taken_by_role_from_the_pool_s_own_records() {
        let (usdc, token, creator) = (
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        );
        // USDC token 0, then token 1: the vaults follow their mints either way.
        let usdc_first = pool(usdc, token, creator);
        assert_eq!(
            pool_vaults_by_role(&usdc_first, &creator, &usdc, &token).unwrap(),
            (usdc_first.token_0_vault, usdc_first.token_1_vault)
        );
        let token_first = pool(token, usdc, creator);
        assert_eq!(
            pool_vaults_by_role(&token_first, &creator, &usdc, &token).unwrap(),
            (token_first.token_1_vault, token_first.token_0_vault)
        );
    }

    #[test]
    fn a_pool_that_is_not_the_curve_s_is_refused() {
        let (usdc, token, creator) = (
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        );
        let mismatch = u32::from(FanexError::AccountMismatch);
        let other = Pubkey::new_unique();
        // Another creator, another token, another quote mint, the same mint twice.
        assert_eq!(
            code(pool_vaults_by_role(
                &pool(usdc, token, other),
                &creator,
                &usdc,
                &token
            )),
            mismatch
        );
        assert_eq!(
            code(pool_vaults_by_role(
                &pool(usdc, other, creator),
                &creator,
                &usdc,
                &token
            )),
            mismatch
        );
        assert_eq!(
            code(pool_vaults_by_role(
                &pool(other, token, creator),
                &creator,
                &usdc,
                &token
            )),
            mismatch
        );
        assert_eq!(
            code(pool_vaults_by_role(
                &pool(usdc, usdc, creator),
                &creator,
                &usdc,
                &token
            )),
            mismatch
        );
    }

    #[test]
    fn the_pool_creator_gets_the_lock_s_rents_and_the_slack() {
        let rent = Rent::default();
        let rents: u64 = [82usize, 165, LOCKED_LIQUIDITY_LEN, 165]
            .iter()
            .map(|len| rent.minimum_balance(*len))
            .sum();
        assert_eq!(
            lock_funding(&rent, 0),
            Some(rents + POOL_CREATOR_SLACK_LAMPORTS)
        );
        // Each account's rent apart, never one rent for their summed size.
        assert!(rents > rent.minimum_balance(LOCK_OPENS.iter().sum()));
        // The caller's extra on top, refunded with the rest; past u64, nothing.
        assert_eq!(
            lock_funding(&rent, MAX_GRADUATION_EXTRA_LAMPORTS),
            Some(rents + POOL_CREATOR_SLACK_LAMPORTS + MAX_GRADUATION_EXTRA_LAMPORTS)
        );
        assert_eq!(lock_funding(&rent, u64::MAX), None);
        // About 0.018 SOL in all, 0.008 of it the rents, what the lock does not spend refunded.
        let total = lock_funding(&rent, 0).unwrap();
        assert!(total > 10_000_000 && total < 20_000_000, "{total}");
    }

    #[test]
    fn the_lock_step_costs_its_caller_0_006_sol_on_mainnet() {
        // Mainnet's rent (2026-10-23), as the pool step's test reads it.
        let rent = Rent {
            lamports_per_byte_year: 5_080,
            exemption_threshold: 1.0,
            ..Rent::default()
        };
        let rents: u64 = LOCK_OPENS.iter().map(|len| rent.minimum_balance(*len)).sum();
        assert_eq!(rents, 5_994_400);
        assert_eq!(
            lock_funding(&rent, 0),
            Some(5_994_400 + POOL_CREATOR_SLACK_LAMPORTS)
        );
    }

    #[test]
    fn the_extra_is_bounded_at_a_tenth_of_a_sol() {
        assert_eq!(MAX_GRADUATION_EXTRA_LAMPORTS, 100_000_000);
        // Ten times the slack: room for about 19,685 bytes more at mainnet's rent.
        assert_eq!(MAX_GRADUATION_EXTRA_LAMPORTS / 5_080, 19_685);
    }
}
