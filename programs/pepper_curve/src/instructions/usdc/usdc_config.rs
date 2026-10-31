//! The USDC side's config (USDC curve design §10.2, §10.3.1, §10.3.2). `initialize_usdc_config`
//! is signed by the program's upgrade authority as its ProgramData records it, the gate
//! `set_usdc_raydium` and `pepper_dividends`' `initialize_config` read (the internal audit,
//! 2026-10-29): until then `GlobalConfig.admin` signed it, so on mainnet the hot key would have
//! written every USDC role, the mint and the migration authority after the upgrade authority had
//! gone to the vault. Nobody but that key can create the config first, and it names its first admin,
//! an account the system program owns (the upgrade Squad's vault on mainnet), never the signer by
//! default. `update_usdc_config` is signed by `UsdcConfig.admin`. The USDC mint is set once, here;
//! the five Raydium fields start all-zero and are the upgrade authority's (`set_usdc_raydium`,
//! §10.3.10); the fee account, at init and at every change, is the associated USDC account of the
//! fee wallet `GlobalConfig.fanex_fee_wallet` names, not frozen, and closable by this config's PDA
//! alone (`validation::fee_usdc_account_valid`): every buy and every sale names it, so neither the
//! admin key nor the fee wallet can stop the swaps through it. Both emit `UsdcConfigUpdated`, the
//! config's keys as they stand after the write.
use anchor_lang::prelude::*;
use anchor_lang::solana_program::bpf_loader_upgradeable;
use anchor_spl::token::{Mint, TokenAccount};

use crate::constants::*;
use crate::errors::FanexError;
use crate::events::UsdcConfigUpdated;
use crate::instructions::usdc::set_usdc_raydium::program_data_upgrade_authority;
use crate::state::*;
use crate::validation::fee_usdc_account_valid;

/// The keys `initialize_usdc_config` writes beside its accounts: every role but the admin, which is
/// an account so that its owner is checked. The USDC mint and the fee account come as accounts too,
/// so the program checks what they are.
#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeUsdcConfigParams {
    /// May only pause: a hot key, so a pause stays fast.
    pub pauser: Pubkey,
    /// Signs the migration permits of the SOL curves (§10.10); all-zero, nobody.
    pub migration_authority: Pubkey,
    /// Receives a graduated pool's Fee Key, so it harvests the LP fees (§10.3.9); never all-zero
    /// (`LpFeeOwnerUnset`).
    pub lp_fee_owner: Pubkey,
    /// The only payer `create_curve_usdc` takes: the sponsor (Rick, 2026-10-22, §10.3.4).
    pub launcher: Pubkey,
}

#[derive(Accounts)]
pub struct InitializeUsdcConfig<'info> {
    /// Pays the config's rent: on mainnet the vault itself, since a vault transaction carries no
    /// other keypair's signature.
    #[account(mut)]
    pub payer: Signer<'info>,

    /// The program's upgrade authority, as its ProgramData records it. Not `GlobalConfig.admin`.
    pub upgrade_authority: Signer<'info>,

    /// CHECK: bound by its seeds and its owner, its header read by `program_data_upgrade_authority`.
    /// The program's ProgramData, the upgradeable loader's PDA of the program id (`ConstraintSeeds`
    /// anywhere else, so another program's ProgramData, whoever holds it, is refused), owned by the
    /// loader, whose `upgrade_authority_address` must be the signer: `Unauthorized` for any other
    /// key, `GlobalConfig.admin` included, for an immutable program, which records none, and for a
    /// program loaded without the upgradeable loader, which has no ProgramData. `set_usdc_raydium`'s
    /// gate, read the same way.
    #[account(
        seeds = [crate::ID.as_ref()],
        bump,
        seeds::program = bpf_loader_upgradeable::ID,
        owner = bpf_loader_upgradeable::ID @ FanexError::Unauthorized,
        constraint = program_data_upgrade_authority(&program_data.try_borrow_data()?)
            == Some(upgrade_authority.key()) @ FanexError::Unauthorized,
    )]
    pub program_data: UncheckedAccount<'info>,

    /// The first `UsdcConfig.admin`: an account the system program owns, as a Squads vault is
    /// (`AccountNotSystemOwned` otherwise, the all-zero key included, which is the system program's
    /// own). The multisig account is the Squads program's and never signs: an admin handed to it
    /// would be lost for good (MULTISIG.md §2).
    pub admin: SystemAccount<'info>,

    /// Read for one key, the fee wallet the fee account must belong to.
    #[account(seeds = [CONFIG_SEED], bump = global_config.bump)]
    pub global_config: Box<Account<'info, GlobalConfig>>,

    /// A plain `init`: a second initialize fails on the account already there.
    #[account(
        init,
        payer = payer,
        space = 8 + UsdcConfig::INIT_SPACE,
        seeds = [USDC_CONFIG_SEED],
        bump,
    )]
    pub usdc_config: Box<Account<'info, UsdcConfig>>,

    /// A classic SPL mint of 6 decimals (`ConstraintMintDecimals` otherwise), set once: no
    /// instruction changes it after this one.
    #[account(mint::decimals = USDC_MINT_DECIMALS)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    /// The fee wallet's USDC account: of `usdc_mint` (`ConstraintTokenMint` otherwise), owned by
    /// `GlobalConfig.fanex_fee_wallet` (`AccountMismatch` otherwise), then held in the handler to
    /// `fee_usdc_account_valid` (`FeeUsdcAccountInvalid`): the wallet's ATA, not frozen, closable
    /// by this config's PDA alone.
    #[account(
        token::mint = usdc_mint,
        constraint = fee_usdc_account.owner == global_config.fanex_fee_wallet @ FanexError::AccountMismatch,
    )]
    pub fee_usdc_account: Box<Account<'info, TokenAccount>>,

    pub system_program: Program<'info, System>,
}

/// Create the USDC side's config: version 1, not paused, `admin` the account passed, the mint and
/// the fee account checked above and here (`FeeUsdcAccountInvalid`), the keys of `params`,
/// `lp_fee_owner` never all-zero (`LpFeeOwnerUnset`: every Fee Key would go to an address nobody
/// signs for, for good, and with it every fee the locked LP earns), and the five Raydium fields
/// all-zero, so no curve launches or graduates until the upgrade authority sets them
/// (`RaydiumNotSet`).
pub fn initialize_usdc_config(
    ctx: Context<InitializeUsdcConfig>,
    params: InitializeUsdcConfigParams,
) -> Result<()> {
    require!(
        params.lp_fee_owner != Pubkey::default(),
        FanexError::LpFeeOwnerUnset
    );
    require!(
        ctx.accounts.admin.key() != Pubkey::default(),
        FanexError::AdminUnset
    );
    let usdc_mint = ctx.accounts.usdc_mint.key();
    let fee = &ctx.accounts.fee_usdc_account;
    require!(
        fee_usdc_account_valid(
            &fee.key(),
            fee,
            &ctx.accounts.global_config.fanex_fee_wallet,
            &usdc_mint,
            &ctx.accounts.usdc_config.key(),
        ),
        FanexError::FeeUsdcAccountInvalid
    );
    let fee_usdc_account = fee.key();
    let config = &mut ctx.accounts.usdc_config;
    config.version = USDC_ACCOUNT_VERSION;
    config.bump = ctx.bumps.usdc_config;
    config.paused = false;
    config.admin = ctx.accounts.admin.key();
    config.pauser = params.pauser;
    config.migration_authority = params.migration_authority;
    config.usdc_mint = usdc_mint;
    config.fee_usdc_account = fee_usdc_account;
    config.lp_fee_owner = params.lp_fee_owner;
    config.raydium_cpmm_program = Pubkey::default();
    config.raydium_amm_config = Pubkey::default();
    config.raydium_create_pool_fee = Pubkey::default();
    config.raydium_lock_program = Pubkey::default();
    config.raydium_lock_authority = Pubkey::default();
    config.launcher = params.launcher;
    emit!(keys_of(config));
    Ok(())
}

#[derive(Accounts)]
pub struct UpdateUsdcConfig<'info> {
    /// `UsdcConfig.admin`: the Squad's vault once handed over.
    pub admin: Signer<'info>,

    #[account(
        mut,
        seeds = [USDC_CONFIG_SEED],
        bump = usdc_config.bump,
        has_one = admin @ FanexError::Unauthorized,
    )]
    pub usdc_config: Box<Account<'info, UsdcConfig>>,

    /// Read for one key, the fee account's owner, as at init.
    #[account(seeds = [CONFIG_SEED], bump = global_config.bump)]
    pub global_config: Box<Account<'info, GlobalConfig>>,

    /// The new fee account, absent to keep the one there: held to the init's rule, of
    /// `usdc_config.usdc_mint` (`ConstraintTokenMint`), owned by `GlobalConfig.fanex_fee_wallet`
    /// (`AccountMismatch`), then `fee_usdc_account_valid` (`FeeUsdcAccountInvalid`). An account, not
    /// an argument: an address alone could be checked for none of it.
    #[account(
        token::mint = usdc_config.usdc_mint,
        constraint = new_fee_usdc_account.owner == global_config.fanex_fee_wallet @ FanexError::AccountMismatch,
    )]
    pub new_fee_usdc_account: Option<Box<Account<'info, TokenAccount>>>,
}

/// Change the config's keys. `None`, or an absent `new_fee_usdc_account`, leaves a field as it is;
/// an all-zero `new_lp_fee_owner` is refused (`LpFeeOwnerUnset`), as at init, and an all-zero
/// `new_admin` (`AdminUnset`, the internal audit, 2026-10-29: only the admin unpauses, and every
/// migrated curve is born paused). `usdc_mint` and the Raydium fields have no argument: the first is
/// set once, the others are the upgrade authority's. ⚠️ A wrong `new_admin` is irreversible: only
/// the key written can sign the next change. An off-curve key is taken, since a Squads vault is one.
pub fn update_usdc_config(
    ctx: Context<UpdateUsdcConfig>,
    new_admin: Option<Pubkey>,
    new_pauser: Option<Pubkey>,
    new_migration_authority: Option<Pubkey>,
    new_lp_fee_owner: Option<Pubkey>,
    new_launcher: Option<Pubkey>,
) -> Result<()> {
    require!(
        new_lp_fee_owner != Some(Pubkey::default()),
        FanexError::LpFeeOwnerUnset
    );
    require!(new_admin != Some(Pubkey::default()), FanexError::AdminUnset);
    if let Some(fee) = ctx.accounts.new_fee_usdc_account.as_deref() {
        require!(
            fee_usdc_account_valid(
                &fee.key(),
                fee,
                &ctx.accounts.global_config.fanex_fee_wallet,
                &ctx.accounts.usdc_config.usdc_mint,
                &ctx.accounts.usdc_config.key(),
            ),
            FanexError::FeeUsdcAccountInvalid
        );
    }
    let new_fee_usdc_account = ctx.accounts.new_fee_usdc_account.as_ref().map(|a| a.key());
    let config = &mut ctx.accounts.usdc_config;
    if let Some(admin) = new_admin {
        config.admin = admin;
    }
    if let Some(pauser) = new_pauser {
        config.pauser = pauser;
    }
    if let Some(migration_authority) = new_migration_authority {
        config.migration_authority = migration_authority;
    }
    if let Some(fee_usdc_account) = new_fee_usdc_account {
        config.fee_usdc_account = fee_usdc_account;
    }
    if let Some(lp_fee_owner) = new_lp_fee_owner {
        config.lp_fee_owner = lp_fee_owner;
    }
    if let Some(launcher) = new_launcher {
        config.launcher = launcher;
    }
    emit!(keys_of(config));
    Ok(())
}

/// The config's keys as `UsdcConfigUpdated` carries them.
fn keys_of(config: &UsdcConfig) -> UsdcConfigUpdated {
    UsdcConfigUpdated {
        admin: config.admin,
        pauser: config.pauser,
        migration_authority: config.migration_authority,
        fee_usdc_account: config.fee_usdc_account,
        lp_fee_owner: config.lp_fee_owner,
        launcher: config.launcher,
    }
}
