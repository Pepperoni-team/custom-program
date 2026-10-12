use anchor_lang::prelude::*;

#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Copy, PartialEq, Eq, Debug)]
pub enum CurvePhase {
    Bonding,
    Graduating,
    Graduated,
}

#[account]
#[derive(InitSpace)]
pub struct GlobalConfig {
    pub admin: Pubkey,
    pub fanex_fee_wallet: Pubkey,
    pub graduation_authority: Pubkey,
    pub paused: bool,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct Curve {
    pub mint: Pubkey,
    pub creator: Pubkey,
    pub phase: CurvePhase,

    pub creator_target_lamports: u64,
    pub cpmm_seed_lamports: u64,
    pub creator_split_bps: u64,
    pub fanex_split_bps: u64,
    pub creator_cap_lamports: u64,
    pub fanex_cap_lamports: u64,

    pub virtual_sol_reserves: u64,
    pub virtual_token_reserves: u64,
    pub real_sol_reserves: u64,
    pub real_token_reserves: u64,
    pub migration_token_reserves: u64,

    pub creator_received_lamports: u64,
    pub fanex_received_lamports: u64,

    pub paused: bool,

    pub created_at: i64,
    pub graduated_at: i64,

    pub bump: u8,
    pub sol_vault_bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct SolVault {
    pub mint: Pubkey,
    pub bump: u8,
}

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

#[account]
#[derive(InitSpace)]
pub struct Distributor {
    pub mint: Pubkey,
    pub month: i64,
    pub merkle_root: [u8; 32],
    pub total_amount: u64,
    pub claimed_amount: u64,
    pub funder: Pubkey,
    pub expires_at: i64,
    pub bump: u8,
}

#[account]
pub struct DistributorV2 {
    pub mint: Pubkey,
    pub month: i64,
    pub merkle_root: [u8; 32],
    pub total_amount: u64,
    pub claimed_amount: u64,
    pub funder: Pubkey,
    pub expires_at: i64,
    pub bump: u8,
    pub leaf_count: u32,
    pub claimed: Vec<u8>,
}

impl DistributorV2 {
    pub const HEADER_SPACE: usize = 8 + 32 + 8 + 32 + 8 + 8 + 32 + 8 + 1 + 4 + 4;

    pub fn space(leaf_count: u32) -> usize {
        let leaves = leaf_count.min(crate::constants::MAX_DISTRIBUTOR_V2_LEAVES);
        Self::HEADER_SPACE + crate::validation::claim_registry_bytes(leaves)
    }
}

#[account]
#[derive(InitSpace)]
pub struct ClaimStatus {
    pub claimant: Pubkey,
    pub amount: u64,
    pub claimed_at: i64,
    pub rent_payer: Pubkey,
}
