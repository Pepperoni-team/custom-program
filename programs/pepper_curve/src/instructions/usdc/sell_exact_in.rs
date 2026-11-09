//! `sell_exact_in(tokens_in, min_usdc_out)`: a sale on a USDC curve (USDC curve design §10.3.6). It
//! calls the math crate's `quote_sell` and moves exactly what it returns, as `buy_exact_in` does
//! with `quote_buy` (§10.4).
//!
//! A fixed list of 11 accounts a router can call (decision 3): no `init_if_needed`, no
//! `remaining_accounts`, no system or associated-token program, and no `Option`. ⚠️ The field order
//! below is the instruction's account order, which the Jupiter SDK builds its metas in (§10.5):
//! never reorder, insert or drop a field.
use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};
use pepper_curve_math::quote_sell;

use crate::constants::*;
use crate::errors::FanexError;
use crate::events::TradeUsdc;
use crate::state::*;
use crate::transfer::transfer_checked;

#[derive(Accounts)]
pub struct SellExactIn<'info> {
    /// 0. The user, or a router's program authority: the only signer, and not the fee payer.
    pub user_transfer_authority: Signer<'info>,

    /// 1. The tokens sold, owned by account 0: a delegate of the owner is refused.
    #[account(
        mut,
        token::mint = token_mint,
        token::authority = user_transfer_authority,
    )]
    pub user_source_token: Box<Account<'info, TokenAccount>>,

    /// 2. Where the proceeds go: any USDC account, any owner, inside the launch window too (a sale
    /// has no launch rule).
    #[account(mut, token::mint = usdc_mint)]
    pub user_destination_usdc: Box<Account<'info, TokenAccount>>,

    /// 3. The USDC side's config: its pause, its mint and its fee account.
    #[account(seeds = [USDC_CONFIG_SEED], bump = usdc_config.bump)]
    pub usdc_config: Box<Account<'info, UsdcConfig>>,

    /// 4. A `CurveUsdc` by its type: a SOL `Curve` here is refused by its discriminator (3002).
    #[account(mut, seeds = [CURVE_SEED, curve.mint.as_ref()], bump = curve.bump)]
    pub curve: Box<Account<'info, CurveUsdc>>,

    /// 5. The curve's reserve, which pays the proceeds and the fee, the curve signing. Its balance is
    /// never read: a sale is bounded by `real_usdc_reserves`, so USDC sent straight to it pays no one.
    #[account(
        mut,
        seeds = [USDC_VAULT_SEED, curve.mint.as_ref()],
        bump = curve.usdc_vault_bump,
    )]
    pub usdc_vault: Box<Account<'info, TokenAccount>>,

    /// 6. Where the tokens sold go back: the curve PDA's associated token account of the mint.
    #[account(
        mut,
        associated_token::mint = token_mint,
        associated_token::authority = curve,
    )]
    pub token_vault: Box<Account<'info, TokenAccount>>,

    /// 7. The fee wallet's USDC account: the 1% fee, out of `usdc_vault`.
    #[account(mut, address = usdc_config.fee_usdc_account @ FanexError::AccountMismatch)]
    pub fee_usdc_account: Box<Account<'info, TokenAccount>>,

    /// 8. The USDC mint the config names, for `transfer_checked`.
    #[account(address = usdc_config.usdc_mint @ FanexError::AccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    /// 9. The curve's token mint, for `transfer_checked`.
    #[account(address = curve.mint @ FanexError::AccountMismatch)]
    pub token_mint: Box<Account<'info, Mint>>,

    /// 10. Classic SPL Token: USDC and the curve's token are both classic mints.
    pub token_program: Program<'info, Token>,
}

/// Sells `tokens_in` for at least `min_usdc_out` µUSDC after the fee, in today's `sell` order
/// (§10.3.6): the two pauses and the phase; the crate's quote (`ZeroAmount` for nothing in or
/// nothing out, `InsufficientReserves` past the curve's real reserve, `MathOverflow`); the slippage
/// on the net. Then the quote's amounts move, each by `transfer_checked`, a zero one skipped: the
/// tokens into `token_vault` (account 0 signs), the proceeds to the destination and the fee to the
/// fee account, both out of `usdc_vault` (the curve signs). No payout transit: a token transfer shows
/// on every explorer. No creator or Fanex share and no launch cap on a sale, as today.
pub fn handler(ctx: Context<SellExactIn>, tokens_in: u64, min_usdc_out: u64) -> Result<()> {
    require!(!ctx.accounts.usdc_config.paused, FanexError::GloballyPaused);
    require!(!ctx.accounts.curve.paused, FanexError::CurvePaused);
    require!(
        ctx.accounts.curve.phase == PHASE_BONDING,
        FanexError::NotBonding
    );

    let quote =
        quote_sell(&ctx.accounts.curve.swap_state(), tokens_in).map_err(FanexError::from)?;
    // Against the net the seller receives, as `sell` checks it.
    require!(quote.proceeds >= min_usdc_out, FanexError::SlippageExceeded);

    let token_program = ctx.accounts.token_program.to_account_info();
    transfer_checked(
        &token_program,
        &ctx.accounts.token_mint.to_account_info(),
        &ctx.accounts.user_source_token.to_account_info(),
        &ctx.accounts.token_vault.to_account_info(),
        &ctx.accounts.user_transfer_authority.to_account_info(),
        tokens_in,
        TOKEN_DECIMALS,
        &[],
    )?;

    let mint = ctx.accounts.curve.mint;
    let curve_bump = ctx.accounts.curve.bump;
    let signer_seeds: &[&[&[u8]]] = &[&[CURVE_SEED, mint.as_ref(), &[curve_bump]]];
    let usdc_mint = ctx.accounts.usdc_mint.to_account_info();
    let usdc_vault = ctx.accounts.usdc_vault.to_account_info();
    let curve_authority = ctx.accounts.curve.to_account_info();
    transfer_checked(
        &token_program,
        &usdc_mint,
        &usdc_vault,
        &ctx.accounts.user_destination_usdc.to_account_info(),
        &curve_authority,
        quote.proceeds,
        USDC_MINT_DECIMALS,
        signer_seeds,
    )?;
    transfer_checked(
        &token_program,
        &usdc_mint,
        &usdc_vault,
        &ctx.accounts.fee_usdc_account.to_account_info(),
        &curve_authority,
        quote.platform_fee,
        USDC_MINT_DECIMALS,
        signer_seeds,
    )?;

    // The counters the crate computed, never the vaults' balances. A sale never graduates.
    let curve = &mut ctx.accounts.curve;
    curve.set_swap_counters(&quote.after);

    emit!(TradeUsdc {
        mint,
        authority: ctx.accounts.user_transfer_authority.key(),
        // Account 0 itself, which must own the source.
        token_owner: ctx.accounts.user_source_token.owner,
        is_buy: false,
        usdc_amount: quote.reserve_out,
        token_amount: tokens_in,
        creator_fee: 0,
        fanex_fee: 0,
        platform_fee: quote.platform_fee,
        real_usdc_reserves: curve.real_usdc_reserves,
        real_token_reserves: curve.real_token_reserves,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use anchor_lang::prelude::Pubkey;
    use anchor_lang::ToAccountMetas;

    /// The 11 positions a router's metas are built in (§10.3.6, §10.5): a reorder breaks every
    /// Jupiter route, so it fails here first.
    #[test]
    fn the_sale_takes_eleven_accounts_in_the_spec_s_order() {
        let mut keys: [Pubkey; 11] = std::array::from_fn(|_| Pubkey::new_unique());
        keys[10] = anchor_spl::token::ID;
        let metas = crate::accounts::SellExactIn {
            user_transfer_authority: keys[0],
            user_source_token: keys[1],
            user_destination_usdc: keys[2],
            usdc_config: keys[3],
            curve: keys[4],
            usdc_vault: keys[5],
            token_vault: keys[6],
            fee_usdc_account: keys[7],
            usdc_mint: keys[8],
            token_mint: keys[9],
            token_program: keys[10],
        }
        .to_account_metas(None);

        assert_eq!(metas.iter().map(|m| m.pubkey).collect::<Vec<_>>(), keys);
        // Account 0 alone signs, and pays nothing: the fee payer is the transaction's own.
        let signers: Vec<usize> = (0..11).filter(|&i| metas[i].is_signer).collect();
        assert_eq!(signers, [0]);
        let writable: Vec<usize> = (0..11).filter(|&i| metas[i].is_writable).collect();
        assert_eq!(writable, [1, 2, 4, 5, 6, 7]);
    }
}
