//! The launch cap's switch (2026-10-14, docs/solana/PEPPER-CURVE-PROGRAM.md
//! §5.2): the key that may launch a curve without the cap, and the waiver it
//! signs into a launch. A creator's launch transaction is theirs alone
//! (`create_curve` is signed by the launcher), so the exemption is an account
//! only that key can make, and only before the curve exists.
use anchor_lang::prelude::*;

use crate::constants::*;
use crate::errors::FanexError;
use crate::state::*;

#[derive(Accounts)]
pub struct SetLaunchGuardAuthority<'info> {
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ FanexError::Unauthorized
    )]
    pub config: Account<'info, GlobalConfig>,

    /// Pays `LaunchGuardConfig`'s rent the first time.
    #[account(mut)]
    pub admin: Signer<'info>,

    #[account(
        init_if_needed,
        payer = admin,
        space = 8 + LaunchGuardConfig::INIT_SPACE,
        seeds = [LAUNCH_GUARD_SEED],
        bump,
    )]
    pub launch_guard: Account<'info, LaunchGuardConfig>,

    pub system_program: Program<'info, System>,
}

/// Name the key that may launch a curve without the launch cap. All-zero lets
/// nobody; a waiver already made stays.
pub fn set_launch_guard_authority(
    ctx: Context<SetLaunchGuardAuthority>,
    authority: Pubkey,
) -> Result<()> {
    let guard = &mut ctx.accounts.launch_guard;
    guard.authority = authority;
    guard.bump = ctx.bumps.launch_guard;
    Ok(())
}

#[derive(Accounts)]
pub struct WaiveLaunchGuard<'info> {
    /// The launcher: pays the waiver's rent, as it pays the launch.
    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(
        seeds = [LAUNCH_GUARD_SEED],
        bump = launch_guard.bump,
        has_one = authority @ FanexError::Unauthorized,
    )]
    pub launch_guard: Account<'info, LaunchGuardConfig>,

    pub authority: Signer<'info>,

    /// CHECK: the mint the curve will be created on, not created yet: only its
    /// address is read, as the seed of the curve and of the waiver.
    pub mint: UncheckedAccount<'info>,

    /// CHECK: the mint's curve, which must not exist yet (the handler checks
    /// its owner): a waiver is made with the launch, never on a live curve.
    #[account(seeds = [CURVE_SEED, mint.key().as_ref()], bump)]
    pub curve: UncheckedAccount<'info>,

    #[account(
        init,
        payer = payer,
        space = 8 + LaunchGuardWaiver::INIT_SPACE,
        seeds = [LAUNCH_GUARD_WAIVER_SEED, mint.key().as_ref()],
        bump,
    )]
    pub waiver: Account<'info, LaunchGuardWaiver>,

    pub system_program: Program<'info, System>,
}

/// Launch the mint's curve without the launch cap: only the launch guard
/// authority may, and only before the curve exists, so nobody lifts the cap
/// from a live curve.
pub fn waive_launch_guard(ctx: Context<WaiveLaunchGuard>) -> Result<()> {
    require!(
        ctx.accounts.curve.owner != ctx.program_id,
        FanexError::LaunchGuardAfterLaunch
    );
    let waiver = &mut ctx.accounts.waiver;
    waiver.mint = ctx.accounts.mint.key();
    waiver.bump = ctx.bumps.waiver;
    Ok(())
}
