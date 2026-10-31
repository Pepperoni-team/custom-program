//! `graduate_usdc_pool()`: the first step of a USDC curve's graduation (USDC curve design §10.3.9,
//! decision 5), phase Graduating → PoolCreated. The program creates the token/USDC pool itself, by
//! CPI into Raydium's CPMM `initialize`, so no wallet ever holds the curve's reserves between the
//! curve and the pool: they move from the curve's vaults to the pool creator, a system-owned PDA of
//! ours, and from there into the pool, inside this one instruction. The second step,
//! `graduate_usdc_lock`, locks the LP.
//!
//! Permissionless: it chooses no destination, so anyone may crank it, the keeper by default, and
//! the caller pays what Raydium charges (the pool's rents and its fee), the lamports CPMM does not
//! spend refunded, an extra of up to 0.1 SOL included should Raydium's accounts grow past the
//! slack (2026-10-24). Gated by both USDC pauses, unlike the SOL side's `graduate`: it moves a curve's
//! whole reserves into a program Raydium can upgrade, so the pauser must be able to stop it as fast
//! as a swap (§10.3.3).
//!
//! The CPI is encoded by hand (`raydium.rs`): Raydium's CPI crates pin Anchor 1.0.2, this program
//! is on 0.31.1. The pool sits at our PDA `["cpmm_pool", mint]`, never at CPMM's canonical
//! `["pool", amm_config, mint_0, mint_1]`, which any holder of the token could create first at a
//! price of their choosing.
use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::Instruction;
use anchor_lang::solana_program::program::invoke_signed;
use anchor_lang::solana_program::program_pack::Pack;
use anchor_lang::system_program::{self, Transfer};
use anchor_spl::associated_token::{self, AssociatedToken, Create};
use anchor_spl::token::spl_token::state::Account as SplTokenAccount;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::constants::*;
use crate::errors::FanexError;
use crate::events::GraduationPoolCreated;
use crate::raydium::{
    check_amm_config, cpmm_initialize_data, cpmm_token_0_first, pool_creator_funding,
    raydium_addresses_set, CpmmInitializeAccounts, MAX_CREATE_POOL_FEE_LAMPORTS,
    MAX_GRADUATION_EXTRA_LAMPORTS,
};
use crate::state::*;
use crate::transfer::transfer_checked;

#[derive(Accounts)]
pub struct GraduateUsdcPool<'info> {
    /// Anyone, the keeper by default: pays the pool creator's two ATAs where they do not exist yet,
    /// and hands the pool creator what CPMM charges, getting back what it does not spend.
    #[account(mut)]
    pub caller: Signer<'info>,

    /// The USDC side's config: its pause, its mint and the Raydium addresses the CPI goes to.
    #[account(seeds = [USDC_CONFIG_SEED], bump = usdc_config.bump)]
    pub usdc_config: Box<Account<'info, UsdcConfig>>,

    /// A `CurveUsdc` by its type: a SOL `Curve` here is refused by its discriminator (3002).
    #[account(mut, seeds = [CURVE_SEED, curve.mint.as_ref()], bump = curve.bump)]
    pub curve: Box<Account<'info, CurveUsdc>>,

    /// The curve's USDC reserve, by its seeds: `real_usdc_reserves` leaves it, never its balance.
    #[account(
        mut,
        seeds = [USDC_VAULT_SEED, curve.mint.as_ref()],
        bump = curve.usdc_vault_bump,
    )]
    pub usdc_vault: Box<Account<'info, TokenAccount>>,

    /// The curve's tokens, the curve PDA's associated account: `real_token_reserves +
    /// migration_token_reserves` leave it, never its balance.
    #[account(
        mut,
        associated_token::mint = token_mint,
        associated_token::authority = curve,
    )]
    pub token_vault: Box<Account<'info, TokenAccount>>,

    /// The USDC mint the config names, for `transfer_checked` and the pool.
    #[account(address = usdc_config.usdc_mint @ FanexError::AccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    /// The curve's token mint, for `transfer_checked` and the pool.
    #[account(address = curve.mint @ FanexError::AccountMismatch)]
    pub token_mint: Box<Account<'info, Mint>>,

    /// The pool's creator, `["pool_creator", mint]`: a system-owned PDA with no data, since CPMM
    /// takes its rents and its fee from the creator's lamports by System transfers, which the curve,
    /// an account with data, could not make. Signs the CPI.
    #[account(mut, seeds = [POOL_CREATOR_SEED, curve.mint.as_ref()], bump)]
    pub pool_creator: SystemAccount<'info>,

    /// CHECK: opened by the associated token program's `create_idempotent`, which derives it.
    /// The pool creator's USDC ATA: an ATA's address fixes its owner and its mint, so one a
    /// stranger opened first is the same account, taken as it is. anchor-spl's CPI names the ATA it
    /// derives from the pool creator and the mint, so any other account here fails it
    /// (`MissingAccount`) before a token moves.
    #[account(mut)]
    pub pool_creator_usdc: UncheckedAccount<'info>,

    /// CHECK: opened by `create_idempotent`, which derives it.
    /// The pool creator's ATA of the curve's token, as `pool_creator_usdc`.
    #[account(mut)]
    pub pool_creator_token: UncheckedAccount<'info>,

    /// CHECK: bound in the handler to `usdc_config.raydium_cpmm_program`.
    /// Raydium's CPMM, the program the reserves go into.
    pub cpmm_program: UncheckedAccount<'info>,

    /// CHECK: checked in the handler by `raydium::check_amm_config`.
    /// The CPMM's 1% fee tier, `usdc_config.raydium_amm_config`, still at 1% and creating pools.
    pub amm_config: UncheckedAccount<'info>,

    /// CHECK: CPMM's `["vault_and_lp_mint_auth_seed"]`, whose seeds CPMM checks.
    pub cpmm_authority: UncheckedAccount<'info>,

    /// CHECK: our PDA, `["cpmm_pool", mint]`, uninitialized on entry: CPMM creates the pool there,
    /// the program signing for it.
    #[account(mut, seeds = [CPMM_POOL_SEED, curve.mint.as_ref()], bump)]
    pub pool_state: UncheckedAccount<'info>,

    /// CHECK: CPMM's `["pool_lp_mint", pool_state]`, which CPMM creates and checks.
    #[account(mut)]
    pub lp_mint: UncheckedAccount<'info>,

    /// CHECK: the pool creator's ATA of the LP mint, which CPMM creates (an `init` it binds to the
    /// creator and the LP mint), read after the CPI for the LP the pool minted.
    #[account(mut)]
    pub pool_creator_lp: UncheckedAccount<'info>,

    /// CHECK: CPMM's `["pool_vault", pool_state, usdc_mint]`, which CPMM creates and checks.
    #[account(mut)]
    pub pool_usdc_vault: UncheckedAccount<'info>,

    /// CHECK: CPMM's `["pool_vault", pool_state, token_mint]`, which CPMM creates and checks.
    #[account(mut)]
    pub pool_token_vault: UncheckedAccount<'info>,

    /// CHECK: CPMM binds it to its own fee receiver, a constant of its code
    /// (`create_pool_fee_reveiver::ID`), so the program takes it as given (2026-10-24): a pin to
    /// `usdc_config.raydium_create_pool_fee` refused every graduation for the 48 hours a config
    /// change takes, should Raydium move it, and the fee is the caller's lamports, never a curve's.
    /// The wrapped-SOL account Raydium takes its pool-creation fee into.
    #[account(mut)]
    pub create_pool_fee: UncheckedAccount<'info>,

    /// CHECK: CPMM's `["observation", pool_state]`, which CPMM creates and checks.
    #[account(mut)]
    pub observation: UncheckedAccount<'info>,

    /// Classic SPL Token: USDC and the curve's token are both classic mints, so it is CPMM's
    /// `token_program`, `token_0_program` and `token_1_program` alike.
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

/// Creates the curve's pool, in this order (§10.3.9): the two USDC pauses (6000, 6001), the phase
/// (`NotGraduating`), the five Raydium addresses set (`RaydiumNotSet`), the CPMM program the
/// config's (`AccountMismatch`), the `amm_config` the config's, owned by that CPMM, at 1% and
/// creating pools (`AccountMismatch`, `GraduationFeeTierChanged`, `PoolCreationDisabled`), the
/// caller's `extra_lamports` at most `MAX_GRADUATION_EXTRA_LAMPORTS`
/// (`GraduationExtraFundingTooLarge`), the tier's `create_pool_fee` at most
/// `MAX_CREATE_POOL_FEE_LAMPORTS` (`CreatePoolFeeTooHigh`). Nothing moves before the last of them. Then the pool
/// creator's two ATAs opened idempotently, the caller paying; the counters, never the vaults'
/// balances, moved to them, the curve signing; the pool creator funded by the caller, the extra
/// included; CPMM's `initialize` with those amounts, the pool creator and the pool state signing;
/// the LP read from its account; every lamport left on the pool creator back to the caller. Emits
/// `GraduationPoolCreated`.
pub fn handler(ctx: Context<GraduateUsdcPool>, extra_lamports: u64) -> Result<()> {
    require!(!ctx.accounts.usdc_config.paused, FanexError::GloballyPaused);
    require!(!ctx.accounts.curve.paused, FanexError::CurvePaused);
    require!(
        ctx.accounts.curve.phase == PHASE_GRADUATING,
        FanexError::NotGraduating
    );

    let config = &ctx.accounts.usdc_config;
    require!(
        raydium_addresses_set(&config.raydium_addresses()),
        FanexError::RaydiumNotSet
    );
    require_keys_eq!(
        ctx.accounts.cpmm_program.key(),
        config.raydium_cpmm_program,
        FanexError::AccountMismatch
    );
    // Read again here, not only at `set_usdc_raydium`: Raydium's admin can change a tier's rate, or
    // stop its pool creation, after the address was set.
    let tier = {
        let amm_config = &ctx.accounts.amm_config;
        check_amm_config(
            amm_config.key,
            amm_config.owner,
            &amm_config.try_borrow_data()?,
            &config.raydium_amm_config,
            &config.raydium_cpmm_program,
        )?
    };
    require!(
        extra_lamports <= MAX_GRADUATION_EXTRA_LAMPORTS,
        FanexError::GraduationExtraFundingTooLarge
    );
    // The tier's fee is Raydium's admin's to set, with no time lock: bounded here, before anything
    // moves, so a raise never drains the caller (the internal audit, 2026-10-29).
    require!(
        tier.create_pool_fee <= MAX_CREATE_POOL_FEE_LAMPORTS,
        FanexError::CreatePoolFeeTooHigh
    );

    // ── The pool creator's two ATAs, opened when missing, taken as they are when not ──
    for (account, mint) in [
        (&ctx.accounts.pool_creator_usdc, &ctx.accounts.usdc_mint),
        (&ctx.accounts.pool_creator_token, &ctx.accounts.token_mint),
    ] {
        associated_token::create_idempotent(CpiContext::new(
            ctx.accounts.associated_token_program.to_account_info(),
            Create {
                payer: ctx.accounts.caller.to_account_info(),
                associated_token: account.to_account_info(),
                authority: ctx.accounts.pool_creator.to_account_info(),
                mint: mint.to_account_info(),
                system_program: ctx.accounts.system_program.to_account_info(),
                token_program: ctx.accounts.token_program.to_account_info(),
            },
        ))?;
    }

    // ── The counters to the pool creator, signed by the curve (trap 8) ──
    // What was sent straight to a vault stays behind, uncounted: `initialize` takes its amounts
    // as arguments, so what a stranger put in the pool creator's ATAs stays there too.
    let curve = &ctx.accounts.curve;
    let usdc_seeded = curve.real_usdc_reserves;
    let tokens_seeded = curve
        .real_token_reserves
        .checked_add(curve.migration_token_reserves)
        .ok_or(FanexError::MathOverflow)?;
    let mint = curve.mint;
    let curve_bump = curve.bump;
    let curve_seeds: &[&[&[u8]]] = &[&[CURVE_SEED, mint.as_ref(), &[curve_bump]]];
    let token_program = ctx.accounts.token_program.to_account_info();
    transfer_checked(
        &token_program,
        &ctx.accounts.usdc_mint.to_account_info(),
        &ctx.accounts.usdc_vault.to_account_info(),
        &ctx.accounts.pool_creator_usdc.to_account_info(),
        &ctx.accounts.curve.to_account_info(),
        usdc_seeded,
        USDC_MINT_DECIMALS,
        curve_seeds,
    )?;
    transfer_checked(
        &token_program,
        &ctx.accounts.token_mint.to_account_info(),
        &ctx.accounts.token_vault.to_account_info(),
        &ctx.accounts.pool_creator_token.to_account_info(),
        &ctx.accounts.curve.to_account_info(),
        tokens_seeded,
        TOKEN_DECIMALS,
        curve_seeds,
    )?;

    // ── What CPMM charges the pool creator: the tier's fee, the rents, the slack, the extra ──
    let funding = pool_creator_funding(tier.create_pool_fee, &ctx.accounts.rent, extra_lamports)
        .ok_or(FanexError::MathOverflow)?;
    system_program::transfer(
        CpiContext::new(
            ctx.accounts.system_program.to_account_info(),
            Transfer {
                from: ctx.accounts.caller.to_account_info(),
                to: ctx.accounts.pool_creator.to_account_info(),
            },
        ),
        funding,
    )?;

    // ── CPMM's `initialize`, token 0 the lower mint address ──
    let usdc_first = cpmm_token_0_first(&ctx.accounts.usdc_mint.key(), &mint);
    let (usdc_side, token_side) = (
        (
            ctx.accounts.usdc_mint.key(),
            ctx.accounts.pool_creator_usdc.key(),
            ctx.accounts.pool_usdc_vault.key(),
            usdc_seeded,
        ),
        (
            mint,
            ctx.accounts.pool_creator_token.key(),
            ctx.accounts.pool_token_vault.key(),
            tokens_seeded,
        ),
    );
    let (side_0, side_1) = if usdc_first {
        (usdc_side, token_side)
    } else {
        (token_side, usdc_side)
    };
    let token_program_id = ctx.accounts.token_program.key();
    let metas = CpmmInitializeAccounts {
        creator: ctx.accounts.pool_creator.key(),
        amm_config: ctx.accounts.amm_config.key(),
        authority: ctx.accounts.cpmm_authority.key(),
        pool_state: ctx.accounts.pool_state.key(),
        token_0_mint: side_0.0,
        token_1_mint: side_1.0,
        lp_mint: ctx.accounts.lp_mint.key(),
        creator_token_0: side_0.1,
        creator_token_1: side_1.1,
        creator_lp_token: ctx.accounts.pool_creator_lp.key(),
        token_0_vault: side_0.2,
        token_1_vault: side_1.2,
        create_pool_fee: ctx.accounts.create_pool_fee.key(),
        observation: ctx.accounts.observation.key(),
        token_program: token_program_id,
        token_0_program: token_program_id,
        token_1_program: token_program_id,
        associated_token_program: ctx.accounts.associated_token_program.key(),
        system_program: ctx.accounts.system_program.key(),
        rent: ctx.accounts.rent.key(),
    }
    .to_account_metas();
    let open_time =
        u64::try_from(Clock::get()?.unix_timestamp).map_err(|_| FanexError::MathOverflow)?;
    let initialize = Instruction {
        program_id: ctx.accounts.cpmm_program.key(),
        accounts: metas,
        data: cpmm_initialize_data(side_0.3, side_1.3, open_time).to_vec(),
    };
    let pool_creator_bump = ctx.bumps.pool_creator;
    let pool_state_bump = ctx.bumps.pool_state;
    let pool_creator_seeds: &[&[u8]] = &[POOL_CREATOR_SEED, mint.as_ref(), &[pool_creator_bump]];
    let pool_state_seeds: &[&[u8]] = &[CPMM_POOL_SEED, mint.as_ref(), &[pool_state_bump]];
    invoke_signed(
        &initialize,
        &[
            ctx.accounts.pool_creator.to_account_info(),
            ctx.accounts.amm_config.to_account_info(),
            ctx.accounts.cpmm_authority.to_account_info(),
            ctx.accounts.pool_state.to_account_info(),
            ctx.accounts.usdc_mint.to_account_info(),
            ctx.accounts.token_mint.to_account_info(),
            ctx.accounts.lp_mint.to_account_info(),
            ctx.accounts.pool_creator_usdc.to_account_info(),
            ctx.accounts.pool_creator_token.to_account_info(),
            ctx.accounts.pool_creator_lp.to_account_info(),
            ctx.accounts.pool_usdc_vault.to_account_info(),
            ctx.accounts.pool_token_vault.to_account_info(),
            ctx.accounts.create_pool_fee.to_account_info(),
            ctx.accounts.observation.to_account_info(),
            ctx.accounts.token_program.to_account_info(),
            ctx.accounts.associated_token_program.to_account_info(),
            ctx.accounts.system_program.to_account_info(),
            ctx.accounts.rent.to_account_info(),
            ctx.accounts.cpmm_program.to_account_info(),
        ],
        &[pool_creator_seeds, pool_state_seeds],
    )?;

    // ── The LP, read from its account: CPMM opened it inside the CPI, so no typed account of this
    // instruction holds its balance ──
    let lp_amount = {
        let lp = ctx.accounts.pool_creator_lp.to_account_info();
        require_keys_eq!(*lp.owner, token_program_id, FanexError::AccountMismatch);
        let account = SplTokenAccount::unpack(&lp.try_borrow_data()?)?;
        require_keys_eq!(
            account.owner,
            ctx.accounts.pool_creator.key(),
            FanexError::AccountMismatch
        );
        require_keys_eq!(
            account.mint,
            ctx.accounts.lp_mint.key(),
            FanexError::AccountMismatch
        );
        account.amount
    };

    // ── Every lamport left on the pool creator back to the caller: all of them, so the account
    // never ends between zero and rent-exempt, which would fail the transaction ──
    let left = ctx.accounts.pool_creator.lamports();
    if left > 0 {
        system_program::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.system_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.pool_creator.to_account_info(),
                    to: ctx.accounts.caller.to_account_info(),
                },
                &[pool_creator_seeds],
            ),
            left,
        )?;
    }

    // The reserve counters stay as they are until `graduate_usdc_lock` zeroes them: no swap runs
    // outside Bonding, and they say what seeded the pool.
    ctx.accounts.curve.phase = PHASE_POOL_CREATED;
    emit!(GraduationPoolCreated {
        mint,
        pool_state: ctx.accounts.pool_state.key(),
        usdc_seeded,
        tokens_seeded,
        lp_amount,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use anchor_lang::prelude::Pubkey;
    use anchor_lang::ToAccountMetas;

    /// The instruction's 24 accounts in the order a client builds them; the caller alone signs.
    #[test]
    fn the_caller_alone_signs_and_the_pool_accounts_are_writable() {
        let k: [Pubkey; 24] = std::array::from_fn(|_| Pubkey::new_unique());
        let metas = crate::accounts::GraduateUsdcPool {
            caller: k[0],
            usdc_config: k[1],
            curve: k[2],
            usdc_vault: k[3],
            token_vault: k[4],
            usdc_mint: k[5],
            token_mint: k[6],
            pool_creator: k[7],
            pool_creator_usdc: k[8],
            pool_creator_token: k[9],
            cpmm_program: k[10],
            amm_config: k[11],
            cpmm_authority: k[12],
            pool_state: k[13],
            lp_mint: k[14],
            pool_creator_lp: k[15],
            pool_usdc_vault: k[16],
            pool_token_vault: k[17],
            create_pool_fee: k[18],
            observation: k[19],
            token_program: k[20],
            associated_token_program: k[21],
            system_program: k[22],
            rent: k[23],
        }
        .to_account_metas(None);

        assert_eq!(metas.iter().map(|m| m.pubkey).collect::<Vec<_>>(), k);
        let signers: Vec<usize> = (0..24).filter(|&i| metas[i].is_signer).collect();
        assert_eq!(signers, [0]);
        let writable: Vec<usize> = (0..24).filter(|&i| metas[i].is_writable).collect();
        assert_eq!(
            writable,
            [0, 2, 3, 4, 7, 8, 9, 13, 14, 15, 16, 17, 18, 19],
            "the caller, the curve and its vaults, the pool creator and its ATAs, and what CPMM opens or pays into"
        );
    }
}
