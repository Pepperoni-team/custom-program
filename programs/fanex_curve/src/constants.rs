use anchor_lang::prelude::*;

pub const BPS_DENOMINATOR: u64 = 10_000;

pub const PLATFORM_SWAP_FEE_BPS: u64 = 100;

pub const DIVIDEND_PAYER: Pubkey = Pubkey::new_from_array([
    66, 126, 147, 234, 142, 16, 174, 190, 51, 253, 246, 8, 22, 10, 172, 133, 183, 237, 57, 53, 101,
    186, 207, 49, 23, 170, 76, 198, 112, 72, 18, 99,
]);

pub const MAX_DISTRIBUTOR_EXPIRY_SECONDS: i64 = 400 * 24 * 60 * 60;

pub const LAMPORTS_PER_SOL: u64 = 1_000_000_000;

pub const TOKEN_DECIMALS: u8 = 6;

pub const TOKEN_TOTAL_SUPPLY: u64 = 100_000_000 * 1_000_000;

pub const LAUNCH_GUARD_SECONDS: i64 = 300;

pub const LAUNCH_WALLET_CAP: u64 = 1_000_000_000_000;

pub const MIN_CREATOR_TARGET_LAMPORTS: u64 = 16 * LAMPORTS_PER_SOL;

pub const MIN_CPMM_SEED_LAMPORTS: u64 = 24 * LAMPORTS_PER_SOL;
pub const CAP_CPMM_SEED_LAMPORTS: u64 = 800 * LAMPORTS_PER_SOL;

pub const VESTING_SUPPLY_BPS: u64 = 200;
pub const VESTING_CLIFF_SECONDS: i64 = 12 * 30 * 24 * 60 * 60;
pub const VESTING_DURATION_SECONDS: i64 = 0;

pub const MAX_NAME_LEN: usize = 32;
pub const MAX_SYMBOL_LEN: usize = 12;
pub const MAX_URI_LEN: usize = 200;

pub const CONFIG_SEED: &[u8] = b"config";
pub const CURVE_SEED: &[u8] = b"curve";
pub const SOL_VAULT_SEED: &[u8] = b"sol_vault";
pub const TOKEN_VAULT_SEED: &[u8] = b"token_vault";
pub const VESTING_VAULT_SEED: &[u8] = b"vesting_vault";
pub const VESTING_SEED: &[u8] = b"vesting";
pub const DISTRIBUTOR_SEED: &[u8] = b"distributor";
pub const CLAIM_SEED: &[u8] = b"claim";

pub const PAYOUT_TRANSIT_SEED: &[u8] = b"payout";

pub const FAN_LED_PLACEHOLDER_SEED: &[u8] = b"fan-led-placeholder";

pub const MAX_DISTRIBUTOR_V2_LEAVES: u32 = 65_536;

pub const DIVIDEND_LEAF_V2_DOMAIN: &[u8] = b"fanex:dividend:v2";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_v2_registry_fits_an_account_created_by_a_program() {
        const MAX_CREATED_ACCOUNT_BYTES: usize = 10 * 1024;
        let bitmap = (MAX_DISTRIBUTOR_V2_LEAVES as usize).div_ceil(8);
        assert_eq!(bitmap, 8 * 1024);
        assert!(crate::state::DistributorV2::HEADER_SPACE + bitmap <= MAX_CREATED_ACCOUNT_BYTES);
    }

    #[test]
    fn the_launch_cap_is_one_percent_of_the_supply_for_five_minutes() {
        assert_eq!(LAUNCH_WALLET_CAP, 1_000_000 * 1_000_000);
        assert_eq!(LAUNCH_WALLET_CAP * 100, TOKEN_TOTAL_SUPPLY);
        assert_eq!(LAUNCH_GUARD_SECONDS, 300);
    }

    #[test]
    fn min_creator_target_is_16_sol() {
        assert_eq!(MIN_CREATOR_TARGET_LAMPORTS, 16 * LAMPORTS_PER_SOL);
    }

    #[test]
    fn pool_floor_is_24_sol_and_above_the_target_floor() {
        assert_eq!(MIN_CPMM_SEED_LAMPORTS, 24 * LAMPORTS_PER_SOL);
        assert_eq!(CAP_CPMM_SEED_LAMPORTS, 800 * LAMPORTS_PER_SOL);
        assert!(MIN_CREATOR_TARGET_LAMPORTS < MIN_CPMM_SEED_LAMPORTS);
    }
}
