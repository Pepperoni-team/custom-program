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
    pub is_buy: bool,
    pub sol_amount: u64,
    pub token_amount: u64,
    pub creator_fee: u64,
    pub fanex_fee: u64,
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

#[event]
pub struct DividendClaimed {
    pub mint: Pubkey,
    pub month: i64,
    pub claimant: Pubkey,
    pub amount: u64,
}

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

#[event]
pub struct CurveCreatorSynced {
    pub mint: Pubkey,
    pub previous: Pubkey,
    pub new_creator: Pubkey,
}
