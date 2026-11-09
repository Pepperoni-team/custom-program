//! The program's addresses and the two accounts a quote reads, decoded by hand.
//!
//! `pepper_curve` is an Anchor 0.31.1 program on the Solana 2 crates, Jupiter's interface is on the
//! Solana 3 and 4 generation, so this crate never imports the program (USDC curve design §10.5): it
//! reads `CurveUsdc` and `UsdcConfig` at the offsets the program writes them
//! (`programs/pepper_curve/src/state.rs`, pinned there by `usdc_layout_tests`), and its tests hold
//! every offset, size and discriminator to the program's committed IDL.

use std::fmt;

use solana_pubkey::{pubkey, Pubkey};

/// `pepper_curve`, the same address on every cluster.
pub const PROGRAM_ID: Pubkey = pubkey!("CDPwGWGWbAE8FL5f2oVdzfkTutrnsKJcAcgqWktC3dwZ");
/// Classic SPL Token: USDC and every curve's token are classic mints.
pub const TOKEN_PROGRAM_ID: Pubkey = pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey =
    pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

/// `["curve", mint]`: a `CurveUsdc`, where a SOL `Curve` would sit (a mint has one or the other).
pub const CURVE_SEED: &[u8] = b"curve";
/// `["usdc_config"]`: the USDC side's one config.
pub const USDC_CONFIG_SEED: &[u8] = b"usdc_config";
/// `["usdc_vault", mint]`: a curve's USDC reserve, a token account whose authority is the curve.
pub const USDC_VAULT_SEED: &[u8] = b"usdc_vault";

/// The `UsdcConfig` PDA, `["usdc_config"]` under the program: one address on every cluster, since
/// the program's is one.
pub const USDC_CONFIG_ADDRESS: Pubkey = pubkey!("CuC2WeLhhwVA65gYCKk2Zu12kC5uGJSyriFKWJwkT9F9");

/// The layout version both accounts start with; another one is refused, never guessed.
pub const ACCOUNT_VERSION: u8 = 1;
/// `CurveUsdc.phase` is a raw u8: 0 Bonding, 1 Graduating, 2 Graduated, 3 PoolCreated. The swaps run
/// in Bonding alone.
pub const PHASE_BONDING: u8 = 0;

/// `CurveUsdc`, 354 bytes on chain: the 8-byte discriminator, then the fields at these offsets.
pub mod curve_usdc {
    pub const DISCRIMINATOR: [u8; 8] = [208, 28, 161, 208, 98, 148, 66, 109];
    pub const LEN: usize = 354;

    pub const VERSION: usize = 8;
    pub const BUMP: usize = 9;
    pub const USDC_VAULT_BUMP: usize = 10;
    pub const PHASE: usize = 11;
    pub const PAUSED: usize = 12;
    pub const MIGRATED: usize = 13;
    pub const MINT: usize = 14;
    pub const CREATOR: usize = 46;
    pub const CREATOR_USDC_ACCOUNT: usize = 78;
    pub const CREATOR_SPLIT_BPS: usize = 110;
    pub const FANEX_SPLIT_BPS: usize = 112;
    pub const CREATOR_TARGET_USDC: usize = 114;
    pub const GRADUATION_THRESHOLD_USDC: usize = 122;
    pub const CREATOR_CAP_USDC: usize = 130;
    pub const FANEX_CAP_USDC: usize = 138;
    pub const VIRTUAL_USDC_RESERVES: usize = 146;
    pub const VIRTUAL_TOKEN_RESERVES: usize = 154;
    pub const REAL_USDC_RESERVES: usize = 162;
    pub const REAL_TOKEN_RESERVES: usize = 170;
    pub const MIGRATION_TOKEN_RESERVES: usize = 178;
    pub const CREATOR_RECEIVED_USDC: usize = 186;
    pub const FANEX_RECEIVED_USDC: usize = 194;
    pub const CREATED_AT: usize = 202;
    pub const GRADUATED_AT: usize = 210;
    pub const MIGRATED_AT: usize = 218;
    /// N+3's mark of a creator's own claim (2026-11-09): no quote or swap reads it.
    pub const CLAIMED_AT: usize = 226;
    pub const RESERVED: usize = 234;

    /// Every field by its IDL name, in the order the program declares them.
    pub const FIELDS: [(&str, usize); 27] = [
        ("version", VERSION),
        ("bump", BUMP),
        ("usdc_vault_bump", USDC_VAULT_BUMP),
        ("phase", PHASE),
        ("paused", PAUSED),
        ("migrated", MIGRATED),
        ("mint", MINT),
        ("creator", CREATOR),
        ("creator_usdc_account", CREATOR_USDC_ACCOUNT),
        ("creator_split_bps", CREATOR_SPLIT_BPS),
        ("fanex_split_bps", FANEX_SPLIT_BPS),
        ("creator_target_usdc", CREATOR_TARGET_USDC),
        ("graduation_threshold_usdc", GRADUATION_THRESHOLD_USDC),
        ("creator_cap_usdc", CREATOR_CAP_USDC),
        ("fanex_cap_usdc", FANEX_CAP_USDC),
        ("virtual_usdc_reserves", VIRTUAL_USDC_RESERVES),
        ("virtual_token_reserves", VIRTUAL_TOKEN_RESERVES),
        ("real_usdc_reserves", REAL_USDC_RESERVES),
        ("real_token_reserves", REAL_TOKEN_RESERVES),
        ("migration_token_reserves", MIGRATION_TOKEN_RESERVES),
        ("creator_received_usdc", CREATOR_RECEIVED_USDC),
        ("fanex_received_usdc", FANEX_RECEIVED_USDC),
        ("created_at", CREATED_AT),
        ("graduated_at", GRADUATED_AT),
        ("migrated_at", MIGRATED_AT),
        ("claimed_at", CLAIMED_AT),
        ("reserved", RESERVED),
    ];
}

/// `UsdcConfig`, 491 bytes on chain: the 8-byte discriminator, then the fields at these offsets.
pub mod usdc_config {
    pub const DISCRIMINATOR: [u8; 8] = [48, 197, 208, 34, 24, 38, 56, 253];
    pub const LEN: usize = 491;

    pub const VERSION: usize = 8;
    pub const BUMP: usize = 9;
    pub const PAUSED: usize = 10;
    pub const ADMIN: usize = 11;
    pub const PAUSER: usize = 43;
    pub const MIGRATION_AUTHORITY: usize = 75;
    pub const USDC_MINT: usize = 107;
    pub const FEE_USDC_ACCOUNT: usize = 139;
    pub const LP_FEE_OWNER: usize = 171;
    pub const RAYDIUM_CPMM_PROGRAM: usize = 203;
    pub const RAYDIUM_AMM_CONFIG: usize = 235;
    pub const RAYDIUM_CREATE_POOL_FEE: usize = 267;
    pub const RAYDIUM_LOCK_PROGRAM: usize = 299;
    pub const RAYDIUM_LOCK_AUTHORITY: usize = 331;
    pub const LAUNCHER: usize = 363;
    /// N+3's key that co-signs a creator's claim of a fan-led market (2026-11-05): no quote or
    /// swap reads it.
    pub const CLAIM_APPROVER: usize = 395;
    pub const RESERVED: usize = 427;

    /// Every field by its IDL name, in the order the program declares them.
    pub const FIELDS: [(&str, usize); 17] = [
        ("version", VERSION),
        ("bump", BUMP),
        ("paused", PAUSED),
        ("admin", ADMIN),
        ("pauser", PAUSER),
        ("migration_authority", MIGRATION_AUTHORITY),
        ("usdc_mint", USDC_MINT),
        ("fee_usdc_account", FEE_USDC_ACCOUNT),
        ("lp_fee_owner", LP_FEE_OWNER),
        ("raydium_cpmm_program", RAYDIUM_CPMM_PROGRAM),
        ("raydium_amm_config", RAYDIUM_AMM_CONFIG),
        ("raydium_create_pool_fee", RAYDIUM_CREATE_POOL_FEE),
        ("raydium_lock_program", RAYDIUM_LOCK_PROGRAM),
        ("raydium_lock_authority", RAYDIUM_LOCK_AUTHORITY),
        ("launcher", LAUNCHER),
        ("claim_approver", CLAIM_APPROVER),
        ("reserved", RESERVED),
    ];
}

/// Why an account was not read: each one a refusal, never a field guessed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutError {
    /// Not owned by `pepper_curve`: anyone can write any bytes into an account of their own.
    WrongOwner {
        account: &'static str,
        owner: Pubkey,
    },
    WrongLength {
        account: &'static str,
        len: usize,
    },
    /// Another account type, a SOL `Curve` at a curve's address among them.
    WrongDiscriminator {
        account: &'static str,
    },
    WrongVersion {
        account: &'static str,
        version: u8,
    },
    /// A byte other than 0 or 1 where a bool sits, which Anchor's Borsh refuses too.
    NotABool {
        account: &'static str,
        offset: usize,
        byte: u8,
    },
    /// The account is not at the address its own seeds and bump give.
    WrongAddress {
        account: &'static str,
        expected: Pubkey,
        found: Pubkey,
    },
    /// A bump that gives no address off the curve for its seeds.
    BadBump {
        account: &'static str,
        bump: u8,
    },
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LayoutError::WrongOwner { account, owner } => {
                write!(
                    f,
                    "{account}: owned by {owner}, not by pepper_curve {PROGRAM_ID}"
                )
            }
            LayoutError::WrongLength { account, len } => write!(f, "{account}: {len} bytes"),
            LayoutError::WrongDiscriminator { account } => {
                write!(f, "{account}: another account type")
            }
            LayoutError::WrongVersion { account, version } => {
                write!(
                    f,
                    "{account}: layout version {version}, this SDK reads {ACCOUNT_VERSION}"
                )
            }
            LayoutError::NotABool {
                account,
                offset,
                byte,
            } => {
                write!(f, "{account}: byte {byte} at {offset} is not a bool")
            }
            LayoutError::WrongAddress {
                account,
                expected,
                found,
            } => {
                write!(f, "{account}: at {found}, its seeds give {expected}")
            }
            LayoutError::BadBump { account, bump } => {
                write!(f, "{account}: bump {bump} gives no address")
            }
        }
    }
}

impl std::error::Error for LayoutError {}

fn header(
    data: &[u8],
    account: &'static str,
    len: usize,
    discriminator: &[u8; 8],
) -> Result<(), LayoutError> {
    if data.len() != len {
        return Err(LayoutError::WrongLength {
            account,
            len: data.len(),
        });
    }
    if data[..8] != discriminator[..] {
        return Err(LayoutError::WrongDiscriminator { account });
    }
    let version = data[8];
    if version != ACCOUNT_VERSION {
        return Err(LayoutError::WrongVersion { account, version });
    }
    Ok(())
}

fn u16_at(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(data[offset..offset + 2].try_into().expect("two bytes"))
}

fn u64_at(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().expect("eight bytes"))
}

fn i64_at(data: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(data[offset..offset + 8].try_into().expect("eight bytes"))
}

fn pubkey_at(data: &[u8], offset: usize) -> Pubkey {
    Pubkey::new_from_array(data[offset..offset + 32].try_into().expect("32 bytes"))
}

fn bool_at(data: &[u8], offset: usize, account: &'static str) -> Result<bool, LayoutError> {
    match data[offset] {
        0 => Ok(false),
        1 => Ok(true),
        byte => Err(LayoutError::NotABool {
            account,
            offset,
            byte,
        }),
    }
}

/// A USDC curve as the program stores it (`state.rs`'s `CurveUsdc`): USDC amounts in µUSDC, token
/// amounts in base units, each field its SOL twin's meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CurveUsdc {
    pub bump: u8,
    pub usdc_vault_bump: u8,
    pub phase: u8,
    pub paused: bool,
    pub migrated: bool,
    pub mint: Pubkey,
    pub creator: Pubkey,
    /// Where a buy pays the creator's raise share; all-zero when the curve takes no creator split.
    pub creator_usdc_account: Pubkey,
    pub creator_split_bps: u16,
    pub fanex_split_bps: u16,
    pub creator_target_usdc: u64,
    pub graduation_threshold_usdc: u64,
    pub creator_cap_usdc: u64,
    pub fanex_cap_usdc: u64,
    pub virtual_usdc_reserves: u64,
    pub virtual_token_reserves: u64,
    /// A counter: the vault's balance is never read for a price, so a donation moves nothing.
    pub real_usdc_reserves: u64,
    pub real_token_reserves: u64,
    pub migration_token_reserves: u64,
    pub creator_received_usdc: u64,
    pub fanex_received_usdc: u64,
    pub created_at: i64,
    pub graduated_at: i64,
    pub migrated_at: i64,
}

impl CurveUsdc {
    pub const NAME: &'static str = "CurveUsdc";

    /// Reads a curve's account data: 354 bytes, `CurveUsdc`'s discriminator, version 1.
    pub fn decode(data: &[u8]) -> Result<Self, LayoutError> {
        use curve_usdc::*;
        header(data, Self::NAME, LEN, &DISCRIMINATOR)?;
        Ok(Self {
            bump: data[BUMP],
            usdc_vault_bump: data[USDC_VAULT_BUMP],
            phase: data[PHASE],
            paused: bool_at(data, PAUSED, Self::NAME)?,
            migrated: bool_at(data, MIGRATED, Self::NAME)?,
            mint: pubkey_at(data, MINT),
            creator: pubkey_at(data, CREATOR),
            creator_usdc_account: pubkey_at(data, CREATOR_USDC_ACCOUNT),
            creator_split_bps: u16_at(data, CREATOR_SPLIT_BPS),
            fanex_split_bps: u16_at(data, FANEX_SPLIT_BPS),
            creator_target_usdc: u64_at(data, CREATOR_TARGET_USDC),
            graduation_threshold_usdc: u64_at(data, GRADUATION_THRESHOLD_USDC),
            creator_cap_usdc: u64_at(data, CREATOR_CAP_USDC),
            fanex_cap_usdc: u64_at(data, FANEX_CAP_USDC),
            virtual_usdc_reserves: u64_at(data, VIRTUAL_USDC_RESERVES),
            virtual_token_reserves: u64_at(data, VIRTUAL_TOKEN_RESERVES),
            real_usdc_reserves: u64_at(data, REAL_USDC_RESERVES),
            real_token_reserves: u64_at(data, REAL_TOKEN_RESERVES),
            migration_token_reserves: u64_at(data, MIGRATION_TOKEN_RESERVES),
            creator_received_usdc: u64_at(data, CREATOR_RECEIVED_USDC),
            fanex_received_usdc: u64_at(data, FANEX_RECEIVED_USDC),
            created_at: i64_at(data, CREATED_AT),
            graduated_at: i64_at(data, GRADUATED_AT),
            migrated_at: i64_at(data, MIGRATED_AT),
        })
    }

    /// The account data the program would hold for these fields, its claim mark and its 120 reserved bytes zero: for
    /// fixtures and tests (the parity suite writes its curves with it).
    pub fn to_account_data(&self) -> Vec<u8> {
        use curve_usdc::*;
        let mut data = vec![0u8; LEN];
        data[..8].copy_from_slice(&DISCRIMINATOR);
        data[VERSION] = ACCOUNT_VERSION;
        data[BUMP] = self.bump;
        data[USDC_VAULT_BUMP] = self.usdc_vault_bump;
        data[PHASE] = self.phase;
        data[PAUSED] = u8::from(self.paused);
        data[MIGRATED] = u8::from(self.migrated);
        let mut put =
            |offset: usize, bytes: &[u8]| data[offset..offset + bytes.len()].copy_from_slice(bytes);
        put(MINT, self.mint.as_ref());
        put(CREATOR, self.creator.as_ref());
        put(CREATOR_USDC_ACCOUNT, self.creator_usdc_account.as_ref());
        put(CREATOR_SPLIT_BPS, &self.creator_split_bps.to_le_bytes());
        put(FANEX_SPLIT_BPS, &self.fanex_split_bps.to_le_bytes());
        put(CREATOR_TARGET_USDC, &self.creator_target_usdc.to_le_bytes());
        put(
            GRADUATION_THRESHOLD_USDC,
            &self.graduation_threshold_usdc.to_le_bytes(),
        );
        put(CREATOR_CAP_USDC, &self.creator_cap_usdc.to_le_bytes());
        put(FANEX_CAP_USDC, &self.fanex_cap_usdc.to_le_bytes());
        put(
            VIRTUAL_USDC_RESERVES,
            &self.virtual_usdc_reserves.to_le_bytes(),
        );
        put(
            VIRTUAL_TOKEN_RESERVES,
            &self.virtual_token_reserves.to_le_bytes(),
        );
        put(REAL_USDC_RESERVES, &self.real_usdc_reserves.to_le_bytes());
        put(REAL_TOKEN_RESERVES, &self.real_token_reserves.to_le_bytes());
        put(
            MIGRATION_TOKEN_RESERVES,
            &self.migration_token_reserves.to_le_bytes(),
        );
        put(
            CREATOR_RECEIVED_USDC,
            &self.creator_received_usdc.to_le_bytes(),
        );
        put(FANEX_RECEIVED_USDC, &self.fanex_received_usdc.to_le_bytes());
        put(CREATED_AT, &self.created_at.to_le_bytes());
        put(GRADUATED_AT, &self.graduated_at.to_le_bytes());
        put(MIGRATED_AT, &self.migrated_at.to_le_bytes());
        data
    }
}

/// The USDC side's config as the program stores it (`state.rs`'s `UsdcConfig`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsdcConfig {
    pub bump: u8,
    /// The USDC side's kill switch: no swap runs while it is set.
    pub paused: bool,
    pub admin: Pubkey,
    pub pauser: Pubkey,
    pub migration_authority: Pubkey,
    /// The one USDC mint every curve trades against, set once.
    pub usdc_mint: Pubkey,
    /// The fee wallet's USDC account: every fee and Fanex share lands there.
    pub fee_usdc_account: Pubkey,
    pub lp_fee_owner: Pubkey,
    pub raydium_cpmm_program: Pubkey,
    pub raydium_amm_config: Pubkey,
    pub raydium_create_pool_fee: Pubkey,
    pub raydium_lock_program: Pubkey,
    pub raydium_lock_authority: Pubkey,
    pub launcher: Pubkey,
}

impl UsdcConfig {
    pub const NAME: &'static str = "UsdcConfig";

    /// Reads the config's account data: 491 bytes, `UsdcConfig`'s discriminator, version 1.
    pub fn decode(data: &[u8]) -> Result<Self, LayoutError> {
        use usdc_config::*;
        header(data, Self::NAME, LEN, &DISCRIMINATOR)?;
        Ok(Self {
            bump: data[BUMP],
            paused: bool_at(data, PAUSED, Self::NAME)?,
            admin: pubkey_at(data, ADMIN),
            pauser: pubkey_at(data, PAUSER),
            migration_authority: pubkey_at(data, MIGRATION_AUTHORITY),
            usdc_mint: pubkey_at(data, USDC_MINT),
            fee_usdc_account: pubkey_at(data, FEE_USDC_ACCOUNT),
            lp_fee_owner: pubkey_at(data, LP_FEE_OWNER),
            raydium_cpmm_program: pubkey_at(data, RAYDIUM_CPMM_PROGRAM),
            raydium_amm_config: pubkey_at(data, RAYDIUM_AMM_CONFIG),
            raydium_create_pool_fee: pubkey_at(data, RAYDIUM_CREATE_POOL_FEE),
            raydium_lock_program: pubkey_at(data, RAYDIUM_LOCK_PROGRAM),
            raydium_lock_authority: pubkey_at(data, RAYDIUM_LOCK_AUTHORITY),
            launcher: pubkey_at(data, LAUNCHER),
        })
    }

    /// The account data the program would hold for these fields, its claim approver and its 64
    /// reserved bytes zero: for fixtures and tests.
    pub fn to_account_data(&self) -> Vec<u8> {
        use usdc_config::*;
        let mut data = vec![0u8; LEN];
        data[..8].copy_from_slice(&DISCRIMINATOR);
        data[VERSION] = ACCOUNT_VERSION;
        data[BUMP] = self.bump;
        data[PAUSED] = u8::from(self.paused);
        for (offset, key) in [
            (ADMIN, &self.admin),
            (PAUSER, &self.pauser),
            (MIGRATION_AUTHORITY, &self.migration_authority),
            (USDC_MINT, &self.usdc_mint),
            (FEE_USDC_ACCOUNT, &self.fee_usdc_account),
            (LP_FEE_OWNER, &self.lp_fee_owner),
            (RAYDIUM_CPMM_PROGRAM, &self.raydium_cpmm_program),
            (RAYDIUM_AMM_CONFIG, &self.raydium_amm_config),
            (RAYDIUM_CREATE_POOL_FEE, &self.raydium_create_pool_fee),
            (RAYDIUM_LOCK_PROGRAM, &self.raydium_lock_program),
            (RAYDIUM_LOCK_AUTHORITY, &self.raydium_lock_authority),
            (LAUNCHER, &self.launcher),
        ] {
            data[offset..offset + 32].copy_from_slice(key.as_ref());
        }
        data
    }
}

/// `owner`'s associated token account of `mint` under classic SPL Token, as the program derives it
/// for any key, a PDA's included.
pub fn associated_token_address(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), TOKEN_PROGRAM_ID.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
    .0
}

/// The curve's address and its bump for `mint`, by search: what a launch derives.
pub fn find_curve_address(mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[CURVE_SEED, mint.as_ref()], &PROGRAM_ID)
}

/// The curve's USDC vault and its bump for `mint`, by search.
pub fn find_usdc_vault_address(mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[USDC_VAULT_SEED, mint.as_ref()], &PROGRAM_ID)
}

fn program_address(
    seeds: &[&[u8]],
    account: &'static str,
    bump: u8,
) -> Result<Pubkey, LayoutError> {
    Pubkey::create_program_address(seeds, &PROGRAM_ID)
        .map_err(|_| LayoutError::BadBump { account, bump })
}

/// The three addresses a curve's swaps name besides the config, from the bumps the curve records:
/// one hash each, no search, but the token vault's associated-token derivation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CurveAddresses {
    /// `["curve", mint]`, with `CurveUsdc.bump`.
    pub curve: Pubkey,
    /// `["usdc_vault", mint]`, with `CurveUsdc.usdc_vault_bump`.
    pub usdc_vault: Pubkey,
    /// The curve PDA's associated token account of its mint.
    pub token_vault: Pubkey,
}

impl CurveAddresses {
    pub fn of(curve: &CurveUsdc) -> Result<Self, LayoutError> {
        let mint = curve.mint;
        let address = program_address(
            &[CURVE_SEED, mint.as_ref(), &[curve.bump]],
            CurveUsdc::NAME,
            curve.bump,
        )?;
        let usdc_vault = program_address(
            &[USDC_VAULT_SEED, mint.as_ref(), &[curve.usdc_vault_bump]],
            "usdc_vault",
            curve.usdc_vault_bump,
        )?;
        Ok(Self {
            curve: address,
            usdc_vault,
            token_vault: associated_token_address(&address, &mint),
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn key(byte: u8) -> Pubkey {
        Pubkey::new_from_array([byte; 32])
    }

    /// Every field a value of its own, so a field read from another's offset reads another number.
    pub(crate) fn distinct_curve() -> CurveUsdc {
        CurveUsdc {
            bump: 253,
            usdc_vault_bump: 252,
            phase: 3,
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
        }
    }

    pub(crate) fn distinct_config() -> UsdcConfig {
        UsdcConfig {
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
        }
    }

    /// The bytes the program's own `usdc_layout_tests` pin for the same values (state.rs), field by
    /// field: the writer here is not the reader's mirror image, it is the program's layout.
    #[test]
    fn a_curve_reads_back_every_field_from_the_program_s_offsets() {
        let curve = distinct_curve();
        let data = curve.to_account_data();
        assert_eq!(data.len(), 354);
        assert_eq!(&data[..8], &[208, 28, 161, 208, 98, 148, 66, 109]);
        assert_eq!(data[8], 1);
        assert_eq!(
            (data[9], data[10], data[11], data[12], data[13]),
            (253, 252, 3, 1, 1)
        );
        assert_eq!(&data[14..46], &[21; 32]);
        assert_eq!(&data[46..78], &[22; 32]);
        assert_eq!(&data[78..110], &[23; 32]);
        assert_eq!(&data[110..112], &0x0102u16.to_le_bytes());
        assert_eq!(&data[112..114], &0x0304u16.to_le_bytes());
        for (i, offset) in (114..202).step_by(8).enumerate() {
            assert_eq!(
                &data[offset..offset + 8],
                &(0x1111_1111_1111_1101u64 + i as u64).to_le_bytes(),
                "u64 at {offset}"
            );
        }
        assert_eq!(&data[202..210], &(-0x2222_2222_2222_2201i64).to_le_bytes());
        assert_eq!(&data[210..218], &0x2222_2222_2222_2202i64.to_le_bytes());
        assert_eq!(&data[218..226], &0x2222_2222_2222_2203i64.to_le_bytes());
        assert_eq!(&data[226..234], &[0u8; 8], "the claim mark, which no quote reads");
        assert_eq!(&data[234..], &[0u8; 120]);
        assert_eq!(CurveUsdc::decode(&data), Ok(curve));
    }

    #[test]
    fn a_config_reads_back_every_field_from_the_program_s_offsets() {
        let config = distinct_config();
        let data = config.to_account_data();
        assert_eq!(data.len(), 491);
        assert_eq!(&data[..8], &[48, 197, 208, 34, 24, 38, 56, 253]);
        assert_eq!((data[8], data[9], data[10]), (1, 254, 1));
        for (i, offset) in (11..395).step_by(32).enumerate() {
            assert_eq!(
                &data[offset..offset + 32],
                &[i as u8 + 1; 32],
                "key at {offset}"
            );
        }
        assert_eq!(&data[395..427], &[0u8; 32], "the claim approver, which no quote reads");
        assert_eq!(&data[427..], &[0u8; 64]);
        assert_eq!(UsdcConfig::decode(&data), Ok(config));
    }

    #[test]
    fn an_account_of_another_size_type_or_version_is_refused() {
        let data = distinct_curve().to_account_data();
        assert_eq!(
            CurveUsdc::decode(&data[..353]),
            Err(LayoutError::WrongLength {
                account: "CurveUsdc",
                len: 353
            })
        );
        let mut longer = data.clone();
        longer.push(0);
        assert!(matches!(
            CurveUsdc::decode(&longer),
            Err(LayoutError::WrongLength { .. })
        ));
        // A SOL curve's discriminator, or the config's, at a curve's address.
        let mut other = data.clone();
        other[..8].copy_from_slice(&usdc_config::DISCRIMINATOR);
        assert_eq!(
            CurveUsdc::decode(&other),
            Err(LayoutError::WrongDiscriminator {
                account: "CurveUsdc"
            })
        );
        let mut v2 = data.clone();
        v2[8] = 2;
        assert_eq!(
            CurveUsdc::decode(&v2),
            Err(LayoutError::WrongVersion {
                account: "CurveUsdc",
                version: 2
            })
        );
        let mut flag = data;
        flag[curve_usdc::PAUSED] = 2;
        assert_eq!(
            CurveUsdc::decode(&flag),
            Err(LayoutError::NotABool {
                account: "CurveUsdc",
                offset: 12,
                byte: 2
            })
        );

        let config = distinct_config().to_account_data();
        assert!(matches!(
            UsdcConfig::decode(&config[..490]),
            Err(LayoutError::WrongLength { .. })
        ));
        let mut curve_bytes = config.clone();
        curve_bytes[..8].copy_from_slice(&curve_usdc::DISCRIMINATOR);
        assert!(matches!(
            UsdcConfig::decode(&curve_bytes),
            Err(LayoutError::WrongDiscriminator { .. })
        ));
        let mut paused = config;
        paused[usdc_config::PAUSED] = 7;
        assert!(matches!(
            UsdcConfig::decode(&paused),
            Err(LayoutError::NotABool {
                offset: 10,
                byte: 7,
                ..
            })
        ));
    }

    #[test]
    fn the_config_sits_at_its_seed_on_every_cluster() {
        assert_eq!(
            Pubkey::find_program_address(&[USDC_CONFIG_SEED], &PROGRAM_ID).0,
            USDC_CONFIG_ADDRESS
        );
    }

    #[test]
    fn a_curve_s_addresses_come_from_the_bumps_it_records() {
        let mint = key(42);
        let (curve_address, bump) = find_curve_address(&mint);
        let (vault, vault_bump) = find_usdc_vault_address(&mint);
        let curve = CurveUsdc {
            mint,
            bump,
            usdc_vault_bump: vault_bump,
            ..distinct_curve()
        };
        let addresses = CurveAddresses::of(&curve).unwrap();
        assert_eq!(addresses.curve, curve_address);
        assert_eq!(addresses.usdc_vault, vault);
        assert_eq!(
            addresses.token_vault,
            associated_token_address(&curve_address, &mint)
        );
        // A bump that gives a point on the curve, or another address, is refused or tells.
        let other = CurveUsdc {
            bump: bump.wrapping_sub(1),
            ..curve
        };
        match CurveAddresses::of(&other) {
            Err(LayoutError::BadBump { .. }) => {}
            Ok(found) => assert_ne!(found.curve, curve_address),
            Err(e) => panic!("unexpected {e}"),
        }
    }
}
