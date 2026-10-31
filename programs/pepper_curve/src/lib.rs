//! # pepper_curve
//!
//! On-chain bonding curve for Fanex creator tokens. Replaces the Raydium
//! LaunchLab presale: the creator/Fanex fee split is now taken **atomically**
//! inside the same instruction that delivers tokens, so it can no longer be
//! stripped (root fix for B1). Post-graduation trading still hands off to
//! Raydium CPMM (presale_only scope), so Jupiter/aggregator discoverability is
//! preserved.
//!
//! Instruction surface:
//!   - `initialize_config` / `update_config` / `set_paused` / `set_curve_paused`
//!   - `set_launch_guard_authority` / `waive_launch_guard` — the key that may
//!     launch a curve without the launch cap, and its waiver, made with the launch
//!   - `create_curve` — mint fixed supply, lock 2% creator vesting, fix supply
//!   - `buy` / `sell` — constant-product curve with atomic, capped fee split
//!   - `graduate` — keeper releases SOL + tokens to seed Raydium CPMM
//!   - `claim_vesting` — creator linear vesting
//!   - `set_vesting_beneficiary` — hand an unclaimed fan-led vesting
//!     allocation to the creator who proved channel ownership (placeholder-only)
//!   - `sync_curve_creator` — copy that claimed beneficiary into `Curve.creator`
//!     (placeholder-only, no address argument)
//!   - `init_distributor` / `claim_dividend` / `close_distributor` — on-chain
//!     merkle dividend distribution (retires B2/B3 trust assumptions);
//!     `set_distributor_funder` hands a v1 month's close to the dividend payer
//!   - `init_distributor_v2` / `claim_dividend_v2` / `close_distributor_v2` —
//!     the same, with claims recorded in the distributor's own bitmap instead
//!     of one `ClaimStatus` account (and its rent) per claim
//!
//! The USDC curve (USDC curve design §10, on the track A branch, on no cluster),
//! beside the SOL one, whose instructions do not move; its handlers live in
//! `instructions/usdc/`:
//!   - `initialize_usdc_config` / `update_usdc_config` — the USDC side's own
//!     config, created by the program's upgrade authority (its ProgramData read),
//!     which names the first admin, then changed by `UsdcConfig.admin`
//!   - `set_usdc_paused` / `set_curve_usdc_paused` — its kill-switch and one
//!     curve's pause: the pauser or the admin pauses, only the admin unpauses
//!   - `usdc_pause_sol` / `usdc_pause_sol_curve` — the USDC pause of the SOL
//!     side, signed by `UsdcConfig.pauser` alone, outside the Squad's time lock:
//!     pause only, unpausing stays `set_paused` / `set_curve_paused`,
//!     `GlobalConfig.admin`'s
//!   - `claim_vesting_usdc` — the creator vesting of a `CurveUsdc`, by
//!     `transfer_checked`, into a token account that must already exist
//!   - `sync_curve_creator_usdc` — `sync_curve_creator` on a `CurveUsdc`,
//!     signed by `GlobalConfig.admin`
//!   - `set_usdc_raydium` — the five Raydium addresses the launch and the
//!     graduation need, field by field, signed by the program's upgrade
//!     authority alone, read from its ProgramData
//!   - `buy_exact_in` — a buy on a `CurveUsdc` over 13 fixed accounts a router
//!     can call, moving exactly what `pepper_curve_math::quote_buy` returns
//!   - `sell_exact_in` — a sale on a `CurveUsdc` over 11 fixed accounts, the
//!     proceeds and the fee out of `usdc_vault` as `quote_sell` returns them
//!   - `create_curve_usdc` — `create_curve` priced in USDC, paid by
//!     `UsdcConfig.launcher` alone, the metadata's update authority `UsdcConfig.admin`
//!   - `graduate_usdc_pool` — the graduation's first step, permissionless: the
//!     curve's counted reserves into a Raydium CPMM pool at the program's own PDA,
//!     by CPI, Graduating to PoolCreated
//!   - `graduate_usdc_lock` — the graduation's second step, permissionless: the
//!     pool creator's whole LP into Raydium's LP lock, by CPI, the Fee Key to
//!     `UsdcConfig.lp_fee_owner`, PoolCreated to Graduated
//!
//! The internal audit's fix round (2026-10-29), appended after N's instructions:
//!   - `close_creator_raise_usdc` — `UsdcConfig.admin` closes a USDC curve's
//!     creator raise at what it received, so a frozen creator account stops no buy
//!   - `set_distributor_v2_funder` — `set_distributor_funder` for a v2 month
//!   - `close_expired_distributor` / `close_expired_distributor_v2` — an expired
//!     month closed to its recorded funder by anyone

use anchor_lang::prelude::*;

pub mod constants;
pub mod errors;
pub mod events;
pub mod instructions;
pub mod math;
pub mod payout;
pub mod raydium;
pub mod state;
pub mod transfer;
pub mod validation;

use instructions::*;

declare_id!("CDPwGWGWbAE8FL5f2oVdzfkTutrnsKJcAcgqWktC3dwZ");

#[program]
pub mod pepper_curve {
    use super::*;

    // ── Config / admin ─────────────────────────────────────────────────────
    pub fn initialize_config(
        ctx: Context<InitializeConfig>,
        fanex_fee_wallet: Pubkey,
        graduation_authority: Pubkey,
    ) -> Result<()> {
        instructions::initialize_config::handler(ctx, fanex_fee_wallet, graduation_authority)
    }

    pub fn update_config(
        ctx: Context<UpdateConfig>,
        new_admin: Option<Pubkey>,
        new_fanex_fee_wallet: Option<Pubkey>,
        new_graduation_authority: Option<Pubkey>,
    ) -> Result<()> {
        instructions::admin::update_config(
            ctx,
            new_admin,
            new_fanex_fee_wallet,
            new_graduation_authority,
        )
    }

    pub fn set_paused(ctx: Context<UpdateConfig>, paused: bool) -> Result<()> {
        instructions::admin::set_paused(ctx, paused)
    }

    pub fn set_curve_paused(ctx: Context<SetCurvePaused>, paused: bool) -> Result<()> {
        instructions::admin::set_curve_paused(ctx, paused)
    }

    /// Name the key that may launch a curve without the launch cap
    /// (docs/solana/PEPPER-CURVE-PROGRAM.md §5.2). All-zero lets nobody.
    pub fn set_launch_guard_authority(
        ctx: Context<SetLaunchGuardAuthority>,
        authority: Pubkey,
    ) -> Result<()> {
        instructions::launch_guard::set_launch_guard_authority(ctx, authority)
    }

    /// Hand an unclaimed fan-led vesting allocation to the creator who proved
    /// they own the channel. Refuses any vesting account whose beneficiary is
    /// no longer the keyless placeholder, so a real creator's allocation is
    /// unreachable from here.
    pub fn set_vesting_beneficiary(
        ctx: Context<SetVestingBeneficiary>,
        new_beneficiary: Pubkey,
    ) -> Result<()> {
        instructions::set_vesting_beneficiary::handler(ctx, new_beneficiary)
    }

    /// Copy a claimed fan-led vesting beneficiary into `Curve.creator`. Takes no
    /// address: refuses unless `Curve.creator` is still the keyless placeholder
    /// and the vesting has already been handed to a real wallet, so it can
    /// neither touch a real creator nor write anything the vesting does not say.
    pub fn sync_curve_creator(ctx: Context<SyncCurveCreator>) -> Result<()> {
        instructions::sync_curve_creator::handler(ctx)
    }

    // ── Curve lifecycle ──────────────────────────────────────────────────────
    /// Launch the mint's curve without the launch cap: signed by the launch
    /// guard authority, before the curve exists (§5.2).
    pub fn waive_launch_guard(ctx: Context<WaiveLaunchGuard>) -> Result<()> {
        instructions::launch_guard::waive_launch_guard(ctx)
    }

    pub fn create_curve(ctx: Context<CreateCurve>, params: CreateCurveParams) -> Result<()> {
        instructions::create_curve::handler(ctx, params)
    }

    pub fn buy<'info>(
        ctx: Context<'_, '_, '_, 'info, Buy<'info>>,
        sol_in: u64,
        min_tokens_out: u64,
    ) -> Result<()> {
        instructions::buy::handler(ctx, sol_in, min_tokens_out)
    }

    pub fn sell<'info>(
        ctx: Context<'_, '_, '_, 'info, Sell<'info>>,
        token_in: u64,
        min_sol_out: u64,
    ) -> Result<()> {
        instructions::sell::handler(ctx, token_in, min_sol_out)
    }

    pub fn graduate(ctx: Context<Graduate>) -> Result<()> {
        instructions::graduate::handler(ctx)
    }

    pub fn claim_vesting(ctx: Context<ClaimVesting>) -> Result<()> {
        instructions::claim_vesting::handler(ctx)
    }

    // ── Dividends ────────────────────────────────────────────────────────────
    pub fn init_distributor(
        ctx: Context<InitDistributor>,
        month: i64,
        merkle_root: [u8; 32],
        total_amount: u64,
        expires_at: i64,
    ) -> Result<()> {
        instructions::dividends::init_distributor(ctx, month, merkle_root, total_amount, expires_at)
    }

    pub fn claim_dividend<'info>(
        ctx: Context<'_, '_, '_, 'info, ClaimDividend<'info>>,
        amount: u64,
        proof: Vec<[u8; 32]>,
    ) -> Result<()> {
        instructions::dividends::claim_dividend(ctx, amount, proof)
    }

    pub fn close_distributor(ctx: Context<CloseDistributor>) -> Result<()> {
        instructions::dividends::close_distributor(ctx)
    }

    /// Hand a v1 month's close over to the dividend payer (2026-10-24, the
    /// fee-wallet split): signed by its recorded funder, `new_funder` the payer
    /// alone; nothing else moves.
    pub fn set_distributor_funder(
        ctx: Context<SetDistributorFunder>,
        new_funder: Pubkey,
    ) -> Result<()> {
        instructions::dividends::set_distributor_funder(ctx, new_funder)
    }

    pub fn close_claim_status(ctx: Context<CloseClaimStatus>) -> Result<()> {
        instructions::dividends::close_claim_status(ctx)
    }

    pub fn init_distributor_v2(
        ctx: Context<InitDistributorV2>,
        month: i64,
        merkle_root: [u8; 32],
        total_amount: u64,
        expires_at: i64,
        leaf_count: u32,
    ) -> Result<()> {
        instructions::dividends::init_distributor_v2(
            ctx,
            month,
            merkle_root,
            total_amount,
            expires_at,
            leaf_count,
        )
    }

    pub fn claim_dividend_v2<'info>(
        ctx: Context<'_, '_, '_, 'info, ClaimDividendV2<'info>>,
        index: u32,
        amount: u64,
        proof: Vec<[u8; 32]>,
    ) -> Result<()> {
        instructions::dividends::claim_dividend_v2(ctx, index, amount, proof)
    }

    pub fn close_distributor_v2(ctx: Context<CloseDistributorV2>) -> Result<()> {
        instructions::dividends::close_distributor_v2(ctx)
    }

    // ── The USDC curve: config and pauses (USDC curve design §10.3.1 to §10.3.3) ──
    /// Create the USDC side's config, signed by the program's upgrade authority as
    /// its ProgramData records it, naming its first admin (a system-owned account,
    /// the upgrade Squad's vault on mainnet): the USDC mint (6 decimals, set once),
    /// the fee wallet's hardened USDC ATA, the keys of `params`, the Raydium fields
    /// all-zero.
    pub fn initialize_usdc_config(
        ctx: Context<InitializeUsdcConfig>,
        params: InitializeUsdcConfigParams,
    ) -> Result<()> {
        instructions::usdc::usdc_config::initialize_usdc_config(ctx, params)
    }

    /// Change the USDC config's keys, signed by `UsdcConfig.admin`. `None`, or
    /// no `new_fee_usdc_account`, leaves a field as it is.
    pub fn update_usdc_config(
        ctx: Context<UpdateUsdcConfig>,
        new_admin: Option<Pubkey>,
        new_pauser: Option<Pubkey>,
        new_migration_authority: Option<Pubkey>,
        new_lp_fee_owner: Option<Pubkey>,
        new_launcher: Option<Pubkey>,
    ) -> Result<()> {
        instructions::usdc::usdc_config::update_usdc_config(
            ctx,
            new_admin,
            new_pauser,
            new_migration_authority,
            new_lp_fee_owner,
            new_launcher,
        )
    }

    /// The USDC side's kill-switch: the pauser or the admin pauses, only the
    /// admin unpauses (`NotPauser`).
    pub fn set_usdc_paused(ctx: Context<SetUsdcPaused>, paused: bool) -> Result<()> {
        instructions::usdc::usdc_pause::set_usdc_paused(ctx, paused)
    }

    /// One USDC curve's pause, bound by its seeds, under the kill-switch's rule.
    pub fn set_curve_usdc_paused(ctx: Context<SetCurveUsdcPaused>, paused: bool) -> Result<()> {
        instructions::usdc::usdc_pause::set_curve_usdc_paused(ctx, paused)
    }

    /// The USDC pause of the whole SOL side (§10.3.3, Rick, 2026-10-23),
    /// `GlobalConfig.paused = true`, signed by `UsdcConfig.pauser` alone. Takes
    /// no value, so it cannot unpause: that stays `set_paused(false)`,
    /// `GlobalConfig.admin`'s.
    pub fn usdc_pause_sol(ctx: Context<UsdcPauseSol>) -> Result<()> {
        instructions::usdc::usdc_pause::usdc_pause_sol(ctx)
    }

    /// The USDC pause of one SOL curve, `Curve.paused = true`, bound by its
    /// seeds, signed by `UsdcConfig.pauser` alone. Unpausing stays
    /// `set_curve_paused(false)`.
    pub fn usdc_pause_sol_curve(ctx: Context<UsdcPauseSolCurve>) -> Result<()> {
        instructions::usdc::usdc_pause::usdc_pause_sol_curve(ctx)
    }

    // ── The USDC curve: the vesting claim and the fan-led sync (§10.3.7, §10.3.8) ──
    /// `claim_vesting` on a `CurveUsdc`: the same schedule and the same lock,
    /// a curve created from 2026-11-01 released once its pool exists
    /// (PoolCreated or Graduated); the beneficiary's token account must exist,
    /// and the tokens move by `transfer_checked`.
    pub fn claim_vesting_usdc(ctx: Context<ClaimVestingUsdc>) -> Result<()> {
        instructions::usdc::claim_vesting_usdc::handler(ctx)
    }

    /// `sync_curve_creator` on a `CurveUsdc`, signed by `GlobalConfig.admin`
    /// as the SOL one: once, from the fan-led placeholder to the vesting
    /// beneficiary a claim already set, taking no address.
    pub fn sync_curve_creator_usdc(ctx: Context<SyncCurveCreatorUsdc>) -> Result<()> {
        instructions::usdc::sync_curve_creator_usdc::handler(ctx)
    }

    // ── The USDC curve: the Raydium addresses (§10.3.10) ──────────────────────
    /// Write the Raydium addresses a graduation sends a curve's reserves to,
    /// each `None` kept as it is, signed by the program's upgrade authority
    /// alone, as its ProgramData records it (`Unauthorized` for any other key,
    /// `UsdcConfig.admin` included, and for an immutable program): none written
    /// all-zero (`RaydiumNotSet`); when the CPMM or the tier changes, the
    /// `amm_config` the one the config will hold, owned by its CPMM, still at 1%
    /// (`GraduationFeeTierChanged`) and creating pools (`PoolCreationDisabled`).
    pub fn set_usdc_raydium(
        ctx: Context<SetUsdcRaydium>,
        params: SetUsdcRaydiumParams,
    ) -> Result<()> {
        instructions::usdc::set_usdc_raydium::handler(ctx, params)
    }

    // ── The USDC curve: the buy (§10.3.5) ────────────────────────────────────
    /// Spend at most `usdc_in` µUSDC for at least `min_tokens_out` tokens: the
    /// math crate's `quote_buy`, moved exactly by `transfer_checked`, over 13
    /// fixed accounts in the order a router's metas are built in. A partial fill
    /// at graduation charges less than `usdc_in`.
    pub fn buy_exact_in(ctx: Context<BuyExactIn>, usdc_in: u64, min_tokens_out: u64) -> Result<()> {
        instructions::usdc::buy_exact_in::handler(ctx, usdc_in, min_tokens_out)
    }

    // ── The USDC curve: the sale (§10.3.6) ───────────────────────────────────
    /// Sell `tokens_in` for at least `min_usdc_out` µUSDC after the 1% fee: the
    /// math crate's `quote_sell`, the proceeds and the fee out of `usdc_vault`
    /// by `transfer_checked`, the curve signing, over 11 fixed accounts in the
    /// order a router's metas are built in.
    pub fn sell_exact_in(
        ctx: Context<SellExactIn>,
        tokens_in: u64,
        min_usdc_out: u64,
    ) -> Result<()> {
        instructions::usdc::sell_exact_in::handler(ctx, tokens_in, min_usdc_out)
    }

    // ── The USDC curve: the launch (§10.3.4) ──────────────────────────────────
    /// Launch a USDC curve, paid and signed by `UsdcConfig.launcher` alone (the
    /// sponsor): today's checks in µUSDC, the creator's USDC ATA with a creator
    /// split, the supply into the two vaults, the Metaplex metadata under
    /// `UsdcConfig.admin`, the mint authority revoked. No USDC moves.
    pub fn create_curve_usdc(
        ctx: Context<CreateCurveUsdc>,
        params: CreateCurveUsdcParams,
    ) -> Result<()> {
        instructions::usdc::create_curve_usdc::handler(ctx, params)
    }

    // ── The USDC curve: the graduation's first step (§10.3.9) ─────────────────────
    /// Create a graduating curve's Raydium CPMM pool, by CPI, at the program's PDA
    /// `["cpmm_pool", mint]`: the counted reserves, never the vaults' balances, through
    /// the pool creator, a system-owned PDA of ours that pays Raydium's rents and fee,
    /// funded by the caller, `extra_lamports` (at most 0.1 SOL) included, and emptied back
    /// to it. Permissionless, gated by both USDC pauses; Graduating to PoolCreated.
    pub fn graduate_usdc_pool(ctx: Context<GraduateUsdcPool>, extra_lamports: u64) -> Result<()> {
        instructions::usdc::graduate_usdc_pool::handler(ctx, extra_lamports)
    }

    // ── The USDC curve: the graduation's second step (§10.3.9) ────────────────────
    /// Lock a pooled curve's whole LP in Raydium's LP lock, by CPI: the pool creator's
    /// LP balance, the LP mint and the pool's vaults as the pool's own `PoolState`
    /// records them, the Fee Key to `UsdcConfig.lp_fee_owner`, the lock's rents paid by
    /// the pool creator, funded by the caller, `extra_lamports` (at most 0.1 SOL) included,
    /// and emptied back to it. Permissionless, gated by both USDC pauses; PoolCreated to
    /// Graduated, the reserve counters zeroed.
    pub fn graduate_usdc_lock(ctx: Context<GraduateUsdcLock>, extra_lamports: u64) -> Result<()> {
        instructions::usdc::graduate_usdc_lock::handler(ctx, extra_lamports)
    }

    // ── The internal audit's fix round (2026-10-29, docs/security/INTERNAL-AUDIT-2026-10-29.md) ──
    // Appended after N's instructions.

    /// Close a USDC curve's creator raise at what it has received, signed by `UsdcConfig.admin`:
    /// its buys then need no creator USDC account, so one frozen or lost stops nothing. Bonding
    /// only, never paused.
    pub fn close_creator_raise_usdc(ctx: Context<CloseCreatorRaiseUsdc>) -> Result<()> {
        instructions::usdc::close_creator_raise_usdc::handler(ctx)
    }

    /// Hand a v2 month's close over to the dividend payer, as `set_distributor_funder` does a v1
    /// month's: signed by its recorded funder, `new_funder` the payer alone; nothing else moves.
    pub fn set_distributor_v2_funder(
        ctx: Context<SetDistributorV2Funder>,
        new_funder: Pubkey,
    ) -> Result<()> {
        instructions::dividends::set_distributor_v2_funder(ctx, new_funder)
    }

    /// Close an expired v1 month to its recorded funder, permissionless, so no month blocks its
    /// address past its expiry.
    pub fn close_expired_distributor(ctx: Context<CloseExpiredDistributor>) -> Result<()> {
        instructions::dividends::close_expired_distributor(ctx)
    }

    /// `close_expired_distributor` for a v2 month.
    pub fn close_expired_distributor_v2(ctx: Context<CloseExpiredDistributorV2>) -> Result<()> {
        instructions::dividends::close_expired_distributor_v2(ctx)
    }
}
