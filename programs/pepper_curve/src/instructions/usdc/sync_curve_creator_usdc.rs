use anchor_lang::prelude::*;
use anchor_spl::token::Mint;

use crate::constants::*;
use crate::errors::FanexError;
use crate::events::CurveCreatorSynced;
use crate::instructions::set_vesting_beneficiary::fan_led_placeholder;
use crate::state::*;
use crate::validation::validate_curve_creator_syncable;

/// Today's `SyncCurveCreator` on a `CurveUsdc` (USDC curve design §10.3.8), signed as today by
/// `GlobalConfig.admin`, never by `UsdcConfig.admin`: one key serves the fan-led actions of both
/// kinds of curve, and `set_vesting_beneficiary`, which reads only the `Vesting`, serves both
/// unchanged.
#[derive(Accounts)]
pub struct SyncCurveCreatorUsdc<'info> {
    #[account(
        seeds = [CONFIG_SEED],
        bump = global_config.bump,
        has_one = admin @ FanexError::Unauthorized
    )]
    pub global_config: Box<Account<'info, GlobalConfig>>,

    pub admin: Signer<'info>,

    pub mint: Box<Account<'info, Mint>>,

    /// Read-only: the SOURCE of the value written. Never modified here.
    #[account(
        seeds = [VESTING_SEED, mint.key().as_ref()],
        bump = vesting.bump,
        has_one = mint @ FanexError::AccountMismatch,
    )]
    pub vesting: Box<Account<'info, Vesting>>,

    /// A `CurveUsdc` by its type: a SOL `Curve` here is refused by its discriminator (3002), as
    /// the SOL `sync_curve_creator` refuses this one.
    #[account(
        mut,
        seeds = [CURVE_SEED, mint.key().as_ref()],
        bump = curve.bump,
        has_one = mint @ FanexError::AccountMismatch,
    )]
    pub curve: Box<Account<'info, CurveUsdc>>,
}

/// Point `CurveUsdc.creator` at the creator who claimed a fan-led token, once: as the SOL sync, it
/// takes no address, writes only the vesting beneficiary `set_vesting_beneficiary` already moved
/// off the placeholder, and refuses once the creator has left it (`CurveCreatorNotSyncable`). It
/// never meets a creator USDC account: a curve whose creator is the placeholder takes no split
/// (`PlaceholderCannotRaise`), so a curve that syncs has none to move.
pub fn handler(ctx: Context<SyncCurveCreatorUsdc>) -> Result<()> {
    let new_creator = ctx.accounts.vesting.beneficiary;
    let curve = &mut ctx.accounts.curve;
    let previous = curve.creator;

    validate_curve_creator_syncable(previous, new_creator, fan_led_placeholder())?;

    curve.creator = new_creator;

    emit!(CurveCreatorSynced {
        mint: ctx.accounts.mint.key(),
        previous,
        new_creator,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use anchor_lang::prelude::Pubkey;
    use anchor_lang::ToAccountMetas;

    #[test]
    fn the_sync_takes_the_sol_syncs_accounts_in_its_order_and_writes_the_curve_alone() {
        let [global_config, admin, mint, vesting, curve] = std::array::from_fn(|_| Pubkey::new_unique());
        let usdc = crate::accounts::SyncCurveCreatorUsdc { global_config, admin, mint, vesting, curve }
            .to_account_metas(None);
        let sol = crate::accounts::SyncCurveCreator { config: global_config, admin, mint, vesting, curve }
            .to_account_metas(None);

        assert_eq!(usdc, sol);
        assert_eq!(usdc.iter().map(|m| m.pubkey).collect::<Vec<_>>(), [global_config, admin, mint, vesting, curve]);
        assert_eq!(usdc.iter().filter(|m| m.is_signer).map(|m| m.pubkey).collect::<Vec<_>>(), [admin]);
        assert_eq!(usdc.iter().filter(|m| m.is_writable).map(|m| m.pubkey).collect::<Vec<_>>(), [curve]);
    }
}
