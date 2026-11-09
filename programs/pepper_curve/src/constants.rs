//! Economic + structural constants. These mirror the off-chain TypeScript
//! source of truth in `lib/solana/fee-config.ts` / `presale-math.ts`. The
//! program re-derives nothing it can be handed at create time, but it *enforces*
//! every invariant below so a malformed launch can never be created.

use anchor_lang::prelude::*;

/// Basis-points denominator. 100 bps = 1%.
pub const BPS_DENOMINATOR: u64 = 10_000;

/// Platform swap fee: 1% (100 bps) of every buy/sell, paid to
/// `config.fanex_fee_wallet`. This is SEPARATE from the creator/Fanex *fundraise*
/// split (`split_with_cap`): it's a per-trade platform fee skimmed from the
/// buyer's input (buy) / the seller's gross SOL-out (sell), and unlike the
/// fundraise it does NOT count toward any cap and never stops.
pub const PLATFORM_SWAP_FEE_BPS: u64 = 100;

/// The wallet that pays Fanex's dividend months in place of the fee wallet
/// (2026-10-07): `5UZrYBpbmG1kZKobQbM1Jqn8JDCfpSaR8sTm9ApzC554`, mirrored by
/// `DIVIDEND_PAYER_ADDRESS` in `lib/solana/dividend-payer.ts`. A v2 distributor
/// funder next to the curve's creator (`may_fund_distributor_v2`), and since
/// 2026-10-08 a v2 one only (security card point 5): v2 takes the graduation
/// authority's approval too (the fee wallet's until 2026-10-24), so this key
/// alone, stolen, opens no month. Since 2026-10-24 also the one key a v1 month's
/// close may be handed to (`set_distributor_funder`). A month closes with its
/// recorded funder (`has_one = funder`), whoever may fund now.
pub const DIVIDEND_PAYER: Pubkey = Pubkey::new_from_array([
    66, 126, 147, 234, 142, 16, 174, 190, 51, 253, 246, 8, 22, 10, 172, 133, 183, 237, 57, 53, 101,
    186, 207, 49, 23, 170, 76, 198, 112, 72, 18, 99,
]);

/// The furthest a distributor may expire, from now: 400 days (2026-10-08,
/// security card point 5). The app commits a 365-day claim window
/// (`DISTRIBUTOR_EXPIRY_SECONDS` in `lib/solana/pepper-curve/program.ts`); the
/// rest is room for drift. Unbounded, a funder could commit `i64::MAX`, and the
/// month, `init`-once per (mint, month), could never be funded again.
pub const MAX_DISTRIBUTOR_EXPIRY_SECONDS: i64 = 400 * 24 * 60 * 60;

/// 1 SOL in lamports.
pub const LAMPORTS_PER_SOL: u64 = 1_000_000_000;

/// SPL mint decimals for every Fanex token.
pub const TOKEN_DECIMALS: u8 = 6;

/// Fixed total supply: 100,000,000 tokens × 10^6 base units.
pub const TOKEN_TOTAL_SUPPLY: u64 = 100_000_000 * 1_000_000;

/// The launch guard (anti-sniper, 2026-10-12): for this long after a curve's
/// `created_at`, `buy` refuses a buy that would leave the buyer's token account
/// holding more than `LAUNCH_WALLET_CAP` (`validation::launch_cap_allows`). A
/// bot buying in the first blocks took the lowest price and sold it to the fans
/// who came after; five minutes cover the bots and the first wave of a
/// scheduled launch (Rick, 2026-10-12).
///
/// ⚠️ Mirrored by `LAUNCH_GUARD_SECONDS` in `lib/solana/pepper-curve/launch-cap.ts`,
/// held by its test, which reads this file: keep it a literal.
pub const LAUNCH_GUARD_SECONDS: i64 = 300; // 5 minutes

/// The most one token account may hold during the launch guard: 1% of the
/// supply, 1,000,000 tokens (Rick, 2026-10-12). It binds every buyer, the
/// launch's own first buy included. Several wallets get around it, accepted for
/// now (docs/solana/PEPPER-CURVE-PROGRAM.md §5.2).
///
/// ⚠️ Mirrored by `LAUNCH_WALLET_CAP` in `lib/solana/pepper-curve/launch-cap.ts`,
/// held by its test, which reads this file: keep it a literal (the unit test
/// below ties it to `TOKEN_TOTAL_SUPPLY`).
pub const LAUNCH_WALLET_CAP: u64 = 1_000_000_000_000; // 1,000,000 tokens, 6 decimals

/// Curves created at or after this instant (unix seconds, 2026-11-01 00:00:00
/// UTC) release their vesting only once they have graduated
/// (`validation::vesting_claim_allowed`). Before graduation the vested tokens
/// would be sold into the curve and paid from the reserve that backs what the
/// holders bought, and once it is spent every holder's sale is refused
/// (docs/solana/ECONOMIC-MODEL.md §6.2). New curves only (Rick, 2026-10-15):
/// a curve created before keeps the terms it launched with.
///
/// ⚠️ Mirrored by `VESTING_LOCKED_UNTIL_GRADUATION_FROM` in
/// `lib/solana/pepper-curve/vesting-lock.ts`, held by its test, which reads
/// this file: keep it a literal.
pub const VESTING_LOCKED_UNTIL_GRADUATION_FROM: i64 = 1_793_491_200;

/// Graduation floor / minimum creator target (16 SOL).
pub const MIN_CREATOR_TARGET_LAMPORTS: u64 = 16 * LAMPORTS_PER_SOL;

/// CPMM seed bounds (SOL routed into the curve that later seeds the Raydium CPMM).
pub const MIN_CPMM_SEED_LAMPORTS: u64 = 24 * LAMPORTS_PER_SOL;
pub const CAP_CPMM_SEED_LAMPORTS: u64 = 800 * LAMPORTS_PER_SOL;

/// Creator vesting allocation and schedule are now supplied per launch
/// (`CreateCurveParams::vesting_token_supply` / `vesting_cliff_seconds` /
/// `vesting_duration_seconds`, validated in `crate::validation`) instead of
/// being hardcoded. The three constants below are no longer read by the
/// handler — they are the written record of the `revenue` type's defaults:
/// 2% of supply, **12-month cliff, lump-sum** (all-or-nothing). Nothing is
/// releasable for the first ~360 days; at the cliff the FULL 2% becomes
/// claimable in one go (cliff = ~360d, duration = 0 — `vested_amount` returns
/// 0 before the cliff and `total` once `duration == 0` past it, no linear
/// drip). Each `Vesting` account stores its own schedule, stamped at
/// `create_curve`, so changing a per-launch value only affects NEWLY created
/// curves; existing accounts keep what they were stamped with.
pub const VESTING_SUPPLY_BPS: u64 = 200;
pub const VESTING_CLIFF_SECONDS: i64 = 12 * 30 * 24 * 60 * 60; // ~360d cliff
pub const VESTING_DURATION_SECONDS: i64 = 0; // 0 = lump-sum at the cliff (no linear)

/// String field caps (kept small; the rich metadata lives off-chain).
pub const MAX_NAME_LEN: usize = 32;
pub const MAX_SYMBOL_LEN: usize = 12;
pub const MAX_URI_LEN: usize = 200;

// ── PDA seeds ────────────────────────────────────────────────────────────
pub const CONFIG_SEED: &[u8] = b"config";
pub const CURVE_SEED: &[u8] = b"curve";
pub const SOL_VAULT_SEED: &[u8] = b"sol_vault";
pub const TOKEN_VAULT_SEED: &[u8] = b"token_vault";
pub const VESTING_VAULT_SEED: &[u8] = b"vesting_vault";
pub const VESTING_SEED: &[u8] = b"vesting";
pub const DISTRIBUTOR_SEED: &[u8] = b"distributor";
pub const CLAIM_SEED: &[u8] = b"claim";

/// Seed of `LaunchGuardConfig`, `["launch_guard"]` (2026-10-14): who may launch
/// a curve without the launch cap.
///
/// ⚠️ Must stay byte-identical to `SEED.launchGuard` in
/// `lib/solana/pepper-curve/program.ts`.
pub const LAUNCH_GUARD_SEED: &[u8] = b"launch_guard";

/// Seed of a `LaunchGuardWaiver`, `["launch_guard_waiver", mint]` (2026-10-14):
/// a curve launched without the launch cap.
///
/// ⚠️ Must stay byte-identical to `SEED.launchGuardWaiver` in
/// `lib/solana/pepper-curve/program.ts`: a client deriving another address
/// passes a waiver `buy` does not read, and the cap applies.
pub const LAUNCH_GUARD_WAIVER_SEED: &[u8] = b"launch_guard_waiver";

/// Seed of the payout transit, `["payout", source]`: a zero-data, system-owned
/// PDA a payout passes through so it reaches the user by a System Program
/// transfer, the only form an explorer lists as SOL received (`crate::payout`).
///
/// ⚠️ Must stay byte-identical to `SEED.payoutTransit` in
/// `lib/solana/pepper-curve/program.ts`: a client deriving another address
/// gets every sell and claim refused with `AccountMismatch`.
pub const PAYOUT_TRANSIT_SEED: &[u8] = b"payout";

/// Seed of the keyless placeholder beneficiary used by fan-led launches.
/// The PDA it derives is off-curve by construction, so no private key exists
/// for it: an unclaimed allocation can be reassigned but never spent.
///
/// ⚠️ Must stay byte-identical to `SEED.fanLedPlaceholder` in
/// `lib/solana/pepper-curve/program.ts`. If the two ever diverge, tokens
/// already launched point at an address this program cannot reassign.
pub const FAN_LED_PLACEHOLDER_SEED: &[u8] = b"fan-led-placeholder";

// ── Claim registry (`DistributorV2`) ─────────────────────────────────────

/// Most leaves a `DistributorV2` can record: one bit each, so an 8 KiB bitmap,
/// which with the 145-byte header stays inside the 10 KiB an account can be
/// created with from inside a program. A month with more payable wallets is
/// funded as a v1 distributor instead.
///
/// ⚠️ Mirrored by `MAX_DISTRIBUTOR_V2_LEAVES` in
/// `lib/solana/pepper-curve/claim-registry-switch.ts`, held by its test, which
/// reads this file.
pub const MAX_DISTRIBUTOR_V2_LEAVES: u32 = 65_536;

/// Domain tag that opens every v2 dividend leaf (`math::dividend_leaf_v2`). A v1
/// leaf hashes 40 bytes and an internal node 64, so the tag is not what keeps
/// them apart; it keeps a v2 leaf from meaning anything outside this program.
///
/// ⚠️ Must stay byte-identical to `DIVIDEND_LEAF_V2_DOMAIN` in
/// `lib/solana/pepper-curve/merkle.ts`: a client hashing another tag builds
/// trees no claim can prove.
pub const DIVIDEND_LEAF_V2_DOMAIN: &[u8] = b"fanex:dividend:v2";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_v2_registry_fits_an_account_created_by_a_program() {
        // 10 KiB: the most a program can allocate when it creates an account.
        const MAX_CREATED_ACCOUNT_BYTES: usize = 10 * 1024;
        let bitmap = (MAX_DISTRIBUTOR_V2_LEAVES as usize).div_ceil(8);
        assert_eq!(bitmap, 8 * 1024);
        assert!(crate::state::DistributorV2::HEADER_SPACE + bitmap <= MAX_CREATED_ACCOUNT_BYTES);
    }

    #[test]
    fn the_launch_cap_is_one_percent_of_the_supply_for_five_minutes() {
        assert_eq!(LAUNCH_WALLET_CAP, 1_000_000 * 1_000_000);
        assert_eq!(LAUNCH_WALLET_CAP * 100, TOKEN_TOTAL_SUPPLY);
        assert_eq!(LAUNCH_GUARD_SECONDS, 300);
    }

    #[test]
    fn min_creator_target_is_16_sol() {
        assert_eq!(MIN_CREATOR_TARGET_LAMPORTS, 16 * LAMPORTS_PER_SOL);
    }

    #[test]
    fn pool_floor_is_24_sol_and_above_the_target_floor() {
        // The liquidity floor is intentionally ABOVE the creator-target floor:
        // a small creator still seeds a >=24 SOL pool.
        assert_eq!(MIN_CPMM_SEED_LAMPORTS, 24 * LAMPORTS_PER_SOL);
        assert_eq!(CAP_CPMM_SEED_LAMPORTS, 800 * LAMPORTS_PER_SOL);
        assert!(MIN_CREATOR_TARGET_LAMPORTS < MIN_CPMM_SEED_LAMPORTS);
    }
}

// ── The USDC curve (design §10: docs/superpowers/specs/2026-10-11-usdc-curve-program-design.md) ──
//
// Appended below the SOL constants and their tests, so no line above moves: the SOL side stays byte
// for byte as it is (§10.1), and the diff that proves it reads as an append. Every amount is in
// µUSDC (USDC base units, 6 decimals). The seeds stay literal `b"…"` lines: the TypeScript tests
// read them off this file.

/// Seed of `UsdcConfig`, `["usdc_config"]`: the USDC side's own config (§10.2).
pub const USDC_CONFIG_SEED: &[u8] = b"usdc_config";

/// Seed of a USDC curve's reserve, `["usdc_vault", mint]`: a token account of
/// `UsdcConfig.usdc_mint` whose authority is the curve PDA, not an ATA (§10.2).
pub const USDC_VAULT_SEED: &[u8] = b"usdc_vault";

/// Seed of the pool creator, `["pool_creator", mint]`: a system-owned PDA with no data that creates
/// the graduated pool, so CPMM takes its rents and fee from it, never from the curve (§10.3.9).
pub const POOL_CREATOR_SEED: &[u8] = b"pool_creator";

/// Seed of the graduated pool's state, `["cpmm_pool", mint]`: the pool sits at our address, not at
/// CPMM's canonical one, which any holder could create first at a price of their choosing (§10.3.9).
pub const CPMM_POOL_SEED: &[u8] = b"cpmm_pool";

/// Seed of the LP lock's Fee Key mint, `["fee_nft", mint]`: the lock creates that mint without
/// seeds, so the program signs for it (§10.3.9).
pub const FEE_NFT_SEED: &[u8] = b"fee_nft";

/// The least creator target a USDC launch with a split may ask: 1,600 USDC (§10.8, Rick,
/// 2026-10-22), today's 16 SOL at the economic model's $100/SOL.
pub const MIN_CREATOR_TARGET_USDC: u64 = 1_600_000_000;

/// The graduation threshold's floor: 2,400 USDC (§10.8), today's 24 SOL. Above the target floor, so
/// a small creator still seeds a pool of at least this much.
pub const MIN_GRADUATION_THRESHOLD_USDC: u64 = 2_400_000_000;

/// The graduation threshold's cap: 80,000 USDC (§10.8), today's 800 SOL.
pub const CAP_GRADUATION_THRESHOLD_USDC: u64 = 80_000_000_000;

/// The trade fee rate of the Raydium CPMM tier a USDC curve graduates into, in Raydium's millionths
/// (its fee rate denominator is 1,000,000): 1%, the tier today's keeper seeds on. The graduation and
/// `set_usdc_raydium` refuse an `amm_config` that charges anything else (`GraduationFeeTierChanged`,
/// §10.3.9, §10.3.10).
pub const RAYDIUM_TRADE_FEE_RATE_1PCT: u64 = 10_000;

/// `CurveUsdc.phase`, a raw u8 (§10.2, 2026-10-22), not `CurvePhase`, which stays as it is so the
/// SOL IDL does not move. 0 to 2 keep the SOL meanings; PoolCreated, which sits between Graduating
/// and Graduated, is numbered last.
pub const PHASE_BONDING: u8 = 0;
pub const PHASE_GRADUATING: u8 = 1;
pub const PHASE_GRADUATED: u8 = 2;
pub const PHASE_POOL_CREATED: u8 = 3;

/// The `version` byte every USDC account opens with (§10.2): 1, the layout `state.rs` pins.
pub const USDC_ACCOUNT_VERSION: u8 = 1;

/// The decimals of the USDC mint `initialize_usdc_config` takes (`mint::decimals`, §10.3.1):
/// Circle's USDC, on both clusters. Named apart from the LiteSVM harness's own `USDC_DECIMALS`.
pub const USDC_MINT_DECIMALS: u8 = 6;

// The internal audit's fix round (2026-10-29, docs/security/INTERNAL-AUDIT-2026-10-29.md): the raise
// terms `create_curve_usdc` holds a launch to, so the launcher's key alone cannot launch a curve that
// pays a buy's net to its creator past the economic model's relation (ECONOMIC-MODEL §3.1, §4.2).

/// The highest creator target a USDC launch may ask: 100,000 USDC, the economic model's highest row
/// (a 1,000-SOL target, its creator taking 52.22% of a buy's net) at its $100/SOL reference rate.
/// ⚠️ An assumption for Rick to confirm: the launch rules have no fixed maximum, only a video's
/// realism limit, which the program cannot read.
pub const MAX_CREATOR_TARGET_USDC: u64 = 100_000_000_000;

/// The creator's and Fanex's caps as percents of the target (ECONOMIC-MODEL §4.2: 94/6, inside the
/// target), each within a base unit of its share.
pub const CREATOR_TARGET_SHARE_PCT: u64 = 94;
pub const FANEX_TARGET_SHARE_PCT: u64 = 6;

/// A revenue launch's graduation threshold over its target, 3/2 (ECONOMIC-MODEL §3.1, the seed's
/// `max(24, min(800, 1.5 × target))` SOL at the USDC bounds): the launch's threshold is exactly
/// `max(MIN, min(CAP, ⌊target × 3 / 2⌋))`, the formula the launch builder computes.
pub const REVENUE_THRESHOLD_SLOPE_NUMERATOR: u64 = 3;
pub const REVENUE_THRESHOLD_SLOPE_DENOMINATOR: u64 = 2;

#[cfg(test)]
mod usdc_tests {
    use super::*;
    use crate::state::CurvePhase;
    use anchor_lang::prelude::AnchorSerialize;

    /// µUSDC in one USDC.
    const USDC: u64 = 1_000_000;

    #[test]
    fn the_usdc_seeds_are_pinned() {
        // A changed byte moves every USDC address: the curve's vault, its pool and its lock.
        assert_eq!(USDC_CONFIG_SEED, b"usdc_config");
        assert_eq!(USDC_VAULT_SEED, b"usdc_vault");
        assert_eq!(POOL_CREATOR_SEED, b"pool_creator");
        assert_eq!(CPMM_POOL_SEED, b"cpmm_pool");
        assert_eq!(FEE_NFT_SEED, b"fee_nft");
    }

    #[test]
    fn no_two_seeds_of_the_program_are_the_same() {
        // Two equal seeds over the same mint would name one address for two accounts.
        let seeds: [&[u8]; 17] = [
            CONFIG_SEED,
            CURVE_SEED,
            SOL_VAULT_SEED,
            TOKEN_VAULT_SEED,
            VESTING_VAULT_SEED,
            VESTING_SEED,
            DISTRIBUTOR_SEED,
            CLAIM_SEED,
            LAUNCH_GUARD_SEED,
            LAUNCH_GUARD_WAIVER_SEED,
            PAYOUT_TRANSIT_SEED,
            FAN_LED_PLACEHOLDER_SEED,
            USDC_CONFIG_SEED,
            USDC_VAULT_SEED,
            POOL_CREATOR_SEED,
            CPMM_POOL_SEED,
            FEE_NFT_SEED,
        ];
        for (i, a) in seeds.iter().enumerate() {
            for b in &seeds[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn the_usdc_bounds_are_the_sol_ones_at_100_dollars_a_sol() {
        assert_eq!(MIN_CREATOR_TARGET_USDC, 1_600 * USDC);
        assert_eq!(MIN_GRADUATION_THRESHOLD_USDC, 2_400 * USDC);
        assert_eq!(CAP_GRADUATION_THRESHOLD_USDC, 80_000 * USDC);
        let at_100_dollars = |lamports: u64| lamports / LAMPORTS_PER_SOL * 100 * USDC;
        assert_eq!(
            MIN_CREATOR_TARGET_USDC,
            at_100_dollars(MIN_CREATOR_TARGET_LAMPORTS)
        );
        assert_eq!(
            MIN_GRADUATION_THRESHOLD_USDC,
            at_100_dollars(MIN_CPMM_SEED_LAMPORTS)
        );
        assert_eq!(
            CAP_GRADUATION_THRESHOLD_USDC,
            at_100_dollars(CAP_CPMM_SEED_LAMPORTS)
        );
    }

    #[test]
    fn the_usdc_pool_floor_is_above_the_target_floor() {
        // As on the SOL side: a small creator still seeds a pool of at least 2,400 USDC.
        assert!(MIN_CREATOR_TARGET_USDC < MIN_GRADUATION_THRESHOLD_USDC);
        assert!(MIN_GRADUATION_THRESHOLD_USDC < CAP_GRADUATION_THRESHOLD_USDC);
    }

    #[test]
    fn the_raise_terms_are_the_economic_model_s() {
        // The highest row of ECONOMIC-MODEL §4.2, 1,000 SOL, at the $100/SOL the other bounds use.
        assert_eq!(MAX_CREATOR_TARGET_USDC, 100_000 * USDC);
        assert_eq!(MAX_CREATOR_TARGET_USDC, 1_000 * 100 * USDC);
        assert!(MAX_CREATOR_TARGET_USDC > MIN_CREATOR_TARGET_USDC);
        assert_eq!(CREATOR_TARGET_SHARE_PCT + FANEX_TARGET_SHARE_PCT, 100);
        assert_eq!((CREATOR_TARGET_SHARE_PCT, FANEX_TARGET_SHARE_PCT), (94, 6));
        // 1.5, the seed's slope.
        assert_eq!(
            (REVENUE_THRESHOLD_SLOPE_NUMERATOR, REVENUE_THRESHOLD_SLOPE_DENOMINATOR),
            (3, 2)
        );
    }

    #[test]
    fn the_raydium_tier_is_one_percent() {
        const RAYDIUM_FEE_RATE_DENOMINATOR: u64 = 1_000_000;
        assert_eq!(RAYDIUM_TRADE_FEE_RATE_1PCT, 10_000);
        assert_eq!(
            RAYDIUM_TRADE_FEE_RATE_1PCT * 100,
            RAYDIUM_FEE_RATE_DENOMINATOR
        );
    }

    #[test]
    fn the_usdc_phases_keep_the_sol_numbers_and_pool_created_comes_last() {
        let wire = |phase: CurvePhase| {
            let mut out = Vec::new();
            phase.serialize(&mut out).unwrap();
            out
        };
        assert_eq!(wire(CurvePhase::Bonding), [PHASE_BONDING]);
        assert_eq!(wire(CurvePhase::Graduating), [PHASE_GRADUATING]);
        assert_eq!(wire(CurvePhase::Graduated), [PHASE_GRADUATED]);
        assert_eq!(
            [
                PHASE_BONDING,
                PHASE_GRADUATING,
                PHASE_GRADUATED,
                PHASE_POOL_CREATED
            ],
            [0, 1, 2, 3]
        );
    }

    #[test]
    fn the_usdc_config_takes_a_six_decimal_mint_and_writes_version_1() {
        // A USDC mint of other decimals would price every launch a thousandfold off.
        assert_eq!(USDC_MINT_DECIMALS, 6);
        assert_eq!(USDC, 10u64.pow(u32::from(USDC_MINT_DECIMALS)));
        assert_eq!(USDC_ACCOUNT_VERSION, 1);
    }
}

// ── The USDC launch's strings (design §10.3.4, 2026-10-23) ──────────────────────────────────────

/// The longest symbol `create_curve_usdc` takes: 10 bytes, Metaplex's own limit, where a SOL launch
/// takes `MAX_SYMBOL_LEN`'s 12. An 11-byte symbol passes the SOL cap and Metaplex refuses it inside
/// its CPI (LiteSVM `metaplex_create_curve.rs`), so the USDC launch refuses it first, with
/// `StringTooLong`. The name and the uri keep `MAX_NAME_LEN` and `MAX_URI_LEN`, Metaplex's too.
pub const MAX_USDC_SYMBOL_LEN: usize = 10;

#[cfg(test)]
mod usdc_launch_tests {
    use super::*;
    use anchor_spl::metadata::mpl_token_metadata;

    #[test]
    fn the_usdc_launch_takes_the_strings_metaplex_takes() {
        assert_eq!(MAX_USDC_SYMBOL_LEN, 10);
        assert_eq!(MAX_USDC_SYMBOL_LEN, mpl_token_metadata::MAX_SYMBOL_LENGTH);
        assert_eq!(MAX_NAME_LEN, mpl_token_metadata::MAX_NAME_LENGTH);
        assert_eq!(MAX_URI_LEN, mpl_token_metadata::MAX_URI_LENGTH);
        // The SOL cap stays as it is: the live launches were checked against it.
        assert!(MAX_USDC_SYMBOL_LEN < MAX_SYMBOL_LEN);
    }
}
