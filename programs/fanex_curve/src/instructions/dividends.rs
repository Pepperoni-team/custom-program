use anchor_lang::prelude::*;
use anchor_lang::system_program;

use crate::constants::*;
use crate::errors::FanexError;
use crate::events::{ClaimStatusClosed, DistributorFunded, DividendClaimed};
use crate::math::{dividend_leaf, dividend_leaf_v2, verify_merkle_proof};
use crate::payout::pay_out;
use crate::state::*;
use crate::validation::{
    claim_registry_bytes, expiry_in_range, leaf_bit, leaf_count_in_range, may_fund_distributor_v1,
    may_fund_distributor_v2,
};

#[derive(Accounts)]
#[instruction(month: i64)]
pub struct InitDistributor<'info> {
    #[account(mut)]
    pub funder: Signer<'info>,

    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, GlobalConfig>,

    /// CHECK: the mint the distribution is for (bound into the PDA + state only).
    pub mint: UncheckedAccount<'info>,

    #[account(
        seeds = [CURVE_SEED, mint.key().as_ref()],
        bump = curve.bump,
    )]
    pub curve: Account<'info, Curve>,

    #[account(
        init,
        payer = funder,
        space = 8 + Distributor::INIT_SPACE,
        seeds = [DISTRIBUTOR_SEED, mint.key().as_ref(), &month.to_le_bytes()],
        bump
    )]
    pub distributor: Account<'info, Distributor>,

    pub system_program: Program<'info, System>,
}

pub fn init_distributor(
    ctx: Context<InitDistributor>,
    month: i64,
    merkle_root: [u8; 32],
    total_amount: u64,
    expires_at: i64,
) -> Result<()> {
    require!(!ctx.accounts.config.paused, FanexError::GloballyPaused);
    require!(total_amount > 0, FanexError::ZeroAmount);

    let funder = ctx.accounts.funder.key();
    require!(
        may_fund_distributor_v1(&funder, &ctx.accounts.config.fanex_fee_wallet),
        FanexError::Unauthorized
    );
    require!(expiry_in_range(expires_at, Clock::get()?.unix_timestamp), FanexError::ExpiryTooFar);

    let distributor = &mut ctx.accounts.distributor;
    distributor.mint = ctx.accounts.mint.key();
    distributor.month = month;
    distributor.merkle_root = merkle_root;
    distributor.total_amount = total_amount;
    distributor.claimed_amount = 0;
    distributor.funder = ctx.accounts.funder.key();
    distributor.expires_at = expires_at;
    distributor.bump = ctx.bumps.distributor;

    system_program::transfer(
        CpiContext::new(
            ctx.accounts.system_program.to_account_info(),
            system_program::Transfer {
                from: ctx.accounts.funder.to_account_info(),
                to: ctx.accounts.distributor.to_account_info(),
            },
        ),
        total_amount,
    )?;

    emit!(DistributorFunded {
        mint: ctx.accounts.distributor.mint,
        month,
        total_amount,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct ClaimDividend<'info> {
    #[account(mut)]
    pub claimant: Signer<'info>,

    #[account(mut)]
    pub rent_payer: Signer<'info>,

    #[account(mut)]
    pub distributor: Account<'info, Distributor>,

    #[account(
        init,
        payer = rent_payer,
        space = 8 + ClaimStatus::INIT_SPACE,
        seeds = [CLAIM_SEED, distributor.key().as_ref(), claimant.key().as_ref()],
        bump
    )]
    pub claim_status: Account<'info, ClaimStatus>,

    pub system_program: Program<'info, System>,
}

pub fn claim_dividend<'info>(
    ctx: Context<'_, '_, '_, 'info, ClaimDividend<'info>>,
    amount: u64,
    proof: Vec<[u8; 32]>,
) -> Result<()> {
    require!(amount > 0, FanexError::ZeroAmount);

    let leaf = dividend_leaf(&ctx.accounts.claimant.key(), amount);
    require!(
        verify_merkle_proof(&proof, ctx.accounts.distributor.merkle_root, leaf),
        FanexError::InvalidProof
    );

    let new_claimed = ctx
        .accounts
        .distributor
        .claimed_amount
        .checked_add(amount)
        .ok_or(FanexError::MathOverflow)?;
    require!(
        new_claimed <= ctx.accounts.distributor.total_amount,
        FanexError::ExceedsFundedTotal
    );

    pay_out(
        &ctx.accounts.distributor.to_account_info(),
        &ctx.accounts.claimant.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        ctx.remaining_accounts,
        amount,
    )?;

    ctx.accounts.distributor.claimed_amount = new_claimed;

    let claim_status = &mut ctx.accounts.claim_status;
    claim_status.claimant = ctx.accounts.claimant.key();
    claim_status.amount = amount;
    claim_status.claimed_at = Clock::get()?.unix_timestamp;
    claim_status.rent_payer = ctx.accounts.rent_payer.key();

    emit!(DividendClaimed {
        mint: ctx.accounts.distributor.mint,
        month: ctx.accounts.distributor.month,
        claimant: ctx.accounts.claimant.key(),
        amount,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct CloseDistributor<'info> {
    #[account(
        mut,
        close = funder,
        has_one = funder @ FanexError::Unauthorized,
    )]
    pub distributor: Account<'info, Distributor>,

    #[account(mut)]
    pub funder: Signer<'info>,
}

pub fn close_distributor(ctx: Context<CloseDistributor>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let d = &ctx.accounts.distributor;
    require!(
        crate::validation::may_close_distributor(
            d.claimed_amount,
            d.total_amount,
            now,
            d.expires_at
        ),
        FanexError::NotExpired
    );
    Ok(())
}

#[derive(Accounts)]
pub struct CloseClaimStatus<'info> {
    /// CHECK: parsed by hand, deliberately. Two formats exist on chain (56-byte
    /// pre-upgrade, 88-byte with `rent_payer`) and `Account<ClaimStatus>` can
    /// only read the new one; owner, discriminator and seeds are all verified
    /// in the handler.
    #[account(mut)]
    pub claim_status: UncheckedAccount<'info>,

    /// CHECK: the distributor ADDRESS this ClaimStatus was seeded on. It must be
    /// DEAD (its account closed): while the distributor lives, the ClaimStatus
    /// is the double-claim guard and closing it would re-open the claim.
    pub distributor: UncheckedAccount<'info>,

    /// CHECK: receives the rent. Must equal the recorded payer — `rent_payer`
    /// on the new format, `claimant` on the legacy one — so a permissionless
    /// caller can trigger the refund but never redirect it.
    #[account(mut)]
    pub rent_recipient: UncheckedAccount<'info>,
}

pub fn close_claim_status(ctx: Context<CloseClaimStatus>) -> Result<()> {
    let cs = &ctx.accounts.claim_status;
    require!(cs.owner == &crate::ID, FanexError::ClaimStatusFormat);

    let data = cs.try_borrow_data()?;
    require!(
        data.len() >= 8 && data[..8] == *ClaimStatus::DISCRIMINATOR,
        FanexError::ClaimStatusFormat
    );
    let refund_range = crate::validation::claim_status_refund_range(data.len())
        .ok_or(FanexError::ClaimStatusFormat)?;

    let claimant = Pubkey::try_from(&data[8..40]).map_err(|_| FanexError::ClaimStatusFormat)?;
    let refund_to =
        Pubkey::try_from(&data[refund_range]).map_err(|_| FanexError::ClaimStatusFormat)?;
    drop(data);

    let (expected, _bump) = Pubkey::find_program_address(
        &[
            CLAIM_SEED,
            ctx.accounts.distributor.key().as_ref(),
            claimant.as_ref(),
        ],
        &crate::ID,
    );
    require!(expected == cs.key(), FanexError::AccountMismatch);

    let dist = &ctx.accounts.distributor;
    require!(
        !(dist.owner == &crate::ID && !dist.data_is_empty()),
        FanexError::DistributorStillOpen
    );

    require!(
        ctx.accounts.rent_recipient.key() == refund_to,
        FanexError::WrongRentRecipient
    );

    let lamports = cs.lamports();
    {
        let recipient_ai = ctx.accounts.rent_recipient.to_account_info();
        let cs_ai = cs.to_account_info();
        **recipient_ai.try_borrow_mut_lamports()? = recipient_ai
            .lamports()
            .checked_add(lamports)
            .ok_or(FanexError::MathOverflow)?;
        **cs_ai.try_borrow_mut_lamports()? = 0;
        cs_ai.try_borrow_mut_data()?[..8].fill(0);
    }

    emit!(ClaimStatusClosed {
        claim_status: cs.key(),
        refunded_to: refund_to,
        lamports,
    });

    Ok(())
}

#[derive(Accounts)]
#[instruction(month: i64, merkle_root: [u8; 32], total_amount: u64, expires_at: i64, leaf_count: u32)]
pub struct InitDistributorV2<'info> {
    #[account(mut)]
    pub funder: Signer<'info>,

    #[account(address = config.fanex_fee_wallet @ FanexError::Unauthorized)]
    pub approver: Signer<'info>,

    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, GlobalConfig>,

    /// CHECK: the mint the distribution is for (bound into the PDA + state only).
    pub mint: UncheckedAccount<'info>,

    #[account(
        seeds = [CURVE_SEED, mint.key().as_ref()],
        bump = curve.bump,
    )]
    pub curve: Account<'info, Curve>,

    #[account(
        init,
        payer = funder,
        space = DistributorV2::space(leaf_count),
        seeds = [DISTRIBUTOR_SEED, mint.key().as_ref(), &month.to_le_bytes()],
        bump
    )]
    pub distributor: Account<'info, DistributorV2>,

    pub system_program: Program<'info, System>,
}

pub fn init_distributor_v2(
    ctx: Context<InitDistributorV2>,
    month: i64,
    merkle_root: [u8; 32],
    total_amount: u64,
    expires_at: i64,
    leaf_count: u32,
) -> Result<()> {
    require!(!ctx.accounts.config.paused, FanexError::GloballyPaused);
    require!(total_amount > 0, FanexError::ZeroAmount);
    require!(leaf_count_in_range(leaf_count), FanexError::InvalidLeafCount);

    let funder = ctx.accounts.funder.key();
    require!(
        may_fund_distributor_v2(
            &funder,
            &ctx.accounts.config.fanex_fee_wallet,
            &ctx.accounts.curve.creator
        ),
        FanexError::Unauthorized
    );
    require!(expiry_in_range(expires_at, Clock::get()?.unix_timestamp), FanexError::ExpiryTooFar);

    let distributor = &mut ctx.accounts.distributor;
    distributor.mint = ctx.accounts.mint.key();
    distributor.month = month;
    distributor.merkle_root = merkle_root;
    distributor.total_amount = total_amount;
    distributor.claimed_amount = 0;
    distributor.funder = funder;
    distributor.expires_at = expires_at;
    distributor.bump = ctx.bumps.distributor;
    distributor.leaf_count = leaf_count;
    distributor.claimed = vec![0u8; claim_registry_bytes(leaf_count)];

    system_program::transfer(
        CpiContext::new(
            ctx.accounts.system_program.to_account_info(),
            system_program::Transfer {
                from: ctx.accounts.funder.to_account_info(),
                to: ctx.accounts.distributor.to_account_info(),
            },
        ),
        total_amount,
    )?;

    emit!(DistributorFunded {
        mint: ctx.accounts.distributor.mint,
        month,
        total_amount,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct ClaimDividendV2<'info> {
    #[account(mut)]
    pub claimant: Signer<'info>,

    #[account(mut)]
    pub distributor: Account<'info, DistributorV2>,

    pub system_program: Program<'info, System>,
}

pub fn claim_dividend_v2<'info>(
    ctx: Context<'_, '_, '_, 'info, ClaimDividendV2<'info>>,
    index: u32,
    amount: u64,
    proof: Vec<[u8; 32]>,
) -> Result<()> {
    require!(amount > 0, FanexError::ZeroAmount);

    let distributor = &ctx.accounts.distributor;
    require!(index < distributor.leaf_count, FanexError::InvalidLeafIndex);

    let leaf = dividend_leaf_v2(index, &ctx.accounts.claimant.key(), amount);
    require!(
        verify_merkle_proof(&proof, distributor.merkle_root, leaf),
        FanexError::InvalidProof
    );

    let (byte, mask) = leaf_bit(index);
    let registry_byte = *distributor
        .claimed
        .get(byte)
        .ok_or(FanexError::InvalidLeafIndex)?;
    require!(registry_byte & mask == 0, FanexError::AlreadyClaimed);

    let new_claimed = distributor
        .claimed_amount
        .checked_add(amount)
        .ok_or(FanexError::MathOverflow)?;
    require!(
        new_claimed <= distributor.total_amount,
        FanexError::ExceedsFundedTotal
    );

    pay_out(
        &ctx.accounts.distributor.to_account_info(),
        &ctx.accounts.claimant.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        ctx.remaining_accounts,
        amount,
    )?;

    let distributor = &mut ctx.accounts.distributor;
    distributor.claimed_amount = new_claimed;
    distributor.claimed[byte] |= mask;

    emit!(DividendClaimed {
        mint: distributor.mint,
        month: distributor.month,
        claimant: ctx.accounts.claimant.key(),
        amount,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct CloseDistributorV2<'info> {
    #[account(
        mut,
        close = funder,
        has_one = funder @ FanexError::Unauthorized,
    )]
    pub distributor: Account<'info, DistributorV2>,

    #[account(mut)]
    pub funder: Signer<'info>,
}

pub fn close_distributor_v2(ctx: Context<CloseDistributorV2>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let d = &ctx.accounts.distributor;
    require!(
        crate::validation::may_close_distributor(
            d.claimed_amount,
            d.total_amount,
            now,
            d.expires_at
        ),
        FanexError::NotExpired
    );
    Ok(())
}
