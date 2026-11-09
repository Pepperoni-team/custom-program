//! Config, pause and the admin (internal audit of 2026-10-29; docs/solana/PEPPER-RAISE-PROGRAM.md §3, §9).
//! `initialize_config` is born by this program's upgrade authority, as its ProgramData records it,
//! like `pepper_dividends`' config and `pepper_curve`'s `set_usdc_raydium`: it was first-caller-wins
//! (ruling S2), so a stranger could take the admin, the attester, the fee wallet and the USDC mint at
//! deploy. Now nobody else can, and the mainnet init can wait for the upgrade Squad's proposal. The
//! admin is named as an account. The pool fee is refused above its 3% ceiling (amended spec D16) and
//! the launch fee above USD 50 (`LAUNCH_FEE_MAX`), on both paths. The admin moves in two steps:
//! `propose_admin`, then `accept_admin` signed by the new admin.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::bpf_loader_upgradeable;

use crate::raise::constants::*;
use crate::errors::FanexError;
use crate::raise::state::*;
use crate::raise::validation::{check_launch_fee, check_pool_fee, program_data_upgrade_authority};

/// The first three accounts are the first-caller-wins init's, in its order (`payer` stands where
/// `admin` did), so a program built before the gate still reads them from a builder of these
/// accounts; the gate's three are appended after them.
#[derive(Accounts)]
pub struct InitializeRaiseConfig<'info> {
    /// Pays the config's rent: on mainnet the vault itself, since a vault transaction carries no
    /// other keypair's signature.
    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(
        init,
        payer = payer,
        space = 8 + RaiseConfig::INIT_SPACE,
        seeds = [RAISE_CONFIG_SEED],
        bump
    )]
    pub config: Account<'info, RaiseConfig>,

    pub system_program: Program<'info, System>,

    /// This program's upgrade authority, as its ProgramData records it.
    pub upgrade_authority: Signer<'info>,

    /// CHECK: bound by its seeds and its owner, its header read by
    /// `program_data_upgrade_authority`. The program's ProgramData, the upgradeable loader's PDA
    /// of the program id (`ConstraintSeeds` anywhere else, so another program's ProgramData is
    /// refused), owned by the loader, whose recorded authority must be the signer:
    /// `Unauthorized` for any other key, for an immutable program, which records none, and for a
    /// program loaded without the upgradeable loader, which has no ProgramData.
    #[account(
        seeds = [crate::ID.as_ref()],
        bump,
        seeds::program = bpf_loader_upgradeable::ID,
        owner = bpf_loader_upgradeable::ID @ FanexError::RaiseUnauthorized,
        constraint = program_data_upgrade_authority(&program_data.try_borrow_data()?)
            == Some(upgrade_authority.key()) @ FanexError::RaiseUnauthorized,
    )]
    pub program_data: UncheckedAccount<'info>,

    /// The admin: an account the system program owns, as a Squads vault is (`AccountNotSystemOwned`
    /// otherwise, the all-zero key included). The multisig account is the Squads program's and never
    /// signs: an admin handed to it would be lost for ever (MULTISIG.md §2).
    pub admin: SystemAccount<'info>,
}

pub fn initialize_raise_config(
    ctx: Context<InitializeRaiseConfig>,
    attester: Pubkey,
    fee_wallet: Pubkey,
    usdc_mint: Pubkey,
    launch_fee: u64,
    pool_fee_bps: u16,
) -> Result<()> {
    check_pool_fee(pool_fee_bps)?;
    check_launch_fee(launch_fee)?;
    let config = &mut ctx.accounts.config;
    config.admin = ctx.accounts.admin.key();
    config.attester = attester;
    config.fee_wallet = fee_wallet;
    config.usdc_mint = usdc_mint;
    config.launch_fee = launch_fee;
    config.pool_fee_bps = pool_fee_bps;
    config.paused = false;
    config.bump = ctx.bumps.config;
    Ok(())
}

#[derive(Accounts)]
pub struct UpdateRaiseConfig<'info> {
    #[account(
        mut,
        seeds = [RAISE_CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ FanexError::RaiseUnauthorized
    )]
    pub config: Account<'info, RaiseConfig>,
    pub admin: Signer<'info>,
}

/// `None` leaves a field unchanged. The USDC mint is not updatable: every
/// vault is bound to it. A new pool fee applies to every raise from the next
/// trade, open raises included. `new_admin` stays in the arguments, so a builder
/// of today's arguments still decodes, but must be `None`: the admin moves through
/// `propose_admin` and `accept_admin` (`AdminMovesInTwoSteps`).
pub fn update_raise_config(
    ctx: Context<UpdateRaiseConfig>,
    new_admin: Option<Pubkey>,
    new_attester: Option<Pubkey>,
    new_fee_wallet: Option<Pubkey>,
    new_launch_fee: Option<u64>,
    new_pool_fee_bps: Option<u16>,
) -> Result<()> {
    require!(new_admin.is_none(), FanexError::AdminMovesInTwoSteps);
    let config = &mut ctx.accounts.config;
    if let Some(attester) = new_attester {
        config.attester = attester;
    }
    if let Some(wallet) = new_fee_wallet {
        config.fee_wallet = wallet;
    }
    if let Some(fee) = new_launch_fee {
        check_launch_fee(fee)?;
        config.launch_fee = fee;
    }
    if let Some(bps) = new_pool_fee_bps {
        check_pool_fee(bps)?;
        config.pool_fee_bps = bps;
    }
    Ok(())
}

pub fn set_raise_paused(ctx: Context<UpdateRaiseConfig>, paused: bool) -> Result<()> {
    ctx.accounts.config.paused = paused;
    Ok(())
}

#[derive(Accounts)]
pub struct ProposeAdmin<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    #[account(seeds = [RAISE_CONFIG_SEED], bump = config.bump, has_one = admin @ FanexError::RaiseUnauthorized)]
    pub config: Account<'info, RaiseConfig>,

    /// The proposal, written again by a later one: proposing the admin itself and accepting it
    /// withdraws one.
    #[account(
        init_if_needed,
        payer = admin,
        space = 8 + PendingAdmin::INIT_SPACE,
        seeds = [PENDING_ADMIN_SEED],
        bump
    )]
    pub pending_admin: Account<'info, PendingAdmin>,

    /// The next admin, held to `initialize_config`'s rule: an account the system program owns
    /// (`AccountNotSystemOwned` otherwise, the all-zero key and a multisig account included).
    pub new_admin: SystemAccount<'info>,

    pub system_program: Program<'info, System>,
}

/// The admin names its successor; nothing changes until the successor accepts.
pub fn propose_admin(ctx: Context<ProposeAdmin>) -> Result<()> {
    let pending = &mut ctx.accounts.pending_admin;
    pending.admin = ctx.accounts.new_admin.key();
    pending.payer = ctx.accounts.admin.key();
    pending.bump = ctx.bumps.pending_admin;
    Ok(())
}

#[derive(Accounts)]
pub struct AcceptAdmin<'info> {
    /// The admin proposed, signing for itself: a key nobody can sign for never becomes the admin.
    pub new_admin: Signer<'info>,

    #[account(mut, seeds = [RAISE_CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, RaiseConfig>,

    #[account(
        mut,
        seeds = [PENDING_ADMIN_SEED],
        bump = pending_admin.bump,
        constraint = pending_admin.admin == new_admin.key() @ FanexError::RaiseUnauthorized,
        close = payer
    )]
    pub pending_admin: Account<'info, PendingAdmin>,

    /// CHECK: receives the proposal's rent; bound to the payer it records.
    #[account(mut, address = pending_admin.payer @ FanexError::RaiseAccountMismatch)]
    pub payer: UncheckedAccount<'info>,
}

/// The proposed admin takes over; the proposal closes, its rent back to whoever paid it.
pub fn accept_admin(ctx: Context<AcceptAdmin>) -> Result<()> {
    ctx.accounts.config.admin = ctx.accounts.new_admin.key();
    Ok(())
}
