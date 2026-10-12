use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::metadata::{
    create_metadata_accounts_v3, mpl_token_metadata::types::DataV2,
    CreateMetadataAccountsV3, Metadata,
};
use anchor_spl::token::{self, Mint, MintTo, SetAuthority, Token, TokenAccount};
use anchor_spl::token::spl_token::instruction::AuthorityType;

use crate::constants::*;
use crate::errors::FanexError;
use crate::events::CurveCreated;
use crate::state::*;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CreateCurveParams {
    pub creator_target_lamports: u64,
    pub cpmm_seed_lamports: u64,
    pub creator_split_bps: u64,
    pub fanex_split_bps: u64,
    pub creator_cap_lamports: u64,
    pub fanex_cap_lamports: u64,
    pub virtual_sol_reserves: u64,
    pub virtual_token_reserves: u64,
    pub curve_token_supply: u64,
    pub migration_token_supply: u64,
    pub vesting_token_supply: u64,
    pub vesting_cliff_seconds: i64,
    pub vesting_duration_seconds: i64,
    pub name: String,
    pub symbol: String,
    pub uri: String,
}

#[derive(Accounts)]
pub struct CreateCurve<'info> {
    #[account(mut)]
    pub launcher: Signer<'info>,

    /// CHECK: The launch owner — recipient of the creator's 94% atomic split on
    /// every buy and beneficiary of the vested 2%. Stored as `curve.creator` and
    /// later enforced by `buy`/`sell`/`claim_vesting`. MAY differ from `launcher`
    /// (admin-concierge). No signature is required: whoever pays the create tx
    /// designates the creator wallet.
    pub creator: UncheckedAccount<'info>,

    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, GlobalConfig>,

    #[account(
        init,
        payer = launcher,
        mint::decimals = TOKEN_DECIMALS,
        mint::authority = curve,
    )]
    pub mint: Account<'info, Mint>,

    #[account(
        init,
        payer = launcher,
        space = 8 + Curve::INIT_SPACE,
        seeds = [CURVE_SEED, mint.key().as_ref()],
        bump
    )]
    pub curve: Account<'info, Curve>,

    #[account(
        init,
        payer = launcher,
        space = 8 + SolVault::INIT_SPACE,
        seeds = [SOL_VAULT_SEED, mint.key().as_ref()],
        bump
    )]
    pub sol_vault: Account<'info, SolVault>,

    #[account(
        init,
        payer = launcher,
        space = 8 + Vesting::INIT_SPACE,
        seeds = [VESTING_SEED, mint.key().as_ref()],
        bump
    )]
    pub vesting: Account<'info, Vesting>,

    #[account(
        init,
        payer = launcher,
        associated_token::mint = mint,
        associated_token::authority = curve,
    )]
    pub token_vault: Account<'info, TokenAccount>,

    #[account(
        init,
        payer = launcher,
        seeds = [VESTING_VAULT_SEED, mint.key().as_ref()],
        bump,
        token::mint = mint,
        token::authority = curve,
    )]
    pub vesting_vault: Account<'info, TokenAccount>,

    /// CHECK: Metaplex Token Metadata PDA for `mint`. Uninitialized on entry;
    /// the Metaplex CPI in the handler derives + validates it against `mint` and
    /// creates it. Mutable because the CPI initializes it.
    #[account(mut)]
    pub metadata: UncheckedAccount<'info>,

    /// CHECK: Set as the token metadata's update authority (not a signer here).
    /// Must equal `config.fanex_fee_wallet` so Fanex retains the ability to fix
    /// name/symbol/uri later.
    #[account(address = config.fanex_fee_wallet @ FanexError::AccountMismatch)]
    pub metadata_update_authority: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub token_metadata_program: Program<'info, Metadata>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

pub fn handler(ctx: Context<CreateCurve>, params: CreateCurveParams) -> Result<()> {
    require!(!ctx.accounts.config.paused, FanexError::GloballyPaused);

    require!(params.name.len() <= MAX_NAME_LEN, FanexError::StringTooLong);
    require!(params.symbol.len() <= MAX_SYMBOL_LEN, FanexError::StringTooLong);
    require!(params.uri.len() <= MAX_URI_LEN, FanexError::StringTooLong);

    let split_sum = params
        .creator_split_bps
        .checked_add(params.fanex_split_bps)
        .ok_or(FanexError::MathOverflow)?;
    require!(split_sum < BPS_DENOMINATOR, FanexError::InvalidSplitBps);
    crate::validation::validate_creator_target(params.creator_target_lamports, split_sum)?;
    if params.creator_split_bps > 0 {
        crate::validation::validate_creator_can_receive_raise(
            ctx.accounts.creator.key(),
            crate::instructions::set_vesting_beneficiary::fan_led_placeholder(),
            params.creator_split_bps,
        )?;
    }
    require!(
        params.cpmm_seed_lamports >= MIN_CPMM_SEED_LAMPORTS
            && params.cpmm_seed_lamports <= CAP_CPMM_SEED_LAMPORTS,
        FanexError::CpmmSeedOutOfRange
    );

    let cap_sum = params
        .creator_cap_lamports
        .checked_add(params.fanex_cap_lamports)
        .ok_or(FanexError::MathOverflow)?;
    let cap_diff = cap_sum.abs_diff(params.creator_target_lamports);
    require!(cap_diff <= 1, FanexError::InvalidCaps);

    let supply_sum = params
        .curve_token_supply
        .checked_add(params.migration_token_supply)
        .ok_or(FanexError::MathOverflow)?
        .checked_add(params.vesting_token_supply)
        .ok_or(FanexError::MathOverflow)?;
    require!(supply_sum == TOKEN_TOTAL_SUPPLY, FanexError::InvalidSupplyAllocation);

    crate::validation::validate_vesting_supply(params.vesting_token_supply)?;
    crate::validation::validate_vesting_schedule(
        params.vesting_cliff_seconds,
        params.vesting_duration_seconds,
    )?;

    require!(params.virtual_sol_reserves > 0, FanexError::ZeroAmount);
    require!(
        params.virtual_token_reserves >= params.curve_token_supply
            && params.virtual_token_reserves > 0,
        FanexError::InvalidSupplyAllocation
    );

    let now = Clock::get()?.unix_timestamp;
    let mint_key = ctx.accounts.mint.key();

    let curve = &mut ctx.accounts.curve;
    curve.mint = mint_key;
    curve.creator = ctx.accounts.creator.key();
    curve.phase = CurvePhase::Bonding;
    curve.creator_target_lamports = params.creator_target_lamports;
    curve.cpmm_seed_lamports = params.cpmm_seed_lamports;
    curve.creator_split_bps = params.creator_split_bps;
    curve.fanex_split_bps = params.fanex_split_bps;
    curve.creator_cap_lamports = params.creator_cap_lamports;
    curve.fanex_cap_lamports = params.fanex_cap_lamports;
    curve.virtual_sol_reserves = params.virtual_sol_reserves;
    curve.virtual_token_reserves = params.virtual_token_reserves;
    curve.real_sol_reserves = 0;
    curve.real_token_reserves = params.curve_token_supply;
    curve.migration_token_reserves = params.migration_token_supply;
    curve.creator_received_lamports = 0;
    curve.fanex_received_lamports = 0;
    curve.paused = false;
    curve.created_at = now;
    curve.graduated_at = 0;
    curve.bump = ctx.bumps.curve;
    curve.sol_vault_bump = ctx.bumps.sol_vault;

    let sol_vault = &mut ctx.accounts.sol_vault;
    sol_vault.mint = mint_key;
    sol_vault.bump = ctx.bumps.sol_vault;

    let vesting = &mut ctx.accounts.vesting;
    vesting.mint = mint_key;
    vesting.beneficiary = ctx.accounts.creator.key();
    vesting.total_amount = params.vesting_token_supply;
    vesting.released_amount = 0;
    vesting.start_time = now;
    vesting.cliff_seconds = params.vesting_cliff_seconds;
    vesting.duration_seconds = params.vesting_duration_seconds;
    vesting.bump = ctx.bumps.vesting;

    let curve_bump = ctx.bumps.curve;
    let signer_seeds: &[&[&[u8]]] = &[&[CURVE_SEED, mint_key.as_ref(), &[curve_bump]]];

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

    create_metadata_accounts_v3(
        CpiContext::new_with_signer(
            ctx.accounts.token_metadata_program.to_account_info(),
            CreateMetadataAccountsV3 {
                metadata: ctx.accounts.metadata.to_account_info(),
                mint: ctx.accounts.mint.to_account_info(),
                mint_authority: ctx.accounts.curve.to_account_info(),
                payer: ctx.accounts.launcher.to_account_info(),
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
        true,
        false,
        None,
    )?;

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

    emit!(CurveCreated {
        mint: mint_key,
        creator: ctx.accounts.creator.key(),
        creator_target_lamports: params.creator_target_lamports,
        cpmm_seed_lamports: params.cpmm_seed_lamports,
    });

    Ok(())
}
