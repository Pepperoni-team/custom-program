//! `graduate_usdc_lock()`: the second step of a USDC curve's graduation (USDC curve design §10.3.9,
//! decision 5), phase PoolCreated → Graduated. The program locks the pool creator's whole LP, by
//! CPI into Raydium's LP lock, which has no withdrawal, so the pool's liquidity stays in the pool
//! for good, and the Fee Key, the NFT that harvests the locked LP's fees, goes to
//! `UsdcConfig.lp_fee_owner`.
//!
//! Permissionless, as the first step: it chooses no destination, so anyone may crank it, the keeper
//! by default, in `graduate_usdc_pool`'s transaction or in one of its own. Between the two the LP
//! waits on the pool creator, a PDA only this program signs for, and it signs for it only here, in
//! the CPI to the lock the config names. The caller pays the lock's rents through the pool creator,
//! which pays them (so the lock spends no more than it is handed), and gets back what the lock does
//! not spend, an extra of up to 0.1 SOL included, as the first step's. Gated by both USDC pauses,
//! as the first step (§10.3.3).
//!
//! The CPI is encoded by hand (`raydium.rs`), as the pool's: Raydium's CPI crates pin Anchor 1.0.2,
//! this program is on 0.31.1. The LP mint and the pool's vaults are bound to the pool's own
//! `PoolState`, never to the config's CPMM, so a curve in PoolCreated keeps its pool whatever
//! `set_usdc_raydium` names after it (§10.3.10).
use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::Instruction;
use anchor_lang::solana_program::program::invoke_signed;
use anchor_lang::solana_program::program_pack::Pack;
use anchor_lang::system_program::{self, Transfer};
use anchor_spl::associated_token::{get_associated_token_address, AssociatedToken};
use anchor_spl::token::spl_token::state::Account as SplTokenAccount;
use anchor_spl::token::Token;

use crate::constants::*;
use crate::errors::FanexError;
use crate::events::GraduatedUsdc;
use crate::raydium::{
    lock_cp_liquidity_data, lock_funding, parse_pool_state, pool_vaults_by_role,
    raydium_addresses_set, LockCpLiquidityAccounts, MAX_GRADUATION_EXTRA_LAMPORTS,
};
use crate::state::*;

#[derive(Accounts)]
pub struct GraduateUsdcLock<'info> {
    /// Anyone, the keeper by default: hands the pool creator what the lock's accounts cost, and
    /// gets back what the lock does not spend.
    #[account(mut)]
    pub caller: Signer<'info>,

    /// The USDC side's config: its pause, the lock and its authority, the Fee Key's owner.
    #[account(seeds = [USDC_CONFIG_SEED], bump = usdc_config.bump)]
    pub usdc_config: Box<Account<'info, UsdcConfig>>,

    /// A `CurveUsdc` by its type: a SOL `Curve` here is refused by its discriminator (3002).
    #[account(mut, seeds = [CURVE_SEED, curve.mint.as_ref()], bump = curve.bump)]
    pub curve: Box<Account<'info, CurveUsdc>>,

    /// The pool's creator, `["pool_creator", mint]`, the LP's owner since `graduate_usdc_pool`:
    /// pays the lock's rents from what the caller hands it, and signs the CPI as the lock's payer
    /// and as the liquidity's owner.
    #[account(mut, seeds = [POOL_CREATOR_SEED, curve.mint.as_ref()], bump)]
    pub pool_creator: SystemAccount<'info>,

    /// CHECK: read in the handler: the pool creator's ATA of the pool's LP mint, where CPMM minted
    /// the LP, and no other account the pool creator may own.
    #[account(mut)]
    pub pool_creator_lp: UncheckedAccount<'info>,

    /// CHECK: bound in the handler to `usdc_config.raydium_lock_program`.
    /// Raydium's LP lock, the program the LP goes into.
    pub lock_program: UncheckedAccount<'info>,

    /// CHECK: bound in the handler to `usdc_config.raydium_lock_authority`: the lock's authority
    /// PDA, the owner of its LP vault.
    pub lock_authority: UncheckedAccount<'info>,

    /// CHECK: `usdc_config.lp_fee_owner`, who receives the Fee Key.
    #[account(address = usdc_config.lp_fee_owner @ FanexError::AccountMismatch)]
    pub fee_nft_owner: UncheckedAccount<'info>,

    /// CHECK: our PDA, `["fee_nft", mint]`, uninitialized on entry: the lock creates the Fee Key's
    /// mint there with no seeds of its own, so the program signs for it.
    #[account(mut, seeds = [FEE_NFT_SEED, curve.mint.as_ref()], bump)]
    pub fee_nft_mint: UncheckedAccount<'info>,

    /// CHECK: the Fee Key's token account, `fee_nft_owner`'s ATA of the Fee Key's mint, which the
    /// lock creates and checks.
    #[account(mut)]
    pub fee_nft_account: UncheckedAccount<'info>,

    /// CHECK: our PDA, `["cpmm_pool", mint]`: the pool `graduate_usdc_pool` created there, read in
    /// the handler as the `PoolState` its CPMM wrote, whichever CPMM the config names now.
    #[account(seeds = [CPMM_POOL_SEED, curve.mint.as_ref()], bump)]
    pub pool_state: UncheckedAccount<'info>,

    /// CHECK: the lock's record, `["locked_liquidity", fee_nft_mint]` under the lock, which the
    /// lock creates and checks.
    #[account(mut)]
    pub locked_liquidity: UncheckedAccount<'info>,

    /// CHECK: bound in the handler to the LP mint the pool records.
    pub lp_mint: UncheckedAccount<'info>,

    /// CHECK: the lock's LP vault, the lock authority's ATA of the LP mint, which the lock opens
    /// when missing and checks.
    #[account(mut)]
    pub locked_lp_vault: UncheckedAccount<'info>,

    /// CHECK: bound in the handler to the USDC vault the pool records.
    #[account(mut)]
    pub pool_usdc_vault: UncheckedAccount<'info>,

    /// CHECK: bound in the handler to the token vault the pool records.
    #[account(mut)]
    pub pool_token_vault: UncheckedAccount<'info>,

    /// CHECK: the Fee Key's Metaplex metadata address, by its seeds under Metaplex: the lock writes
    /// it only when asked for metadata, never here (`with_metadata = false`), and does not check it
    /// then (LiteSVM: any address passes), so the program binds it rather than hand the lock a
    /// writable account the caller chose.
    #[account(
        mut,
        seeds = [b"metadata", anchor_spl::metadata::ID.as_ref(), fee_nft_mint.key().as_ref()],
        seeds::program = anchor_spl::metadata::ID,
        bump,
    )]
    pub metadata_account: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,

    /// CHECK: Metaplex Token Metadata, checked by its address alone, as the launch's.
    #[account(address = anchor_spl::metadata::ID)]
    pub metadata_program: UncheckedAccount<'info>,

    pub rent: Sysvar<'info, Rent>,
}

/// Locks the curve's LP, in this order (§10.3.9): the two USDC pauses (6000, 6001), the phase
/// (`PoolNotCreated`), the five Raydium addresses set (`RaydiumNotSet`), the lock and its authority
/// the config's (`AccountMismatch`), the pool at our PDA a `PoolState` created by the pool creator
/// over USDC and the curve's token, its LP mint and its two vaults the ones passed, the LP account
/// the pool creator's ATA of that LP mint (each `AccountMismatch`), the caller's `extra_lamports` at
/// most `MAX_GRADUATION_EXTRA_LAMPORTS` (`GraduationExtraFundingTooLarge`). Nothing moves before the
/// last of them. Then the pool creator funded by the caller, the extra included; the lock's
/// `lock_cp_liquidity` with the LP account's whole balance and no metadata, the pool creator and the
/// Fee Key's mint signing; every lamport left on the pool creator back to the caller; the curve
/// Graduated, its three reserve counters zeroed. Emits `GraduatedUsdc`.
pub fn handler(ctx: Context<GraduateUsdcLock>, extra_lamports: u64) -> Result<()> {
    require!(!ctx.accounts.usdc_config.paused, FanexError::GloballyPaused);
    require!(!ctx.accounts.curve.paused, FanexError::CurvePaused);
    require!(
        ctx.accounts.curve.phase == PHASE_POOL_CREATED,
        FanexError::PoolNotCreated
    );

    let config = &ctx.accounts.usdc_config;
    require!(
        raydium_addresses_set(&config.raydium_addresses()),
        FanexError::RaydiumNotSet
    );
    require_keys_eq!(
        ctx.accounts.lock_program.key(),
        config.raydium_lock_program,
        FanexError::AccountMismatch
    );
    require_keys_eq!(
        ctx.accounts.lock_authority.key(),
        config.raydium_lock_authority,
        FanexError::AccountMismatch
    );

    // ── The pool's own records, never the config's CPMM: only the CPMM `graduate_usdc_pool`
    // called could write at our PDA, which no one else can sign for ──
    let mint = ctx.accounts.curve.mint;
    let pool_creator = ctx.accounts.pool_creator.key();
    let pool = parse_pool_state(&ctx.accounts.pool_state.try_borrow_data()?)
        .ok_or(FanexError::AccountMismatch)?;
    let (usdc_vault, token_vault) =
        pool_vaults_by_role(&pool, &pool_creator, &config.usdc_mint, &mint)?;
    require_keys_eq!(
        ctx.accounts.lp_mint.key(),
        pool.lp_mint,
        FanexError::AccountMismatch
    );
    require_keys_eq!(
        ctx.accounts.pool_usdc_vault.key(),
        usdc_vault,
        FanexError::AccountMismatch
    );
    require_keys_eq!(
        ctx.accounts.pool_token_vault.key(),
        token_vault,
        FanexError::AccountMismatch
    );

    // ── The LP: the whole balance of the pool creator's ATA of the LP mint, where CPMM minted it.
    // Any other token account the pool creator owns, which anyone may open with a few LP in it, is
    // refused: locking it would mark the curve Graduated with its LP left unlocked ──
    let token_program_id = ctx.accounts.token_program.key();
    let lp_amount = {
        let lp = ctx.accounts.pool_creator_lp.to_account_info();
        require_keys_eq!(
            lp.key(),
            get_associated_token_address(&pool_creator, &pool.lp_mint),
            FanexError::AccountMismatch
        );
        require_keys_eq!(*lp.owner, token_program_id, FanexError::AccountMismatch);
        let account = SplTokenAccount::unpack(&lp.try_borrow_data()?)?;
        account.amount
    };
    require!(
        extra_lamports <= MAX_GRADUATION_EXTRA_LAMPORTS,
        FanexError::GraduationExtraFundingTooLarge
    );

    // ── What the lock charges its payer: the rents of what it opens, the slack, the extra ──
    let funding =
        lock_funding(&ctx.accounts.rent, extra_lamports).ok_or(FanexError::MathOverflow)?;
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

    // ── The lock's `lock_cp_liquidity`: the whole LP, no metadata, the pool's vaults in its order ──
    let usdc_first = pool.token_0_vault == usdc_vault;
    let (vault_0, vault_1) = if usdc_first {
        (
            &ctx.accounts.pool_usdc_vault,
            &ctx.accounts.pool_token_vault,
        )
    } else {
        (
            &ctx.accounts.pool_token_vault,
            &ctx.accounts.pool_usdc_vault,
        )
    };
    let metas = LockCpLiquidityAccounts {
        authority: ctx.accounts.lock_authority.key(),
        payer: pool_creator,
        liquidity_owner: pool_creator,
        fee_nft_owner: ctx.accounts.fee_nft_owner.key(),
        fee_nft_mint: ctx.accounts.fee_nft_mint.key(),
        fee_nft_account: ctx.accounts.fee_nft_account.key(),
        pool_state: ctx.accounts.pool_state.key(),
        locked_liquidity: ctx.accounts.locked_liquidity.key(),
        lp_mint: pool.lp_mint,
        liquidity_owner_lp: ctx.accounts.pool_creator_lp.key(),
        locked_lp_vault: ctx.accounts.locked_lp_vault.key(),
        token_0_vault: vault_0.key(),
        token_1_vault: vault_1.key(),
        metadata_account: ctx.accounts.metadata_account.key(),
        rent: ctx.accounts.rent.key(),
        system_program: ctx.accounts.system_program.key(),
        token_program: token_program_id,
        associated_token_program: ctx.accounts.associated_token_program.key(),
        metadata_program: ctx.accounts.metadata_program.key(),
    }
    .to_account_metas();
    let lock = Instruction {
        program_id: ctx.accounts.lock_program.key(),
        accounts: metas,
        data: lock_cp_liquidity_data(lp_amount, false).to_vec(),
    };
    let pool_creator_bump = ctx.bumps.pool_creator;
    let fee_nft_mint_bump = ctx.bumps.fee_nft_mint;
    let pool_creator_seeds: &[&[u8]] = &[POOL_CREATOR_SEED, mint.as_ref(), &[pool_creator_bump]];
    let fee_nft_mint_seeds: &[&[u8]] = &[FEE_NFT_SEED, mint.as_ref(), &[fee_nft_mint_bump]];
    invoke_signed(
        &lock,
        &[
            ctx.accounts.lock_authority.to_account_info(),
            ctx.accounts.pool_creator.to_account_info(),
            ctx.accounts.fee_nft_owner.to_account_info(),
            ctx.accounts.fee_nft_mint.to_account_info(),
            ctx.accounts.fee_nft_account.to_account_info(),
            ctx.accounts.pool_state.to_account_info(),
            ctx.accounts.locked_liquidity.to_account_info(),
            ctx.accounts.lp_mint.to_account_info(),
            ctx.accounts.pool_creator_lp.to_account_info(),
            ctx.accounts.locked_lp_vault.to_account_info(),
            ctx.accounts.pool_usdc_vault.to_account_info(),
            ctx.accounts.pool_token_vault.to_account_info(),
            ctx.accounts.metadata_account.to_account_info(),
            ctx.accounts.rent.to_account_info(),
            ctx.accounts.system_program.to_account_info(),
            ctx.accounts.token_program.to_account_info(),
            ctx.accounts.associated_token_program.to_account_info(),
            ctx.accounts.metadata_program.to_account_info(),
            ctx.accounts.lock_program.to_account_info(),
        ],
        &[pool_creator_seeds, fee_nft_mint_seeds],
    )?;

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

    // The counters say what the pool was seeded with until here, as the SOL side's `graduate`
    // leaves them; the lock ends the curve's part.
    let curve = &mut ctx.accounts.curve;
    curve.phase = PHASE_GRADUATED;
    curve.graduated_at = Clock::get()?.unix_timestamp;
    curve.real_usdc_reserves = 0;
    curve.real_token_reserves = 0;
    curve.migration_token_reserves = 0;
    emit!(GraduatedUsdc {
        mint,
        pool_state: ctx.accounts.pool_state.key(),
        lp_locked: lp_amount,
        fee_nft_mint: ctx.accounts.fee_nft_mint.key(),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use anchor_lang::prelude::Pubkey;
    use anchor_lang::ToAccountMetas;

    /// The instruction's 22 accounts in the order a client builds them; the caller alone signs.
    #[test]
    fn the_caller_alone_signs_and_what_the_lock_writes_is_writable() {
        let k: [Pubkey; 22] = std::array::from_fn(|_| Pubkey::new_unique());
        let metas = crate::accounts::GraduateUsdcLock {
            caller: k[0],
            usdc_config: k[1],
            curve: k[2],
            pool_creator: k[3],
            pool_creator_lp: k[4],
            lock_program: k[5],
            lock_authority: k[6],
            fee_nft_owner: k[7],
            fee_nft_mint: k[8],
            fee_nft_account: k[9],
            pool_state: k[10],
            locked_liquidity: k[11],
            lp_mint: k[12],
            locked_lp_vault: k[13],
            pool_usdc_vault: k[14],
            pool_token_vault: k[15],
            metadata_account: k[16],
            token_program: k[17],
            associated_token_program: k[18],
            system_program: k[19],
            metadata_program: k[20],
            rent: k[21],
        }
        .to_account_metas(None);

        assert_eq!(metas.iter().map(|m| m.pubkey).collect::<Vec<_>>(), k);
        let signers: Vec<usize> = (0..22).filter(|&i| metas[i].is_signer).collect();
        assert_eq!(signers, [0]);
        let writable: Vec<usize> = (0..22).filter(|&i| metas[i].is_writable).collect();
        assert_eq!(
            writable,
            [0, 2, 3, 4, 8, 9, 11, 13, 14, 15, 16],
            "the caller, the curve, the pool creator and its LP, and what the lock opens or writes"
        );
    }
}
