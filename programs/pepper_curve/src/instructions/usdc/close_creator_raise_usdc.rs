//! `close_creator_raise_usdc()` (the internal audit, 2026-10-29): `UsdcConfig.admin`, the upgrade
//! Squad's vault, closes a USDC curve's creator raise at what it has received. While a creator's cap
//! is open, every buy must carry the creator's USDC account and pays the creator's share into it
//! (`buy_exact_in`'s account 8); that account is the creator's own ATA, the one address a launch
//! names, so it cannot be pointed elsewhere: frozen by Circle, every buy owing the creator a share
//! fails, the cap never fills, and the curve never graduates. Closing the raise ends that: the cap
//! becomes what was received, so `creator_usdc_account_required` is false, the record is cleared as a
//! launch without a split leaves it, and every later buy's creator share goes to the curve. Nothing
//! else moves: the target stays, so the holders' raise scale stays at received / target, as a raise
//! the migration closes. Never paused (an admin instruction, as `update_usdc_config`); Bonding only.
//! Emits `CreatorRaiseClosedUsdc`.
use anchor_lang::prelude::*;

use crate::constants::*;
use crate::errors::FanexError;
use crate::events::CreatorRaiseClosedUsdc;
use crate::state::*;

#[derive(Accounts)]
pub struct CloseCreatorRaiseUsdc<'info> {
    /// `UsdcConfig.admin` (`Unauthorized` for any other key, the pauser included).
    #[account(address = usdc_config.admin @ FanexError::Unauthorized)]
    pub admin: Signer<'info>,

    #[account(seeds = [USDC_CONFIG_SEED], bump = usdc_config.bump)]
    pub usdc_config: Box<Account<'info, UsdcConfig>>,

    /// A `CurveUsdc` by its type and its seeds: a SOL `Curve` is refused by its discriminator (3002).
    #[account(mut, seeds = [CURVE_SEED, curve.mint.as_ref()], bump = curve.bump)]
    pub curve: Box<Account<'info, CurveUsdc>>,
}

/// The curve in Bonding (`NotBonding`: past it no buy pays the raise); then the creator's cap set to
/// what it has received and its USDC account cleared, both as a curve whose raise is over holds them.
pub fn handler(ctx: Context<CloseCreatorRaiseUsdc>) -> Result<()> {
    let curve = &mut ctx.accounts.curve;
    require!(curve.phase == PHASE_BONDING, FanexError::NotBonding);
    curve.creator_cap_usdc = curve.creator_received_usdc;
    curve.creator_usdc_account = Pubkey::default();
    emit!(CreatorRaiseClosedUsdc {
        mint: curve.mint,
        creator_cap_usdc: curve.creator_cap_usdc,
        by: ctx.accounts.admin.key(),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use anchor_lang::prelude::Pubkey;
    use anchor_lang::{Discriminator, InstructionData, ToAccountMetas};

    #[test]
    fn the_admin_signs_and_the_curve_alone_is_written() {
        let [admin, usdc_config, curve] = std::array::from_fn(|_| Pubkey::new_unique());
        let metas = crate::accounts::CloseCreatorRaiseUsdc { admin, usdc_config, curve }.to_account_metas(None);
        assert_eq!(metas.iter().map(|m| m.pubkey).collect::<Vec<_>>(), [admin, usdc_config, curve]);
        assert_eq!(metas.iter().filter(|m| m.is_signer).map(|m| m.pubkey).collect::<Vec<_>>(), [admin]);
        assert_eq!(metas.iter().filter(|m| m.is_writable).map(|m| m.pubkey).collect::<Vec<_>>(), [curve]);
    }

    #[test]
    fn it_takes_no_argument() {
        let data = crate::instruction::CloseCreatorRaiseUsdc {}.data();
        assert_eq!(data, crate::instruction::CloseCreatorRaiseUsdc::DISCRIMINATOR);
        assert_eq!(data.len(), 8);
    }
}
