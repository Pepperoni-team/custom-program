use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};
use pepper_curve_math::vested_amount;

use crate::constants::*;
use crate::errors::FanexError;
use crate::events::VestingReleased;
use crate::state::*;
use crate::transfer::transfer_checked;
use crate::validation::usdc_vesting_claim_allowed;

/// Today's `ClaimVesting` on a `CurveUsdc` (USDC curve design §10.3.7). Two differences: the
/// beneficiary's token account must already exist (no `init_if_needed`, so no associated-token or
/// system program, and the beneficiary is no fee payer: the sponsor opens the account in the same
/// transaction and the creator spends no SOL), and the tokens move by `transfer_checked`.
#[derive(Accounts)]
pub struct ClaimVestingUsdc<'info> {
    pub beneficiary: Signer<'info>,

    /// A `CurveUsdc` by its type: a SOL `Curve` here is refused by its discriminator (3002), as
    /// the SOL `claim_vesting` refuses this one.
    #[account(
        seeds = [CURVE_SEED, mint.key().as_ref()],
        bump = curve.bump,
        has_one = mint @ FanexError::AccountMismatch,
    )]
    pub curve: Box<Account<'info, CurveUsdc>>,

    pub mint: Box<Account<'info, Mint>>,

    #[account(
        mut,
        seeds = [VESTING_SEED, mint.key().as_ref()],
        bump = vesting.bump,
        has_one = mint @ FanexError::AccountMismatch,
        constraint = vesting.beneficiary == beneficiary.key() @ FanexError::Unauthorized,
    )]
    pub vesting: Box<Account<'info, Vesting>>,

    #[account(
        mut,
        seeds = [VESTING_VAULT_SEED, mint.key().as_ref()],
        bump,
        token::mint = mint,
        token::authority = curve,
    )]
    pub vesting_vault: Box<Account<'info, TokenAccount>>,

    /// The beneficiary's associated token account of the mint, still owned by the beneficiary;
    /// missing, the claim is refused (`AccountNotInitialized`), never opened at their expense.
    #[account(
        mut,
        associated_token::mint = mint,
        associated_token::authority = beneficiary,
    )]
    pub beneficiary_token_account: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

/// Pays `vested_amount(total, start, cliff, duration, now) - released` from the vesting vault to the
/// beneficiary, signed by the curve. A curve created from `VESTING_LOCKED_UNTIL_GRADUATION_FROM`,
/// and a migrated one whenever it launched (the internal audit, 2026-10-29), releases nothing before
/// its pool exists, PoolCreated as well as Graduated (Rick, 2026-10-22): sold into the curve, its
/// vested tokens would be paid from the holders' reserve.
pub fn handler(ctx: Context<ClaimVestingUsdc>) -> Result<()> {
    let curve = &ctx.accounts.curve;
    require!(
        usdc_vesting_claim_allowed(curve.created_at, curve.phase, curve.migrated),
        FanexError::VestingLockedUntilGraduation
    );

    let now = Clock::get()?.unix_timestamp;
    let vesting = &ctx.accounts.vesting;
    let vested = vested_amount(
        vesting.total_amount,
        vesting.start_time,
        vesting.cliff_seconds,
        vesting.duration_seconds,
        now,
    );
    let releasable = vested
        .checked_sub(vesting.released_amount)
        .ok_or(FanexError::MathOverflow)?;
    require!(releasable > 0, FanexError::NothingToRelease);

    let mint_key = ctx.accounts.mint.key();
    let curve_bump = ctx.accounts.curve.bump;
    let signer_seeds: &[&[&[u8]]] = &[&[CURVE_SEED, mint_key.as_ref(), &[curve_bump]]];

    transfer_checked(
        &ctx.accounts.token_program.to_account_info(),
        &ctx.accounts.mint.to_account_info(),
        &ctx.accounts.vesting_vault.to_account_info(),
        &ctx.accounts.beneficiary_token_account.to_account_info(),
        &ctx.accounts.curve.to_account_info(),
        releasable,
        TOKEN_DECIMALS,
        signer_seeds,
    )?;

    ctx.accounts.vesting.released_amount = ctx
        .accounts
        .vesting
        .released_amount
        .checked_add(releasable)
        .ok_or(FanexError::MathOverflow)?;

    emit!(VestingReleased {
        mint: mint_key,
        beneficiary: ctx.accounts.beneficiary.key(),
        amount: releasable,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use anchor_lang::prelude::Pubkey;
    use anchor_lang::ToAccountMetas;

    #[test]
    fn the_claim_has_no_payer_and_no_program_that_could_open_an_account() {
        let [beneficiary, curve, mint, vesting, vesting_vault, token_account] =
            std::array::from_fn(|_| Pubkey::new_unique());
        let metas = crate::accounts::ClaimVestingUsdc {
            beneficiary,
            curve,
            mint,
            vesting,
            vesting_vault,
            beneficiary_token_account: token_account,
            token_program: anchor_spl::token::ID,
        }
        .to_account_metas(None);

        let keys: Vec<Pubkey> = metas.iter().map(|m| m.pubkey).collect();
        assert_eq!(
            keys,
            [beneficiary, curve, mint, vesting, vesting_vault, token_account, anchor_spl::token::ID]
        );
        // The beneficiary signs and pays nothing: the sponsor opens its account beforehand.
        assert!(metas[0].is_signer && !metas[0].is_writable);
        assert_eq!(metas.iter().filter(|m| m.is_signer).count(), 1);
        let writable: Vec<Pubkey> = metas.iter().filter(|m| m.is_writable).map(|m| m.pubkey).collect();
        assert_eq!(writable, [vesting, vesting_vault, token_account]);
    }
}
