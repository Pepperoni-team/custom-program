//! The vault's undo of a creator's own claim (Rick, 2026-11-09; upgrade N+3): while nothing of the
//! claimed vesting has vested, `UsdcConfig.admin` puts a market `claim_fan_led_usdc` handed over back
//! on the fan-led placeholder, its vesting's beneficiary and its curve's creator both, and the real
//! owner claims it again the usual way (the channel checked, their signature, the approver's).
//!
//! It answers a claim made through a stolen Google or X session, or with a stolen approver key,
//! without a wait before the claim. The vault can only undo: it never names an owner, so it can hand
//! no market to an address of its choosing. It reaches only a market `claim_fan_led_usdc` claimed
//! (`CurveUsdc.claimed_at`, which the admin's two steps never write), never a real creator's launch,
//! and only while the allocation has vested nothing, the window in which the thief has taken nothing
//! the chain holds.
use anchor_lang::prelude::*;
use anchor_spl::token::Mint;

use crate::constants::*;
use crate::errors::FanexError;
use crate::events::FanLedClaimRevoked;
use crate::instructions::set_vesting_beneficiary::fan_led_placeholder;
use crate::math::vested_amount;
use crate::state::*;

#[derive(Accounts)]
pub struct RevokeFanLedUsdc<'info> {
    /// `UsdcConfig.admin`: the upgrade Squad's vault.
    pub admin: Signer<'info>,

    #[account(
        seeds = [USDC_CONFIG_SEED],
        bump = usdc_config.bump,
        has_one = admin @ FanexError::Unauthorized,
    )]
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

/// Put a self-claimed market back on the placeholder: `Vesting.beneficiary`, `CurveUsdc.creator`
/// and `claimed_at` cleared, nothing else (the vesting's terms, released amount and start time stay;
/// the next claim restarts the clock). `ClaimNotRevocable` unless `revocable` holds.
pub fn handler(ctx: Context<RevokeFanLedUsdc>) -> Result<()> {
    let placeholder = fan_led_placeholder();
    let now = Clock::get()?.unix_timestamp;
    revocable(&ctx.accounts.vesting, &ctx.accounts.curve, placeholder, now)?;

    let previous = ctx.accounts.vesting.beneficiary;
    ctx.accounts.vesting.beneficiary = placeholder;
    ctx.accounts.curve.creator = placeholder;
    ctx.accounts.curve.claimed_at = 0;

    emit!(FanLedClaimRevoked {
        mint: ctx.accounts.mint.key(),
        previous,
        by: ctx.accounts.admin.key(),
    });
    Ok(())
}

/// The undo's rule, kept free of the runtime: a market `claim_fan_led_usdc` claimed (its
/// `claimed_at` set), still owned as that claim left it (the vesting's beneficiary and the curve's
/// creator the same key, not the placeholder), and nothing of its vesting released or vested now.
pub fn revocable(vesting: &Vesting, curve: &CurveUsdc, placeholder: Pubkey, now: i64) -> Result<()> {
    require!(curve.claimed_at != 0, FanexError::ClaimNotRevocable);
    require_keys_eq!(vesting.beneficiary, curve.creator, FanexError::ClaimNotRevocable);
    require_keys_neq!(curve.creator, placeholder, FanexError::ClaimNotRevocable);
    require!(vesting.released_amount == 0, FanexError::ClaimNotRevocable);
    let vested = vested_amount(
        vesting.total_amount,
        vesting.start_time,
        vesting.cliff_seconds,
        vesting.duration_seconds,
        now,
    );
    require!(vested == 0, FanexError::ClaimNotRevocable);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::{Space, ToAccountMetas};

    const DAY: i64 = 86_400;
    const CLAIMED: i64 = 1_800_000_000;

    fn claimed(owner: Pubkey) -> (Vesting, CurveUsdc) {
        let vesting = Vesting {
            mint: Pubkey::new_unique(),
            beneficiary: owner,
            total_amount: 10_000,
            released_amount: 0,
            start_time: CLAIMED,
            cliff_seconds: 90 * DAY,
            duration_seconds: 270 * DAY,
            bump: 255,
        };
        let mut curve = CurveUsdc::try_from_slice(&[0u8; CurveUsdc::INIT_SPACE]).unwrap();
        curve.creator = owner;
        curve.claimed_at = CLAIMED;
        (vesting, curve)
    }

    #[test]
    fn a_self_claimed_market_is_revocable_until_its_first_unit_vests() {
        let placeholder = fan_led_placeholder();
        let (vesting, curve) = claimed(Pubkey::new_unique());
        assert!(revocable(&vesting, &curve, placeholder, CLAIMED).is_ok());
        // The ramp starts from nothing at the cliff: still nothing vested there.
        assert!(revocable(&vesting, &curve, placeholder, CLAIMED + 90 * DAY).is_ok());
        let refused = error!(FanexError::ClaimNotRevocable);
        assert_eq!(revocable(&vesting, &curve, placeholder, CLAIMED + 91 * DAY).unwrap_err(), refused);
    }

    #[test]
    fn refuses_a_market_no_self_claim_handed_over_or_one_released_from() {
        let placeholder = fan_led_placeholder();
        let refused = error!(FanexError::ClaimNotRevocable);
        let owner = Pubkey::new_unique();

        let (vesting, mut never) = claimed(owner);
        never.claimed_at = 0;
        assert_eq!(revocable(&vesting, &never, placeholder, CLAIMED).unwrap_err(), refused);

        let (mut released, curve) = claimed(owner);
        released.released_amount = 1;
        assert_eq!(revocable(&released, &curve, placeholder, CLAIMED).unwrap_err(), refused);

        let (mut moved, curve) = claimed(owner);
        moved.beneficiary = Pubkey::new_unique();
        assert_eq!(revocable(&moved, &curve, placeholder, CLAIMED).unwrap_err(), refused);

        let (mut vesting, mut back) = claimed(placeholder);
        vesting.beneficiary = placeholder;
        back.creator = placeholder;
        assert_eq!(revocable(&vesting, &back, placeholder, CLAIMED).unwrap_err(), refused);
    }

    #[test]
    fn the_admin_alone_signs_and_the_vesting_and_the_curve_are_written() {
        let [admin, usdc_config, mint, vesting, curve] = std::array::from_fn(|_| Pubkey::new_unique());
        let metas = crate::accounts::RevokeFanLedUsdc { admin, usdc_config, mint, vesting, curve }.to_account_metas(None);

        assert_eq!(metas.iter().map(|m| m.pubkey).collect::<Vec<_>>(), [admin, usdc_config, mint, vesting, curve]);
        assert_eq!(metas.iter().filter(|m| m.is_signer).map(|m| m.pubkey).collect::<Vec<_>>(), [admin]);
        assert_eq!(metas.iter().filter(|m| m.is_writable).map(|m| m.pubkey).collect::<Vec<_>>(), [vesting, curve]);
    }
}
