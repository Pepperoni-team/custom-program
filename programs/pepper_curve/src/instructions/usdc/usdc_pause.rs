//! The USDC pause (USDC curve design §10.3.3; Rick, 2026-10-23: «si chiamerà pausa USDC»): one key,
//! `UsdcConfig.pauser`, pauses the USDC side and the SOL side, and only an admin unpauses.
//!
//! **The USDC side**: `UsdcConfig.paused`, its kill-switch, and one curve's `CurveUsdc.paused`. The
//! pauser, a hot key, or the admin may pause; only the admin unpauses (`NotPauser` otherwise,
//! `validation::may_set_usdc_pause`), so a stolen pauser key can stop trading but never restart it.
//! Neither instruction reads a pause flag: a paused side still takes its own unpause. Each emits
//! `UsdcPauseSet`, with who wrote it.
//!
//! **The SOL side** (Rick, 2026-10-23: «la pausa deve essere immediata»). Once `GlobalConfig.admin`
//! is the upgrade Squad's vault, `set_paused` and `set_curve_paused`, which it signs, execute no
//! earlier than 48 hours after their approval, and `GlobalConfig` (106 bytes, no spare byte) has no
//! room for a pauser of its own. So the same pauser pauses the SOL side too: `usdc_pause_sol` writes
//! `GlobalConfig.paused = true`, `usdc_pause_sol_curve` one `Curve.paused = true`.
//! - **Pause only.** Neither takes an argument, so neither can unpause: unpausing stays
//!   `set_paused(false)` and `set_curve_paused(false)`, `GlobalConfig.admin`'s (`admin.rs`,
//!   unchanged). A stolen pauser key can at most stop SOL trading until the admin unpauses.
//! - **The pauser alone signs** (`has_one = pauser`, `Unauthorized` for any other key, as every
//!   single-signer SOL instruction refuses; `NotPauser` stays the USDC side's rule, whose message
//!   names the admin as a pauser). The admins are refused too: `GlobalConfig.admin` already pauses
//!   and unpauses through `set_paused` and `set_curve_paused`, and a second path to the same flag
//!   would add a key to reason about and nothing else. ⚠️ `UsdcConfig.admin` signs no SOL-side
//!   instruction, but it names the pauser (`update_usdc_config`), so it appoints the key that pauses
//!   the SOL side, never one that unpauses it (the internal audit, 2026-10-29): while the two admins
//!   are different keys, a stolen `UsdcConfig.admin` can stop SOL trading until `GlobalConfig.admin`
//!   unpauses it. Both admins end on the upgrade Squad's vault (MULTISIG.md §6).
//! - Neither reads a pause flag, so a paused side, either one, never stops the pauser; writing
//!   `true` onto a flag already `true` is no error and emits again. Each emits `UsdcPauseSolSet`,
//!   `curve` all-zero for the whole SOL side.
//!
//! **The raise** (the product owner, 2026-11-05: the kill switch pauses everything). `RaiseConfig`'s
//! own pause, `set_raise_paused`, is `RaiseConfig.admin`'s, the upgrade Squad's vault on mainnet, 48
//! hours behind its time lock; and its exit pool trades from the first release. So the same pauser
//! pauses it too: `usdc_pause_raise` writes `RaiseConfig.paused = true`, under the SOL pauses' rules:
//! no argument, so no unpause (that stays `set_raise_paused(false)`, the raise admin's); the pauser
//! alone signs (`Unauthorized` otherwise, the raise admin included, which pauses through its own
//! instruction); no flag read, `true` onto `true` no error. It stops what the raise's pause stops
//! (`create_raise`, `buy_quotas`, `declare_month`, the attester's `redeclare_month` and the two pool
//! trades) and never a refund, a payment, a settlement, a claim, a sweep or a close. It emits nothing,
//! as `set_raise_paused` emits nothing: the transaction names its signer.
//!
//! The flags gate the handlers that check them, listed in `lib/solana/curve-admin-rules.ts`
//! (`USDC_GLOBAL_PAUSE_GATES` and `USDC_CURVE_PAUSE_GATES` for the USDC side, `GLOBAL_PAUSE_GATES`
//! and `CURVE_PAUSE_GATES` for the SOL one), held to the source by its test; the four instructions
//! here sit in its `USDC_NEVER_PAUSED`, since no flag gates them. Each side's handlers read only
//! that side's flags: `GlobalConfig.paused` stops no USDC curve and `UsdcConfig.paused` no SOL curve,
//! so the kill switch sends both (`set_usdc_paused(true)` and `usdc_pause_sol`).
use anchor_lang::prelude::*;

use crate::constants::*;
use crate::errors::FanexError;
use crate::events::{UsdcPauseSet, UsdcPauseSolSet};
use crate::raise::constants::RAISE_CONFIG_SEED;
use crate::raise::state::RaiseConfig;
use crate::state::*;
use crate::validation::may_set_usdc_pause;

// ── The USDC side ────────────────────────────────────────────────────────────

#[derive(Accounts)]
pub struct SetUsdcPaused<'info> {
    /// The pauser or the admin: the handler decides which may write what.
    pub authority: Signer<'info>,

    #[account(mut, seeds = [USDC_CONFIG_SEED], bump = usdc_config.bump)]
    pub usdc_config: Box<Account<'info, UsdcConfig>>,
}

/// Write the USDC side's kill-switch.
pub fn set_usdc_paused(ctx: Context<SetUsdcPaused>, paused: bool) -> Result<()> {
    let by = ctx.accounts.authority.key();
    let config = &mut ctx.accounts.usdc_config;
    require!(
        may_set_usdc_pause(&by, &config.admin, &config.pauser, paused),
        FanexError::NotPauser
    );
    config.paused = paused;
    emit!(UsdcPauseSet {
        curve: Pubkey::default(),
        paused,
        by,
    });
    Ok(())
}

#[derive(Accounts)]
pub struct SetCurveUsdcPaused<'info> {
    /// The pauser or the admin: the handler decides which may write what.
    pub authority: Signer<'info>,

    /// Read for its two keys alone.
    #[account(seeds = [USDC_CONFIG_SEED], bump = usdc_config.bump)]
    pub usdc_config: Box<Account<'info, UsdcConfig>>,

    /// Bound by its seeds (`ConstraintSeeds` at any other address), unlike the SOL
    /// `set_curve_paused`, which checks none; a SOL `Curve` fails its discriminator first (3002).
    #[account(mut, seeds = [CURVE_SEED, curve.mint.as_ref()], bump = curve.bump)]
    pub curve: Box<Account<'info, CurveUsdc>>,
}

/// Write one USDC curve's pause, under the same rule as the kill-switch's.
pub fn set_curve_usdc_paused(ctx: Context<SetCurveUsdcPaused>, paused: bool) -> Result<()> {
    let by = ctx.accounts.authority.key();
    let config = &ctx.accounts.usdc_config;
    require!(
        may_set_usdc_pause(&by, &config.admin, &config.pauser, paused),
        FanexError::NotPauser
    );
    let curve = &mut ctx.accounts.curve;
    curve.paused = paused;
    emit!(UsdcPauseSet {
        curve: curve.key(),
        paused,
        by,
    });
    Ok(())
}

// ── The SOL side, pause only ─────────────────────────────────────────────────

#[derive(Accounts)]
pub struct UsdcPauseSol<'info> {
    /// `UsdcConfig.pauser`, the only key this instruction takes. It pays nothing.
    pub pauser: Signer<'info>,

    /// Read for its pauser alone, bound by its seeds (Anchor's 2006 anywhere else).
    #[account(
        seeds = [USDC_CONFIG_SEED],
        bump = usdc_config.bump,
        has_one = pauser @ FanexError::Unauthorized,
    )]
    pub usdc_config: Box<Account<'info, UsdcConfig>>,

    /// The SOL side's config, bound by its seeds.
    #[account(mut, seeds = [CONFIG_SEED], bump = global_config.bump)]
    pub global_config: Box<Account<'info, GlobalConfig>>,
}

/// Pause the whole SOL side: `GlobalConfig.paused = true`, which refuses `create_curve`, `buy`,
/// `sell`, `init_distributor` and `init_distributor_v2`.
pub fn usdc_pause_sol(ctx: Context<UsdcPauseSol>) -> Result<()> {
    ctx.accounts.global_config.paused = true;
    emit!(UsdcPauseSolSet {
        curve: Pubkey::default(),
        by: ctx.accounts.pauser.key(),
    });
    Ok(())
}

#[derive(Accounts)]
pub struct UsdcPauseSolCurve<'info> {
    /// `UsdcConfig.pauser`, the only key this instruction takes. It pays nothing.
    pub pauser: Signer<'info>,

    /// Read for its pauser alone, bound by its seeds (Anchor's 2006 anywhere else).
    #[account(
        seeds = [USDC_CONFIG_SEED],
        bump = usdc_config.bump,
        has_one = pauser @ FanexError::Unauthorized,
    )]
    pub usdc_config: Box<Account<'info, UsdcConfig>>,

    /// A SOL `Curve`, bound by its seeds `["curve", curve.mint]` and its bump (Anchor's 2006 anywhere
    /// else), unlike `set_curve_paused`, which checks none; a `CurveUsdc` fails its discriminator first
    /// (3002), so a USDC curve is paused by `set_curve_usdc_paused` alone.
    #[account(mut, seeds = [CURVE_SEED, curve.mint.as_ref()], bump = curve.bump)]
    pub curve: Box<Account<'info, Curve>>,
}

/// Pause one SOL curve: `Curve.paused = true`, which refuses `buy` and `sell` on it.
pub fn usdc_pause_sol_curve(ctx: Context<UsdcPauseSolCurve>) -> Result<()> {
    let curve = &mut ctx.accounts.curve;
    curve.paused = true;
    emit!(UsdcPauseSolSet {
        curve: curve.key(),
        by: ctx.accounts.pauser.key(),
    });
    Ok(())
}

// ── The raise, pause only ────────────────────────────────────────────────────

#[derive(Accounts)]
pub struct UsdcPauseRaise<'info> {
    /// `UsdcConfig.pauser`, the only key this instruction takes. It pays nothing.
    pub pauser: Signer<'info>,

    /// Read for its pauser alone, bound by its seeds (Anchor's 2006 anywhere else).
    #[account(
        seeds = [USDC_CONFIG_SEED],
        bump = usdc_config.bump,
        has_one = pauser @ FanexError::Unauthorized,
    )]
    pub usdc_config: Box<Account<'info, UsdcConfig>>,

    /// The raise's config, bound by its seeds.
    #[account(mut, seeds = [RAISE_CONFIG_SEED], bump = raise_config.bump)]
    pub raise_config: Box<Account<'info, RaiseConfig>>,
}

/// Pause the raise: `RaiseConfig.paused = true`, which refuses `create_raise`, `buy_quotas`,
/// `declare_month`, the attester's `redeclare_month`, `sell_to_pool` and `buy_from_pool`.
pub fn usdc_pause_raise(ctx: Context<UsdcPauseRaise>) -> Result<()> {
    ctx.accounts.raise_config.paused = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use anchor_lang::prelude::Pubkey;
    use anchor_lang::{Discriminator, InstructionData, ToAccountMetas};

    #[test]
    fn the_pauser_signs_alone_and_the_sol_flag_is_the_only_account_written() {
        let [pauser, usdc_config, global_config, curve] = std::array::from_fn(|_| Pubkey::new_unique());

        let side = crate::accounts::UsdcPauseSol { pauser, usdc_config, global_config }.to_account_metas(None);
        assert_eq!(side.iter().map(|m| m.pubkey).collect::<Vec<_>>(), [pauser, usdc_config, global_config]);
        assert_eq!(side.iter().filter(|m| m.is_signer).map(|m| m.pubkey).collect::<Vec<_>>(), [pauser]);
        assert_eq!(side.iter().filter(|m| m.is_writable).map(|m| m.pubkey).collect::<Vec<_>>(), [global_config]);

        let one = crate::accounts::UsdcPauseSolCurve { pauser, usdc_config, curve }.to_account_metas(None);
        assert_eq!(one.iter().map(|m| m.pubkey).collect::<Vec<_>>(), [pauser, usdc_config, curve]);
        assert_eq!(one.iter().filter(|m| m.is_signer).map(|m| m.pubkey).collect::<Vec<_>>(), [pauser]);
        assert_eq!(one.iter().filter(|m| m.is_writable).map(|m| m.pubkey).collect::<Vec<_>>(), [curve]);
    }

    #[test]
    fn neither_sol_pause_takes_a_value_so_neither_can_unpause() {
        // The data is the 8-byte discriminator and nothing else: no `paused` to write `false` with.
        let side = crate::instruction::UsdcPauseSol {}.data();
        assert_eq!(side, crate::instruction::UsdcPauseSol::DISCRIMINATOR);
        let one = crate::instruction::UsdcPauseSolCurve {}.data();
        assert_eq!(one, crate::instruction::UsdcPauseSolCurve::DISCRIMINATOR);
        assert_eq!((side.len(), one.len()), (8, 8));
        // Apart from the SOL side's own pauses, and the USDC side's, which take the value.
        assert_ne!(side, crate::instruction::SetPaused { paused: true }.data()[..8]);
        assert_ne!(one, crate::instruction::SetCurvePaused { paused: true }.data()[..8]);
        assert_ne!(side, crate::instruction::SetUsdcPaused { paused: true }.data()[..8]);
        assert_ne!(one, crate::instruction::SetCurveUsdcPaused { paused: true }.data()[..8]);
    }

    #[test]
    fn the_pauser_pauses_the_raise_alone_and_cannot_unpause_it() {
        let [pauser, usdc_config, raise_config] = std::array::from_fn(|_| Pubkey::new_unique());
        let metas = crate::accounts::UsdcPauseRaise { pauser, usdc_config, raise_config }.to_account_metas(None);
        assert_eq!(metas.iter().map(|m| m.pubkey).collect::<Vec<_>>(), [pauser, usdc_config, raise_config]);
        assert_eq!(metas.iter().filter(|m| m.is_signer).map(|m| m.pubkey).collect::<Vec<_>>(), [pauser]);
        assert_eq!(metas.iter().filter(|m| m.is_writable).map(|m| m.pubkey).collect::<Vec<_>>(), [raise_config]);

        // No `paused` to write `false` with, and apart from the raise admin's own pause.
        let data = crate::instruction::UsdcPauseRaise {}.data();
        assert_eq!(data, crate::instruction::UsdcPauseRaise::DISCRIMINATOR);
        assert_eq!(data.len(), 8);
        assert_ne!(data, crate::instruction::SetRaisePaused { paused: true }.data()[..8]);
    }
}
