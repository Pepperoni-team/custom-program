//! The creator's own claim of a fan-led USDC market (Rick, 2026-11-05; upgrade N+3): the two
//! steps of the admin's hand-over, `set_vesting_beneficiary` and `sync_curve_creator_usdc`, in one
//! instruction the creator signs, co-signed by `UsdcConfig.claim_approver`, the server's key, once
//! the channel check has passed. No vault proposal waits between the check and the claim.
//!
//! It widens nothing the admin path did not already allow: the same placeholder-only rules
//! (`validate_beneficiary_reassignable`, `validate_curve_creator_syncable`), so a market a creator
//! already holds is never moved, and the new owner is the signer, never an address passed in, so
//! an allocation never goes to a key nobody holds. A stolen approver key reaches the unclaimed
//! fan-led markets alone, as the admin key does today; the admin answers it with
//! `set_claim_approver` (all-zero turns the claim off).
use anchor_lang::prelude::*;
use anchor_spl::token::Mint;

use crate::constants::*;
use crate::errors::FanexError;
use crate::events::{CurveCreatorSynced, VestingBeneficiarySet};
use crate::instructions::set_vesting_beneficiary::{fan_led_placeholder, hand_over};
use crate::state::*;
use crate::validation::{validate_beneficiary_reassignable, validate_curve_creator_syncable};

#[derive(Accounts)]
pub struct ClaimFanLedUsdc<'info> {
    /// The creator who proved they own the channel: the vesting's and the curve's new owner.
    pub creator: Signer<'info>,

    /// `UsdcConfig.claim_approver`, checked in the handler: `ClaimApproverUnset` while it is
    /// all-zero, `Unauthorized` for any other key.
    pub approver: Signer<'info>,

    #[account(seeds = [USDC_CONFIG_SEED], bump = usdc_config.bump)]
    pub usdc_config: Box<Account<'info, UsdcConfig>>,

    pub mint: Box<Account<'info, Mint>>,

    #[account(
        mut,
        seeds = [VESTING_SEED, mint.key().as_ref()],
        bump = vesting.bump,
        has_one = mint @ FanexError::AccountMismatch,
    )]
    pub vesting: Box<Account<'info, Vesting>>,

    /// A `CurveUsdc` by its type: a SOL `Curve` is refused by its discriminator (3002).
    #[account(
        mut,
        seeds = [CURVE_SEED, mint.key().as_ref()],
        bump = curve.bump,
        has_one = mint @ FanexError::AccountMismatch,
    )]
    pub curve: Box<Account<'info, CurveUsdc>>,
}

/// Hand an unclaimed fan-led market to its creator: the vesting's beneficiary and its clock
/// (`hand_over`), then `CurveUsdc.creator` and `claimed_at` (what lets the vault undo the claim
/// while nothing has vested, `revoke_fan_led_usdc`), each from the placeholder only, both or neither. Emits
/// the two events the admin's two steps emit, so every reader of those sees a claim either way.
pub fn handler(ctx: Context<ClaimFanLedUsdc>) -> Result<()> {
    approver_valid(ctx.accounts.usdc_config.claim_approver, ctx.accounts.approver.key())?;

    let creator = ctx.accounts.creator.key();
    let mint = ctx.accounts.mint.key();
    let placeholder = fan_led_placeholder();
    let previous_beneficiary = ctx.accounts.vesting.beneficiary;
    let previous_creator = ctx.accounts.curve.creator;

    validate_beneficiary_reassignable(previous_beneficiary, placeholder)?;
    validate_curve_creator_syncable(previous_creator, creator, placeholder)?;

    let now = Clock::get()?.unix_timestamp;
    hand_over(&mut ctx.accounts.vesting, creator, now);
    ctx.accounts.curve.creator = creator;
    // The mark `revoke_fan_led_usdc` reads: this market changed hands by a self-claim.
    ctx.accounts.curve.claimed_at = now;

    emit!(VestingBeneficiarySet {
        mint,
        previous: previous_beneficiary,
        new_beneficiary: creator,
    });
    emit!(CurveCreatorSynced {
        mint,
        previous: previous_creator,
        new_creator: creator,
    });
    Ok(())
}

/// The approver's rule, kept free of the runtime: none set refuses every claim, and only the key
/// set approves one.
pub fn approver_valid(configured: Pubkey, signer: Pubkey) -> Result<()> {
    require_keys_neq!(configured, Pubkey::default(), FanexError::ClaimApproverUnset);
    require_keys_eq!(signer, configured, FanexError::Unauthorized);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::ToAccountMetas;

    #[test]
    fn no_approver_set_refuses_every_claim() {
        let err = approver_valid(Pubkey::default(), Pubkey::new_unique()).unwrap_err();
        assert_eq!(err, error!(FanexError::ClaimApproverUnset));
        let err = approver_valid(Pubkey::default(), Pubkey::default()).unwrap_err();
        assert_eq!(err, error!(FanexError::ClaimApproverUnset));
    }

    #[test]
    fn only_the_approver_set_approves() {
        let approver = Pubkey::new_unique();
        assert!(approver_valid(approver, approver).is_ok());
        let err = approver_valid(approver, Pubkey::new_unique()).unwrap_err();
        assert_eq!(err, error!(FanexError::Unauthorized));
    }

    #[test]
    fn the_creator_and_the_approver_sign_and_the_vesting_and_the_curve_are_written() {
        let [creator, approver, usdc_config, mint, vesting, curve] = std::array::from_fn(|_| Pubkey::new_unique());
        let metas = crate::accounts::ClaimFanLedUsdc { creator, approver, usdc_config, mint, vesting, curve }
            .to_account_metas(None);

        assert_eq!(metas.iter().map(|m| m.pubkey).collect::<Vec<_>>(), [creator, approver, usdc_config, mint, vesting, curve]);
        assert_eq!(metas.iter().filter(|m| m.is_signer).map(|m| m.pubkey).collect::<Vec<_>>(), [creator, approver]);
        assert_eq!(metas.iter().filter(|m| m.is_writable).map(|m| m.pubkey).collect::<Vec<_>>(), [vesting, curve]);
    }
}
