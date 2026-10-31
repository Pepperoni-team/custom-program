//! The config (spec §4.1, §4.2): born by this program's upgrade authority, read from its
//! ProgramData, so nobody can take it first; changed by its admin alone, the upgrade Squad's
//! vault. `usdc_mint` and `curve_program` are set once and never change.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::bpf_loader_upgradeable;
use anchor_spl::token::Mint;

use crate::constants::{ACCOUNT_VERSION, CONFIG_SEED, USDC_DECIMALS};
use crate::errors::DividendsError;
use crate::events::ConfigUpdated;
use crate::state::DividendsConfig;
use crate::validation::{check_approver, known_version, program_data_upgrade_authority};

/// The keys `initialize_config` writes beside its accounts: every role but the admin, which is
/// an account so that its owner can be checked.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfigKeys {
    pub pauser: Pubkey,
    pub approver: Pubkey,
    pub closer: Pubkey,
    pub pepper_funder: Pubkey,
    pub payer_funder: Pubkey,
}

/// What `update_config` changes: each key it names, the others kept. The admin moves through
/// the `new_admin` account, the set-once fields through nothing.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConfigUpdate {
    pub pauser: Option<Pubkey>,
    pub approver: Option<Pubkey>,
    pub closer: Option<Pubkey>,
    pub pepper_funder: Option<Pubkey>,
    pub payer_funder: Option<Pubkey>,
}

#[derive(Accounts)]
pub struct InitializeConfig<'info> {
    /// Pays the config's rent: on mainnet the vault itself, since a vault transaction carries no
    /// other keypair's signature.
    #[account(mut)]
    pub payer: Signer<'info>,

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
        owner = bpf_loader_upgradeable::ID @ DividendsError::Unauthorized,
        constraint = program_data_upgrade_authority(&program_data.try_borrow_data()?)
            == Some(upgrade_authority.key()) @ DividendsError::Unauthorized,
    )]
    pub program_data: UncheckedAccount<'info>,

    #[account(init, payer = payer, space = 8 + DividendsConfig::INIT_SPACE, seeds = [CONFIG_SEED], bump)]
    pub config: Box<Account<'info, DividendsConfig>>,

    /// The admin: an account the system program owns, as a Squads vault is (`AccountNotSystemOwned`
    /// otherwise). The multisig account is the Squads program's and never signs: an admin handed to
    /// it would be lost for ever (MULTISIG.md §2).
    pub admin: SystemAccount<'info>,

    /// The classic SPL Token USDC of 6 decimals (`AccountOwnedByWrongProgram` for Token-2022's,
    /// `ConstraintMintDecimals` for another number of decimals). Set once.
    #[account(mint::decimals = USDC_DECIMALS)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    /// CHECK: executable and owned by the upgradeable loader, as `pepper_curve` is
    /// (`ConstraintExecutable`, `ConstraintOwner`): set once, a wrong key would turn
    /// `LegacyMonthLive` off for good and every creator month into `CurveUnreadable`. Which program
    /// it is the caller's to check (B13's script requires `pepper_curve`'s id).
    #[account(executable, owner = bpf_loader_upgradeable::ID)]
    pub curve_program: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateConfig<'info> {
    #[account(address = config.admin @ DividendsError::Unauthorized)]
    pub admin: Signer<'info>,

    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = known_version(config.version) @ DividendsError::UnsupportedVersion,
    )]
    pub config: Box<Account<'info, DividendsConfig>>,

    /// The next admin, held to `initialize_config`'s owner rule (`AccountNotSystemOwned`); `None`
    /// keeps the admin.
    pub new_admin: Option<SystemAccount<'info>>,
}

/// The six keys a config holds after a write, as `ConfigUpdated` logs them.
pub fn config_updated(config: &DividendsConfig) -> ConfigUpdated {
    ConfigUpdated {
        admin: config.admin,
        pauser: config.pauser,
        approver: config.approver,
        closer: config.closer,
        pepper_funder: config.pepper_funder,
        payer_funder: config.payer_funder,
    }
}

/// Writes every field of the config, after the accounts' checks above: the approver a key of its
/// own (`ApproverIsFunder`), funding open, claims never paused (0).
pub fn initialize_config(ctx: Context<InitializeConfig>, keys: ConfigKeys) -> Result<()> {
    check_approver(&keys.approver, &keys.pepper_funder, &keys.payer_funder)?;
    let config = &mut ctx.accounts.config;
    config.version = ACCOUNT_VERSION;
    config.bump = ctx.bumps.config;
    config.paused = false;
    config.claims_paused_until = 0;
    config.admin = ctx.accounts.admin.key();
    config.pauser = keys.pauser;
    config.approver = keys.approver;
    config.closer = keys.closer;
    config.usdc_mint = ctx.accounts.usdc_mint.key();
    config.pepper_funder = keys.pepper_funder;
    config.payer_funder = keys.payer_funder;
    config.curve_program = ctx.accounts.curve_program.key();
    config.reserved = [0; 128];
    emit!(config_updated(config));
    Ok(())
}

/// Changes the keys `update` names and the admin when `new_admin` is passed, then holds the
/// approver, new or kept, to the funders the config now names (`ApproverIsFunder`).
pub fn update_config(ctx: Context<UpdateConfig>, update: ConfigUpdate) -> Result<()> {
    let config = &mut ctx.accounts.config;
    if let Some(new_admin) = &ctx.accounts.new_admin {
        config.admin = new_admin.key();
    }
    config.pauser = update.pauser.unwrap_or(config.pauser);
    config.approver = update.approver.unwrap_or(config.approver);
    config.closer = update.closer.unwrap_or(config.closer);
    config.pepper_funder = update.pepper_funder.unwrap_or(config.pepper_funder);
    config.payer_funder = update.payer_funder.unwrap_or(config.payer_funder);
    check_approver(&config.approver, &config.pepper_funder, &config.payer_funder)?;
    emit!(config_updated(config));
    Ok(())
}
