use anchor_lang::prelude::*;

pub mod constants;
pub mod errors;
pub mod events;
pub mod instructions;
pub mod math;
pub mod payout;
pub mod state;
pub mod validation;

use instructions::*;

declare_id!("CDPwGWGWbAE8FL5f2oVdzfkTutrnsKJcAcgqWktC3dwZ");

#[program]
pub mod fanex_curve {
    use super::*;

    pub fn initialize_config(
        ctx: Context<InitializeConfig>,
        fanex_fee_wallet: Pubkey,
        graduation_authority: Pubkey,
    ) -> Result<()> {
        instructions::initialize_config::handler(ctx, fanex_fee_wallet, graduation_authority)
    }

    pub fn update_config(
        ctx: Context<UpdateConfig>,
        new_admin: Option<Pubkey>,
        new_fanex_fee_wallet: Option<Pubkey>,
        new_graduation_authority: Option<Pubkey>,
    ) -> Result<()> {
        instructions::admin::update_config(
            ctx,
            new_admin,
            new_fanex_fee_wallet,
            new_graduation_authority,
        )
    }

    pub fn set_paused(ctx: Context<UpdateConfig>, paused: bool) -> Result<()> {
        instructions::admin::set_paused(ctx, paused)
    }

    pub fn set_curve_paused(ctx: Context<SetCurvePaused>, paused: bool) -> Result<()> {
        instructions::admin::set_curve_paused(ctx, paused)
    }

    pub fn set_vesting_beneficiary(
        ctx: Context<SetVestingBeneficiary>,
        new_beneficiary: Pubkey,
    ) -> Result<()> {
        instructions::set_vesting_beneficiary::handler(ctx, new_beneficiary)
    }

    pub fn sync_curve_creator(ctx: Context<SyncCurveCreator>) -> Result<()> {
        instructions::sync_curve_creator::handler(ctx)
    }

    pub fn create_curve(ctx: Context<CreateCurve>, params: CreateCurveParams) -> Result<()> {
        instructions::create_curve::handler(ctx, params)
    }

    pub fn buy(ctx: Context<Buy>, sol_in: u64, min_tokens_out: u64) -> Result<()> {
        instructions::buy::handler(ctx, sol_in, min_tokens_out)
    }

    pub fn sell<'info>(
        ctx: Context<'_, '_, '_, 'info, Sell<'info>>,
        token_in: u64,
        min_sol_out: u64,
    ) -> Result<()> {
        instructions::sell::handler(ctx, token_in, min_sol_out)
    }

    pub fn graduate(ctx: Context<Graduate>) -> Result<()> {
        instructions::graduate::handler(ctx)
    }

    pub fn claim_vesting(ctx: Context<ClaimVesting>) -> Result<()> {
        instructions::claim_vesting::handler(ctx)
    }

    pub fn init_distributor(
        ctx: Context<InitDistributor>,
        month: i64,
        merkle_root: [u8; 32],
        total_amount: u64,
        expires_at: i64,
    ) -> Result<()> {
        instructions::dividends::init_distributor(ctx, month, merkle_root, total_amount, expires_at)
    }

    pub fn claim_dividend<'info>(
        ctx: Context<'_, '_, '_, 'info, ClaimDividend<'info>>,
        amount: u64,
        proof: Vec<[u8; 32]>,
    ) -> Result<()> {
        instructions::dividends::claim_dividend(ctx, amount, proof)
    }

    pub fn close_distributor(ctx: Context<CloseDistributor>) -> Result<()> {
        instructions::dividends::close_distributor(ctx)
    }

    pub fn close_claim_status(ctx: Context<CloseClaimStatus>) -> Result<()> {
        instructions::dividends::close_claim_status(ctx)
    }

    pub fn init_distributor_v2(
        ctx: Context<InitDistributorV2>,
        month: i64,
        merkle_root: [u8; 32],
        total_amount: u64,
        expires_at: i64,
        leaf_count: u32,
    ) -> Result<()> {
        instructions::dividends::init_distributor_v2(
            ctx,
            month,
            merkle_root,
            total_amount,
            expires_at,
            leaf_count,
        )
    }

    pub fn claim_dividend_v2<'info>(
        ctx: Context<'_, '_, '_, 'info, ClaimDividendV2<'info>>,
        index: u32,
        amount: u64,
        proof: Vec<[u8; 32]>,
    ) -> Result<()> {
        instructions::dividends::claim_dividend_v2(ctx, index, amount, proof)
    }

    pub fn close_distributor_v2(ctx: Context<CloseDistributorV2>) -> Result<()> {
        instructions::dividends::close_distributor_v2(ctx)
    }
}
