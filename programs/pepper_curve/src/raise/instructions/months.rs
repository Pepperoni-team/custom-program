//! The monthly cycle (spec §4.6, §4.7, D11; amended spec §4; the 2026-10-19
//! design). Fanex declares what a month owes from the 15th of the next month;
//! the creator pays the oldest open month first (3% to Fanex, 97% to the fans'
//! accumulator, with no cap), and while the raise is active a payment before
//! that month's due date refills the guarantee first; from the 6th of the
//! month after, anyone settles the month, and the guarantee covers what the
//! oldest open month is still missing. Until that due date the attester can
//! lower a declared due and the admin correct it either way (`redeclare_month`,
//! the internal audit of 2026-10-29), and a late payment refills the guarantee
//! after the arrears.

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::raise::accrual::acc_increment;
use crate::raise::calendar::due_ts;
use crate::raise::checked::{add, sub};
use crate::raise::constants::*;
use crate::errors::FanexError;
use crate::raise::events::{GuaranteeReplenished, MonthDeclared, MonthFunded, MonthSettled, MonthWrittenOff};
use crate::raise::ledger::{
    check_declaration, check_redeclaration, check_settlement_order, has_open_months, is_overdue, next_streak,
    oldest_open_after_redeclaration, open_amount, refills_after_arrears,
};
use crate::raise::money::{allocate_payment, payment_plan, Payment};
use crate::raise::state::*;
use crate::raise::transfer::{usdc_transfer, RaiseSeeds};

/// `MonthFunded.month` of a payment not tied to a declared month.
pub const DIRECT_MONTH: u32 = u32::MAX;

/// Raises the accumulator by what reaches the fans.
pub fn credit_fans(raise: &mut Raise, plan: &Payment) -> Result<()> {
    if plan.credit == 0 {
        return Ok(());
    }
    let increment = acc_increment(plan.credit, raise.units_outstanding)?;
    raise.acc_per_unit = raise
        .acc_per_unit
        .checked_add(increment)
        .ok_or_else(|| error!(FanexError::RaiseMathOverflow))?;
    raise.credited_total = add(raise.credited_total, plan.credit)?;
    Ok(())
}

/// Payments keep arriving after close: arrears stay owed (spec §4.8).
fn require_paying_state(raise: &Raise) -> Result<()> {
    require!(
        matches!(raise.state, RaiseState::Active | RaiseState::Closed),
        FanexError::NotActive
    );
    Ok(())
}


/// What a payment may refill: what the guarantee is owed while the raise is
/// active, nothing once it is closed (close returned the guarantee to the
/// creator, and a payment then goes to the arrears or the fans only).
fn refillable(raise: &Raise) -> u64 {
    if raise.state == RaiseState::Active {
        raise.guarantee_owed
    } else {
        0
    }
}

/// Books what a payment moved into the guarantee vault: the counters move
/// exactly as the vault does (ruling P3).
fn rebuild_guarantee(raise: &mut Raise, raise_key: Pubkey, amount: u64) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    raise.guarantee_owed = sub(raise.guarantee_owed, amount)?;
    raise.guarantee_usdc = add(raise.guarantee_usdc, amount)?;
    emit!(GuaranteeReplenished { raise: raise_key, amount });
    Ok(())
}

#[derive(Accounts)]
#[instruction(month: u32)]
pub struct DeclareMonth<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(address = config.attester @ FanexError::RaiseUnauthorized)]
    pub attester: Signer<'info>,

    #[account(seeds = [RAISE_CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, RaiseConfig>>,

    #[account(mut)]
    pub raise: Box<Account<'info, Raise>>,

    #[account(
        init,
        payer = payer,
        space = 8 + Month::INIT_SPACE,
        seeds = [MONTH_SEED, raise.key().as_ref(), &month.to_le_bytes()],
        bump
    )]
    pub month_account: Box<Account<'info, Month>>,

    pub system_program: Program<'info, System>,
}

pub fn declare_month(ctx: Context<DeclareMonth>, month: u32, due: u64, estimated: bool) -> Result<()> {
    require!(!ctx.accounts.config.paused, FanexError::RaiseGloballyPaused);
    let now = Clock::get()?.unix_timestamp;
    let raise_key = ctx.accounts.raise.key();

    let raise = &mut ctx.accounts.raise;
    require!(raise.state == RaiseState::Active, FanexError::NotActive);
    check_declaration(month, raise.last_declared_month, raise.first_month, raise.term_end_month, now)?;
    raise.last_declared_month = month;
    raise.count_month_opened()?;
    if due == 0 && raise.oldest_open_month == month {
        raise.oldest_open_month = month + 1;
    }

    let month_account = &mut ctx.accounts.month_account;
    month_account.raise = raise_key;
    month_account.month = month;
    month_account.due = due;
    month_account.estimated = estimated;
    month_account.due_ts = due_ts(month);
    month_account.bump = ctx.bumps.month_account;

    emit!(MonthDeclared { raise: raise_key, month, due, estimated });
    Ok(())
}

#[derive(Accounts)]
pub struct RedeclareMonth<'info> {
    /// The attester, lowering a due it declared too high (a keeper's fault), or the admin, either
    /// way, correcting one a stolen attester key made; under the pause, the admin alone.
    pub signer: Signer<'info>,

    #[account(seeds = [RAISE_CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, RaiseConfig>>,

    #[account(mut)]
    pub raise: Box<Account<'info, Raise>>,

    #[account(mut, has_one = raise @ FanexError::RaiseAccountMismatch)]
    pub month_account: Box<Account<'info, Month>>,
}

/// A declared month's due, corrected (internal audit of 2026-10-29): until its due date, before
/// anything is settled or covered on it, never below what the creator already paid
/// (`ledger::check_redeclaration`). The attester only lowers a due: raising one takes the admin,
/// whose 48-hour proposal the creator sees coming, so the attester's key gains no way to make an
/// honest creator late at the last second. The pointer follows what the month now owes. Logs
/// `MonthDeclared` again, with the new due: the last one for a month is the one in force.
pub fn redeclare_month(ctx: Context<RedeclareMonth>, due: u64, estimated: bool) -> Result<()> {
    let signer = ctx.accounts.signer.key();
    let config = &ctx.accounts.config;
    let by_admin = signer == config.admin;
    require!(by_admin || signer == config.attester, FanexError::RaiseUnauthorized);
    require!(by_admin || !config.paused, FanexError::RaiseGloballyPaused);
    require!(by_admin || due <= ctx.accounts.month_account.due, FanexError::RaiseUnauthorized);
    let now = Clock::get()?.unix_timestamp;
    let raise_key = ctx.accounts.raise.key();

    let raise = &mut ctx.accounts.raise;
    require!(raise.state == RaiseState::Active, FanexError::NotActive);
    let m = &mut ctx.accounts.month_account;
    check_redeclaration(now, m.due_ts, m.finalized, m.covered, m.month, raise.last_declared_month, m.paid, due)?;
    m.due = due;
    m.estimated = estimated;
    let open = open_amount(m.due, m.paid, m.covered)?;
    raise.oldest_open_month = oldest_open_after_redeclaration(raise.oldest_open_month, m.month, open);

    emit!(MonthDeclared { raise: raise_key, month: m.month, due, estimated });
    Ok(())
}

#[derive(Accounts)]
pub struct FundMonth<'info> {
    pub creator: Signer<'info>,

    #[account(seeds = [RAISE_CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, RaiseConfig>>,

    #[account(address = config.usdc_mint @ FanexError::RaiseAccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    #[account(mut, has_one = creator @ FanexError::RaiseUnauthorized)]
    pub raise: Box<Account<'info, Raise>>,

    #[account(mut, has_one = raise @ FanexError::RaiseAccountMismatch)]
    pub month_account: Box<Account<'info, Month>>,

    #[account(
        mut,
        token::mint = usdc_mint,
        constraint = creator_usdc.owner == creator.key() @ FanexError::RaiseAccountMismatch
    )]
    pub creator_usdc: Box<Account<'info, TokenAccount>>,

    #[account(mut, seeds = [PAYOUTS_SEED, raise.key().as_ref()], bump = raise.payouts_bump)]
    pub payouts_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        token::mint = usdc_mint,
        constraint = fee_usdc.owner == config.fee_wallet @ FanexError::RaiseAccountMismatch
    )]
    pub fee_usdc: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,

    /// The guarantee a payment refills first, before a month not yet due
    /// (2026-10-19). Appended, so every account before it keeps its place.
    #[account(mut, seeds = [GUARANTEE_SEED, raise.key().as_ref()], bump = raise.guarantee_bump)]
    pub guarantee_vault: Box<Account<'info, TokenAccount>>,
}

/// Pays the oldest open month: once it is overdue, its arrears first and then
/// the guarantee with what is left, while the month after it is not settled
/// yet (the internal audit of 2026-10-29); before its due date, while the raise
/// is active, after refilling the guarantee up to what it is owed
/// (`money::allocate_payment`). A payment that only refills the guarantee
/// leaves the month as open as it was.
pub fn fund_month(ctx: Context<FundMonth>, amount: u64) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let raise_key = ctx.accounts.raise.key();
    let (month, open, overdue, owed) = {
        let raise = &ctx.accounts.raise;
        let m = &ctx.accounts.month_account;
        require_paying_state(raise)?;
        require!(m.month == raise.oldest_open_month, FanexError::NotOldestOpenMonth);
        let overdue = is_overdue(now, m.due_ts);
        let owed = if overdue && !refills_after_arrears(m.month, raise.last_finalized_month) { 0 } else { refillable(raise) };
        (m.month, open_amount(m.due, m.paid, m.covered)?, overdue, owed)
    };
    if open == 0 {
        // Nothing to pay here: move the pointer on and let the next month be paid.
        // No money moves, the guarantee's refill included: the next payment makes it.
        ctx.accounts.raise.oldest_open_month = month + 1;
        return Ok(());
    }
    let (to_guarantee, plan) = allocate_payment(amount, open, overdue, owed)?;
    require!(add(to_guarantee, plan.gross)? > 0, FanexError::RaiseZeroAmount);

    let token_program = ctx.accounts.token_program.to_account_info();
    let mint = ctx.accounts.usdc_mint.to_account_info();
    let from = ctx.accounts.creator_usdc.to_account_info();
    let creator = ctx.accounts.creator.to_account_info();
    usdc_transfer(&token_program, &mint, &from, &ctx.accounts.guarantee_vault.to_account_info(), &creator, to_guarantee, &[])?;
    usdc_transfer(&token_program, &mint, &from, &ctx.accounts.fee_usdc.to_account_info(), &creator, plan.fee, &[])?;
    usdc_transfer(&token_program, &mint, &from, &ctx.accounts.payouts_vault.to_account_info(), &creator, plan.credit, &[])?;

    rebuild_guarantee(&mut ctx.accounts.raise, raise_key, to_guarantee)?;
    if plan.gross == 0 {
        // Only the guarantee was refilled: the month stays as open as it was.
        return Ok(());
    }
    credit_fans(&mut ctx.accounts.raise, &plan)?;
    let m = &mut ctx.accounts.month_account;
    m.paid = add(m.paid, plan.gross)?;
    if !overdue {
        m.paid_on_time = add(m.paid_on_time, plan.gross)?;
    }
    m.credited = add(m.credited, plan.credit)?;
    let still_open = open_amount(m.due, m.paid, m.covered)?;

    if still_open == 0 {
        ctx.accounts.raise.oldest_open_month = month + 1;
    }
    emit!(MonthFunded { raise: raise_key, month, gross: plan.gross, fee: plan.fee, credit: plan.credit });
    Ok(())
}

#[derive(Accounts)]
pub struct FundDirect<'info> {
    pub creator: Signer<'info>,

    #[account(seeds = [RAISE_CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, RaiseConfig>>,

    #[account(address = config.usdc_mint @ FanexError::RaiseAccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    #[account(mut, has_one = creator @ FanexError::RaiseUnauthorized)]
    pub raise: Box<Account<'info, Raise>>,

    #[account(
        mut,
        token::mint = usdc_mint,
        constraint = creator_usdc.owner == creator.key() @ FanexError::RaiseAccountMismatch
    )]
    pub creator_usdc: Box<Account<'info, TokenAccount>>,

    #[account(mut, seeds = [PAYOUTS_SEED, raise.key().as_ref()], bump = raise.payouts_bump)]
    pub payouts_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        token::mint = usdc_mint,
        constraint = fee_usdc.owner == config.fee_wallet @ FanexError::RaiseAccountMismatch
    )]
    pub fee_usdc: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,

    /// The guarantee a payment refills first, before a month not yet due
    /// (2026-10-19). Appended, so every account before it keeps its place.
    #[account(mut, seeds = [GUARANTEE_SEED, raise.key().as_ref()], bump = raise.guarantee_bump)]
    pub guarantee_vault: Box<Account<'info, TokenAccount>>,
}

/// A payment with no declaration behind it (spec §6.4: the creator can pay even
/// if Fanex stops declaring). Refused while a declared month is still open
/// (spec §4.7): the oldest open month is always paid first, through
/// `fund_month`, before a direct payment can reach the fans. While the raise is
/// active it refills the guarantee first, up to what it is owed and with no
/// fee (the 2026-10-19 design); the rest reaches the fans with the same 3%.
pub fn fund_direct(ctx: Context<FundDirect>, amount: u64) -> Result<()> {
    let raise_key = ctx.accounts.raise.key();
    let owed = {
        let raise = &ctx.accounts.raise;
        require_paying_state(raise)?;
        require!(
            !has_open_months(raise.oldest_open_month, raise.last_declared_month),
            FanexError::OpenMonthsFirst
        );
        refillable(raise)
    };
    let (to_guarantee, plan) = allocate_payment(amount, u64::MAX, false, owed)?;
    require!(add(to_guarantee, plan.gross)? > 0, FanexError::RaiseZeroAmount);

    let token_program = ctx.accounts.token_program.to_account_info();
    let mint = ctx.accounts.usdc_mint.to_account_info();
    let from = ctx.accounts.creator_usdc.to_account_info();
    let creator = ctx.accounts.creator.to_account_info();
    usdc_transfer(&token_program, &mint, &from, &ctx.accounts.guarantee_vault.to_account_info(), &creator, to_guarantee, &[])?;
    usdc_transfer(&token_program, &mint, &from, &ctx.accounts.fee_usdc.to_account_info(), &creator, plan.fee, &[])?;
    usdc_transfer(&token_program, &mint, &from, &ctx.accounts.payouts_vault.to_account_info(), &creator, plan.credit, &[])?;

    rebuild_guarantee(&mut ctx.accounts.raise, raise_key, to_guarantee)?;
    if plan.gross > 0 {
        credit_fans(&mut ctx.accounts.raise, &plan)?;
        emit!(MonthFunded { raise: raise_key, month: DIRECT_MONTH, gross: plan.gross, fee: plan.fee, credit: plan.credit });
    }
    Ok(())
}

#[derive(Accounts)]
pub struct SettleMonth<'info> {
    pub caller: Signer<'info>,

    #[account(seeds = [RAISE_CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, RaiseConfig>>,

    #[account(address = config.usdc_mint @ FanexError::RaiseAccountMismatch)]
    pub usdc_mint: Box<Account<'info, Mint>>,

    #[account(mut)]
    pub raise: Box<Account<'info, Raise>>,

    #[account(mut, has_one = raise @ FanexError::RaiseAccountMismatch)]
    pub month_account: Box<Account<'info, Month>>,

    #[account(mut, seeds = [GUARANTEE_SEED, raise.key().as_ref()], bump = raise.guarantee_bump)]
    pub guarantee_vault: Box<Account<'info, TokenAccount>>,

    #[account(mut, seeds = [PAYOUTS_SEED, raise.key().as_ref()], bump = raise.payouts_bump)]
    pub payouts_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        token::mint = usdc_mint,
        constraint = fee_usdc.owner == config.fee_wallet @ FanexError::RaiseAccountMismatch
    )]
    pub fee_usdc: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

/// Permissionless, from the due date. Covers the oldest open month from the
/// guarantee (as much as `Raise.guarantee_usdc` says it holds, never the vault
/// balance: ruling P3) and records whether the creator paid it on time.
pub fn settle_month(ctx: Context<SettleMonth>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let raise_key = ctx.accounts.raise.key();
    let (month, is_oldest, open) = {
        let raise = &ctx.accounts.raise;
        let m = &ctx.accounts.month_account;
        require_paying_state(raise)?;
        require!(now >= m.due_ts, FanexError::NotDue);
        require!(!m.finalized, FanexError::MonthAlreadyFinalized);
        check_settlement_order(m.month, raise.last_finalized_month)?;
        (m.month, m.month == raise.oldest_open_month, open_amount(m.due, m.paid, m.covered)?)
    };

    let mut covered = 0u64;
    if is_oldest && open > 0 {
        let plan = payment_plan(ctx.accounts.raise.guarantee_usdc, open)?;
        if plan.gross > 0 {
            let raise_seeds = RaiseSeeds::of(&ctx.accounts.raise);
            let seeds = raise_seeds.seeds();
            let signer: &[&[&[u8]]] = &[&seeds];
            let token_program = ctx.accounts.token_program.to_account_info();
            let mint = ctx.accounts.usdc_mint.to_account_info();
            let from = ctx.accounts.guarantee_vault.to_account_info();
            let authority = ctx.accounts.raise.to_account_info();
            usdc_transfer(&token_program, &mint, &from, &ctx.accounts.fee_usdc.to_account_info(), &authority, plan.fee, signer)?;
            usdc_transfer(&token_program, &mint, &from, &ctx.accounts.payouts_vault.to_account_info(), &authority, plan.credit, signer)?;

            credit_fans(&mut ctx.accounts.raise, &plan)?;
            let raise = &mut ctx.accounts.raise;
            raise.guarantee_usdc = sub(raise.guarantee_usdc, plan.gross)?;
            raise.guarantee_owed = add(raise.guarantee_owed, plan.gross)?;
            let m = &mut ctx.accounts.month_account;
            m.covered = add(m.covered, plan.gross)?;
            m.credited = add(m.credited, plan.credit)?;
            covered = plan.gross;
        }
    }

    let m = &mut ctx.accounts.month_account;
    let still_open = open_amount(m.due, m.paid, m.covered)?;
    let on_time = m.paid_on_time >= m.due;
    m.finalized = true;

    let raise = &mut ctx.accounts.raise;
    if is_oldest && still_open == 0 {
        raise.oldest_open_month = month + 1;
    }
    raise.on_time_streak = next_streak(raise.on_time_streak, on_time);
    raise.last_finalized_month = month;

    emit!(MonthSettled { raise: raise_key, month, covered, on_time });
    Ok(())
}

/// Permissionless and available under the pause. No token accounts, transfers or position writes.
#[derive(Accounts)]
pub struct WriteOffMonth<'info> {
    pub caller: Signer<'info>,
    #[account(mut)]
    pub raise: Box<Account<'info, Raise>>,
    #[account(has_one = raise @ FanexError::RaiseAccountMismatch)]
    pub month_account: Box<Account<'info, Month>>,
}

pub fn write_off_month(ctx: Context<WriteOffMonth>) -> Result<()> {
    let m = &ctx.accounts.month_account;
    let raise = &mut ctx.accounts.raise;
    require!(crate::raise::ledger::month_write_off_allowed(raise.state, raise.units_outstanding,
        m.finalized, m.month, raise.oldest_open_month, raise.last_finalized_month), FanexError::MonthNotExtinguishable);
    let bit = crate::raise::ledger::month_write_off_bit(m.month, raise.first_month, raise.term_end_month)?;
    let written_off = open_amount(m.due, m.paid, m.covered)?;
    raise.written_off_months |= bit;
    raise.oldest_open_month = m.month.checked_add(1).ok_or_else(|| error!(FanexError::RaiseMathOverflow))?;
    emit!(MonthWrittenOff { raise: raise.key(), month: m.month, due: m.due, paid: m.paid,
        paid_on_time: m.paid_on_time, covered: m.covered, written_off });
    Ok(())
}
