//! The USDC curve's launch (USDC curve design §10.3.4): today's `create_curve` priced in µUSDC, paid
//! and signed by `UsdcConfig.launcher` alone, the sponsor (Rick, 2026-10-22: only Pepper launches on
//! the USDC curve), with the bounds of §10.8 through the pure rules at the end of `validation.rs`.
//! What is new beside today's checks: the creator's USDC account a curve with a creator split pays
//! its raise into, the creator's own associated USDC account closable by the payer alone (trap 7),
//! and the reserve, `usdc_vault`, a PDA token account of the curve. The supply, the Metaplex
//! metadata and the revoke are today's, the payer in place of the launcher; no USDC moves. Since the
//! internal audit (2026-10-29) the raise is held to the economic model's relation and the target to
//! a ceiling (`validate_usdc_raise_terms`), and neither the curve's part of the supply nor the
//! pool's may be 0, so the launcher's key alone launches only the terms the app would.
use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::metadata::{
    create_metadata_accounts_v3, mpl_token_metadata::types::DataV2, CreateMetadataAccountsV3,
};
use anchor_spl::token::spl_token::instruction::AuthorityType;
use anchor_spl::token::spl_token::state::Account as SplTokenAccount;
use anchor_spl::token::{self, Mint, MintTo, SetAuthority, Token, TokenAccount};

use crate::constants::*;
use crate::errors::FanexError;
use crate::events::CurveCreatedUsdc;
use crate::instructions::set_vesting_beneficiary::fan_led_placeholder;
use crate::raydium::raydium_addresses_set;
use crate::state::*;
use crate::validation::{
    creator_usdc_account_valid, usdc_split_sum_bps, validate_caps_sum,
    validate_creator_can_receive_raise, validate_creator_target_floor,
    validate_graduation_threshold_usdc, validate_supply_allocation, validate_usdc_raise_terms,
    validate_vesting_schedule, validate_vesting_supply,
};

/// The launch's terms, computed by the server and checked here, as `CreateCurveParams` is: in µUSDC
/// where today's are in lamports, the two splits a u16 as `CurveUsdc` keeps them.
#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CreateCurveUsdcParams {
    pub creator_target_usdc: u64,
    /// The curve's USDC that ends the bonding phase and seeds the pool: today's `cpmm_seed_lamports`.
    pub graduation_threshold_usdc: u64,
    pub creator_split_bps: u16,
    pub fanex_split_bps: u16,
    pub creator_cap_usdc: u64,
    pub fanex_cap_usdc: u64,
    pub virtual_usdc_reserves: u64,
    pub virtual_token_reserves: u64,
    /// Tokens for sale on the curve (the first `real_token_reserves`).
    pub curve_token_supply: u64,
    /// Tokens kept to seed the pool at graduation.
    pub migration_token_supply: u64,
    /// Tokens vested to the creator.
    pub vesting_token_supply: u64,
    pub vesting_cliff_seconds: i64,
    pub vesting_duration_seconds: i64,
    pub name: String,
    /// At most `MAX_USDC_SYMBOL_LEN` (10) bytes, Metaplex's own limit.
    pub symbol: String,
    pub uri: String,
}

#[derive(Accounts)]
pub struct CreateCurveUsdc<'info> {
    /// The sponsor, `UsdcConfig.launcher` (`Unauthorized` for any other key): signs and pays every
    /// rent, so a creator spends no SOL and no stranger launches terms the screens never state.
    #[account(
        mut,
        address = usdc_config.launcher @ FanexError::Unauthorized,
    )]
    pub payer: Signer<'info>,

    /// CHECK: the launch's owner, stored as `curve.creator` and as the vesting's beneficiary; it
    /// need not sign. With a creator split it may not be the fan-led placeholder
    /// (`PlaceholderCannotRaise`), and its USDC ATA is the one the split is paid into.
    pub creator: UncheckedAccount<'info>,

    #[account(seeds = [USDC_CONFIG_SEED], bump = usdc_config.bump)]
    pub usdc_config: Box<Account<'info, UsdcConfig>>,

    #[account(address = usdc_config.usdc_mint @ FanexError::AccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    /// The new token, a fresh keypair signing for its own `init`: its mint authority the curve PDA,
    /// revoked after the one-time mint, and no freeze authority.
    #[account(
        init,
        payer = payer,
        mint::decimals = TOKEN_DECIMALS,
        mint::authority = curve,
    )]
    pub mint: Box<Account<'info, Mint>>,

    /// At `["curve", mint]`, where a SOL `Curve` would sit: a mint has one or the other.
    #[account(
        init,
        payer = payer,
        space = 8 + CurveUsdc::INIT_SPACE,
        seeds = [CURVE_SEED, mint.key().as_ref()],
        bump,
    )]
    pub curve: Box<Account<'info, CurveUsdc>>,

    /// The curve's USDC reserve: a token account of `usdc_mint` at its own PDA, the curve its
    /// authority, not an ATA (§10.2).
    #[account(
        init,
        payer = payer,
        seeds = [USDC_VAULT_SEED, mint.key().as_ref()],
        bump,
        token::mint = usdc_mint,
        token::authority = curve,
    )]
    pub usdc_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        init,
        payer = payer,
        space = 8 + Vesting::INIT_SPACE,
        seeds = [VESTING_SEED, mint.key().as_ref()],
        bump,
    )]
    pub vesting: Box<Account<'info, Vesting>>,

    /// The tokens for sale and for the pool, as today: the curve PDA's associated account.
    #[account(
        init,
        payer = payer,
        associated_token::mint = mint,
        associated_token::authority = curve,
    )]
    pub token_vault: Box<Account<'info, TokenAccount>>,

    /// The creator's vested tokens, as today.
    #[account(
        init,
        payer = payer,
        seeds = [VESTING_VAULT_SEED, mint.key().as_ref()],
        bump,
        token::mint = mint,
        token::authority = curve,
    )]
    pub vesting_vault: Box<Account<'info, TokenAccount>>,

    /// The creator's USDC associated token account, closable by the payer alone: required with a
    /// creator split and refused without one (`CreatorUsdcAccountInvalid` either way, checked in
    /// the handler by `creator_usdc_account_to_store`). Absent, it is passed as the program's own
    /// id. Only read: the launch transaction opens and hardens it before this instruction.
    pub creator_usdc_account: Option<Box<Account<'info, TokenAccount>>>,

    /// CHECK: the Metaplex metadata PDA of `mint`, uninitialized on entry: Metaplex derives and
    /// checks it against the mint, and creates it.
    #[account(mut)]
    pub metadata: UncheckedAccount<'info>,

    /// CHECK: the metadata's update authority, not a signer: `UsdcConfig.admin`, the Squad's vault
    /// before the first USDC launch (Rick, 2026-10-22), where a SOL launch names the fee wallet.
    /// Metaplex keeps it for good, so the hot key that signed `initialize_usdc_config` hands the
    /// admin over first.
    #[account(address = usdc_config.admin @ FanexError::AccountMismatch)]
    pub metadata_update_authority: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,

    /// CHECK: Metaplex Token Metadata, checked by its address alone (2026-10-22): a typed
    /// `Program<Metadata>` checks it is executable with the accounts, before the handler, so no
    /// refusal below could run where Metaplex is not loaded.
    #[account(address = anchor_spl::metadata::ID)]
    pub token_metadata_program: UncheckedAccount<'info>,

    pub rent: Sysvar<'info, Rent>,
}

/// The creator USDC account a launch stores in `curve.creator_usdc_account` (§10.3.4, trap 7): with
/// a creator split, the account passed, which `creator_usdc_account_valid` holds to the creator's
/// USDC ATA, theirs, not frozen and closable by `payer` alone; with none, no account at all, and the
/// field all-zero. Anything else, an account missing, wrong or passed without a split, is
/// `CreatorUsdcAccountInvalid`.
pub fn creator_usdc_account_to_store(
    creator_split_bps: u16,
    passed: Option<(Pubkey, &SplTokenAccount)>,
    creator: &Pubkey,
    usdc_mint: &Pubkey,
    payer: &Pubkey,
) -> Result<Pubkey> {
    match (creator_split_bps > 0, passed) {
        (true, Some((address, account)))
            if creator_usdc_account_valid(&address, account, creator, usdc_mint, payer) =>
        {
            Ok(address)
        }
        (false, None) => Ok(Pubkey::default()),
        _ => err!(FanexError::CreatorUsdcAccountInvalid),
    }
}

/// The pause, the Raydium addresses set, the checks of §10.3.4 in today's `create_curve` order, then
/// the creator's USDC account; then the curve, its vesting, the supply into the two vaults, the
/// Metaplex metadata and the revoke.
pub fn handler(ctx: Context<CreateCurveUsdc>, params: CreateCurveUsdcParams) -> Result<()> {
    require!(!ctx.accounts.usdc_config.paused, FanexError::GloballyPaused);
    // ── A curve exists only where it can graduate (2026-10-24): one launched before
    // `set_usdc_raydium` would reach its threshold and wait on `RaydiumNotSet` ──
    require!(
        raydium_addresses_set(&ctx.accounts.usdc_config.raydium_addresses()),
        FanexError::RaydiumNotSet
    );

    // ── String bounds: the symbol at Metaplex's 10, where a SOL launch takes 12 ──
    require!(params.name.len() <= MAX_NAME_LEN, FanexError::StringTooLong);
    require!(
        params.symbol.len() <= MAX_USDC_SYMBOL_LEN,
        FanexError::StringTooLong
    );
    require!(params.uri.len() <= MAX_URI_LEN, FanexError::StringTooLong);

    // ── Economic invariants, in µUSDC ──────────────────────────────────────
    let split_sum = usdc_split_sum_bps(params.creator_split_bps, params.fanex_split_bps)?;
    validate_creator_target_floor(
        params.creator_target_usdc,
        split_sum,
        MIN_CREATOR_TARGET_USDC,
    )?;
    // Derived only when there is a raise, as today: `find_program_address` costs compute units.
    if params.creator_split_bps > 0 {
        validate_creator_can_receive_raise(
            ctx.accounts.creator.key(),
            fan_led_placeholder(),
            u64::from(params.creator_split_bps),
        )?;
    }
    validate_graduation_threshold_usdc(params.graduation_threshold_usdc)?;
    validate_caps_sum(
        params.creator_cap_usdc,
        params.fanex_cap_usdc,
        params.creator_target_usdc,
    )?;
    // The raise held to the economic model's relation, the target to its ceiling (the internal
    // audit, 2026-10-29): what the launcher's key alone may launch.
    validate_usdc_raise_terms(
        params.creator_target_usdc,
        params.graduation_threshold_usdc,
        params.creator_split_bps,
        params.fanex_split_bps,
        params.creator_cap_usdc,
        params.fanex_cap_usdc,
    )?;
    validate_supply_allocation(
        params.curve_token_supply,
        params.migration_token_supply,
        params.vesting_token_supply,
    )?;
    validate_vesting_supply(params.vesting_token_supply)?;
    validate_vesting_schedule(params.vesting_cliff_seconds, params.vesting_duration_seconds)?;
    require!(params.virtual_usdc_reserves > 0, FanexError::ZeroAmount);
    require!(
        params.virtual_token_reserves >= params.curve_token_supply
            && params.virtual_token_reserves > 0,
        FanexError::InvalidSupplyAllocation
    );

    // ── The creator's USDC account (trap 7) ──────────────────────────────
    // `***`: through the reference, the Anchor `Account`, then the `TokenAccount` to its SPL state.
    let passed = match ctx.accounts.creator_usdc_account.as_deref() {
        Some(account) => Some((account.key(), &***account)),
        None => None,
    };
    let creator_usdc_account = creator_usdc_account_to_store(
        params.creator_split_bps,
        passed,
        &ctx.accounts.creator.key(),
        &ctx.accounts.usdc_mint.key(),
        &ctx.accounts.payer.key(),
    )?;

    let now = Clock::get()?.unix_timestamp;
    let mint_key = ctx.accounts.mint.key();

    // ── Persist the curve ─────────────────────────────────────────────────
    let curve = &mut ctx.accounts.curve;
    curve.version = USDC_ACCOUNT_VERSION;
    curve.bump = ctx.bumps.curve;
    curve.usdc_vault_bump = ctx.bumps.usdc_vault;
    curve.phase = PHASE_BONDING;
    curve.paused = false;
    curve.migrated = false;
    curve.mint = mint_key;
    curve.creator = ctx.accounts.creator.key();
    curve.creator_usdc_account = creator_usdc_account;
    curve.creator_split_bps = params.creator_split_bps;
    curve.fanex_split_bps = params.fanex_split_bps;
    curve.creator_target_usdc = params.creator_target_usdc;
    curve.graduation_threshold_usdc = params.graduation_threshold_usdc;
    curve.creator_cap_usdc = params.creator_cap_usdc;
    curve.fanex_cap_usdc = params.fanex_cap_usdc;
    curve.virtual_usdc_reserves = params.virtual_usdc_reserves;
    curve.virtual_token_reserves = params.virtual_token_reserves;
    curve.real_usdc_reserves = 0;
    curve.real_token_reserves = params.curve_token_supply;
    curve.migration_token_reserves = params.migration_token_supply;
    curve.creator_received_usdc = 0;
    curve.fanex_received_usdc = 0;
    curve.created_at = now;
    curve.graduated_at = 0;
    curve.migrated_at = 0;
    curve.reserved = [0; 128];

    // The vesting as today: the creator its beneficiary, counting from the launch.
    let vesting = &mut ctx.accounts.vesting;
    vesting.mint = mint_key;
    vesting.beneficiary = ctx.accounts.creator.key();
    vesting.total_amount = params.vesting_token_supply;
    vesting.released_amount = 0;
    vesting.start_time = now;
    vesting.cliff_seconds = params.vesting_cliff_seconds;
    vesting.duration_seconds = params.vesting_duration_seconds;
    vesting.bump = ctx.bumps.vesting;

    // ── Mint the fixed supply, then revoke the mint authority (today's block) ──
    let curve_bump = ctx.bumps.curve;
    let signer_seeds: &[&[&[u8]]] = &[&[CURVE_SEED, mint_key.as_ref(), &[curve_bump]]];

    // Curve-sale + migration tokens → token_vault.
    let tradeable = params
        .curve_token_supply
        .checked_add(params.migration_token_supply)
        .ok_or(FanexError::MathOverflow)?;
    token::mint_to(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            MintTo {
                mint: ctx.accounts.mint.to_account_info(),
                to: ctx.accounts.token_vault.to_account_info(),
                authority: ctx.accounts.curve.to_account_info(),
            },
            signer_seeds,
        ),
        tradeable,
    )?;

    // Vesting tokens → vesting_vault.
    token::mint_to(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            MintTo {
                mint: ctx.accounts.mint.to_account_info(),
                to: ctx.accounts.vesting_vault.to_account_info(),
                authority: ctx.accounts.curve.to_account_info(),
            },
            signer_seeds,
        ),
        params.vesting_token_supply,
    )?;

    // The Metaplex metadata, while the curve PDA is still the mint authority, before the revoke
    // below: wallets, explorers and routers show the name and the image it points at. Mutable, its
    // update authority `UsdcConfig.admin`, not a signer.
    create_metadata_accounts_v3(
        CpiContext::new_with_signer(
            ctx.accounts.token_metadata_program.to_account_info(),
            CreateMetadataAccountsV3 {
                metadata: ctx.accounts.metadata.to_account_info(),
                mint: ctx.accounts.mint.to_account_info(),
                mint_authority: ctx.accounts.curve.to_account_info(),
                payer: ctx.accounts.payer.to_account_info(),
                update_authority: ctx.accounts.metadata_update_authority.to_account_info(),
                system_program: ctx.accounts.system_program.to_account_info(),
                rent: ctx.accounts.rent.to_account_info(),
            },
            signer_seeds,
        ),
        DataV2 {
            name: params.name.clone(),
            symbol: params.symbol.clone(),
            uri: params.uri.clone(),
            seller_fee_basis_points: 0,
            creators: None,
            collection: None,
            uses: None,
        },
        true,  // is_mutable
        false, // update_authority_is_signer (the admin does not sign a launch)
        None,  // collection_details
    )?;

    // Fix the supply for good: revoke the mint authority.
    token::set_authority(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            SetAuthority {
                current_authority: ctx.accounts.curve.to_account_info(),
                account_or_mint: ctx.accounts.mint.to_account_info(),
            },
            signer_seeds,
        ),
        AuthorityType::MintTokens,
        None,
    )?;

    emit!(CurveCreatedUsdc {
        mint: mint_key,
        creator: ctx.accounts.creator.key(),
        creator_target_usdc: params.creator_target_usdc,
        graduation_threshold_usdc: params.graduation_threshold_usdc,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::ToAccountMetas;
    use anchor_spl::associated_token::get_associated_token_address;
    use anchor_spl::token::spl_token::solana_program::program_option::COption;
    use anchor_spl::token::spl_token::state::AccountState;

    struct Keys {
        creator: Pubkey,
        usdc_mint: Pubkey,
        payer: Pubkey,
    }

    fn keys() -> Keys {
        Keys {
            creator: Pubkey::new_unique(),
            usdc_mint: Pubkey::new_unique(),
            payer: Pubkey::new_unique(),
        }
    }

    /// The creator's ATA as the launch transaction leaves it: open, closable by the payer alone.
    fn hardened(k: &Keys) -> (Pubkey, SplTokenAccount) {
        let account = SplTokenAccount {
            mint: k.usdc_mint,
            owner: k.creator,
            amount: 0,
            delegate: COption::None,
            state: AccountState::Initialized,
            is_native: COption::None,
            delegated_amount: 0,
            close_authority: COption::Some(k.payer),
        };
        (get_associated_token_address(&k.creator, &k.usdc_mint), account)
    }

    fn store(k: &Keys, split: u16, passed: Option<(Pubkey, &SplTokenAccount)>) -> Result<Pubkey> {
        creator_usdc_account_to_store(split, passed, &k.creator, &k.usdc_mint, &k.payer)
    }

    #[test]
    fn a_creator_split_stores_the_creators_hardened_ata() {
        let k = keys();
        let (address, account) = hardened(&k);
        assert_eq!(store(&k, 6_580, Some((address, &account))).unwrap(), address);
    }

    #[test]
    fn a_creator_split_without_its_account_or_with_a_wrong_one_is_refused() {
        let k = keys();
        let refused = error!(FanexError::CreatorUsdcAccountInvalid);
        assert_eq!(store(&k, 6_580, None).unwrap_err(), refused);
        let (address, account) = hardened(&k);
        assert_eq!(
            store(&k, 1, Some((Pubkey::new_unique(), &account))).unwrap_err(),
            refused
        );
        let open = SplTokenAccount {
            close_authority: COption::None,
            ..account
        };
        assert_eq!(store(&k, 1, Some((address, &open))).unwrap_err(), refused);
        let frozen = SplTokenAccount {
            state: AccountState::Frozen,
            ..account
        };
        assert_eq!(store(&k, 1, Some((address, &frozen))).unwrap_err(), refused);
    }

    #[test]
    fn no_creator_split_takes_no_account_and_stores_all_zero() {
        let k = keys();
        assert_eq!(store(&k, 0, None).unwrap(), Pubkey::default());
        // Even the creator's valid ATA: a curve that pays the creator nothing names no account.
        let (address, account) = hardened(&k);
        assert_eq!(
            store(&k, 0, Some((address, &account))).unwrap_err(),
            error!(FanexError::CreatorUsdcAccountInvalid)
        );
    }

    #[test]
    fn the_launch_is_signed_by_the_payer_and_the_mint_alone() {
        let [payer, creator, usdc_config, usdc_mint, mint, curve, usdc_vault, vesting, token_vault, vesting_vault, metadata, update_authority] =
            std::array::from_fn(|_| Pubkey::new_unique());
        let accounts = |creator_usdc_account: Option<Pubkey>| crate::accounts::CreateCurveUsdc {
            payer,
            creator,
            usdc_config,
            usdc_mint,
            mint,
            curve,
            usdc_vault,
            vesting,
            token_vault,
            vesting_vault,
            creator_usdc_account,
            metadata,
            metadata_update_authority: update_authority,
            token_program: anchor_spl::token::ID,
            associated_token_program: anchor_spl::associated_token::ID,
            system_program: anchor_lang::system_program::ID,
            token_metadata_program: anchor_spl::metadata::ID,
            rent: anchor_lang::solana_program::sysvar::rent::ID,
        };

        let creator_account = Pubkey::new_unique();
        let metas = accounts(Some(creator_account)).to_account_metas(None);
        assert_eq!(metas.len(), 18);
        // The creator signs the `SetAuthority` before this instruction, never this one.
        let signers: Vec<Pubkey> = metas.iter().filter(|m| m.is_signer).map(|m| m.pubkey).collect();
        assert_eq!(signers, [payer, mint]);
        let writable: Vec<Pubkey> = metas.iter().filter(|m| m.is_writable).map(|m| m.pubkey).collect();
        assert_eq!(
            writable,
            [payer, mint, curve, usdc_vault, vesting, token_vault, vesting_vault, metadata]
        );
        assert_eq!(metas[10].pubkey, creator_account);
        assert!(!metas[10].is_writable);

        // Absent, the creator's account is the program's own id, so no position moves.
        let without = accounts(None).to_account_metas(None);
        assert_eq!(without.len(), 18);
        assert_eq!(without[10].pubkey, crate::ID);
        assert!(!without[10].is_writable && !without[10].is_signer);
    }
}
