//! On-chain monthly dividend distribution (merkle distributor).
//!
//! Off-chain, Fanex/creator snapshots eligible holders + computes each wallet's
//! SOL share from the linked video's revenue, builds a merkle tree over
//! (claimant, amount), and funds a `Distributor` with the total SOL. Holders
//! then `claim_dividend` permissionlessly with a proof. This structurally
//! retires the trusted-signature (B3) and double-pay (B2) risks of the old
//! signed-transfer flow: the program only pays what the committed root allows,
//! exactly once per wallet.
//!
//! Two formats share the distributor PDA. v1 (`Distributor`) marks a claim by
//! creating a `ClaimStatus` account, whose rent the claim's payer fronts until
//! the month is closed. v2 (`DistributorV2`) marks it with one bit of the
//! distributor's own registry, so a claim creates nothing and costs only its
//! fee. Each instruction takes one format's account and refuses the other.

use anchor_lang::prelude::*;
use anchor_lang::system_program;

use crate::constants::*;
use crate::errors::FanexError;
use crate::events::{ClaimStatusClosed, DistributorFunded, DistributorFunderSet, DividendClaimed};
use crate::math::{dividend_leaf, dividend_leaf_v2, verify_merkle_proof};
use crate::payout::pay_out;
use crate::state::*;
use crate::validation::{
    claim_registry_bytes, expiry_in_range, leaf_bit, leaf_count_in_range, may_fund_distributor_v1,
    may_fund_distributor_v2, may_rehome_distributor_funder,
};

#[derive(Accounts)]
#[instruction(month: i64)]
pub struct InitDistributor<'info> {
    #[account(mut)]
    pub funder: Signer<'info>,

    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, GlobalConfig>,

    /// CHECK: the mint the distribution is for (bound into the PDA + state only).
    pub mint: UncheckedAccount<'info>,

    /// The curve for `mint`. Derived from the mint, so a distributor can't be
    /// opened for a non-existent curve. Until 2026-10-05 it also authorized the
    /// creator as a v1 funder; a creator now funds only in v2, with Fanex's
    /// approval (`InitDistributorV2::approver`). Kept so the account list, and
    /// every client built on it, does not move.
    #[account(
        seeds = [CURVE_SEED, mint.key().as_ref()],
        bump = curve.bump,
    )]
    pub curve: Account<'info, Curve>,

    #[account(
        init,
        payer = funder,
        space = 8 + Distributor::INIT_SPACE,
        seeds = [DISTRIBUTOR_SEED, mint.key().as_ref(), &month.to_le_bytes()],
        bump
    )]
    pub distributor: Account<'info, Distributor>,

    pub system_program: Program<'info, System>,
}

pub fn init_distributor(
    ctx: Context<InitDistributor>,
    month: i64,
    merkle_root: [u8; 32],
    total_amount: u64,
    expires_at: i64,
) -> Result<()> {
    require!(!ctx.accounts.config.paused, FanexError::GloballyPaused);
    require!(total_amount > 0, FanexError::ZeroAmount);

    // ── Only Fanex's wallets open a v1 distributor (2026-10-05) ──
    // The distributor PDA is keyed solely by (mint, month) and is `init`-once, so
    // any funder who may open it may also squat it: commit a root of their own
    // (or an expiry of now), and the month can never be funded again. The
    // creator used to be the second allowed funder here, signing in the browser
    // a transaction nothing forced to be the one Fanex prepared. v1 has no
    // approval to check, so a creator now funds only through
    // `init_distributor_v2`, whose approver is Fanex; and since both versions
    // share this PDA, leaving v1 open to the creator would squat v2 as well.
    // Since 2026-10-08 (security card point 5) that is one config key alone: the
    // dividend payer funds only v2, where that key approves too, so the payer's
    // key alone, stolen, opens no month. The key is `config.graduation_authority`
    // since 2026-10-24 (the fee-wallet split), the server's, which approves every
    // v2 month too; the fee wallet until then, now receive-only. And no month is
    // held past `MAX_DISTRIBUTOR_EXPIRY_SECONDS`, whoever funds it.
    let funder = ctx.accounts.funder.key();
    require!(
        may_fund_distributor_v1(&funder, &ctx.accounts.config.graduation_authority),
        FanexError::Unauthorized
    );
    require!(expiry_in_range(expires_at, Clock::get()?.unix_timestamp), FanexError::ExpiryTooFar);

    let distributor = &mut ctx.accounts.distributor;
    distributor.mint = ctx.accounts.mint.key();
    distributor.month = month;
    distributor.merkle_root = merkle_root;
    distributor.total_amount = total_amount;
    distributor.claimed_amount = 0;
    distributor.funder = ctx.accounts.funder.key();
    distributor.expires_at = expires_at;
    distributor.bump = ctx.bumps.distributor;

    // Fund the distributor PDA with the SOL to be claimed.
    system_program::transfer(
        CpiContext::new(
            ctx.accounts.system_program.to_account_info(),
            system_program::Transfer {
                from: ctx.accounts.funder.to_account_info(),
                to: ctx.accounts.distributor.to_account_info(),
            },
        ),
        total_amount,
    )?;

    emit!(DistributorFunded {
        mint: ctx.accounts.distributor.mint,
        month,
        total_amount,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct ClaimDividend<'info> {
    #[account(mut)]
    pub claimant: Signer<'info>,

    /// Funds the `ClaimStatus` rent (~0.00128 SOL). In the ordinary flow this
    /// is the Fanex sponsor wallet, which already co-signs the claim tx as its
    /// fee payer — so a holder pays NOTHING to claim, and a 0-SOL wallet is no
    /// longer stuck. The client falls back to `rent_payer = claimant` when no
    /// sponsor key is configured; the program accepts any signer here because
    /// paying rent grants no authority — the proof and the seeds still bind
    /// everything to `claimant`.
    #[account(mut)]
    pub rent_payer: Signer<'info>,

    #[account(mut)]
    pub distributor: Account<'info, Distributor>,

    /// Existence marks this wallet as having claimed. `init` fails if it already
    /// exists → a wallet can claim its allocation exactly once.
    #[account(
        init,
        payer = rent_payer,
        space = 8 + ClaimStatus::INIT_SPACE,
        seeds = [CLAIM_SEED, distributor.key().as_ref(), claimant.key().as_ref()],
        bump
    )]
    pub claim_status: Account<'info, ClaimStatus>,

    pub system_program: Program<'info, System>,
}

pub fn claim_dividend<'info>(
    ctx: Context<'_, '_, '_, 'info, ClaimDividend<'info>>,
    amount: u64,
    proof: Vec<[u8; 32]>,
) -> Result<()> {
    require!(amount > 0, FanexError::ZeroAmount);

    let leaf = dividend_leaf(&ctx.accounts.claimant.key(), amount);
    require!(
        verify_merkle_proof(&proof, ctx.accounts.distributor.merkle_root, leaf),
        FanexError::InvalidProof
    );

    let new_claimed = ctx
        .accounts
        .distributor
        .claimed_amount
        .checked_add(amount)
        .ok_or(FanexError::MathOverflow)?;
    require!(
        new_claimed <= ctx.accounts.distributor.total_amount,
        FanexError::ExceedsFundedTotal
    );

    // Pay out of the program-owned distributor PDA (the bound `amount <=
    // remaining funded` keeps it rent-exempt), through the payout transit when
    // the client passed it, so it shows as SOL received (`crate::payout`).
    pay_out(
        &ctx.accounts.distributor.to_account_info(),
        &ctx.accounts.claimant.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        ctx.remaining_accounts,
        amount,
    )?;

    ctx.accounts.distributor.claimed_amount = new_claimed;

    let claim_status = &mut ctx.accounts.claim_status;
    claim_status.claimant = ctx.accounts.claimant.key();
    claim_status.amount = amount;
    claim_status.claimed_at = Clock::get()?.unix_timestamp;
    claim_status.rent_payer = ctx.accounts.rent_payer.key();

    emit!(DividendClaimed {
        mint: ctx.accounts.distributor.mint,
        month: ctx.accounts.distributor.month,
        claimant: ctx.accounts.claimant.key(),
        amount,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct CloseDistributor<'info> {
    #[account(
        mut,
        close = funder,
        has_one = funder @ FanexError::Unauthorized,
    )]
    pub distributor: Account<'info, Distributor>,

    #[account(mut)]
    pub funder: Signer<'info>,
}

/// After expiry, the funder reclaims any unclaimed SOL (+ the PDA rent).
pub fn close_distributor(ctx: Context<CloseDistributor>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let d = &ctx.accounts.distributor;
    require!(
        crate::validation::may_close_distributor(
            d.claimed_amount,
            d.total_amount,
            now,
            d.expires_at
        ),
        FanexError::NotExpired
    );
    Ok(())
}

#[derive(Accounts)]
pub struct SetDistributorFunder<'info> {
    /// The month's recorded funder: only who funded a month hands its close over.
    pub funder: Signer<'info>,

    #[account(mut, has_one = funder @ FanexError::Unauthorized)]
    pub distributor: Account<'info, Distributor>,
}

/// Hands a v1 month's close over to the dividend payer (2026-10-24, the fee-wallet
/// split; Rick, 2026-10-23: «Sì, nell'upgrade»): the months the hot wallet funded
/// then close with the payer's key, and their leftovers return to its float, so
/// the hot key can be retired after the migration. `new_funder` may be
/// `DIVIDEND_PAYER` alone (`Unauthorized` otherwise). Nothing else moves: the root,
/// the claims, the expiry and the lamports stay as they are, and a v1 claim's
/// `ClaimStatus` keeps its own rent payer. Never paused: it moves no funds. Emits
/// `DistributorFunderSet`.
pub fn set_distributor_funder(ctx: Context<SetDistributorFunder>, new_funder: Pubkey) -> Result<()> {
    require!(may_rehome_distributor_funder(&new_funder), FanexError::Unauthorized);
    let distributor = &mut ctx.accounts.distributor;
    let previous = distributor.funder;
    distributor.funder = new_funder;
    emit!(DistributorFunderSet {
        mint: distributor.mint,
        month: distributor.month,
        previous,
        funder: new_funder,
    });
    Ok(())
}

#[derive(Accounts)]
pub struct CloseClaimStatus<'info> {
    /// CHECK: parsed by hand, deliberately. Two formats exist on chain (56-byte
    /// pre-upgrade, 88-byte with `rent_payer`) and `Account<ClaimStatus>` can
    /// only read the new one; owner, discriminator and seeds are all verified
    /// in the handler.
    #[account(mut)]
    pub claim_status: UncheckedAccount<'info>,

    /// CHECK: the distributor ADDRESS this ClaimStatus was seeded on. It must be
    /// DEAD (its account closed): while the distributor lives, the ClaimStatus
    /// is the double-claim guard and closing it would re-open the claim.
    pub distributor: UncheckedAccount<'info>,

    /// CHECK: receives the rent. Must equal the recorded payer — `rent_payer`
    /// on the new format, `claimant` on the legacy one — so a permissionless
    /// caller can trigger the refund but never redirect it.
    #[account(mut)]
    pub rent_recipient: UncheckedAccount<'info>,
}

/// Reclaim a spent `ClaimStatus`'s rent once its distributor is closed.
///
/// Permissionless on purpose: the refund target is fixed by the account's own
/// data, so the only thing a stranger can do by calling this is give the
/// rightful payer their deposit back. With the Fanex wallet sponsoring rent
/// (see `ClaimDividend.rent_payer`), this is what makes that sponsorship a
/// revolving float instead of a per-claim cost.
///
/// ⚠️ Known, accepted edge: `init_distributor` could re-create a distributor at
/// the same (mint, month) address after a close, and a claimant whose
/// ClaimStatus was reclaimed could then claim from the re-funded month again.
/// Only the funder can put SOL there, the app's ledger is UNIQUE per month and
/// never re-funds, so the only wallet a re-fund can hurt is the funder's own.
pub fn close_claim_status(ctx: Context<CloseClaimStatus>) -> Result<()> {
    let cs = &ctx.accounts.claim_status;
    require!(cs.owner == &crate::ID, FanexError::ClaimStatusFormat);

    let data = cs.try_borrow_data()?;
    require!(
        data.len() >= 8 && data[..8] == *ClaimStatus::DISCRIMINATOR,
        FanexError::ClaimStatusFormat
    );
    let refund_range = crate::validation::claim_status_refund_range(data.len())
        .ok_or(FanexError::ClaimStatusFormat)?;

    let claimant = Pubkey::try_from(&data[8..40]).map_err(|_| FanexError::ClaimStatusFormat)?;
    let refund_to =
        Pubkey::try_from(&data[refund_range]).map_err(|_| FanexError::ClaimStatusFormat)?;
    drop(data);

    // The seeds bind this ClaimStatus to the distributor account passed in —
    // without this, a dead UNRELATED address would satisfy the liveness check.
    let (expected, _bump) = Pubkey::find_program_address(
        &[
            CLAIM_SEED,
            ctx.accounts.distributor.key().as_ref(),
            claimant.as_ref(),
        ],
        &crate::ID,
    );
    require!(expected == cs.key(), FanexError::AccountMismatch);

    // Alive means: still program-owned with data. A closed account is reaped to
    // the system program (or holds stray lamports someone airdropped — still
    // not a Distributor, still closed).
    let dist = &ctx.accounts.distributor;
    require!(
        !(dist.owner == &crate::ID && !dist.data_is_empty()),
        FanexError::DistributorStillOpen
    );

    require!(
        ctx.accounts.rent_recipient.key() == refund_to,
        FanexError::WrongRentRecipient
    );

    let lamports = cs.lamports();
    {
        let recipient_ai = ctx.accounts.rent_recipient.to_account_info();
        let cs_ai = cs.to_account_info();
        **recipient_ai.try_borrow_mut_lamports()? = recipient_ai
            .lamports()
            .checked_add(lamports)
            .ok_or(FanexError::MathOverflow)?;
        **cs_ai.try_borrow_mut_lamports()? = 0;
        // Zero the discriminator so a not-yet-reaped account can never pass the
        // checks above twice in one slot.
        cs_ai.try_borrow_mut_data()?[..8].fill(0);
    }

    emit!(ClaimStatusClosed {
        claim_status: cs.key(),
        refunded_to: refund_to,
        lamports,
    });

    Ok(())
}

// ── v2: the claim registry ───────────────────────────────────────────────────

#[derive(Accounts)]
#[instruction(month: i64, merkle_root: [u8; 32], total_amount: u64, expires_at: i64, leaf_count: u32)]
pub struct InitDistributorV2<'info> {
    #[account(mut)]
    pub funder: Signer<'info>,

    /// Fanex's approval of what this month commits (2026-10-05): the signature of
    /// `config.graduation_authority`, the server's key, since 2026-10-24 (the
    /// fee-wallet split; the fee wallet's until then), given on the transaction
    /// the server prepared, so the root, total and expiry are the ones Fanex
    /// planned even when the creator funds and sends it. It approves only: it is
    /// not written and pays nothing. Never a funder: the dividend payer and the
    /// creator fund, so neither key alone, stolen, opens a month.
    #[account(address = config.graduation_authority @ FanexError::Unauthorized)]
    pub approver: Signer<'info>,

    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, GlobalConfig>,

    /// CHECK: the mint the distribution is for (bound into the PDA + state only).
    pub mint: UncheckedAccount<'info>,

    /// The curve for `mint`, read only for the funder gate, as in `InitDistributor`.
    #[account(
        seeds = [CURVE_SEED, mint.key().as_ref()],
        bump = curve.bump,
    )]
    pub curve: Account<'info, Curve>,

    /// The same seeds as a v1 `Distributor`: a month takes one distributor,
    /// whichever its version, and `init` fails while either exists. The funder
    /// pays the registry once, inside this account's rent.
    #[account(
        init,
        payer = funder,
        space = DistributorV2::space(leaf_count),
        seeds = [DISTRIBUTOR_SEED, mint.key().as_ref(), &month.to_le_bytes()],
        bump
    )]
    pub distributor: Account<'info, DistributorV2>,

    pub system_program: Program<'info, System>,
}

/// `init_distributor` for a month whose claims the distributor records itself.
/// The gates: global pause, a positive total, the dividend payer or the curve's
/// creator as funder (since 2026-10-24 no config key: the fee-wallet split),
/// the graduation authority's signature as approver (the account constraint),
/// and a leaf count the registry can hold.
pub fn init_distributor_v2(
    ctx: Context<InitDistributorV2>,
    month: i64,
    merkle_root: [u8; 32],
    total_amount: u64,
    expires_at: i64,
    leaf_count: u32,
) -> Result<()> {
    require!(!ctx.accounts.config.paused, FanexError::GloballyPaused);
    require!(total_amount > 0, FanexError::ZeroAmount);
    require!(leaf_count_in_range(leaf_count), FanexError::InvalidLeafCount);

    // Anti-griefing: the PDA is init-once per month. The creator may fund, and
    // the approver constraint is what binds that funding to Fanex's plan.
    let funder = ctx.accounts.funder.key();
    require!(
        may_fund_distributor_v2(&funder, &ctx.accounts.curve.creator),
        FanexError::Unauthorized
    );
    // No month held past `MAX_DISTRIBUTOR_EXPIRY_SECONDS` (2026-10-08, security card point 5).
    require!(expiry_in_range(expires_at, Clock::get()?.unix_timestamp), FanexError::ExpiryTooFar);

    let distributor = &mut ctx.accounts.distributor;
    distributor.mint = ctx.accounts.mint.key();
    distributor.month = month;
    distributor.merkle_root = merkle_root;
    distributor.total_amount = total_amount;
    distributor.claimed_amount = 0;
    distributor.funder = funder;
    distributor.expires_at = expires_at;
    distributor.bump = ctx.bumps.distributor;
    distributor.leaf_count = leaf_count;
    distributor.claimed = vec![0u8; claim_registry_bytes(leaf_count)];

    system_program::transfer(
        CpiContext::new(
            ctx.accounts.system_program.to_account_info(),
            system_program::Transfer {
                from: ctx.accounts.funder.to_account_info(),
                to: ctx.accounts.distributor.to_account_info(),
            },
        ),
        total_amount,
    )?;

    emit!(DistributorFunded {
        mint: ctx.accounts.distributor.mint,
        month,
        total_amount,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct ClaimDividendV2<'info> {
    /// Signs, as in v1: the claim is a pull, and only the holder pulls. Who pays
    /// the fee is the transaction's business; there is no rent to pay.
    #[account(mut)]
    pub claimant: Signer<'info>,

    #[account(mut)]
    pub distributor: Account<'info, DistributorV2>,

    pub system_program: Program<'info, System>,
}

/// Claim leaf `index` of a `DistributorV2`. The proof binds the index, the
/// claimant and the amount together, and the leaf's bit in the registry stops
/// a second claim: nothing is created, so nothing has to be closed later.
pub fn claim_dividend_v2<'info>(
    ctx: Context<'_, '_, '_, 'info, ClaimDividendV2<'info>>,
    index: u32,
    amount: u64,
    proof: Vec<[u8; 32]>,
) -> Result<()> {
    require!(amount > 0, FanexError::ZeroAmount);

    let distributor = &ctx.accounts.distributor;
    require!(index < distributor.leaf_count, FanexError::InvalidLeafIndex);

    let leaf = dividend_leaf_v2(index, &ctx.accounts.claimant.key(), amount);
    require!(
        verify_merkle_proof(&proof, distributor.merkle_root, leaf),
        FanexError::InvalidProof
    );

    let (byte, mask) = leaf_bit(index);
    let registry_byte = *distributor
        .claimed
        .get(byte)
        .ok_or(FanexError::InvalidLeafIndex)?;
    require!(registry_byte & mask == 0, FanexError::AlreadyClaimed);

    let new_claimed = distributor
        .claimed_amount
        .checked_add(amount)
        .ok_or(FanexError::MathOverflow)?;
    require!(
        new_claimed <= distributor.total_amount,
        FanexError::ExceedsFundedTotal
    );

    // As in v1: out of the distributor (the bound above keeps it rent-exempt),
    // through the payout transit when the client passed it (`crate::payout`).
    pay_out(
        &ctx.accounts.distributor.to_account_info(),
        &ctx.accounts.claimant.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        ctx.remaining_accounts,
        amount,
    )?;

    let distributor = &mut ctx.accounts.distributor;
    distributor.claimed_amount = new_claimed;
    distributor.claimed[byte] |= mask;

    emit!(DividendClaimed {
        mint: distributor.mint,
        month: distributor.month,
        claimant: ctx.accounts.claimant.key(),
        amount,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct CloseDistributorV2<'info> {
    #[account(
        mut,
        close = funder,
        has_one = funder @ FanexError::Unauthorized,
    )]
    pub distributor: Account<'info, DistributorV2>,

    #[account(mut)]
    pub funder: Signer<'info>,
}

/// `close_distributor` for a v2 month, under the same rule. Its claims left no
/// account behind, so this one close returns everything the month holds.
pub fn close_distributor_v2(ctx: Context<CloseDistributorV2>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let d = &ctx.accounts.distributor;
    require!(
        crate::validation::may_close_distributor(
            d.claimed_amount,
            d.total_amount,
            now,
            d.expires_at
        ),
        FanexError::NotExpired
    );
    Ok(())
}

// ── The internal audit's fix round (2026-10-29, docs/security/INTERNAL-AUDIT-2026-10-29.md) ──

#[derive(Accounts)]
pub struct SetDistributorV2Funder<'info> {
    /// The month's recorded funder: only who funded a month hands its close over.
    pub funder: Signer<'info>,

    #[account(mut, has_one = funder @ FanexError::Unauthorized)]
    pub distributor: Account<'info, DistributorV2>,
}

/// `set_distributor_funder` for a v2 month: until the fee-wallet split the hot wallet
/// (`config.fanex_fee_wallet`) funded v2 months too, and only a month's recorded funder
/// closes it, so a month it funded needs its key until it closes. Signed by that
/// funder; `new_funder` the dividend payer alone (`Unauthorized` otherwise), so the hot
/// key can be retired after the migration with no v2 month left on it. Nothing else
/// moves: the root, the claims' bits, the expiry and the lamports stay as they are.
/// Never paused: it moves no funds. Emits `DistributorFunderSet`.
pub fn set_distributor_v2_funder(ctx: Context<SetDistributorV2Funder>, new_funder: Pubkey) -> Result<()> {
    require!(may_rehome_distributor_funder(&new_funder), FanexError::Unauthorized);
    let distributor = &mut ctx.accounts.distributor;
    let previous = distributor.funder;
    distributor.funder = new_funder;
    emit!(DistributorFunderSet {
        mint: distributor.mint,
        month: distributor.month,
        previous,
        funder: new_funder,
    });
    Ok(())
}

#[derive(Accounts)]
pub struct CloseExpiredDistributor<'info> {
    /// A v1 month, closed to its recorded funder.
    #[account(mut, close = funder, has_one = funder @ FanexError::Unauthorized)]
    pub distributor: Account<'info, Distributor>,

    /// CHECK: the month's recorded funder, bound by `has_one`; it receives the month's
    /// lamports and need not sign.
    #[account(mut)]
    pub funder: UncheckedAccount<'info>,
}

/// Close an expired v1 month, permissionless: the lamports go to its recorded funder,
/// whoever sends it, so a stranger can only return a funder's own money. Until now only
/// that funder could close a month, so one it never closed held its `["distributor",
/// mint, month]` address for good, whatever its expiry: a key that may fund a month
/// (the graduation authority alone on v1) could squat a token's future months, and
/// `pepper_dividends` refuses a month whose SOL distributor still holds data. Now a
/// month blocks its address at most until `expires_at`, at most 400 days from its
/// funding. Only at expiry (`NotExpired` before it): the early close of a fully claimed
/// month stays the funder's, through `close_distributor`. A v1 claim's `ClaimStatus`
/// closes after it through `close_claim_status`, as after any close. Never paused.
pub fn close_expired_distributor(ctx: Context<CloseExpiredDistributor>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(
        now >= ctx.accounts.distributor.expires_at,
        FanexError::NotExpired
    );
    Ok(())
}

#[derive(Accounts)]
pub struct CloseExpiredDistributorV2<'info> {
    /// A v2 month, closed to its recorded funder.
    #[account(mut, close = funder, has_one = funder @ FanexError::Unauthorized)]
    pub distributor: Account<'info, DistributorV2>,

    /// CHECK: the month's recorded funder, bound by `has_one`; it receives the month's
    /// lamports and need not sign.
    #[account(mut)]
    pub funder: UncheckedAccount<'info>,
}

/// `close_expired_distributor` for a v2 month, under the same rule: permissionless at
/// expiry, everything the month holds to its recorded funder.
pub fn close_expired_distributor_v2(ctx: Context<CloseExpiredDistributorV2>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(
        now >= ctx.accounts.distributor.expires_at,
        FanexError::NotExpired
    );
    Ok(())
}

#[cfg(test)]
mod audit_fix_tests {
    use anchor_lang::prelude::Pubkey;
    use anchor_lang::{Discriminator, InstructionData, ToAccountMetas};

    #[test]
    fn an_expired_month_closes_with_no_signer_to_its_recorded_funder() {
        let [distributor, funder] = std::array::from_fn(|_| Pubkey::new_unique());
        for metas in [
            crate::accounts::CloseExpiredDistributor { distributor, funder }.to_account_metas(None),
            crate::accounts::CloseExpiredDistributorV2 { distributor, funder }.to_account_metas(None),
        ] {
            assert_eq!(metas.iter().map(|m| m.pubkey).collect::<Vec<_>>(), [distributor, funder]);
            assert!(metas.iter().all(|m| !m.is_signer && m.is_writable));
        }
        assert_eq!(crate::instruction::CloseExpiredDistributor {}.data(), crate::instruction::CloseExpiredDistributor::DISCRIMINATOR);
        assert_eq!(crate::instruction::CloseExpiredDistributorV2 {}.data(), crate::instruction::CloseExpiredDistributorV2::DISCRIMINATOR);
    }

    #[test]
    fn a_v2_month_s_funder_signs_its_hand_over() {
        let [funder, distributor] = std::array::from_fn(|_| Pubkey::new_unique());
        let metas = crate::accounts::SetDistributorV2Funder { funder, distributor }.to_account_metas(None);
        assert_eq!(metas.iter().filter(|m| m.is_signer).map(|m| m.pubkey).collect::<Vec<_>>(), [funder]);
        assert_eq!(metas.iter().filter(|m| m.is_writable).map(|m| m.pubkey).collect::<Vec<_>>(), [distributor]);
    }
}
