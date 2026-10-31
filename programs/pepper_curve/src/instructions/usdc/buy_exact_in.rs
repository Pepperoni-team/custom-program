//! `buy_exact_in(usdc_in, min_tokens_out)`: a buy on a USDC curve (USDC curve design §10.3.5), the
//! first handler that calls the math crate's `quote_buy` and moves exactly what it returns, so what
//! a router quotes from the accounts and what the program executes are one function (§10.4).
//!
//! A fixed list of 13 accounts a router can call (decision 3): no `init_if_needed`, no
//! `remaining_accounts`, no system or associated-token program, and an absent `Option` passed as the
//! program's own ID, so no position ever moves. ⚠️ The field order below is the instruction's account
//! order, which the Jupiter SDK builds its metas in (§10.5): never reorder, insert or drop a field.
use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};
use pepper_curve_math::quote_buy;

use crate::constants::*;
use crate::errors::FanexError;
use crate::events::{GraduationReadyUsdc, TradeUsdc};
use crate::state::*;
use crate::transfer::transfer_checked;
use crate::validation::{
    creator_usdc_account_required, launch_cap_allows, launch_destination_allowed,
    launch_guard_waived,
};

#[derive(Accounts)]
pub struct BuyExactIn<'info> {
    /// 0. The user, or a router's program authority: the only signer, and not the fee payer.
    pub user_transfer_authority: Signer<'info>,

    /// 1. The USDC that pays, owned by account 0.
    #[account(
        mut,
        token::mint = usdc_mint,
        token::authority = user_transfer_authority,
    )]
    pub user_source_usdc: Box<Account<'info, TokenAccount>>,

    /// 2. Where the tokens go: any owner, but inside the launch window only account 0's associated
    /// token account of the mint (`LaunchDestinationNotOwnAta`, checked in the handler).
    #[account(mut, token::mint = token_mint)]
    pub user_destination_token: Box<Account<'info, TokenAccount>>,

    /// 3. The USDC side's config: its pause, its mint and its fee account.
    #[account(seeds = [USDC_CONFIG_SEED], bump = usdc_config.bump)]
    pub usdc_config: Box<Account<'info, UsdcConfig>>,

    /// 4. A `CurveUsdc` by its type: a SOL `Curve` here is refused by its discriminator (3002).
    #[account(mut, seeds = [CURVE_SEED, curve.mint.as_ref()], bump = curve.bump)]
    pub curve: Box<Account<'info, CurveUsdc>>,

    /// 5. The curve's reserve, a PDA token account whose authority is the curve. Its balance is never
    /// read: the price runs on the curve's counters, so USDC sent straight to it moves nothing.
    #[account(
        mut,
        seeds = [USDC_VAULT_SEED, curve.mint.as_ref()],
        bump = curve.usdc_vault_bump,
    )]
    pub usdc_vault: Box<Account<'info, TokenAccount>>,

    /// 6. The tokens for sale: the curve PDA's associated token account of the mint.
    #[account(
        mut,
        associated_token::mint = token_mint,
        associated_token::authority = curve,
    )]
    pub token_vault: Box<Account<'info, TokenAccount>>,

    /// 7. The fee wallet's USDC account: the 1% fee and Fanex's raise share, in one transfer.
    #[account(mut, address = usdc_config.fee_usdc_account @ FanexError::AccountMismatch)]
    pub fee_usdc_account: Box<Account<'info, TokenAccount>>,

    /// 8. The creator's USDC account, where the creator's raise share is paid. Required, by the
    /// curve's counters, exactly while the creator's cap is open (`creator_usdc_account_required`,
    /// `CreatorUsdcAccountInvalid` without it); otherwise accepted if passed, still bound to the
    /// curve's record, and paid only a non-zero share.
    #[account(mut, address = curve.creator_usdc_account @ FanexError::AccountMismatch)]
    pub creator_usdc_account: Option<Box<Account<'info, TokenAccount>>>,

    /// 9. The USDC mint the config names, for `transfer_checked`.
    #[account(address = usdc_config.usdc_mint @ FanexError::AccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    /// 10. The curve's token mint, for `transfer_checked`.
    #[account(address = curve.mint @ FanexError::AccountMismatch)]
    pub token_mint: Box<Account<'info, Mint>>,

    /// 11. The curve's `LaunchGuardWaiver`, read only where the launch cap would refuse
    /// (`validation::launch_guard_waived`: the program's account, the waiver's discriminator, the
    /// curve's mint, its PDA), as `buy` reads its waiver; anything else passed here lifts nothing. A
    /// typed account would be read and seed-checked whenever passed.
    // `CHECK:` on a line of its own, last: Anchor's IDL drops every doc line that starts with it.
    /// CHECK: verified by `validation::launch_guard_waived`, only where the cap would refuse.
    pub launch_guard_waiver: Option<UncheckedAccount<'info>>,

    /// 12. Classic SPL Token: USDC and the curve's token are both classic mints.
    pub token_program: Program<'info, Token>,
}

/// Charges at most `usdc_in` µUSDC for at least `min_tokens_out` tokens, in today's `buy` order
/// (§10.3.5): the two pauses and the phase; the crate's quote (`ZeroAmount`, `CurveComplete`,
/// `MathOverflow`); account 8 by the counters; inside the launch window the signer's own ATA and
/// then the cap, lifted only by the curve's waiver; the slippage, then a zero delivery. Then the
/// quote's amounts move, each by `transfer_checked`, a zero one skipped: the creator's share, Fanex's
/// share with the fee, the curve's amount into `usdc_vault` (account 0 signs), the tokens out of
/// `token_vault` (the curve signs). A partial fill at graduation charges less than `usdc_in`; only
/// what is charged leaves the source.
pub fn handler(ctx: Context<BuyExactIn>, usdc_in: u64, min_tokens_out: u64) -> Result<()> {
    require!(!ctx.accounts.usdc_config.paused, FanexError::GloballyPaused);
    require!(!ctx.accounts.curve.paused, FanexError::CurvePaused);
    require!(
        ctx.accounts.curve.phase == PHASE_BONDING,
        FanexError::NotBonding
    );

    let now = Clock::get()?.unix_timestamp;
    let curve = &ctx.accounts.curve;
    let quote = quote_buy(&curve.swap_state(), usdc_in, now).map_err(FanexError::from)?;

    // Account 8 by the counters, never by this buy's own split: a tiny buy can round the creator's
    // share to 0 while the cap is open, and the router builds the list from the counters alone.
    if creator_usdc_account_required(
        curve.creator_split_bps,
        curve.creator_received_usdc,
        curve.creator_cap_usdc,
    ) {
        require!(
            ctx.accounts.creator_usdc_account.is_some(),
            FanexError::CreatorUsdcAccountInvalid
        );
    }

    // The launch window (300 s from `created_at`): the destination first, so the cap's holding is
    // the signer's own wallet's, then the cap on the final `tokens_out`, before any USDC moves.
    let destination = &ctx.accounts.user_destination_token;
    let authority = ctx.accounts.user_transfer_authority.key();
    require!(
        launch_destination_allowed(
            curve.created_at,
            now,
            &destination.key(),
            &authority,
            &curve.mint
        ),
        FanexError::LaunchDestinationNotOwnAta
    );
    if !launch_cap_allows(curve.created_at, now, destination.amount, quote.tokens_out) {
        let waived = ctx
            .accounts
            .launch_guard_waiver
            .as_ref()
            .is_some_and(|waiver| {
                launch_guard_waived(std::slice::from_ref(&waiver.to_account_info()), &curve.mint)
            });
        require!(waived, FanexError::LaunchWalletCapExceeded);
    }

    require!(
        quote.tokens_out >= min_tokens_out,
        FanexError::SlippageExceeded
    );
    require!(quote.tokens_out > 0, FanexError::ZeroAmount);

    let token_program = ctx.accounts.token_program.to_account_info();
    let usdc_mint = ctx.accounts.usdc_mint.to_account_info();
    let source = ctx.accounts.user_source_usdc.to_account_info();
    let buyer = ctx.accounts.user_transfer_authority.to_account_info();

    if quote.creator_amount > 0 {
        // Present whenever there is a share to pay: a non-zero share means the cap is still open.
        let creator_account = ctx
            .accounts
            .creator_usdc_account
            .as_ref()
            .ok_or(FanexError::CreatorUsdcAccountInvalid)?;
        transfer_checked(
            &token_program,
            &usdc_mint,
            &source,
            &creator_account.to_account_info(),
            &buyer,
            quote.creator_amount,
            USDC_MINT_DECIMALS,
            &[],
        )?;
    }
    let fee_out = quote
        .fanex_amount
        .checked_add(quote.platform_fee)
        .ok_or(FanexError::MathOverflow)?;
    transfer_checked(
        &token_program,
        &usdc_mint,
        &source,
        &ctx.accounts.fee_usdc_account.to_account_info(),
        &buyer,
        fee_out,
        USDC_MINT_DECIMALS,
        &[],
    )?;
    transfer_checked(
        &token_program,
        &usdc_mint,
        &source,
        &ctx.accounts.usdc_vault.to_account_info(),
        &buyer,
        quote.curve_amount,
        USDC_MINT_DECIMALS,
        &[],
    )?;

    let mint = ctx.accounts.curve.mint;
    let curve_bump = ctx.accounts.curve.bump;
    let signer_seeds: &[&[&[u8]]] = &[&[CURVE_SEED, mint.as_ref(), &[curve_bump]]];
    transfer_checked(
        &token_program,
        &ctx.accounts.token_mint.to_account_info(),
        &ctx.accounts.token_vault.to_account_info(),
        &ctx.accounts.user_destination_token.to_account_info(),
        &ctx.accounts.curve.to_account_info(),
        quote.tokens_out,
        TOKEN_DECIMALS,
        signer_seeds,
    )?;

    // The counters the crate computed, never the vaults' balances; the graduation trigger is its
    // `graduates`: the threshold reached, or the last token sold.
    let curve = &mut ctx.accounts.curve;
    curve.set_swap_counters(&quote.after);
    if quote.graduates {
        curve.phase = PHASE_GRADUATING;
        emit!(GraduationReadyUsdc {
            mint,
            real_usdc_reserves: curve.real_usdc_reserves,
        });
    }

    emit!(TradeUsdc {
        mint,
        authority,
        token_owner: ctx.accounts.user_destination_token.owner,
        is_buy: true,
        usdc_amount: quote.charged,
        token_amount: quote.tokens_out,
        creator_fee: quote.creator_amount,
        fanex_fee: quote.fanex_amount,
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

    fn accounts(
        creator: Option<Pubkey>,
        waiver: Option<Pubkey>,
        keys: &[Pubkey; 13],
    ) -> crate::accounts::BuyExactIn {
        crate::accounts::BuyExactIn {
            user_transfer_authority: keys[0],
            user_source_usdc: keys[1],
            user_destination_token: keys[2],
            usdc_config: keys[3],
            curve: keys[4],
            usdc_vault: keys[5],
            token_vault: keys[6],
            fee_usdc_account: keys[7],
            creator_usdc_account: creator,
            usdc_mint: keys[9],
            token_mint: keys[10],
            launch_guard_waiver: waiver,
            token_program: keys[12],
        }
    }

    /// The 13 positions a router's metas are built in (§10.3.5, §10.5): a reorder breaks every
    /// Jupiter route, so it fails here first.
    #[test]
    fn the_buy_takes_thirteen_accounts_in_the_spec_s_order() {
        let mut keys: [Pubkey; 13] = std::array::from_fn(|_| Pubkey::new_unique());
        keys[12] = anchor_spl::token::ID;
        let metas = accounts(Some(keys[8]), Some(keys[11]), &keys).to_account_metas(None);

        assert_eq!(metas.iter().map(|m| m.pubkey).collect::<Vec<_>>(), keys);
        // Account 0 alone signs, and pays nothing: the fee payer is the transaction's own.
        let signers: Vec<usize> = (0..13).filter(|&i| metas[i].is_signer).collect();
        assert_eq!(signers, [0]);
        let writable: Vec<usize> = (0..13).filter(|&i| metas[i].is_writable).collect();
        assert_eq!(writable, [1, 2, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn an_absent_option_holds_its_place_as_the_program_s_own_id() {
        let mut keys: [Pubkey; 13] = std::array::from_fn(|_| Pubkey::new_unique());
        keys[12] = anchor_spl::token::ID;
        let metas = accounts(None, None, &keys).to_account_metas(None);

        assert_eq!(metas.len(), 13);
        for i in [8, 11] {
            assert_eq!(metas[i].pubkey, crate::ID, "account {i}");
            assert!(!metas[i].is_signer && !metas[i].is_writable, "account {i}");
        }
        assert_eq!(metas[9].pubkey, keys[9]);
        assert_eq!(metas[12].pubkey, anchor_spl::token::ID);
    }
}
