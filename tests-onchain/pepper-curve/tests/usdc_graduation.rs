//! `graduate_usdc_pool` and `graduate_usdc_lock`, the USDC curve's graduation into Raydium's CPMM
//! and its LP lock, run against the built program and the CPMM, LP lock and Metaplex programs
//! dumped from mainnet, with mainnet's 1% tier and its create-pool fee receiver (fixtures/;
//! docs/solana/PEPPER-CURVE-PROGRAM.md §5, §13; usdc-curve design §10.3.9).
//!
//! First what Raydium's own `initialize` does, called directly with the program's hand encoder:
//! it opens a pool, and refuses one on a tier whose `disable_create_pool` is set. Then the
//! graduation: the phase, the pool at the program's own PDA as CPMM records it, its two sides in
//! address order, seeded from the counters (a donation left behind), the LP read back, the pool
//! creator's ATAs opened by a stranger first, every lamport left on the pool creator back to the
//! caller, each refusal with its code and nothing moved, and the compute units. The curves are
//! written into the bank in Graduating, or reach it through a real buy.
//!
//! Then the lock (2026-10-23), below the pool's tests: Raydium's own `lock_cp_liquidity` called
//! directly with the program's encoder, then `graduate_usdc_lock` on a curve the pool step left in
//! PoolCreated: Graduated, the whole LP in the lock's vault, the Fee Key to `lp_fee_owner`, the
//! counters zeroed; what a stranger can do between the two transactions; each refusal; and both
//! steps in one v0 transaction with an address lookup table, measured against apart.

mod common;

use anchor_lang::error::ErrorCode;
use anchor_lang::solana_program::program_option::COption;
use anchor_spl::associated_token::get_associated_token_address as ata;
use anchor_spl::associated_token::spl_associated_token_account;
use anchor_spl::token::spl_token;
use common::*;
use pepper_curve::constants::*;
use pepper_curve::errors::FanexError;
use pepper_curve::events::{GraduatedUsdc, GraduationPoolCreated, GraduationReadyUsdc};
use pepper_curve::instructions::SetUsdcRaydiumParams;
use pepper_curve::raydium::{
    cpmm_initialize_data, lock_cp_liquidity_data, lock_funding, parse_amm_config, parse_pool_state, pool_creator_funding,
    CpmmInitializeAccounts, LockCpLiquidityAccounts, AMM_CONFIG_DISABLE_CREATE_POOL_OFFSET, AMM_CONFIG_TRADE_FEE_RATE_OFFSET,
    CPMM_AUTH_SEED, CPMM_INITIALIZE_OPENS, CPMM_OBSERVATION_SEED, CPMM_POOL_LP_MINT_SEED, CPMM_POOL_VAULT_SEED,
    LOCKED_LIQUIDITY_LEN, LOCKED_LIQUIDITY_SEED, LOCK_OPENS, MAX_GRADUATION_EXTRA_LAMPORTS, POOL_CREATOR_SLACK_LAMPORTS,
    AMM_CONFIG_CREATE_POOL_FEE_OFFSET, MAX_CREATE_POOL_FEE_LAMPORTS,
};
use pepper_curve::state::{CurveUsdc, UsdcConfig};
use pepper_curve_math::CurveState;
use anchor_lang::solana_program::program_pack::Pack;
use anchor_lang::solana_program::rent::Rent;
use anchor_lang::Space;
use solana_sdk::{
    account::Account,
    address_lookup_table::{instruction as lookup_table, AddressLookupTableAccount},
    clock::Clock,
    instruction::{AccountMeta, Instruction, InstructionError},
    message::{v0, VersionedMessage},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction, system_program, sysvar,
    transaction::{Transaction, TransactionError, VersionedTransaction},
};

/// The mainnet CPMM's 1% tier, index 1, and the create-pool fee receiver it compiles in.
const AMM_CONFIG_FIXTURE: &str = "raydium_cpmm_amm_config.json";
const CREATE_POOL_FEE_FIXTURE: &str = "raydium_cpmm_create_pool_fee.json";
/// What the mainnet tier charges to create a pool: 0.15 SOL.
const POOL_FEE: u64 = 150_000_000;

/// CPMM's own refusal of a tier whose `disable_create_pool` is set: its `NotApproved`, the first
/// code of its `ErrorCode` (read on the mainnet binary, 2026-10-23).
const CPMM_NOT_APPROVED: u32 = 6000;

/// The vector whose buy reaches the threshold with tokens left, and the one whose partial fill
/// sells the curve out: their `after` is a curve in Graduating.
const REACHES_THE_THRESHOLD: &str = "equity, near the end: a buy reaches the threshold with tokens left";
const SELLS_OUT: &str = "equity, near the end: the partial fill at graduation empties the curve";
/// Both vectors' clock.
const NOW: i64 = 1_800_000_600;

/// What the graduation's transaction asks for: CPMM's `initialize` alone takes most of the 200,000
/// a single instruction gets by default.
const GRADUATE_CU_LIMIT: u32 = 400_000;
const COMPUTE_BUDGET_ID: Pubkey = Pubkey::from_str_const("ComputeBudget111111111111111111111111111111");

fn anchor(code: ErrorCode) -> u32 {
    code.into()
}

fn compute_unit_limit(units: u32) -> Instruction {
    let mut data = vec![2u8];
    data.extend_from_slice(&units.to_le_bytes());
    Instruction { program_id: COMPUTE_BUDGET_ID, accounts: vec![], data }
}

fn cpmm_pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &RAYDIUM_CPMM_ID).0
}

/// Every address of a curve's pool: the pool creator and its ATAs, ours; the rest CPMM's, from
/// the seeds of the SDK's `cpmm/pda.ts`, under our `pool_state`.
struct Pool {
    pool_creator: Pubkey,
    pool_creator_usdc: Pubkey,
    pool_creator_token: Pubkey,
    authority: Pubkey,
    pool_state: Pubkey,
    lp_mint: Pubkey,
    pool_creator_lp: Pubkey,
    usdc_vault: Pubkey,
    token_vault: Pubkey,
    observation: Pubkey,
}

fn pool_of(usdc_mint: &Pubkey, mint: &Pubkey) -> Pool {
    let pool_creator = pda(&[POOL_CREATOR_SEED, mint.as_ref()]);
    let pool_state = pda(&[CPMM_POOL_SEED, mint.as_ref()]);
    let lp_mint = cpmm_pda(&[CPMM_POOL_LP_MINT_SEED, pool_state.as_ref()]);
    Pool {
        pool_creator,
        pool_creator_usdc: ata(&pool_creator, usdc_mint),
        pool_creator_token: ata(&pool_creator, mint),
        authority: cpmm_pda(&[CPMM_AUTH_SEED]),
        pool_state,
        lp_mint,
        pool_creator_lp: ata(&pool_creator, &lp_mint),
        usdc_vault: cpmm_pda(&[CPMM_POOL_VAULT_SEED, pool_state.as_ref(), usdc_mint.as_ref()]),
        token_vault: cpmm_pda(&[CPMM_POOL_VAULT_SEED, pool_state.as_ref(), mint.as_ref()]),
        observation: cpmm_pda(&[CPMM_OBSERVATION_SEED, pool_state.as_ref()]),
    }
}

/// `graduate_usdc_pool` for the curve of `mint`, by `caller`, every account where the program and
/// CPMM derive it and the tier `amm_config`.
fn graduate_ix_on(env: &Env, caller: &Pubkey, mint: &Pubkey, amm_config: &Pubkey) -> Instruction {
    let pool = pool_of(&env.usdc.mint, mint);
    let curve = curve_pda(mint);
    ix(
        pepper_curve::accounts::GraduateUsdcPool {
            caller: *caller,
            usdc_config: usdc_config_pda(),
            curve,
            usdc_vault: usdc_vault_pda(mint),
            token_vault: ata(&curve, mint),
            usdc_mint: env.usdc.mint,
            token_mint: *mint,
            pool_creator: pool.pool_creator,
            pool_creator_usdc: pool.pool_creator_usdc,
            pool_creator_token: pool.pool_creator_token,
            cpmm_program: RAYDIUM_CPMM_ID,
            amm_config: *amm_config,
            cpmm_authority: pool.authority,
            pool_state: pool.pool_state,
            lp_mint: pool.lp_mint,
            pool_creator_lp: pool.pool_creator_lp,
            pool_usdc_vault: pool.usdc_vault,
            pool_token_vault: pool.token_vault,
            create_pool_fee: MAINNET_CREATE_POOL_FEE,
            observation: pool.observation,
            token_program: spl_token::ID,
            associated_token_program: anchor_spl::associated_token::ID,
            system_program: system_program::ID,
            rent: sysvar::rent::ID,
        },
        pepper_curve::instruction::GraduateUsdcPool { extra_lamports: 0 },
    )
}

/// Either step's instruction asking the caller for `extra` more lamports: its data, the
/// discriminator then the u64, rewritten.
fn with_extra(mut ix: Instruction, extra: u64) -> Instruction {
    assert_eq!(ix.data.len(), 16);
    ix.data[8..].copy_from_slice(&extra.to_le_bytes());
    ix
}

fn graduate_ix(env: &Env, caller: &Pubkey, mint: &Pubkey) -> Instruction {
    graduate_ix_on(env, caller, mint, &MAINNET_AMM_CONFIG)
}

/// `ix` with the account at `position` (the struct's field order) replaced by `key`, its flags kept.
fn with_key(mut ix: Instruction, position: usize, key: Pubkey) -> Instruction {
    assert_eq!(ix.accounts.len(), 24);
    ix.accounts[position].pubkey = key;
    ix
}

/// Positions in `GraduateUsdcPool`.
const AT_USDC_VAULT: usize = 3;
const AT_TOKEN_VAULT: usize = 4;
const AT_POOL_CREATOR: usize = 7;
const AT_POOL_CREATOR_USDC: usize = 8;
const AT_POOL_CREATOR_TOKEN: usize = 9;
const AT_CPMM_PROGRAM: usize = 10;
const AT_CPMM_AUTHORITY: usize = 12;
const AT_POOL_STATE: usize = 13;
const AT_LP_MINT: usize = 14;
const AT_POOL_CREATOR_LP: usize = 15;
const AT_POOL_USDC_VAULT: usize = 16;
const AT_POOL_TOKEN_VAULT: usize = 17;
const AT_CREATE_POOL_FEE: usize = 18;
const AT_OBSERVATION: usize = 19;

struct Landed {
    logs: Vec<String>,
    units: u64,
}

/// `ixs` after the compute limit, signed by `caller` where they name it a signer, the
/// transaction's fee paid by the admin: a refused transaction still costs its fee payer the fee, so
/// the caller's lamports show what the program moved, and nothing else.
fn send_with_budget(env: &mut Env, caller: &Keypair, ixs: &[Instruction]) -> Result<Landed, TransactionError> {
    let mut all = vec![compute_unit_limit(GRADUATE_CU_LIMIT)];
    all.extend_from_slice(ixs);
    let admin = env.admin.insecure_clone();
    let mut signers = vec![&admin];
    let names_caller = ixs.iter().any(|ix| ix.accounts.iter().any(|m| m.is_signer && m.pubkey == caller.pubkey()));
    if names_caller && caller.pubkey() != admin.pubkey() {
        signers.push(caller);
    }
    let tx = Transaction::new_signed_with_payer(&all, Some(&admin.pubkey()), &signers, env.svm.latest_blockhash());
    let result = env.svm.send_transaction(tx);
    env.svm.expire_blockhash();
    result.map(|meta| Landed { logs: meta.logs, units: meta.compute_units_consumed }).map_err(|failed| failed.err)
}

fn graduate(env: &mut Env, caller: &Keypair, mint: &Pubkey) -> Result<Landed, TransactionError> {
    let graduate = graduate_ix(env, &caller.pubkey(), mint);
    send_with_budget(env, caller, &[graduate])
}

/// A refusal's custom code, the compute limit's instruction counted (index 1).
fn expect_refused(result: Result<Landed, TransactionError>, expected: u32) {
    match result {
        Err(TransactionError::InstructionError(1, InstructionError::Custom(c))) => {
            assert_eq!(c, expected, "custom error {c}, expected {expected}")
        }
        Err(other) => panic!("expected custom error {expected}, got {other:?}"),
        Ok(_) => panic!("expected custom error {expected}, the graduation landed"),
    }
}

// ── The suite's chain ────────────────────────────────────────────────────────

fn load_raydium(env: &mut Env) {
    load_fixture_program(&mut env.svm, RAYDIUM_CPMM_ID, RAYDIUM_CPMM_SO);
    assert_eq!(env.load_fixture_account(AMM_CONFIG_FIXTURE), MAINNET_AMM_CONFIG);
    assert_eq!(env.load_fixture_account(CREATE_POOL_FEE_FIXTURE), MAINNET_CREATE_POOL_FEE);
}

/// The harness's `UsdcConfig` naming mainnet's Raydium, as `set_usdc_raydium` would leave it, its
/// fee account opened.
fn write_config_with_raydium(env: &mut Env) -> UsdcConfig {
    env.write_usdc_config();
    let config = env.usdc_config_with_raydium();
    env.put_usdc_config(&config);
    config
}

/// A curve at `mint` in Graduating, its counters `state`'s, written with its vaults.
fn graduating(env: &mut Env, mint: Pubkey, state: &CurveState) -> CurveUsdc {
    let mut curve = env.curve_usdc(mint, state);
    curve.phase = PHASE_GRADUATING;
    env.put_curve_usdc(&curve);
    curve
}

/// What a vector's buy leaves: the counters of a curve that has just reached its threshold.
fn after(name: &str) -> CurveState {
    match vector(name).outcome {
        VectorOutcome::Bought(q) => {
            assert!(q.graduates, "{name} graduates");
            q.after
        }
        other => panic!("{name}: {other:?}"),
    }
}

struct Suite {
    env: Env,
    curve: CurveUsdc,
    keeper: Keypair,
}

/// The mainnet CPMM with its tier and fee receiver, a config naming them, a keeper with 10 SOL, and
/// a curve in Graduating at `mint`, its counters `state`'s.
fn suite_at(env: Env, mint: Pubkey, state: &CurveState) -> Suite {
    let mut env = env;
    load_raydium(&mut env);
    write_config_with_raydium(&mut env);
    let curve = graduating(&mut env, mint, state);
    env.set_time(NOW);
    let keeper = env.funded_wallet(10 * SOL);
    Suite { env, curve, keeper }
}

fn suite() -> Suite {
    suite_at(Env::new(), Pubkey::new_unique(), &after(REACHES_THE_THRESHOLD))
}

/// What a refusal must leave as it found: the curve's bytes, its two vaults, the keeper's
/// lamports, and no pool, pool creator ATA or pool creator lamports.
#[derive(Debug, PartialEq, Eq)]
struct Untouched {
    curve: Vec<u8>,
    usdc_vault: u64,
    token_vault: u64,
    keeper: u64,
    pool_state: bool,
    pool_creator_usdc: bool,
    pool_creator_token: bool,
    pool_creator: u64,
}

fn untouched(s: &Suite) -> Untouched {
    let mint = s.curve.mint;
    let pool = pool_of(&s.env.usdc.mint, &mint);
    let curve = curve_pda(&mint);
    Untouched {
        curve: s.env.svm.get_account(&curve).unwrap().data,
        usdc_vault: s.env.token_amount(&usdc_vault_pda(&mint)),
        token_vault: s.env.token_amount(&ata(&curve, &mint)),
        keeper: s.env.lamports(&s.keeper.pubkey()),
        pool_state: s.env.exists(&pool.pool_state),
        pool_creator_usdc: s.env.exists(&pool.pool_creator_usdc),
        pool_creator_token: s.env.exists(&pool.pool_creator_token),
        pool_creator: s.env.lamports(&pool.pool_creator),
    }
}

/// The integer square root, as CPMM takes the pool's liquidity (`U128::integer_sqrt`).
fn isqrt(n: u128) -> u128 {
    if n < 2 {
        return n;
    }
    let mut x = (n as f64).sqrt() as u128;
    while x * x > n {
        x -= 1;
    }
    while (x + 1) * (x + 1) <= n {
        x += 1;
    }
    x
}

/// CPMM keeps 100 LP base units of every new pool's liquidity locked in the pool (its
/// `lock_lp_amount`): the creator receives the rest.
const CPMM_LOCKED_LP: u64 = 100;

/// What CPMM's `initialize` consumed, by CPI, read from its own log line.
fn cpmm_units(logs: &[String]) -> u64 {
    let prefix = format!("Program {RAYDIUM_CPMM_ID} consumed ");
    logs.iter()
        .find_map(|line| line.strip_prefix(&prefix)?.split(' ').next()?.parse().ok())
        .expect("CPMM ran")
}

// ── What Raydium's own `initialize` does (the program's encoder, no CPI) ────────────────────────

/// A creator holding both sides, called straight into CPMM with `CpmmInitializeAccounts` and
/// `cpmm_initialize_data`, a random keypair as the pool state (which signs): the same encoder the
/// graduation's CPI uses, checked against the mainnet binary outside the program.
struct DirectPool {
    env: Env,
    creator: Keypair,
    pool_state: Keypair,
    ixs: Vec<Instruction>,
}

fn direct_initialize(amm_config: Pubkey) -> DirectPool {
    let mut env = Env::new();
    load_raydium(&mut env);
    env.set_time(NOW);
    let creator = env.funded_wallet(10 * SOL);
    let usdc_mint = env.usdc.mint;
    let token_mint = Pubkey::new_unique();
    env.put_launched_mint(token_mint);
    let creator_usdc = env.usdc_account(&creator.pubkey());
    env.mint_usdc(&creator_usdc, 5_000 * USDC);
    let creator_token = ata(&creator.pubkey(), &token_mint);
    env.put_token_account(creator_token, token_mint, creator.pubkey(), 15_000_000 * 1_000_000, COption::None);

    let pool_state = Keypair::new();
    let ps = pool_state.pubkey();
    let (m0, m1) = if usdc_mint < token_mint { (usdc_mint, token_mint) } else { (token_mint, usdc_mint) };
    let (a0, a1) = if usdc_mint < token_mint { (creator_usdc, creator_token) } else { (creator_token, creator_usdc) };
    let (n0, n1) = if usdc_mint < token_mint { (5_000 * USDC, 15_000_000 * 1_000_000) } else { (15_000_000 * 1_000_000, 5_000 * USDC) };
    let lp_mint = cpmm_pda(&[CPMM_POOL_LP_MINT_SEED, ps.as_ref()]);
    let accounts = CpmmInitializeAccounts {
        creator: creator.pubkey(),
        amm_config,
        authority: cpmm_pda(&[CPMM_AUTH_SEED]),
        pool_state: ps,
        token_0_mint: m0,
        token_1_mint: m1,
        lp_mint,
        creator_token_0: a0,
        creator_token_1: a1,
        creator_lp_token: ata(&creator.pubkey(), &lp_mint),
        token_0_vault: cpmm_pda(&[CPMM_POOL_VAULT_SEED, ps.as_ref(), m0.as_ref()]),
        token_1_vault: cpmm_pda(&[CPMM_POOL_VAULT_SEED, ps.as_ref(), m1.as_ref()]),
        create_pool_fee: MAINNET_CREATE_POOL_FEE,
        observation: cpmm_pda(&[CPMM_OBSERVATION_SEED, ps.as_ref()]),
        token_program: spl_token::ID,
        token_0_program: spl_token::ID,
        token_1_program: spl_token::ID,
        associated_token_program: anchor_spl::associated_token::ID,
        system_program: system_program::ID,
        rent: sysvar::rent::ID,
    };
    let initialize = Instruction {
        program_id: RAYDIUM_CPMM_ID,
        accounts: accounts.to_account_metas(),
        data: cpmm_initialize_data(n0, n1, NOW as u64).to_vec(),
    };
    DirectPool { env, creator, pool_state, ixs: vec![compute_unit_limit(GRADUATE_CU_LIMIT), initialize] }
}

fn send_direct(pool: &mut DirectPool) -> Result<(), TransactionError> {
    let tx = Transaction::new_signed_with_payer(
        &pool.ixs,
        Some(&pool.creator.pubkey()),
        &[&pool.creator, &pool.pool_state],
        pool.env.svm.latest_blockhash(),
    );
    let result = pool.env.svm.send_transaction(tx).map(|_| ()).map_err(|failed| failed.err);
    pool.env.svm.expire_blockhash();
    result
}

#[test]
fn raydium_s_initialize_takes_the_encoder_s_instruction() {
    let mut pool = direct_initialize(MAINNET_AMM_CONFIG);
    send_direct(&mut pool).expect("CPMM opens the pool");
    let account = pool.env.svm.get_account(&pool.pool_state.pubkey()).unwrap();
    assert_eq!(account.owner, RAYDIUM_CPMM_ID);
    let state = parse_pool_state(&account.data).expect("a PoolState");
    assert_eq!((state.amm_config, state.pool_creator), (MAINNET_AMM_CONFIG, pool.creator.pubkey()));
    assert!(state.token_0_mint < state.token_1_mint);
}

#[test]
fn raydium_s_initialize_refuses_a_tier_that_disables_pool_creation() {
    // Raydium's admin flips `disable_create_pool` on the pinned tier: CPMM refuses the pool with
    // its `NotApproved`, the whole transaction undone. The graduation refuses that tier itself,
    // before any move (below), so the reserves never reach the pool creator for nothing.
    let mut pool = direct_initialize(MAINNET_AMM_CONFIG);
    let mut tier = pool.env.svm.get_account(&MAINNET_AMM_CONFIG).unwrap();
    tier.data[AMM_CONFIG_DISABLE_CREATE_POOL_OFFSET] = 1;
    assert!(parse_amm_config(&tier.data).unwrap().disable_create_pool);
    pool.env.svm.set_account(MAINNET_AMM_CONFIG, tier).unwrap();
    match send_direct(&mut pool) {
        Err(TransactionError::InstructionError(1, InstructionError::Custom(c))) => assert_eq!(c, CPMM_NOT_APPROVED),
        other => panic!("expected CPMM's NotApproved, got {other:?}"),
    }
    assert!(!pool.env.exists(&pool.pool_state.pubkey()));
}

// ── The graduation ───────────────────────────────────────────────────────────

#[test]
fn graduates_into_a_pool_at_the_program_s_own_address() {
    let mut s = suite();
    let mint = s.curve.mint;
    let pool = pool_of(&s.env.usdc.mint, &mint);
    let curve_before: CurveUsdc = s.env.fetch(&curve_pda(&mint));
    let usdc_seeded = curve_before.real_usdc_reserves;
    let tokens_seeded = curve_before.real_token_reserves + curve_before.migration_token_reserves;
    let keeper_before = s.env.lamports(&s.keeper.pubkey());
    let receiver_before = s.env.lamports(&MAINNET_CREATE_POOL_FEE);
    let keeper = s.keeper.insecure_clone();

    let landed = graduate(&mut s.env, &keeper, &mint).expect("the graduation lands");
    println!("graduate_usdc_pool: {} compute units, CPMM's initialize {}", landed.units, cpmm_units(&landed.logs));

    // The curve: PoolCreated, its counters as they were (the lock zeroes them), its vaults empty.
    let curve: CurveUsdc = s.env.fetch(&curve_pda(&mint));
    assert_eq!(curve.phase, PHASE_POOL_CREATED);
    let mut expected = curve_before.clone();
    expected.phase = PHASE_POOL_CREATED;
    assert_eq!(account_data(&curve, CurveUsdc::INIT_SPACE), account_data(&expected, CurveUsdc::INIT_SPACE));
    assert_eq!(s.env.token_amount(&usdc_vault_pda(&mint)), 0);
    assert_eq!(s.env.token_amount(&ata(&curve_pda(&mint), &mint)), 0);

    // The pool, at our PDA, as CPMM records it; the canonical address never used.
    let account = s.env.svm.get_account(&pool.pool_state).expect("the pool exists");
    assert_eq!((account.owner, account.data.len()), (RAYDIUM_CPMM_ID, 637));
    let state = parse_pool_state(&account.data).expect("a PoolState");
    let usdc_first = s.env.usdc.mint < mint;
    let (m0, m1) = if usdc_first { (s.env.usdc.mint, mint) } else { (mint, s.env.usdc.mint) };
    let (v0, v1) = if usdc_first { (pool.usdc_vault, pool.token_vault) } else { (pool.token_vault, pool.usdc_vault) };
    assert_eq!(
        [state.amm_config, state.pool_creator, state.token_0_mint, state.token_1_mint, state.token_0_vault, state.token_1_vault],
        [MAINNET_AMM_CONFIG, pool.pool_creator, m0, m1, v0, v1]
    );
    assert_eq!([state.lp_mint, state.observation], [pool.lp_mint, pool.observation]);
    assert_eq!([state.token_0_program, state.token_1_program], [spl_token::ID, spl_token::ID]);
    assert!(state.open_time >= NOW as u64, "{}", state.open_time);
    let canonical = cpmm_pda(&[b"pool", MAINNET_AMM_CONFIG.as_ref(), m0.as_ref(), m1.as_ref()]);
    assert!(!s.env.exists(&canonical));

    // Seeded from the counters, to the base unit; the LP the creator holds is the pool's liquidity
    // less CPMM's locked 100.
    assert_eq!(s.env.token_amount(&pool.usdc_vault), usdc_seeded);
    assert_eq!(s.env.token_amount(&pool.token_vault), tokens_seeded);
    let liquidity = isqrt(u128::from(usdc_seeded) * u128::from(tokens_seeded)) as u64;
    assert_eq!(state.lp_supply, liquidity);
    let lp = s.env.token_account(&pool.pool_creator_lp);
    assert_eq!((lp.mint, lp.owner, lp.amount), (pool.lp_mint, pool.pool_creator, liquidity - CPMM_LOCKED_LP));
    assert_eq!(s.env.token_amount(&pool.pool_creator_usdc), 0);
    assert_eq!(s.env.token_amount(&pool.pool_creator_token), 0);

    let events = events_of::<GraduationPoolCreated>(&landed.logs);
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!((e.mint, e.pool_state), (mint, pool.pool_state));
    assert_eq!([e.usdc_seeded, e.tokens_seeded, e.lp_amount], [usdc_seeded, tokens_seeded, liquidity - CPMM_LOCKED_LP]);

    // What CPMM opened, at the sizes the program funds them by.
    let sizes: Vec<usize> = [pool.pool_state, pool.lp_mint, pool.pool_creator_lp, pool.usdc_vault, pool.token_vault, pool.observation]
        .iter()
        .map(|a| s.env.svm.get_account(a).unwrap().data.len())
        .collect();
    let mut opens = CPMM_INITIALIZE_OPENS.to_vec();
    let mut measured = sizes.clone();
    opens.sort_unstable();
    measured.sort_unstable();
    assert_eq!(measured, opens, "the accounts CPMM opens: {sizes:?}");

    // Every lamport accounted for: the keeper paid the two ATAs, what CPMM opened and Raydium's fee
    // (the admin pays the signatures); the pool creator kept nothing.
    let pool_fee = s.env.lamports(&MAINNET_CREATE_POOL_FEE) - receiver_before;
    assert_eq!(pool_fee, POOL_FEE);
    let opened: u64 = [
        pool.pool_creator_usdc,
        pool.pool_creator_token,
        pool.pool_state,
        pool.lp_mint,
        pool.pool_creator_lp,
        pool.usdc_vault,
        pool.token_vault,
        pool.observation,
    ]
    .iter()
    .map(|a| s.env.lamports(a))
    .sum();
    assert_eq!(keeper_before - s.env.lamports(&s.keeper.pubkey()), opened + POOL_FEE);
    assert_eq!(s.env.lamports(&pool.pool_creator), 0);
    assert!(s.env.svm.get_account(&pool.pool_creator).map_or(true, |a| a.lamports == 0));
    let rent = Rent::default();
    let rents: u64 = CPMM_INITIALIZE_OPENS.iter().map(|len| rent.minimum_balance(*len)).sum();
    assert_eq!(pool_creator_funding(POOL_FEE, &rent, 0), Some(POOL_FEE + rents + POOL_CREATOR_SLACK_LAMPORTS));
    println!("the keeper's cost: {} lamports", keeper_before - s.env.lamports(&s.keeper.pubkey()));
}

#[test]
fn the_pool_takes_its_two_mints_in_address_order_whichever_is_lower() {
    // A mint below the USDC mint and one above it: CPMM's token 0 is the lower address either way.
    // Not all-zero: that is the system program's address.
    let mut low_bytes = [0u8; 32];
    low_bytes[31] = 7;
    let low = Pubkey::new_from_array(low_bytes);
    let high = Pubkey::new_from_array([0xFF; 32]);
    for mint in [low, high] {
        let mut s = suite_at(Env::new(), mint, &after(REACHES_THE_THRESHOLD));
        assert!(low < s.env.usdc.mint && s.env.usdc.mint < high);
        let keeper = s.keeper.insecure_clone();
        graduate(&mut s.env, &keeper, &mint).expect("the graduation lands");
        let pool = pool_of(&s.env.usdc.mint, &mint);
        let state = parse_pool_state(&s.env.svm.get_account(&pool.pool_state).unwrap().data).unwrap();
        let expected = if mint == low {
            (mint, s.env.usdc.mint, pool.token_vault, pool.usdc_vault)
        } else {
            (s.env.usdc.mint, mint, pool.usdc_vault, pool.token_vault)
        };
        assert_eq!((state.token_0_mint, state.token_1_mint, state.token_0_vault, state.token_1_vault), expected);
        let curve: CurveUsdc = s.env.fetch(&curve_pda(&mint));
        assert_eq!(s.env.token_amount(&pool.usdc_vault), curve.real_usdc_reserves);
        assert_eq!(s.env.token_amount(&pool.token_vault), curve.real_token_reserves + curve.migration_token_reserves);
    }
}

#[test]
fn a_sold_out_curve_seeds_its_migration_tokens_alone() {
    let state = after(SELLS_OUT);
    assert_eq!(state.real_token_reserves, 0);
    let mut s = suite_at(Env::new(), Pubkey::new_unique(), &state);
    let mint = s.curve.mint;
    let keeper = s.keeper.insecure_clone();
    let landed = graduate(&mut s.env, &keeper, &mint).expect("the graduation lands");
    let pool = pool_of(&s.env.usdc.mint, &mint);
    assert_eq!(s.env.token_amount(&pool.token_vault), USDC_CURVE_MIGRATION_TOKENS);
    assert_eq!(s.env.token_amount(&pool.usdc_vault), state.real_reserve);
    let e = event::<GraduationPoolCreated>(&landed.logs);
    assert_eq!((e.usdc_seeded, e.tokens_seeded), (state.real_reserve, USDC_CURVE_MIGRATION_TOKENS));
}

/// `buy_exact_in` of `usdc_in` by `buyer` into its own ATA, as the buy suite builds it.
fn buy_ix(env: &Env, curve: &CurveUsdc, buyer: &Pubkey, source: &Pubkey, destination: &Pubkey, usdc_in: u64) -> Instruction {
    let config: UsdcConfig = env.fetch(&usdc_config_pda());
    let address = curve_pda(&curve.mint);
    ix(
        pepper_curve::accounts::BuyExactIn {
            user_transfer_authority: *buyer,
            user_source_usdc: *source,
            user_destination_token: *destination,
            usdc_config: usdc_config_pda(),
            curve: address,
            usdc_vault: usdc_vault_pda(&curve.mint),
            token_vault: ata(&address, &curve.mint),
            fee_usdc_account: config.fee_usdc_account,
            creator_usdc_account: None,
            usdc_mint: config.usdc_mint,
            token_mint: curve.mint,
            launch_guard_waiver: None,
            token_program: spl_token::ID,
        },
        pepper_curve::instruction::BuyExactIn { usdc_in, min_tokens_out: 0 },
    )
}

#[test]
fn a_buy_that_reaches_the_threshold_then_the_graduation_open_the_pool() {
    // The vector's curve in Bonding, its threshold-reaching buy through the program, then the
    // graduation of what the buy left.
    let v = vector(REACHES_THE_THRESHOLD);
    let VectorOp::Buy { amount, now } = v.op else { panic!("a buy") };
    let mut env = Env::new();
    load_raydium(&mut env);
    write_config_with_raydium(&mut env);
    let curve = env.write_curve_usdc(&v.state);
    env.set_time(now);
    let mint = curve.mint;
    let wallet = env.funded_wallet(SOL);
    let source = env.usdc_account(&wallet.pubkey());
    env.mint_usdc(&source, amount);
    let open = spl_associated_token_account::instruction::create_associated_token_account_idempotent(
        &env.admin.pubkey(),
        &wallet.pubkey(),
        &mint,
        &spl_token::ID,
    );
    env.send(&[open], &[]).unwrap();
    let destination = ata(&wallet.pubkey(), &mint);
    let buy = buy_ix(&env, &curve, &wallet.pubkey(), &source, &destination, amount);
    let logs = env.send_logged_paid_by(&wallet, &[buy], &[]).expect("the buy lands");
    assert_eq!(event::<GraduationReadyUsdc>(&logs).mint, mint);
    let graduating: CurveUsdc = env.fetch(&curve_pda(&mint));
    assert_eq!(graduating.phase, PHASE_GRADUATING);
    // Swaps end at Graduating.
    let again = buy_ix(&env, &graduating, &wallet.pubkey(), &source, &destination, USDC);
    expect_err(env.send_paid_by(&wallet, &[again], &[]), code(FanexError::NotBonding));

    let keeper = env.funded_wallet(SOL);
    let landed = graduate(&mut env, &keeper, &mint).expect("the graduation lands");
    let e = event::<GraduationPoolCreated>(&landed.logs);
    let after = after(REACHES_THE_THRESHOLD);
    assert_eq!((e.usdc_seeded, e.tokens_seeded), (after.real_reserve, after.real_token_reserves + USDC_CURVE_MIGRATION_TOKENS));
    let pool = pool_of(&env.usdc.mint, &mint);
    assert_eq!(env.token_amount(&pool.usdc_vault), after.real_reserve);
    assert_eq!(env.fetch::<CurveUsdc>(&curve_pda(&mint)).phase, PHASE_POOL_CREATED);
    // The pool takes the buyer's tokens from here: no curve instruction does.
    let sale = buy_ix(&env, &graduating, &wallet.pubkey(), &source, &destination, USDC);
    expect_err(env.send_paid_by(&wallet, &[sale], &[]), code(FanexError::NotBonding));
}

#[test]
fn what_was_sent_straight_to_the_vaults_stays_behind_uncounted() {
    let mut s = suite();
    let mint = s.curve.mint;
    let curve_address = curve_pda(&mint);
    let usdc_vault = usdc_vault_pda(&mint);
    let token_vault = ata(&curve_address, &mint);
    // A stranger's USDC into `usdc_vault`, and tokens into `token_vault` (written: the mint's
    // authority is revoked).
    s.env.mint_usdc(&usdc_vault, 777 * USDC);
    let held = s.env.token_amount(&token_vault);
    s.env.put_token_account(token_vault, mint, curve_address, held + 5_000_000, COption::None);
    let c = s.curve.clone();

    let keeper = s.keeper.insecure_clone();
    let landed = graduate(&mut s.env, &keeper, &mint).expect("the graduation lands");
    let pool = pool_of(&s.env.usdc.mint, &mint);
    assert_eq!(s.env.token_amount(&pool.usdc_vault), c.real_usdc_reserves);
    assert_eq!(s.env.token_amount(&pool.token_vault), c.real_token_reserves + c.migration_token_reserves);
    assert_eq!(s.env.token_amount(&usdc_vault), 777 * USDC);
    assert_eq!(s.env.token_amount(&token_vault), 5_000_000);
    let e = event::<GraduationPoolCreated>(&landed.logs);
    assert_eq!((e.usdc_seeded, e.tokens_seeded), (c.real_usdc_reserves, c.real_token_reserves + c.migration_token_reserves));
}

#[test]
fn the_pool_creator_s_atas_opened_by_a_stranger_first_still_graduate() {
    // Anyone may open an ATA for any owner: a stranger opens both of the pool creator's, and leaves
    // USDC and tokens in them. The graduation takes them as they are, seeds the counters alone,
    // and leaves the stranger's there.
    let mut s = suite();
    let mint = s.curve.mint;
    let pool = pool_of(&s.env.usdc.mint, &mint);
    let stranger = s.env.funded_wallet(SOL);
    let opens: Vec<Instruction> = [s.env.usdc.mint, mint]
        .iter()
        .map(|m| {
            spl_associated_token_account::instruction::create_associated_token_account(
                &stranger.pubkey(),
                &pool.pool_creator,
                m,
                &spl_token::ID,
            )
        })
        .collect();
    s.env.send_paid_by(&stranger, &opens, &[]).expect("the stranger opens both");
    s.env.mint_usdc(&pool.pool_creator_usdc, 12 * USDC);
    s.env.put_token_account(pool.pool_creator_token, mint, pool.pool_creator, 3_000_000, COption::None);
    let keeper_before = s.env.lamports(&s.keeper.pubkey());
    let c = s.curve.clone();

    let keeper = s.keeper.insecure_clone();
    graduate(&mut s.env, &keeper, &mint).expect("the graduation lands");
    assert_eq!(s.env.fetch::<CurveUsdc>(&curve_pda(&mint)).phase, PHASE_POOL_CREATED);
    assert_eq!(s.env.token_amount(&pool.usdc_vault), c.real_usdc_reserves);
    assert_eq!(s.env.token_amount(&pool.token_vault), c.real_token_reserves + c.migration_token_reserves);
    assert_eq!(s.env.token_amount(&pool.pool_creator_usdc), 12 * USDC);
    assert_eq!(s.env.token_amount(&pool.pool_creator_token), 3_000_000);
    // The keeper paid no ATA: the stranger did.
    let opened: u64 = [pool.pool_state, pool.lp_mint, pool.pool_creator_lp, pool.usdc_vault, pool.token_vault, pool.observation]
        .iter()
        .map(|a| s.env.lamports(a))
        .sum();
    assert_eq!(keeper_before - s.env.lamports(&s.keeper.pubkey()), opened + POOL_FEE);
}

#[test]
fn lamports_sent_to_the_pool_creator_go_back_to_the_caller_with_the_rest() {
    let mut s = suite();
    let mint = s.curve.mint;
    let pool = pool_of(&s.env.usdc.mint, &mint);
    let stranger = s.env.funded_wallet(2 * SOL);
    let gift = system_instruction::transfer(&stranger.pubkey(), &pool.pool_creator, SOL);
    s.env.send_paid_by(&stranger, &[gift], &[]).unwrap();
    let keeper_before = s.env.lamports(&s.keeper.pubkey());

    let keeper = s.keeper.insecure_clone();
    graduate(&mut s.env, &keeper, &mint).expect("the graduation lands");
    assert_eq!(s.env.lamports(&pool.pool_creator), 0);
    let opened: u64 = [
        pool.pool_creator_usdc,
        pool.pool_creator_token,
        pool.pool_state,
        pool.lp_mint,
        pool.pool_creator_lp,
        pool.usdc_vault,
        pool.token_vault,
        pool.observation,
    ]
    .iter()
    .map(|a| s.env.lamports(a))
    .sum();
    // The keeper's cost, less the stranger's SOL: everything on the pool creator came back.
    assert_eq!(keeper_before + SOL - s.env.lamports(&s.keeper.pubkey()), opened + POOL_FEE);
}

#[test]
fn lamports_sent_to_the_pool_s_addresses_first_do_not_stop_it() {
    // Every address the pool will take is known from the mint alone, ours and CPMM's, and anyone may
    // send lamports to any address. Were one of them refused for holding lamports, a stranger could
    // hold the curve in Graduating for good: CPMM creates each over its lamports instead, as the
    // associated token program does the pool creator's two ATAs.
    let mut s = suite();
    let mint = s.curve.mint;
    let pool = pool_of(&s.env.usdc.mint, &mint);
    let stranger = s.env.funded_wallet(SOL);
    let targets = [
        pool.pool_creator_usdc,
        pool.pool_creator_token,
        pool.pool_state,
        pool.lp_mint,
        pool.pool_creator_lp,
        pool.usdc_vault,
        pool.token_vault,
        pool.observation,
    ];
    let gifts: Vec<Instruction> = targets.iter().map(|to| system_instruction::transfer(&stranger.pubkey(), to, 1_000)).collect();
    s.env.send_paid_by(&stranger, &gifts, &[]).unwrap();
    for target in targets {
        assert_eq!(s.env.lamports(&target), 1_000);
    }
    let keeper = s.keeper.insecure_clone();
    graduate(&mut s.env, &keeper, &mint).expect("the graduation lands");
    let c = s.curve.clone();
    assert_eq!(s.env.token_amount(&pool.usdc_vault), c.real_usdc_reserves);
    assert_eq!(s.env.token_amount(&pool.pool_creator_lp), {
        let state = parse_pool_state(&s.env.svm.get_account(&pool.pool_state).unwrap().data).unwrap();
        state.lp_supply - CPMM_LOCKED_LP
    });
}

// ── Each refusal, with nothing moved ─────────────────────────────────────────

#[test]
fn the_usdc_kill_switch_and_the_curve_s_pause_stop_the_graduation() {
    let mut s = suite();
    let mint = s.curve.mint;
    let keeper = s.keeper.insecure_clone();

    let mut config: UsdcConfig = s.env.fetch(&usdc_config_pda());
    config.paused = true;
    s.env.put_usdc_config(&config);
    let before = untouched(&s);
    expect_refused(graduate(&mut s.env, &keeper, &mint), code(FanexError::GloballyPaused));
    assert_eq!(untouched(&s), before);
    config.paused = false;
    s.env.put_usdc_config(&config);

    let mut curve: CurveUsdc = s.env.fetch(&curve_pda(&mint));
    curve.paused = true;
    s.env.put(curve_pda(&mint), account_data(&curve, CurveUsdc::INIT_SPACE), pepper_curve::ID);
    let before = untouched(&s);
    expect_refused(graduate(&mut s.env, &keeper, &mint), code(FanexError::CurvePaused));
    assert_eq!(untouched(&s), before);

    // Unpaused, the same curve graduates.
    curve.paused = false;
    s.env.put(curve_pda(&mint), account_data(&curve, CurveUsdc::INIT_SPACE), pepper_curve::ID);
    graduate(&mut s.env, &keeper, &mint).expect("the graduation lands");
}

#[test]
fn the_sol_pause_leaves_the_usdc_graduation_running() {
    let mut s = suite();
    s.env.set_paused(true);
    let mint = s.curve.mint;
    let keeper = s.keeper.insecure_clone();
    graduate(&mut s.env, &keeper, &mint).expect("each side reads only its own flags");
}

#[test]
fn only_a_graduating_curve_graduates_and_only_once() {
    let mut s = suite();
    let mint = s.curve.mint;
    let keeper = s.keeper.insecure_clone();
    for phase in [PHASE_BONDING, PHASE_GRADUATED, PHASE_POOL_CREATED, 4] {
        let mut curve: CurveUsdc = s.env.fetch(&curve_pda(&mint));
        curve.phase = phase;
        s.env.put(curve_pda(&mint), account_data(&curve, CurveUsdc::INIT_SPACE), pepper_curve::ID);
        let before = untouched(&s);
        expect_refused(graduate(&mut s.env, &keeper, &mint), code(FanexError::NotGraduating));
        assert_eq!(untouched(&s), before, "phase {phase}");
    }
    let mut curve: CurveUsdc = s.env.fetch(&curve_pda(&mint));
    curve.phase = PHASE_GRADUATING;
    s.env.put(curve_pda(&mint), account_data(&curve, CurveUsdc::INIT_SPACE), pepper_curve::ID);
    graduate(&mut s.env, &keeper, &mint).expect("the graduation lands");
    expect_refused(graduate(&mut s.env, &keeper, &mint), code(FanexError::NotGraduating));
}

#[test]
fn no_curve_graduates_while_the_raydium_addresses_are_unset() {
    let mut env = Env::new();
    load_raydium(&mut env);
    env.write_usdc_config();
    let curve = graduating(&mut env, Pubkey::new_unique(), &after(REACHES_THE_THRESHOLD));
    env.set_time(NOW);
    let keeper = env.funded_wallet(10 * SOL);
    let mut s = Suite { env, curve, keeper };
    let mint = s.curve.mint;
    let keeper = s.keeper.insecure_clone();
    let before = untouched(&s);
    // The client names mainnet's Raydium; the config names none.
    expect_refused(graduate(&mut s.env, &keeper, &mint), code(FanexError::RaydiumNotSet));
    assert_eq!(untouched(&s), before);
    // The pauses and the phase are read first.
    let mut config: UsdcConfig = s.env.fetch(&usdc_config_pda());
    config.paused = true;
    s.env.put_usdc_config(&config);
    expect_refused(graduate(&mut s.env, &keeper, &mint), code(FanexError::GloballyPaused));
    config.paused = false;
    s.env.put_usdc_config(&config);
    let mut curve: CurveUsdc = s.env.fetch(&curve_pda(&mint));
    curve.phase = PHASE_BONDING;
    s.env.put(curve_pda(&mint), account_data(&curve, CurveUsdc::INIT_SPACE), pepper_curve::ID);
    expect_refused(graduate(&mut s.env, &keeper, &mint), code(FanexError::NotGraduating));
    assert_eq!(s.env.fetch::<UsdcConfig>(&usdc_config_pda()).raydium_cpmm_program, Pubkey::default());
}

/// The mainnet tier's account with `edit` applied: what Raydium's admin would leave.
fn edited_tier(edit: impl FnOnce(&mut Vec<u8>)) -> Account {
    let (_, mut account) = fixture_account(AMM_CONFIG_FIXTURE);
    edit(&mut account.data);
    account
}

#[test]
fn a_tier_whose_rate_moved_is_refused() {
    let mut s = suite();
    let mint = s.curve.mint;
    let keeper = s.keeper.insecure_clone();
    for rate in [2_500u64, 9_999, 10_001, 40_000] {
        let at = AMM_CONFIG_TRADE_FEE_RATE_OFFSET;
        s.env.svm.set_account(MAINNET_AMM_CONFIG, edited_tier(|d| d[at..at + 8].copy_from_slice(&rate.to_le_bytes()))).unwrap();
        let before = untouched(&s);
        expect_refused(graduate(&mut s.env, &keeper, &mint), code(FanexError::GraduationFeeTierChanged));
        assert_eq!(untouched(&s), before, "rate {rate}");
    }
}

/// `set_usdc_raydium` naming mainnet's Raydium with the tier `amm_config`, as the T6 suite builds it.
fn set_raydium_ix(signer: &Pubkey, amm_config: &Pubkey) -> Instruction {
    ix(
        pepper_curve::accounts::SetUsdcRaydium {
            upgrade_authority: *signer,
            program_data: program_data_pda(&pepper_curve::ID),
            usdc_config: usdc_config_pda(),
            raydium_amm_config: Some(*amm_config),
        },
        pepper_curve::instruction::SetUsdcRaydium {
            params: SetUsdcRaydiumParams {
                raydium_cpmm_program: Some(RAYDIUM_CPMM_ID),
                raydium_amm_config: Some(*amm_config),
                raydium_create_pool_fee: Some(MAINNET_CREATE_POOL_FEE),
                raydium_lock_program: Some(RAYDIUM_LP_LOCK_ID),
                raydium_lock_authority: Some(MAINNET_LOCK_AUTHORITY),
            },
        },
    )
}

#[test]
fn a_tier_that_stops_pool_creation_is_refused_before_any_move_and_the_curve_waits_for_another() {
    // Raydium's admin sets `disable_create_pool` on the pinned tier after `set_usdc_raydium`: the
    // graduation refuses it before any move, the curve stays in Graduating, and goes on once the
    // upgrade authority names another 1% tier (the default of 2026-10-23, for Rick to confirm).
    let authority = Keypair::new();
    let env = Env::upgradeable(Some(authority.pubkey()));
    let mut s = suite_at(env, Pubkey::new_unique(), &after(REACHES_THE_THRESHOLD));
    s.env.svm.airdrop(&authority.pubkey(), SOL).unwrap();
    let mint = s.curve.mint;
    let keeper = s.keeper.insecure_clone();
    let off = AMM_CONFIG_DISABLE_CREATE_POOL_OFFSET;
    s.env.svm.set_account(MAINNET_AMM_CONFIG, edited_tier(|d| d[off] = 1)).unwrap();

    let before = untouched(&s);
    expect_refused(graduate(&mut s.env, &keeper, &mint), code(FanexError::PoolCreationDisabled));
    assert_eq!(untouched(&s), before);
    assert_eq!(s.env.fetch::<CurveUsdc>(&curve_pda(&mint)).phase, PHASE_GRADUATING);

    // `set_usdc_raydium` refuses to name that tier again ...
    let refused = set_raydium_ix(&authority.pubkey(), &MAINNET_AMM_CONFIG);
    expect_err(s.env.send_paid_by(&authority, &[refused], &[]), code(FanexError::PoolCreationDisabled));
    // ... and names another 1% tier of the same CPMM, which the graduation then takes.
    let other_tier = Pubkey::new_unique();
    s.env.svm.set_account(other_tier, edited_tier(|_| {})).unwrap();
    s.env.send_paid_by(&authority, &[set_raydium_ix(&authority.pubkey(), &other_tier)], &[]).expect("another tier");
    // The old tier passed now is another address than the config's.
    let stale = graduate_ix_on(&s.env, &keeper.pubkey(), &mint, &MAINNET_AMM_CONFIG);
    expect_refused(send_with_budget(&mut s.env, &keeper, &[stale]), code(FanexError::AccountMismatch));
    let retry = graduate_ix_on(&s.env, &keeper.pubkey(), &mint, &other_tier);
    send_with_budget(&mut s.env, &keeper, &[retry]).expect("the graduation lands on the new tier");
    let pool = pool_of(&s.env.usdc.mint, &mint);
    let state = parse_pool_state(&s.env.svm.get_account(&pool.pool_state).unwrap().data).unwrap();
    assert_eq!(state.amm_config, other_tier);
    assert_eq!(s.env.fetch::<CurveUsdc>(&curve_pda(&mint)).phase, PHASE_POOL_CREATED);
}

#[test]
fn the_raydium_accounts_are_the_config_s() {
    let mut s = suite();
    let mint = s.curve.mint;
    let keeper = s.keeper.insecure_clone();
    let mismatch = code(FanexError::AccountMismatch);
    let before = untouched(&s);

    // Another program in the CPMM's place: the one a forged CPI would reach, the pool creator
    // signing with the reserves in its ATAs. Our own program, and the token program.
    for program in [pepper_curve::ID, spl_token::ID] {
        let forged = with_key(graduate_ix(&s.env, &keeper.pubkey(), &mint), AT_CPMM_PROGRAM, program);
        expect_refused(send_with_budget(&mut s.env, &keeper, &[forged]), mismatch);
    }
    // Another 1% tier of the same CPMM than the config's, and a tier owned by another program.
    let copy = Pubkey::new_unique();
    s.env.svm.set_account(copy, edited_tier(|_| {})).unwrap();
    let forged = graduate_ix_on(&s.env, &keeper.pubkey(), &mint, &copy);
    expect_refused(send_with_budget(&mut s.env, &keeper, &[forged]), mismatch);
    let mut foreign = edited_tier(|_| {});
    foreign.owner = Pubkey::new_unique();
    s.env.svm.set_account(MAINNET_AMM_CONFIG, foreign).unwrap();
    expect_refused(graduate(&mut s.env, &keeper, &mint), mismatch);
    s.env.svm.set_account(MAINNET_AMM_CONFIG, edited_tier(|_| {})).unwrap();
    assert_eq!(untouched(&s), before);
}

#[test]
fn the_fee_account_is_cpmm_s_to_check_and_a_wrong_one_moves_nothing() {
    // The program passes the fee account as given since 2026-10-24: CPMM binds it to its own fee
    // receiver, so a wrong one is refused inside CPMM's `initialize` (its `ConstraintAddress`), the
    // whole transaction with it, reserves and lamports where they were.
    let mut s = suite();
    let mint = s.curve.mint;
    let keeper = s.keeper.insecure_clone();
    let before = untouched(&s);
    let keeper_before = s.env.lamports(&keeper.pubkey());
    let other_fee = s.env.usdc_account(&keeper.pubkey());
    let forged = with_key(graduate_ix(&s.env, &keeper.pubkey(), &mint), AT_CREATE_POOL_FEE, other_fee);
    let result = send_with_budget(&mut s.env, &keeper, &[forged]);
    expect_refused(result, anchor(ErrorCode::ConstraintAddress));
    assert_eq!(untouched(&s), before);
    assert_eq!(s.env.lamports(&keeper.pubkey()), keeper_before);
    // A fee account the config does not name, CPMM's own: the one CPMM takes, whatever the config says.
    let mut config: UsdcConfig = s.env.fetch(&usdc_config_pda());
    config.raydium_create_pool_fee = Pubkey::new_unique();
    s.env.put_usdc_config(&config);
    graduate(&mut s.env, &keeper, &mint).expect("CPMM's fee receiver, whatever the config names");
}

#[test]
fn a_pool_creation_fee_past_its_bound_is_refused_before_any_move() {
    // The internal audit (2026-10-29): Raydium's admin sets the tier's `create_pool_fee` with one
    // signature and no time lock, and the caller paid it whole. Past 0.5 SOL the step is refused,
    // the caller's lamports and the reserves where they were; at the bound it lands.
    let mut s = suite();
    let mint = s.curve.mint;
    let keeper = s.keeper.insecure_clone();
    let before = untouched(&s);
    let keeper_before = s.env.lamports(&keeper.pubkey());
    let off = AMM_CONFIG_CREATE_POOL_FEE_OFFSET;
    let fee_of = |fee: u64| edited_tier(move |d| d[off..off + 8].copy_from_slice(&fee.to_le_bytes()));
    s.env.svm.set_account(MAINNET_AMM_CONFIG, fee_of(20 * SOL)).unwrap();
    expect_refused(graduate(&mut s.env, &keeper, &mint), code(FanexError::CreatePoolFeeTooHigh));
    s.env.svm.set_account(MAINNET_AMM_CONFIG, fee_of(MAX_CREATE_POOL_FEE_LAMPORTS + 1)).unwrap();
    expect_refused(graduate(&mut s.env, &keeper, &mint), code(FanexError::CreatePoolFeeTooHigh));
    assert_eq!(untouched(&s), before);
    assert_eq!(s.env.lamports(&keeper.pubkey()), keeper_before);
    assert_eq!(s.env.fetch::<CurveUsdc>(&curve_pda(&mint)).phase, PHASE_GRADUATING);

    s.env.svm.set_account(MAINNET_AMM_CONFIG, fee_of(MAX_CREATE_POOL_FEE_LAMPORTS)).unwrap();
    graduate(&mut s.env, &keeper, &mint).expect("the bound itself is paid");
    assert_eq!(s.env.fetch::<CurveUsdc>(&curve_pda(&mint)).phase, PHASE_POOL_CREATED);
}

#[test]
fn an_extra_funding_comes_back_whole_and_one_past_its_bound_is_refused() {
    let mut s = suite();
    let mint = s.curve.mint;
    let keeper = s.keeper.insecure_clone();
    let before = untouched(&s);
    let keeper_before = s.env.lamports(&keeper.pubkey());
    let past = with_extra(graduate_ix(&s.env, &keeper.pubkey(), &mint), MAX_GRADUATION_EXTRA_LAMPORTS + 1);
    expect_refused(send_with_budget(&mut s.env, &keeper, &[past]), code(FanexError::GraduationExtraFundingTooLarge));
    assert_eq!(untouched(&s), before);
    assert_eq!(s.env.lamports(&keeper.pubkey()), keeper_before);

    // The bound itself: handed to the pool creator and back in the same instruction, so the
    // caller's cost is what CPMM opened and its fee, as with no extra.
    let pool = pool_of(&s.env.usdc.mint, &mint);
    let at_bound = with_extra(graduate_ix(&s.env, &keeper.pubkey(), &mint), MAX_GRADUATION_EXTRA_LAMPORTS);
    send_with_budget(&mut s.env, &keeper, &[at_bound]).expect("the graduation lands with the extra");
    assert_eq!(s.env.lamports(&pool.pool_creator), 0);
    let opened: u64 = [
        pool.pool_creator_usdc,
        pool.pool_creator_token,
        pool.pool_state,
        pool.lp_mint,
        pool.pool_creator_lp,
        pool.usdc_vault,
        pool.token_vault,
        pool.observation,
    ]
    .iter()
    .map(|a| s.env.lamports(a))
    .sum();
    assert_eq!(keeper_before - s.env.lamports(&keeper.pubkey()), opened + POOL_FEE);
}

#[test]
fn the_pool_sits_at_the_program_s_pda_and_nowhere_else() {
    let mut s = suite();
    let mint = s.curve.mint;
    let keeper = s.keeper.insecure_clone();
    let before = untouched(&s);
    let seeds = anchor(ErrorCode::ConstraintSeeds);
    let usdc_first = s.env.usdc.mint < mint;
    let (m0, m1) = if usdc_first { (s.env.usdc.mint, mint) } else { (mint, s.env.usdc.mint) };
    // CPMM's canonical address, which CPMM would take without a signature, and a random one.
    let canonical = cpmm_pda(&[b"pool", MAINNET_AMM_CONFIG.as_ref(), m0.as_ref(), m1.as_ref()]);
    for pool_state in [canonical, Pubkey::new_unique()] {
        let forged = with_key(graduate_ix(&s.env, &keeper.pubkey(), &mint), AT_POOL_STATE, pool_state);
        expect_refused(send_with_budget(&mut s.env, &keeper, &[forged]), seeds);
    }
    // Another pool creator: another curve's.
    let other = pda(&[POOL_CREATOR_SEED, Pubkey::new_unique().as_ref()]);
    let forged = with_key(graduate_ix(&s.env, &keeper.pubkey(), &mint), AT_POOL_CREATOR, other);
    expect_refused(send_with_budget(&mut s.env, &keeper, &[forged]), seeds);
    assert_eq!(untouched(&s), before);
}

#[test]
fn the_reserves_leave_the_curve_s_own_vaults_alone() {
    let mut s = suite();
    let mint = s.curve.mint;
    let keeper = s.keeper.insecure_clone();
    // Another curve's USDC vault in `usdc_vault`'s place (its seeds), the curve PDA's USDC ATA in
    // `token_vault`'s (the ATA rule of the curve's own mint).
    let other = graduating(&mut s.env, Pubkey::new_unique(), &after(REACHES_THE_THRESHOLD));
    let before = untouched(&s);
    let forged = with_key(graduate_ix(&s.env, &keeper.pubkey(), &mint), AT_USDC_VAULT, usdc_vault_pda(&other.mint));
    expect_refused(send_with_budget(&mut s.env, &keeper, &[forged]), anchor(ErrorCode::ConstraintSeeds));
    let curve_usdc_ata = ata(&curve_pda(&mint), &s.env.usdc.mint);
    s.env.put_token_account(curve_usdc_ata, s.env.usdc.mint, curve_pda(&mint), 0, COption::None);
    let forged = with_key(graduate_ix(&s.env, &keeper.pubkey(), &mint), AT_TOKEN_VAULT, curve_usdc_ata);
    expect_refused(send_with_budget(&mut s.env, &keeper, &[forged]), anchor(ErrorCode::ConstraintAssociated));
    assert_eq!(untouched(&s), before);
}

#[test]
fn the_pool_creator_s_ata_is_the_one_its_address_names() {
    // The keeper's own USDC account in the pool creator's ATA's place, the one a forged list would
    // pay the curve's USDC into. `create_idempotent`'s instruction (anchor-spl) names the ATA it
    // derives from the pool creator and the mint, so that ATA, passed nowhere, is unknown to the
    // CPI (`MissingAccount`) before a token moves; the associated token program would refuse any
    // other address too (its `InvalidSeeds`).
    let mut s = suite();
    let mint = s.curve.mint;
    let keeper = s.keeper.insecure_clone();
    let own = s.env.usdc_account(&keeper.pubkey());
    let before = untouched(&s);
    for position in [AT_POOL_CREATOR_USDC, AT_POOL_CREATOR_TOKEN] {
        let forged = with_key(graduate_ix(&s.env, &keeper.pubkey(), &mint), position, own);
        match send_with_budget(&mut s.env, &keeper, &[forged]) {
            Err(TransactionError::InstructionError(1, InstructionError::MissingAccount)) => {}
            Err(other) => panic!("account {position}: expected MissingAccount, got {other:?}"),
            Ok(_) => panic!("account {position}: the graduation landed"),
        }
    }
    assert_eq!(untouched(&s), before);
    assert_eq!(s.env.token_amount(&own), 0);
}

#[test]
fn a_graduation_fits_its_compute_budget_whatever_the_mint() {
    // Each fresh mint moves the PDA searches (the pool creator, the pool state, the ATAs, CPMM's
    // own), so the units vary with it: the range over 48 curves, printed and held under 350,000,
    // a margin under the `GRADUATE_CU_LIMIT` its transaction asks for (250,170 to 298,201 over three
    // runs on 2026-10-23: the mints differ from run to run, and so does the range).
    let mut s = suite();
    let keeper = s.keeper.insecure_clone();
    s.env.svm.airdrop(&keeper.pubkey(), 100 * SOL).unwrap();
    let state = after(REACHES_THE_THRESHOLD);
    let mut units = Vec::new();
    for _ in 0..48 {
        let curve = graduating(&mut s.env, Pubkey::new_unique(), &state);
        let landed = graduate(&mut s.env, &keeper, &curve.mint).expect("the graduation lands");
        units.push((landed.units, cpmm_units(&landed.logs)));
    }
    let range = |of: fn(&(u64, u64)) -> u64| (units.iter().map(of).min().unwrap(), units.iter().map(of).max().unwrap());
    let (all, cpmm) = (range(|u| u.0), range(|u| u.1));
    println!("graduate_usdc_pool over 48 curves: {} to {} compute units, CPMM's initialize {} to {}", all.0, all.1, cpmm.0, cpmm.1);
    assert!(all.1 < 350_000, "{units:?}");
}

#[test]
fn cpmm_checks_the_accounts_it_derives() {
    // The program leaves the accounts CPMM derives to CPMM's own checks (§10.3.9, as built): its
    // seeds bind its authority, its LP mint, the pool's two vaults and its observation account, and
    // its `init` of the pool creator's LP account names the ATA it derives. Another address in any
    // of them undoes the whole graduation, the reserves back in the curve's vaults.
    let mut s = suite();
    let mint = s.curve.mint;
    let keeper = s.keeper.insecure_clone();
    let before = untouched(&s);
    for position in [AT_CPMM_AUTHORITY, AT_LP_MINT, AT_POOL_USDC_VAULT, AT_POOL_TOKEN_VAULT, AT_OBSERVATION] {
        let forged = with_key(graduate_ix(&s.env, &keeper.pubkey(), &mint), position, Pubkey::new_unique());
        // Anchor's `ConstraintSeeds`, raised by CPMM on its own account, after the reserves reached
        // the pool creator: undone with the rest.
        expect_refused(send_with_budget(&mut s.env, &keeper, &[forged]), anchor(ErrorCode::ConstraintSeeds));
        assert_eq!(untouched(&s), before, "account {position}");
    }
    // The LP account: CPMM opens it through the associated token program at the address it derives,
    // which a forged list leaves out of the CPI.
    let forged = with_key(graduate_ix(&s.env, &keeper.pubkey(), &mint), AT_POOL_CREATOR_LP, Pubkey::new_unique());
    match send_with_budget(&mut s.env, &keeper, &[forged]) {
        Err(TransactionError::InstructionError(1, InstructionError::MissingAccount)) => {}
        Err(other) => panic!("the LP account: expected MissingAccount, got {other:?}"),
        Ok(_) => panic!("the LP account: the graduation landed"),
    }
    assert_eq!(untouched(&s), before, "the LP account");
    // The vaults are where the reserves go: the caller's own USDC account in the pool's USDC vault's
    // place, and the two vaults swapped, are refused by the same seeds.
    let own = s.env.usdc_account(&keeper.pubkey());
    let before = untouched(&s);
    let forged = with_key(graduate_ix(&s.env, &keeper.pubkey(), &mint), AT_POOL_USDC_VAULT, own);
    expect_refused(send_with_budget(&mut s.env, &keeper, &[forged]), anchor(ErrorCode::ConstraintSeeds));
    let pool = pool_of(&s.env.usdc.mint, &mint);
    let swapped = with_key(
        with_key(graduate_ix(&s.env, &keeper.pubkey(), &mint), AT_POOL_USDC_VAULT, pool.token_vault),
        AT_POOL_TOKEN_VAULT,
        pool.usdc_vault,
    );
    expect_refused(send_with_budget(&mut s.env, &keeper, &[swapped]), anchor(ErrorCode::ConstraintSeeds));
    assert_eq!(untouched(&s), before, "the vaults");
    assert_eq!(s.env.token_amount(&own), 0);
}

#[test]
fn a_sol_curve_is_refused_by_its_discriminator() {
    let mut s = suite();
    let keeper = s.keeper.insecure_clone();
    s.env.write_tradeable_curve(OCT_5_2026 - DAY);
    let sol_mint = s.env.mint;
    let forged = graduate_ix(&s.env, &keeper.pubkey(), &sol_mint);
    expect_refused(send_with_budget(&mut s.env, &keeper, &[forged]), ACCOUNT_DISCRIMINATOR_MISMATCH);
}

#[test]
fn the_caller_must_sign() {
    let mut s = suite();
    let mint = s.curve.mint;
    let keeper = s.keeper.insecure_clone();
    let mut unsigned = graduate_ix(&s.env, &keeper.pubkey(), &mint);
    unsigned.accounts[0] = AccountMeta::new(keeper.pubkey(), false);
    let payer = s.env.funded_wallet(SOL);
    expect_refused(send_with_budget(&mut s.env, &payer, &[unsigned]), ACCOUNT_NOT_SIGNER);
}

// ── The graduation's second step: `graduate_usdc_lock` (§10.3.9, 2026-10-23) ────────────────────

/// Metaplex Token Metadata's PDA of a mint: where the lock would write the Fee Key's metadata.
fn metadata_pda(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"metadata", anchor_spl::metadata::ID.as_ref(), mint.as_ref()], &anchor_spl::metadata::ID).0
}

/// The lock's record of a Fee Key's mint, `["locked_liquidity", fee_nft_mint]` under the lock.
fn locked_liquidity_pda(fee_nft_mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[LOCKED_LIQUIDITY_SEED, fee_nft_mint.as_ref()], &RAYDIUM_LP_LOCK_ID).0
}

/// Every address of a curve's lock: the Fee Key's mint, our PDA; the rest where the lock and the
/// associated token program take them, as the SDK derives them (`lockLp`): the Fee Key's account,
/// the lock's record, its LP vault and the Fee Key's metadata address.
struct Lock {
    fee_nft_mint: Pubkey,
    fee_nft_account: Pubkey,
    locked_liquidity: Pubkey,
    locked_lp_vault: Pubkey,
    metadata_account: Pubkey,
}

fn lock_of(fee_nft_owner: &Pubkey, mint: &Pubkey, lp_mint: &Pubkey) -> Lock {
    let fee_nft_mint = pda(&[FEE_NFT_SEED, mint.as_ref()]);
    Lock {
        fee_nft_mint,
        fee_nft_account: ata(fee_nft_owner, &fee_nft_mint),
        locked_liquidity: locked_liquidity_pda(&fee_nft_mint),
        locked_lp_vault: ata(&MAINNET_LOCK_AUTHORITY, lp_mint),
        metadata_account: metadata_pda(&fee_nft_mint),
    }
}

/// `graduate_usdc_lock` for the curve of `mint`, by `caller`, every account where the program, the
/// lock and CPMM derive it, the Fee Key to the config's `lp_fee_owner`.
fn lock_ix(env: &Env, caller: &Pubkey, mint: &Pubkey) -> Instruction {
    let config: UsdcConfig = env.fetch(&usdc_config_pda());
    let pool = pool_of(&env.usdc.mint, mint);
    let lock = lock_of(&config.lp_fee_owner, mint, &pool.lp_mint);
    ix(
        pepper_curve::accounts::GraduateUsdcLock {
            caller: *caller,
            usdc_config: usdc_config_pda(),
            curve: curve_pda(mint),
            pool_creator: pool.pool_creator,
            pool_creator_lp: pool.pool_creator_lp,
            lock_program: RAYDIUM_LP_LOCK_ID,
            lock_authority: MAINNET_LOCK_AUTHORITY,
            fee_nft_owner: config.lp_fee_owner,
            fee_nft_mint: lock.fee_nft_mint,
            fee_nft_account: lock.fee_nft_account,
            pool_state: pool.pool_state,
            locked_liquidity: lock.locked_liquidity,
            lp_mint: pool.lp_mint,
            locked_lp_vault: lock.locked_lp_vault,
            pool_usdc_vault: pool.usdc_vault,
            pool_token_vault: pool.token_vault,
            metadata_account: lock.metadata_account,
            token_program: spl_token::ID,
            associated_token_program: anchor_spl::associated_token::ID,
            system_program: system_program::ID,
            metadata_program: anchor_spl::metadata::ID,
            rent: sysvar::rent::ID,
        },
        pepper_curve::instruction::GraduateUsdcLock { extra_lamports: 0 },
    )
}

/// `ix` with the account at `position` (`GraduateUsdcLock`'s field order) replaced by `key`.
fn with_lock_key(mut ix: Instruction, position: usize, key: Pubkey) -> Instruction {
    assert_eq!(ix.accounts.len(), 22);
    ix.accounts[position].pubkey = key;
    ix
}

/// Positions in `GraduateUsdcLock`.
const L_POOL_CREATOR: usize = 3;
const L_POOL_CREATOR_LP: usize = 4;
const L_LOCK_PROGRAM: usize = 5;
const L_LOCK_AUTHORITY: usize = 6;
const L_FEE_NFT_OWNER: usize = 7;
const L_FEE_NFT_MINT: usize = 8;
const L_FEE_NFT_ACCOUNT: usize = 9;
const L_POOL_STATE: usize = 10;
const L_LOCKED_LIQUIDITY: usize = 11;
const L_LP_MINT: usize = 12;
const L_LOCKED_LP_VAULT: usize = 13;
const L_POOL_USDC_VAULT: usize = 14;
const L_POOL_TOKEN_VAULT: usize = 15;
const L_METADATA_ACCOUNT: usize = 16;
const L_METADATA_PROGRAM: usize = 20;

/// As `send_with_budget`, the transaction's logs printed when it fails, so a test that fails on a
/// refusal it did not expect shows what the lock said.
fn send_logged_with_budget(env: &mut Env, caller: &Keypair, ixs: &[Instruction]) -> Result<Landed, TransactionError> {
    let mut all = vec![compute_unit_limit(GRADUATE_CU_LIMIT)];
    all.extend_from_slice(ixs);
    let admin = env.admin.insecure_clone();
    let mut signers = vec![&admin];
    let names_caller = ixs.iter().any(|ix| ix.accounts.iter().any(|m| m.is_signer && m.pubkey == caller.pubkey()));
    if names_caller && caller.pubkey() != admin.pubkey() {
        signers.push(caller);
    }
    let tx = Transaction::new_signed_with_payer(&all, Some(&admin.pubkey()), &signers, env.svm.latest_blockhash());
    let result = env.svm.send_transaction(tx);
    env.svm.expire_blockhash();
    result.map(|meta| Landed { logs: meta.logs, units: meta.compute_units_consumed }).map_err(|failed| {
        println!("refused, {:?}:\n{}", failed.err, failed.meta.logs.join("\n"));
        failed.err
    })
}

fn lock(env: &mut Env, caller: &Keypair, mint: &Pubkey) -> Result<Landed, TransactionError> {
    let lock = lock_ix(env, &caller.pubkey(), mint);
    send_logged_with_budget(env, caller, &[lock])
}

/// Raydium's LP lock, and Metaplex, whose program the lock takes as an account.
fn load_lock(env: &mut Env) {
    load_fixture_program(&mut env.svm, RAYDIUM_LP_LOCK_ID, RAYDIUM_LP_LOCK_SO);
    env.load_metaplex();
}

struct LockSuite {
    env: Env,
    mint: Pubkey,
    keeper: Keypair,
    /// The config's `lp_fee_owner`: a key of its own, so the Fee Key is told from the admin's and
    /// the caller's.
    fee_owner: Keypair,
}

/// `suite_at`'s chain with the LP lock and Metaplex loaded and the config's `lp_fee_owner` a key of
/// its own: a curve in Graduating, no pool yet.
fn lock_chain_at(env: Env, mint: Pubkey, state: &CurveState) -> LockSuite {
    let Suite { mut env, keeper, .. } = suite_at(env, mint, state);
    load_lock(&mut env);
    let fee_owner = Keypair::new();
    let mut config: UsdcConfig = env.fetch(&usdc_config_pda());
    config.lp_fee_owner = fee_owner.pubkey();
    env.put_usdc_config(&config);
    LockSuite { env, mint, keeper, fee_owner }
}

/// `lock_chain_at`, then the curve's pool created by `graduate_usdc_pool` in a transaction of its
/// own: a curve in PoolCreated, its LP on the pool creator, as between two transactions on a
/// cluster.
fn lock_suite_at(env: Env, mint: Pubkey, state: &CurveState) -> LockSuite {
    let mut s = lock_chain_at(env, mint, state);
    let keeper = s.keeper.insecure_clone();
    graduate(&mut s.env, &keeper, &mint).expect("the pool step lands");
    assert_eq!(s.env.fetch::<CurveUsdc>(&curve_pda(&mint)).phase, PHASE_POOL_CREATED);
    s
}

fn lock_suite() -> LockSuite {
    lock_suite_at(Env::new(), Pubkey::new_unique(), &after(REACHES_THE_THRESHOLD))
}

/// The amount of the token account at `address`, 0 where there is none.
fn amount_or_zero(env: &Env, address: &Pubkey) -> u64 {
    env.svm
        .get_account(address)
        .filter(|a| a.owner == spl_token::ID)
        .and_then(|a| spl_token::state::Account::unpack(&a.data).ok())
        .map_or(0, |t| t.amount)
}

/// What a refused lock must leave as it found: the curve's bytes, the pool creator's LP and
/// lamports, the keeper's lamports, and none of the lock's accounts.
#[derive(Debug, PartialEq, Eq)]
struct LockUntouched {
    curve: Vec<u8>,
    pool_creator_lp: u64,
    pool_creator: u64,
    keeper: u64,
    fee_nft_mint: bool,
    locked_liquidity: bool,
    locked_lp_vault: u64,
}

fn lock_untouched(s: &LockSuite) -> LockUntouched {
    let pool = pool_of(&s.env.usdc.mint, &s.mint);
    let lock = lock_of(&s.fee_owner.pubkey(), &s.mint, &pool.lp_mint);
    LockUntouched {
        curve: s.env.svm.get_account(&curve_pda(&s.mint)).unwrap().data,
        pool_creator_lp: amount_or_zero(&s.env, &pool.pool_creator_lp),
        pool_creator: s.env.lamports(&pool.pool_creator),
        keeper: s.env.lamports(&s.keeper.pubkey()),
        fee_nft_mint: s.env.exists(&lock.fee_nft_mint),
        locked_liquidity: s.env.exists(&lock.locked_liquidity),
        locked_lp_vault: amount_or_zero(&s.env, &lock.locked_lp_vault),
    }
}

/// What `program` consumed each time it ran, at the top level or by CPI, in the order it ran, read
/// from its own log lines.
fn units_of(logs: &[String], program: &Pubkey) -> Vec<u64> {
    let prefix = format!("Program {program} consumed ");
    logs.iter().filter_map(|line| line.strip_prefix(&prefix)?.split(' ').next()?.parse().ok()).collect()
}

/// The transaction's instruction trace, every top-level instruction and every CPI: the runtime
/// caps it at 64 (`max_instruction_trace_length`).
fn trace_length(logs: &[String]) -> usize {
    logs.iter().filter(|line| line.starts_with("Program ") && line.contains(" invoke [")).count()
}

// ── What Raydium's own lock does (the program's encoder, no CPI) ─────────────────────────────────

#[test]
fn raydium_s_lock_takes_the_encoder_s_instruction() {
    // The pool `direct_initialize` opens, a keypair its creator, then that creator's whole LP locked
    // by a call straight into the lock with `LockCpLiquidityAccounts` and `lock_cp_liquidity_data`,
    // the encoder the graduation's CPI uses, a keypair as the Fee Key's mint and no metadata.
    let mut pool = direct_initialize(MAINNET_AMM_CONFIG);
    send_direct(&mut pool).expect("CPMM opens the pool");
    load_lock(&mut pool.env);
    let creator = pool.creator.insecure_clone();
    let pool_state = pool.pool_state.pubkey();
    let state = parse_pool_state(&pool.env.svm.get_account(&pool_state).unwrap().data).unwrap();
    let creator_lp = ata(&creator.pubkey(), &state.lp_mint);
    let lp = pool.env.token_amount(&creator_lp);
    assert!(lp > 0);
    let nft_mint = Keypair::new();
    let fee_owner = Pubkey::new_unique();
    let accounts = LockCpLiquidityAccounts {
        authority: MAINNET_LOCK_AUTHORITY,
        payer: creator.pubkey(),
        liquidity_owner: creator.pubkey(),
        fee_nft_owner: fee_owner,
        fee_nft_mint: nft_mint.pubkey(),
        fee_nft_account: ata(&fee_owner, &nft_mint.pubkey()),
        pool_state,
        locked_liquidity: locked_liquidity_pda(&nft_mint.pubkey()),
        lp_mint: state.lp_mint,
        liquidity_owner_lp: creator_lp,
        locked_lp_vault: ata(&MAINNET_LOCK_AUTHORITY, &state.lp_mint),
        token_0_vault: state.token_0_vault,
        token_1_vault: state.token_1_vault,
        metadata_account: metadata_pda(&nft_mint.pubkey()),
        rent: sysvar::rent::ID,
        system_program: system_program::ID,
        token_program: spl_token::ID,
        associated_token_program: anchor_spl::associated_token::ID,
        metadata_program: anchor_spl::metadata::ID,
    };
    let lock = Instruction {
        program_id: RAYDIUM_LP_LOCK_ID,
        accounts: accounts.to_account_metas(),
        data: lock_cp_liquidity_data(lp, false).to_vec(),
    };
    let tx = Transaction::new_signed_with_payer(
        &[compute_unit_limit(GRADUATE_CU_LIMIT), lock],
        Some(&creator.pubkey()),
        &[&creator, &nft_mint],
        pool.env.svm.latest_blockhash(),
    );
    let landed = pool
        .env
        .svm
        .send_transaction(tx)
        .unwrap_or_else(|f| panic!("the lock refused it, {:?}:\n{}", f.err, f.meta.logs.join("\n")));
    println!("lock_cp_liquidity called directly: {:?} compute units", units_of(&landed.logs, &RAYDIUM_LP_LOCK_ID));
    assert_eq!(pool.env.token_amount(&creator_lp), 0);
    let vault = pool.env.token_account(&ata(&MAINNET_LOCK_AUTHORITY, &state.lp_mint));
    assert_eq!((vault.mint, vault.owner, vault.amount), (state.lp_mint, MAINNET_LOCK_AUTHORITY, lp));
    let nft = pool.env.token_account(&ata(&fee_owner, &nft_mint.pubkey()));
    assert_eq!((nft.mint, nft.owner, nft.amount), (nft_mint.pubkey(), fee_owner, 1));
    assert!(!pool.env.exists(&metadata_pda(&nft_mint.pubkey())), "no metadata asked, none written");
}

// ── The lock ─────────────────────────────────────────────────────────────────────────────────────

#[test]
fn locks_the_whole_lp_and_hands_the_fee_key_to_lp_fee_owner() {
    let mut s = lock_suite();
    let mint = s.mint;
    let pool = pool_of(&s.env.usdc.mint, &mint);
    let lock_accounts = lock_of(&s.fee_owner.pubkey(), &mint, &pool.lp_mint);
    let before: CurveUsdc = s.env.fetch(&curve_pda(&mint));
    let lp_amount = s.env.token_amount(&pool.pool_creator_lp);
    assert!(lp_amount > 0);
    let pool_state_before = s.env.svm.get_account(&pool.pool_state).unwrap();
    let vaults_before = (s.env.token_amount(&pool.usdc_vault), s.env.token_amount(&pool.token_vault));
    let keeper_before = s.env.lamports(&s.keeper.pubkey());
    let keeper = s.keeper.insecure_clone();

    let landed = lock(&mut s.env, &keeper, &mint).expect("the lock lands");
    println!(
        "graduate_usdc_lock: {} compute units in its transaction, the lock's lock_cp_liquidity {:?}, a trace of {}",
        landed.units,
        units_of(&landed.logs, &RAYDIUM_LP_LOCK_ID),
        trace_length(&landed.logs)
    );

    // The curve: Graduated now, its three reserve counters zeroed, nothing else moved.
    let curve: CurveUsdc = s.env.fetch(&curve_pda(&mint));
    let mut expected = before.clone();
    expected.phase = PHASE_GRADUATED;
    expected.graduated_at = NOW;
    expected.real_usdc_reserves = 0;
    expected.real_token_reserves = 0;
    expected.migration_token_reserves = 0;
    assert_eq!(account_data(&curve, CurveUsdc::INIT_SPACE), account_data(&expected, CurveUsdc::INIT_SPACE));

    // The LP: all of it out of the pool creator's account into the lock's vault, the lock
    // authority's ATA of the LP mint; the pool itself as it was.
    assert_eq!(s.env.token_amount(&pool.pool_creator_lp), 0);
    let vault = s.env.token_account(&lock_accounts.locked_lp_vault);
    assert_eq!((vault.mint, vault.owner, vault.amount), (pool.lp_mint, MAINNET_LOCK_AUTHORITY, lp_amount));
    assert_eq!(s.env.svm.get_account(&pool.pool_state).unwrap(), pool_state_before);
    assert_eq!((s.env.token_amount(&pool.usdc_vault), s.env.token_amount(&pool.token_vault)), vaults_before);

    // The Fee Key: the one token of a 0-decimal mint at our PDA `["fee_nft", mint]`, in
    // `lp_fee_owner`'s ATA of it.
    let nft_mint = s.env.svm.get_account(&lock_accounts.fee_nft_mint).expect("the Fee Key's mint");
    assert_eq!(nft_mint.owner, spl_token::ID);
    let nft_mint = spl_token::state::Mint::unpack(&nft_mint.data).unwrap();
    println!("the Fee Key's mint: {nft_mint:?}");
    assert_eq!((nft_mint.supply, nft_mint.decimals), (1, 0));
    let nft = s.env.token_account(&lock_accounts.fee_nft_account);
    assert_eq!((nft.mint, nft.owner, nft.amount), (lock_accounts.fee_nft_mint, s.fee_owner.pubkey(), 1));

    // The lock's record, the lock's own account at its seeds; no metadata written.
    let record = s.env.svm.get_account(&lock_accounts.locked_liquidity).expect("the lock's record");
    assert_eq!((record.owner, record.data.len()), (RAYDIUM_LP_LOCK_ID, LOCKED_LIQUIDITY_LEN));
    assert!(!s.env.exists(&lock_accounts.metadata_account));

    let events = events_of::<GraduatedUsdc>(&landed.logs);
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!((e.mint, e.pool_state, e.lp_locked, e.fee_nft_mint), (mint, pool.pool_state, lp_amount, lock_accounts.fee_nft_mint));

    // Every lamport: the keeper paid the rents of the four accounts the lock opened, at the sizes
    // the program funds them by (the admin pays the signatures); the pool creator kept nothing.
    let opened = [lock_accounts.fee_nft_mint, lock_accounts.fee_nft_account, lock_accounts.locked_liquidity, lock_accounts.locked_lp_vault];
    let mut sizes: Vec<usize> = opened.iter().map(|a| s.env.svm.get_account(a).unwrap().data.len()).collect();
    let mut opens = LOCK_OPENS.to_vec();
    sizes.sort_unstable();
    opens.sort_unstable();
    assert_eq!(sizes, opens, "the accounts the lock opens");
    let rents: u64 = opened.iter().map(|a| s.env.lamports(a)).sum();
    assert_eq!(keeper_before - s.env.lamports(&s.keeper.pubkey()), rents);
    assert_eq!(s.env.lamports(&pool.pool_creator), 0);
    let rent = Rent::default();
    assert_eq!(lock_funding(&rent, 0), Some(LOCK_OPENS.iter().map(|len| rent.minimum_balance(*len)).sum::<u64>() + POOL_CREATOR_SLACK_LAMPORTS));
    println!("the keeper's cost for the lock: {rents} lamports");
}

#[test]
fn the_lock_s_extra_funding_comes_back_whole_and_one_past_its_bound_is_refused() {
    let mut s = lock_suite();
    let mint = s.mint;
    let pool = pool_of(&s.env.usdc.mint, &mint);
    let accounts = lock_of(&s.fee_owner.pubkey(), &mint, &pool.lp_mint);
    let keeper = s.keeper.insecure_clone();
    let before = lock_untouched(&s);
    let keeper_before = s.env.lamports(&keeper.pubkey());
    let past = with_extra(lock_ix(&s.env, &keeper.pubkey(), &mint), MAX_GRADUATION_EXTRA_LAMPORTS + 1);
    expect_refused(send_logged_with_budget(&mut s.env, &keeper, &[past]), code(FanexError::GraduationExtraFundingTooLarge));
    assert_eq!(lock_untouched(&s), before);
    assert_eq!(s.env.lamports(&keeper.pubkey()), keeper_before);

    let at_bound = with_extra(lock_ix(&s.env, &keeper.pubkey(), &mint), MAX_GRADUATION_EXTRA_LAMPORTS);
    send_logged_with_budget(&mut s.env, &keeper, &[at_bound]).expect("the lock lands with the extra");
    assert_eq!(s.env.lamports(&pool.pool_creator), 0);
    let rents: u64 = [accounts.fee_nft_mint, accounts.fee_nft_account, accounts.locked_liquidity, accounts.locked_lp_vault]
        .iter()
        .map(|a| s.env.lamports(a))
        .sum();
    assert_eq!(keeper_before - s.env.lamports(&keeper.pubkey()), rents);
    assert_eq!(s.env.fetch::<CurveUsdc>(&curve_pda(&mint)).phase, PHASE_GRADUATED);
}

#[test]
fn a_curve_whose_pool_is_not_created_is_refused() {
    // Before `graduate_usdc_pool`: a curve in Graduating, no pool, no LP mint, no LP account.
    let Suite { mut env, curve, keeper } = suite();
    load_lock(&mut env);
    let mint = curve.mint;
    let pool = pool_of(&env.usdc.mint, &mint);
    let before = env.svm.get_account(&curve_pda(&mint)).unwrap().data;
    expect_refused(lock(&mut env, &keeper, &mint), code(FanexError::PoolNotCreated));
    assert_eq!(env.svm.get_account(&curve_pda(&mint)).unwrap().data, before);
    assert!(!env.exists(&pool.pool_state));

    // After it, every phase but PoolCreated, and a second lock once Graduated.
    let mut s = lock_suite();
    let mint = s.mint;
    let keeper = s.keeper.insecure_clone();
    for phase in [PHASE_BONDING, PHASE_GRADUATING, PHASE_GRADUATED, 4] {
        let mut curve: CurveUsdc = s.env.fetch(&curve_pda(&mint));
        curve.phase = phase;
        s.env.put(curve_pda(&mint), account_data(&curve, CurveUsdc::INIT_SPACE), pepper_curve::ID);
        let before = lock_untouched(&s);
        expect_refused(lock(&mut s.env, &keeper, &mint), code(FanexError::PoolNotCreated));
        assert_eq!(lock_untouched(&s), before, "phase {phase}");
    }
    let mut curve: CurveUsdc = s.env.fetch(&curve_pda(&mint));
    curve.phase = PHASE_POOL_CREATED;
    s.env.put(curve_pda(&mint), account_data(&curve, CurveUsdc::INIT_SPACE), pepper_curve::ID);
    lock(&mut s.env, &keeper, &mint).expect("the lock lands");
    expect_refused(lock(&mut s.env, &keeper, &mint), code(FanexError::PoolNotCreated));
}

#[test]
fn the_usdc_kill_switch_and_the_curve_s_pause_stop_the_lock() {
    // Paused between the two steps, the curve waits in PoolCreated with its LP on the pool creator,
    // and the lock goes on once unpaused.
    let mut s = lock_suite();
    let mint = s.mint;
    let keeper = s.keeper.insecure_clone();

    let mut config: UsdcConfig = s.env.fetch(&usdc_config_pda());
    config.paused = true;
    s.env.put_usdc_config(&config);
    let before = lock_untouched(&s);
    expect_refused(lock(&mut s.env, &keeper, &mint), code(FanexError::GloballyPaused));
    assert_eq!(lock_untouched(&s), before);
    config.paused = false;
    s.env.put_usdc_config(&config);

    let mut curve: CurveUsdc = s.env.fetch(&curve_pda(&mint));
    curve.paused = true;
    s.env.put(curve_pda(&mint), account_data(&curve, CurveUsdc::INIT_SPACE), pepper_curve::ID);
    let before = lock_untouched(&s);
    expect_refused(lock(&mut s.env, &keeper, &mint), code(FanexError::CurvePaused));
    assert_eq!(lock_untouched(&s), before);
    // The pauses are read before the phase.
    curve.phase = PHASE_GRADUATING;
    s.env.put(curve_pda(&mint), account_data(&curve, CurveUsdc::INIT_SPACE), pepper_curve::ID);
    expect_refused(lock(&mut s.env, &keeper, &mint), code(FanexError::CurvePaused));

    // Unpaused, the same curve's lock lands; the SOL pause stops nothing on the USDC side.
    curve.paused = false;
    curve.phase = PHASE_POOL_CREATED;
    s.env.put(curve_pda(&mint), account_data(&curve, CurveUsdc::INIT_SPACE), pepper_curve::ID);
    s.env.set_paused(true);
    lock(&mut s.env, &keeper, &mint).expect("the lock lands");
    assert_eq!(s.env.fetch::<CurveUsdc>(&curve_pda(&mint)).phase, PHASE_GRADUATED);
}

#[test]
fn no_lp_is_locked_while_the_raydium_addresses_are_unset() {
    let mut s = lock_suite();
    let mint = s.mint;
    let keeper = s.keeper.insecure_clone();
    let mut config: UsdcConfig = s.env.fetch(&usdc_config_pda());
    let set = config.clone();
    config.raydium_cpmm_program = Pubkey::default();
    config.raydium_amm_config = Pubkey::default();
    config.raydium_create_pool_fee = Pubkey::default();
    config.raydium_lock_program = Pubkey::default();
    config.raydium_lock_authority = Pubkey::default();
    s.env.put_usdc_config(&config);
    let before = lock_untouched(&s);
    // The client names mainnet's lock; the config names none.
    expect_refused(lock(&mut s.env, &keeper, &mint), code(FanexError::RaydiumNotSet));
    assert_eq!(lock_untouched(&s), before);
    // One field unset is enough.
    for field in 0..5 {
        let mut one = set.clone();
        match field {
            0 => one.raydium_cpmm_program = Pubkey::default(),
            1 => one.raydium_amm_config = Pubkey::default(),
            2 => one.raydium_create_pool_fee = Pubkey::default(),
            3 => one.raydium_lock_program = Pubkey::default(),
            _ => one.raydium_lock_authority = Pubkey::default(),
        }
        s.env.put_usdc_config(&one);
        expect_refused(lock(&mut s.env, &keeper, &mint), code(FanexError::RaydiumNotSet));
    }
    // The pauses and the phase are read first.
    config.paused = true;
    s.env.put_usdc_config(&config);
    expect_refused(lock(&mut s.env, &keeper, &mint), code(FanexError::GloballyPaused));
    config.paused = false;
    s.env.put_usdc_config(&config);
    let mut curve: CurveUsdc = s.env.fetch(&curve_pda(&mint));
    curve.phase = PHASE_GRADUATING;
    s.env.put(curve_pda(&mint), account_data(&curve, CurveUsdc::INIT_SPACE), pepper_curve::ID);
    expect_refused(lock(&mut s.env, &keeper, &mint), code(FanexError::PoolNotCreated));
    assert_eq!(lock_untouched(&s).pool_creator_lp, before.pool_creator_lp);
}

#[test]
fn the_lock_its_authority_and_the_fee_key_s_owner_are_the_config_s() {
    let mut s = lock_suite();
    let mint = s.mint;
    let keeper = s.keeper.insecure_clone();
    let mismatch = code(FanexError::AccountMismatch);
    let before = lock_untouched(&s);
    // Another program in the lock's place, the one a forged CPI would hand the pool creator's
    // signature and LP: the CPMM, our own program, the token program.
    for program in [RAYDIUM_CPMM_ID, pepper_curve::ID, spl_token::ID] {
        let forged = with_lock_key(lock_ix(&s.env, &keeper.pubkey(), &mint), L_LOCK_PROGRAM, program);
        expect_refused(send_logged_with_budget(&mut s.env, &keeper, &[forged]), mismatch);
    }
    // Another lock authority: the owner of the vault the LP would go to.
    let forged = with_lock_key(lock_ix(&s.env, &keeper.pubkey(), &mint), L_LOCK_AUTHORITY, keeper.pubkey());
    expect_refused(send_logged_with_budget(&mut s.env, &keeper, &[forged]), mismatch);
    // Another owner for the Fee Key: the caller's own key.
    let forged = with_lock_key(lock_ix(&s.env, &keeper.pubkey(), &mint), L_FEE_NFT_OWNER, keeper.pubkey());
    expect_refused(send_logged_with_budget(&mut s.env, &keeper, &[forged]), mismatch);
    // Another metadata program.
    let forged = with_lock_key(lock_ix(&s.env, &keeper.pubkey(), &mint), L_METADATA_PROGRAM, spl_token::ID);
    expect_refused(send_logged_with_budget(&mut s.env, &keeper, &[forged]), anchor(ErrorCode::ConstraintAddress));
    assert_eq!(lock_untouched(&s), before);
}

#[test]
fn the_lock_takes_the_pool_s_own_lp_mint_vaults_and_lp_account() {
    // A second curve's pool beside the first: its LP mint and vaults are real CPMM accounts of
    // another pool, the ones a forged list would hand the lock.
    let mut s = lock_suite();
    let mint = s.mint;
    let keeper = s.keeper.insecure_clone();
    let other_mint = Pubkey::new_unique();
    graduating(&mut s.env, other_mint, &after(REACHES_THE_THRESHOLD));
    graduate(&mut s.env, &keeper, &other_mint).expect("the other pool");
    let pool = pool_of(&s.env.usdc.mint, &mint);
    let other = pool_of(&s.env.usdc.mint, &other_mint);
    let mismatch = code(FanexError::AccountMismatch);
    let seeds = anchor(ErrorCode::ConstraintSeeds);
    let before = lock_untouched(&s);

    let cases = [
        (L_LP_MINT, other.lp_mint, mismatch),
        (L_POOL_USDC_VAULT, other.usdc_vault, mismatch),
        (L_POOL_TOKEN_VAULT, other.token_vault, mismatch),
        (L_POOL_USDC_VAULT, pool.token_vault, mismatch),
        (L_POOL_TOKEN_VAULT, pool.usdc_vault, mismatch),
        // The other pool's state, and the other curve's pool creator and Fee Key mint: our seeds.
        (L_POOL_STATE, other.pool_state, seeds),
        (L_POOL_CREATOR, other.pool_creator, seeds),
        (L_FEE_NFT_MINT, pda(&[FEE_NFT_SEED, other_mint.as_ref()]), seeds),
        (L_FEE_NFT_MINT, Pubkey::new_unique(), seeds),
        // The other pool creator's LP account, of the other LP mint.
        (L_POOL_CREATOR_LP, other.pool_creator_lp, mismatch),
    ];
    for (position, key, expected) in cases {
        let forged = with_lock_key(lock_ix(&s.env, &keeper.pubkey(), &mint), position, key);
        expect_refused(send_logged_with_budget(&mut s.env, &keeper, &[forged]), expected);
        assert_eq!(lock_untouched(&s), before, "account {position}");
    }
    // Both vaults swapped together.
    let swapped = with_lock_key(
        with_lock_key(lock_ix(&s.env, &keeper.pubkey(), &mint), L_POOL_USDC_VAULT, pool.token_vault),
        L_POOL_TOKEN_VAULT,
        pool.usdc_vault,
    );
    expect_refused(send_logged_with_budget(&mut s.env, &keeper, &[swapped]), mismatch);

    // A token account of the curve's LP mint the pool creator owns that is not its ATA: anyone may
    // open one, put a few LP in it and pass it, which would mark the curve Graduated with the LP of
    // the ATA left unlocked. Refused, the ATA's LP left where it is.
    let stray = Pubkey::new_unique();
    s.env.put_token_account(stray, pool.lp_mint, pool.pool_creator, 5, COption::None);
    let forged = with_lock_key(lock_ix(&s.env, &keeper.pubkey(), &mint), L_POOL_CREATOR_LP, stray);
    expect_refused(send_logged_with_budget(&mut s.env, &keeper, &[forged]), mismatch);
    assert_eq!(lock_untouched(&s), before);

    // The right accounts still lock.
    lock(&mut s.env, &keeper, &mint).expect("the lock lands");
    assert_eq!(s.env.token_amount(&pool.pool_creator_lp), 0);
    assert_eq!(s.env.token_amount(&stray), 5);
}

#[test]
fn the_lock_checks_the_accounts_it_derives() {
    // The accounts the lock opens and the program does not derive (the Fee Key's account, the
    // lock's record, its LP vault) are the lock's to check, as CPMM checks its own in the pool
    // step, and it does, each refusal undone whole: the two ATAs it opens through the associated
    // token program, whose instruction names the ATA it derives, so any other address is missing
    // from the CPI (`MissingAccount`; the caller's own ATA of the Fee Key's mint included, which
    // would hand the caller the Fee Key), and its record by its own seeds (Anchor's 2006).
    let mut s = lock_suite();
    let mint = s.mint;
    let keeper = s.keeper.insecure_clone();
    let pool = pool_of(&s.env.usdc.mint, &mint);
    let lock_accounts = lock_of(&s.fee_owner.pubkey(), &mint, &pool.lp_mint);
    let before = lock_untouched(&s);
    let missing = || TransactionError::InstructionError(1, InstructionError::MissingAccount);
    let seeds = || TransactionError::InstructionError(1, InstructionError::Custom(anchor(ErrorCode::ConstraintSeeds)));
    for (position, key, expected) in [
        (L_FEE_NFT_ACCOUNT, ata(&keeper.pubkey(), &lock_accounts.fee_nft_mint), missing()),
        (L_FEE_NFT_ACCOUNT, Pubkey::new_unique(), missing()),
        (L_LOCKED_LIQUIDITY, Pubkey::new_unique(), seeds()),
        (L_LOCKED_LIQUIDITY, locked_liquidity_pda(&Pubkey::new_unique()), seeds()),
        (L_LOCKED_LP_VAULT, ata(&keeper.pubkey(), &pool.lp_mint), missing()),
        (L_LOCKED_LP_VAULT, Pubkey::new_unique(), missing()),
    ] {
        let forged = with_lock_key(lock_ix(&s.env, &keeper.pubkey(), &mint), position, key);
        let result = send_logged_with_budget(&mut s.env, &keeper, &[forged]);
        assert_eq!(result.err(), Some(expected), "account {position}: {key}");
        assert_eq!(lock_untouched(&s), before, "account {position}");
    }
    // The metadata address, which the lock writes only when asked for metadata and does not check
    // otherwise, is bound by the program to the Fee Key's Metaplex PDA (2006, before the CPI).
    let forged = with_lock_key(lock_ix(&s.env, &keeper.pubkey(), &mint), L_METADATA_ACCOUNT, Pubkey::new_unique());
    expect_refused(send_logged_with_budget(&mut s.env, &keeper, &[forged]), anchor(ErrorCode::ConstraintSeeds));
    let other_metadata = metadata_pda(&pda(&[FEE_NFT_SEED, Pubkey::new_unique().as_ref()]));
    let forged = with_lock_key(lock_ix(&s.env, &keeper.pubkey(), &mint), L_METADATA_ACCOUNT, other_metadata);
    expect_refused(send_logged_with_budget(&mut s.env, &keeper, &[forged]), anchor(ErrorCode::ConstraintSeeds));
    assert_eq!(lock_untouched(&s), before);
    lock(&mut s.env, &keeper, &mint).expect("the right accounts still lock");
}

// ── Between the two transactions: what anyone may do while the LP waits on the pool creator ─────

#[test]
fn the_lock_s_lp_vault_opened_by_a_stranger_first_still_locks() {
    // Once the pool exists, anyone may open the lock's LP vault, the lock authority's ATA of the
    // LP mint, before the lock step runs (the spec's open question, §10.3.9). The lock takes the
    // vault as it finds it: the whole LP locked into it, the stranger's rent left there, the keeper
    // paying only for the three accounts the lock still opens and getting the vault's share back.
    let mut s = lock_suite();
    let mint = s.mint;
    let pool = pool_of(&s.env.usdc.mint, &mint);
    let lock_accounts = lock_of(&s.fee_owner.pubkey(), &mint, &pool.lp_mint);
    let stranger = s.env.funded_wallet(SOL);
    let open = spl_associated_token_account::instruction::create_associated_token_account(
        &stranger.pubkey(),
        &MAINNET_LOCK_AUTHORITY,
        &pool.lp_mint,
        &spl_token::ID,
    );
    s.env.send_paid_by(&stranger, &[open], &[]).expect("the stranger opens the lock's LP vault");
    let vault_rent = s.env.lamports(&lock_accounts.locked_lp_vault);
    assert_eq!(s.env.token_amount(&lock_accounts.locked_lp_vault), 0);
    let lp_amount = s.env.token_amount(&pool.pool_creator_lp);
    // Nor can anyone freeze the LP waiting on the pool creator: CPMM's LP mint has no freeze
    // authority, and the pool creator, which only this program signs for, is the LP account's
    // owner, with no delegate.
    let lp_mint = spl_token::state::Mint::unpack(&s.env.svm.get_account(&pool.lp_mint).unwrap().data).unwrap();
    assert_eq!(lp_mint.freeze_authority, COption::None);
    let lp_account = s.env.token_account(&pool.pool_creator_lp);
    assert_eq!((lp_account.owner, lp_account.delegate, lp_account.close_authority), (pool.pool_creator, COption::None, COption::None));
    let keeper_before = s.env.lamports(&s.keeper.pubkey());
    let keeper = s.keeper.insecure_clone();

    lock(&mut s.env, &keeper, &mint).expect("the lock takes the stranger's vault");
    assert_eq!(s.env.fetch::<CurveUsdc>(&curve_pda(&mint)).phase, PHASE_GRADUATED);
    let vault = s.env.token_account(&lock_accounts.locked_lp_vault);
    assert_eq!((vault.owner, vault.amount), (MAINNET_LOCK_AUTHORITY, lp_amount));
    assert_eq!(s.env.lamports(&lock_accounts.locked_lp_vault), vault_rent);
    assert_eq!(s.env.token_amount(&lock_accounts.fee_nft_account), 1);
    let rents: u64 = [lock_accounts.fee_nft_mint, lock_accounts.fee_nft_account, lock_accounts.locked_liquidity]
        .iter()
        .map(|a| s.env.lamports(a))
        .sum();
    assert_eq!(keeper_before - s.env.lamports(&s.keeper.pubkey()), rents);
    assert_eq!(s.env.lamports(&pool.pool_creator), 0);
}

#[test]
fn lamports_sent_first_to_the_lock_s_addresses_do_not_stop_it() {
    // Every address the lock takes is known from the mint alone (the Fee Key's mint is our PDA; its
    // account, the lock's record and its LP vault derive from it and from the LP mint), and anyone
    // may send lamports to any address: were one of them refused for holding lamports, a stranger
    // could hold the curve in PoolCreated, its LP unlocked, for good. Each gift is rent-exempt for
    // an empty account, as a cluster requires of a new one; what reached the pool creator goes to
    // the caller with the rest.
    let mut s = lock_suite();
    let mint = s.mint;
    let pool = pool_of(&s.env.usdc.mint, &mint);
    let lock_accounts = lock_of(&s.fee_owner.pubkey(), &mint, &pool.lp_mint);
    let gift = 1_000_000;
    let stranger = s.env.funded_wallet(SOL);
    let targets = [
        lock_accounts.fee_nft_mint,
        lock_accounts.fee_nft_account,
        lock_accounts.locked_liquidity,
        lock_accounts.locked_lp_vault,
        lock_accounts.metadata_account,
        pool.pool_creator,
    ];
    let gifts: Vec<Instruction> = targets.iter().map(|to| system_instruction::transfer(&stranger.pubkey(), to, gift)).collect();
    s.env.send_paid_by(&stranger, &gifts, &[]).unwrap();
    let lp_amount = s.env.token_amount(&pool.pool_creator_lp);
    let keeper_before = s.env.lamports(&s.keeper.pubkey());
    let keeper = s.keeper.insecure_clone();

    lock(&mut s.env, &keeper, &mint).expect("the lock lands over the lamports");
    assert_eq!(s.env.fetch::<CurveUsdc>(&curve_pda(&mint)).phase, PHASE_GRADUATED);
    assert_eq!(s.env.token_amount(&lock_accounts.locked_lp_vault), lp_amount);
    assert_eq!(s.env.token_amount(&lock_accounts.fee_nft_account), 1);
    assert_eq!(s.env.lamports(&pool.pool_creator), 0);
    // The keeper paid what the four accounts hold less the stranger's lamports in them, and got the
    // stranger's lamports on the pool creator.
    let opened = [lock_accounts.fee_nft_mint, lock_accounts.fee_nft_account, lock_accounts.locked_liquidity, lock_accounts.locked_lp_vault];
    let held: u64 = opened.iter().map(|a| s.env.lamports(a)).sum();
    assert_eq!(keeper_before + gift - s.env.lamports(&s.keeper.pubkey()), held - 4 * gift);
    assert_eq!(s.env.lamports(&lock_accounts.metadata_account), gift, "the metadata address is left as found");
}

#[test]
fn anyone_may_lock_between_the_two_steps_and_the_fee_key_still_goes_to_lp_fee_owner() {
    // The keeper creates the pool; a stranger sends the lock later, in a transaction of its own:
    // permissionless, the stranger paying the lock's rents and getting back what the lock does not
    // spend, the Fee Key to `lp_fee_owner` and never to the caller.
    let mut s = lock_suite();
    let mint = s.mint;
    let pool = pool_of(&s.env.usdc.mint, &mint);
    let lock_accounts = lock_of(&s.fee_owner.pubkey(), &mint, &pool.lp_mint);
    let stranger = s.env.funded_wallet(SOL);
    let stranger_before = s.env.lamports(&stranger.pubkey());
    let lp_amount = s.env.token_amount(&pool.pool_creator_lp);
    lock(&mut s.env, &stranger, &mint).expect("a stranger's lock lands");
    assert_eq!(s.env.token_amount(&lock_accounts.locked_lp_vault), lp_amount);
    let nft = s.env.token_account(&lock_accounts.fee_nft_account);
    assert_eq!((nft.owner, nft.amount), (s.fee_owner.pubkey(), 1));
    assert!(!s.env.exists(&ata(&stranger.pubkey(), &lock_accounts.fee_nft_mint)));
    let rents: u64 = [lock_accounts.fee_nft_mint, lock_accounts.fee_nft_account, lock_accounts.locked_liquidity, lock_accounts.locked_lp_vault]
        .iter()
        .map(|a| s.env.lamports(a))
        .sum();
    assert_eq!(stranger_before - s.env.lamports(&stranger.pubkey()), rents);
}

/// `set_usdc_raydium` naming `cpmm` and its tier `amm_config`, the fee account and the lock kept.
fn set_raydium_to(signer: &Pubkey, cpmm: Pubkey, amm_config: Pubkey) -> Instruction {
    ix(
        pepper_curve::accounts::SetUsdcRaydium {
            upgrade_authority: *signer,
            program_data: program_data_pda(&pepper_curve::ID),
            usdc_config: usdc_config_pda(),
            raydium_amm_config: Some(amm_config),
        },
        pepper_curve::instruction::SetUsdcRaydium {
            params: SetUsdcRaydiumParams {
                raydium_cpmm_program: Some(cpmm),
                raydium_amm_config: Some(amm_config),
                raydium_create_pool_fee: None,
                raydium_lock_program: None,
                raydium_lock_authority: None,
            },
        },
    )
}

#[test]
fn a_cpmm_change_between_the_two_steps_leaves_the_lock_on_the_pool_s_own() {
    // The upgrade authority names another CPMM and tier after the pool step (a Raydium migration,
    // §10.3.10): the lock binds the LP mint and the vaults to the pool's own `PoolState`, written
    // by the CPMM that created it, and never reads `raydium_cpmm_program`.
    let authority = Keypair::new();
    let env = Env::upgradeable(Some(authority.pubkey()));
    let mut s = lock_suite_at(env, Pubkey::new_unique(), &after(REACHES_THE_THRESHOLD));
    s.env.svm.airdrop(&authority.pubkey(), SOL).unwrap();
    let mint = s.mint;
    let pool = pool_of(&s.env.usdc.mint, &mint);
    let new_cpmm = Pubkey::new_unique();
    let new_tier = Pubkey::new_unique();
    let mut tier = edited_tier(|_| {});
    tier.owner = new_cpmm;
    s.env.svm.set_account(new_tier, tier).unwrap();
    s.env.send_paid_by(&authority, &[set_raydium_to(&authority.pubkey(), new_cpmm, new_tier)], &[]).expect("another CPMM");
    let config: UsdcConfig = s.env.fetch(&usdc_config_pda());
    assert_eq!((config.raydium_cpmm_program, config.raydium_amm_config), (new_cpmm, new_tier));
    assert_eq!(s.env.svm.get_account(&pool.pool_state).unwrap().owner, RAYDIUM_CPMM_ID);

    let lp_amount = s.env.token_amount(&pool.pool_creator_lp);
    let keeper = s.keeper.insecure_clone();
    lock(&mut s.env, &keeper, &mint).expect("the lock lands on the pool's own accounts");
    assert_eq!(s.env.fetch::<CurveUsdc>(&curve_pda(&mint)).phase, PHASE_GRADUATED);
    let vault = s.env.token_account(&ata(&MAINNET_LOCK_AUTHORITY, &pool.lp_mint));
    assert_eq!(vault.amount, lp_amount);
}

#[test]
fn a_sol_curve_is_refused_by_the_lock_too() {
    let mut s = lock_suite();
    let keeper = s.keeper.insecure_clone();
    s.env.write_tradeable_curve(OCT_5_2026 - DAY);
    let sol_mint = s.env.mint;
    let forged = lock_ix(&s.env, &keeper.pubkey(), &sol_mint);
    expect_refused(send_logged_with_budget(&mut s.env, &keeper, &[forged]), ACCOUNT_DISCRIMINATOR_MISMATCH);
}

#[test]
fn the_lock_s_caller_must_sign() {
    let mut s = lock_suite();
    let mint = s.mint;
    let keeper = s.keeper.insecure_clone();
    let mut unsigned = lock_ix(&s.env, &keeper.pubkey(), &mint);
    unsigned.accounts[0] = AccountMeta::new(keeper.pubkey(), false);
    let payer = s.env.funded_wallet(SOL);
    expect_refused(send_logged_with_budget(&mut s.env, &payer, &[unsigned]), ACCOUNT_NOT_SIGNER);
}

// ── Both steps in one v0 transaction (§10.3.9: the keeper sends them as one) ────────────────────

/// The most a transaction may take on a cluster: a packet's 1,232 bytes, 64 accounts locked
/// (`MAX_TX_ACCOUNT_LOCKS` is 128 only once `increase_tx_account_lock_limit` is active), 1,400,000
/// compute units, 64 instructions traced.
const PACKET_BYTES: usize = 1_232;
const ACCOUNT_LOCKS: usize = 64;
const MAX_COMPUTE_UNITS: u32 = 1_400_000;
const MAX_TRACE: usize = 64;

/// Every account the two steps take but the keeper, in the order they first appear: what the
/// keeper's lookup table holds.
fn graduation_table_addresses(env: &Env, keeper: &Pubkey, mint: &Pubkey) -> Vec<Pubkey> {
    let mut addresses: Vec<Pubkey> = Vec::new();
    for ix in [graduate_ix(env, keeper, mint), lock_ix(env, keeper, mint)] {
        for meta in ix.accounts {
            if meta.pubkey != *keeper && meta.pubkey != pepper_curve::ID && !addresses.contains(&meta.pubkey) {
                addresses.push(meta.pubkey);
            }
        }
    }
    addresses
}

/// An address lookup table holding `addresses`, created and extended by `payer` in transactions of
/// their own, as on a cluster, under a fresh authority (LiteSVM's `SlotHashes` holds its first slot
/// alone, so every table derives from it), then a slot later: a table serves only the addresses
/// extended before the current slot.
fn lookup_table_of(env: &mut Env, payer: &Keypair, addresses: &[Pubkey]) -> AddressLookupTableAccount {
    let authority = Keypair::new();
    let (create, table) = lookup_table::create_lookup_table_signed(authority.pubkey(), payer.pubkey(), 0);
    env.send_paid_by(payer, &[create], &[&authority]).expect("the table");
    for chunk in addresses.chunks(20) {
        let extend = lookup_table::extend_lookup_table(table, authority.pubkey(), Some(payer.pubkey()), chunk.to_vec());
        env.send_paid_by(payer, &[extend], &[&authority]).expect("the table's addresses");
    }
    let slot = env.svm.get_sysvar::<Clock>().slot;
    env.svm.warp_to_slot(slot + 1);
    AddressLookupTableAccount { key: table, addresses: addresses.to_vec() }
}

/// A transaction on the wire: the signatures' count, the signatures, the message.
fn wire_bytes(signatures: usize, message: &[u8]) -> usize {
    assert!(signatures < 128, "a one-byte count");
    1 + 64 * signatures + message.len()
}

struct OneTransaction {
    result: Result<Landed, TransactionError>,
    bytes: usize,
    accounts: usize,
}

/// The compute limit, `graduate_usdc_pool` and `graduate_usdc_lock` in one v0 message, the keeper
/// its one signer and fee payer, every other account but the two programs it calls from `table`.
fn both_steps_in_one_v0(env: &mut Env, keeper: &Keypair, mint: &Pubkey, units: u32, table: &AddressLookupTableAccount) -> OneTransaction {
    let ixs = [compute_unit_limit(units), graduate_ix(env, &keeper.pubkey(), mint), lock_ix(env, &keeper.pubkey(), mint)];
    let message = v0::Message::try_compile(&keeper.pubkey(), &ixs, std::slice::from_ref(table), env.svm.latest_blockhash())
        .expect("the message compiles");
    let accounts = message.account_keys.len()
        + message.address_table_lookups.iter().map(|l| l.writable_indexes.len() + l.readonly_indexes.len()).sum::<usize>();
    let message = VersionedMessage::V0(message);
    let bytes = wire_bytes(1, &message.serialize());
    let tx = VersionedTransaction::try_new(message, &[keeper]).expect("signed");
    let result = env.svm.send_transaction(tx);
    env.svm.expire_blockhash();
    let result = result.map(|meta| Landed { logs: meta.logs, units: meta.compute_units_consumed }).map_err(|failed| {
        println!("refused, {:?}:\n{}", failed.err, failed.meta.logs.join("\n"));
        failed.err
    });
    OneTransaction { result, bytes, accounts }
}

/// The legacy transaction a step takes alone, the compute limit first, `payer` paying and
/// signing beside the caller: its bytes on the wire.
fn legacy_bytes(payer: &Pubkey, ixs: &[Instruction]) -> usize {
    let mut all = vec![compute_unit_limit(GRADUATE_CU_LIMIT)];
    all.extend_from_slice(ixs);
    let message = solana_sdk::message::Message::new(&all, Some(payer));
    wire_bytes(usize::from(message.header.num_required_signatures), &message.serialize())
}

#[test]
fn both_steps_fit_one_v0_transaction_with_a_lookup_table_and_each_fits_one_alone() {
    // The keeper's shape (§10.3.9): one v0 transaction, the compute limit then both steps, every
    // account but the keeper and the two programs it calls read from a lookup table. Measured over
    // fresh curves (each mint moves the PDA searches), on the same chain twice: together, then from
    // a copy of the chain taken before, each step in a legacy transaction of its own, which needs
    // no table.
    let mut s = lock_chain_at(Env::new(), Pubkey::new_unique(), &after(REACHES_THE_THRESHOLD));
    let keeper = s.keeper.insecure_clone();
    s.env.svm.airdrop(&keeper.pubkey(), 100 * SOL).unwrap();
    let state = after(REACHES_THE_THRESHOLD);
    let (mut together, mut pool_alone, mut lock_alone) = (Vec::new(), Vec::new(), Vec::new());
    let (mut bytes, mut accounts, mut traces, mut legacy) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for _ in 0..24 {
        let curve = graduating(&mut s.env, Pubkey::new_unique(), &state);
        let mint = curve.mint;
        let addresses = graduation_table_addresses(&s.env, &keeper.pubkey(), &mint);
        let table = lookup_table_of(&mut s.env, &keeper, &addresses);
        let before = s.env.svm.clone();

        let one = both_steps_in_one_v0(&mut s.env, &keeper, &mint, MAX_COMPUTE_UNITS, &table);
        let landed = one.result.expect("both steps in one transaction");
        assert_eq!(s.env.fetch::<CurveUsdc>(&curve_pda(&mint)).phase, PHASE_GRADUATED);
        let steps = units_of(&landed.logs, &pepper_curve::ID);
        assert_eq!(steps.len(), 2, "the two steps");
        together.push((landed.units, steps[0], steps[1]));
        bytes.push(one.bytes);
        accounts.push(one.accounts);
        traces.push(trace_length(&landed.logs));
        legacy.push((
            legacy_bytes(&s.env.admin.pubkey(), &[graduate_ix(&s.env, &keeper.pubkey(), &mint)]),
            legacy_bytes(&s.env.admin.pubkey(), &[lock_ix(&s.env, &keeper.pubkey(), &mint)]),
        ));

        let after_together = std::mem::replace(&mut s.env.svm, before);
        let pooled = graduate(&mut s.env, &keeper, &mint).expect("the pool step alone");
        let locked = lock(&mut s.env, &keeper, &mint).expect("the lock alone");
        pool_alone.push(pooled.units);
        lock_alone.push((locked.units, units_of(&locked.logs, &RAYDIUM_LP_LOCK_ID)[0], trace_length(&locked.logs)));
        s.env.svm = after_together;
    }
    let range = |v: &[u64]| (*v.iter().min().unwrap(), *v.iter().max().unwrap());
    let range_usize = |v: &[usize]| (*v.iter().min().unwrap(), *v.iter().max().unwrap());
    let total = range(&together.iter().map(|t| t.0).collect::<Vec<_>>());
    println!(
        "both steps in one v0 transaction over 24 curves: {} to {} compute units (the pool step {:?}, the lock step {:?}), {:?} bytes, {:?} accounts, a trace of {:?}",
        total.0,
        total.1,
        range(&together.iter().map(|t| t.1).collect::<Vec<_>>()),
        range(&together.iter().map(|t| t.2).collect::<Vec<_>>()),
        range_usize(&bytes),
        range_usize(&accounts),
        range_usize(&traces),
    );
    println!(
        "apart, on the same chains: the pool step's transaction {:?}, the lock's {:?} (its lock_cp_liquidity {:?}, a trace of {:?}); legacy bytes, the pool step {:?}, the lock {:?}",
        range(&pool_alone),
        range(&lock_alone.iter().map(|l| l.0).collect::<Vec<_>>()),
        range(&lock_alone.iter().map(|l| l.1).collect::<Vec<_>>()),
        range_usize(&lock_alone.iter().map(|l| l.2).collect::<Vec<_>>()),
        range_usize(&legacy.iter().map(|l| l.0).collect::<Vec<_>>()),
        range_usize(&legacy.iter().map(|l| l.1).collect::<Vec<_>>()),
    );
    assert!(total.1 < u64::from(MAX_COMPUTE_UNITS), "{together:?}");
    assert!(bytes.iter().all(|b| *b <= PACKET_BYTES), "{bytes:?}");
    assert!(accounts.iter().all(|a| *a <= ACCOUNT_LOCKS), "{accounts:?}");
    assert!(traces.iter().all(|t| *t <= MAX_TRACE), "{traces:?}");
    // Apart, each step fits a legacy transaction, with no table.
    assert!(legacy.iter().all(|(pool, lock)| *pool <= PACKET_BYTES && *lock <= PACKET_BYTES), "{legacy:?}");
}

#[test]
fn dust_on_the_graduation_s_addresses_can_push_one_transaction_past_the_trace_limit_and_two_still_land() {
    // Every account either step opens is created over lamports a stranger sent there first (the
    // tests above), by a transfer, an allocate and an assign where a fresh address takes one
    // create: two more instructions traced each time. Every such address is known from the mint
    // alone, and a gift costs the rent of an empty account, about 0.0009 SOL.
    let mut s = lock_chain_at(Env::new(), Pubkey::new_unique(), &after(REACHES_THE_THRESHOLD));
    let mint = s.mint;
    let keeper = s.keeper.insecure_clone();
    let pool = pool_of(&s.env.usdc.mint, &mint);
    let lock_accounts = lock_of(&s.fee_owner.pubkey(), &mint, &pool.lp_mint);
    let addresses = graduation_table_addresses(&s.env, &keeper.pubkey(), &mint);
    let table = lookup_table_of(&mut s.env, &keeper, &addresses);
    let targets = [
        pool.pool_creator_usdc,
        pool.pool_creator_token,
        pool.pool_state,
        pool.lp_mint,
        pool.pool_creator_lp,
        pool.usdc_vault,
        pool.token_vault,
        pool.observation,
        lock_accounts.fee_nft_mint,
        lock_accounts.fee_nft_account,
        lock_accounts.locked_liquidity,
        lock_accounts.locked_lp_vault,
    ];
    let gift = Rent::default().minimum_balance(0);
    let stranger = s.env.funded_wallet(SOL);
    let mut traced = Vec::new();
    for count in 0..=targets.len() {
        let before = s.env.svm.clone();
        let gifts: Vec<Instruction> = targets[..count].iter().map(|to| system_instruction::transfer(&stranger.pubkey(), to, gift)).collect();
        if !gifts.is_empty() {
            s.env.send_paid_by(&stranger, &gifts, &[]).unwrap();
        }
        let one = both_steps_in_one_v0(&mut s.env, &keeper, &mint, MAX_COMPUTE_UNITS, &table);
        traced.push((count, one.result.as_ref().map(|l| trace_length(&l.logs)).map_err(|e| e.clone())));
        s.env.svm = before;
    }
    println!("both steps in one transaction, the trace by the count of addresses dusted first: {traced:?}");
    let refused = |r: &Result<usize, TransactionError>| {
        matches!(r, Err(TransactionError::InstructionError(_, InstructionError::MaxInstructionTraceLengthExceeded)))
    };
    // Untouched, it lands; past a few gifts the runtime refuses it whole.
    assert!(traced[0].1.is_ok(), "{traced:?}");
    let first_refused = traced.iter().position(|(_, r)| refused(r)).expect("enough dust refuses the one transaction");
    assert!(traced[first_refused..].iter().all(|(_, r)| refused(r)), "{traced:?}");
    assert!(traced[..first_refused].iter().all(|(_, r)| r.as_ref().is_ok_and(|t| *t <= MAX_TRACE)), "{traced:?}");

    // Every address dusted: the one transaction refused, nothing moved, then the two steps in two
    // transactions land, the LP waiting on the pool creator between them.
    let gifts: Vec<Instruction> = targets.iter().map(|to| system_instruction::transfer(&stranger.pubkey(), to, gift)).collect();
    s.env.send_paid_by(&stranger, &gifts, &[]).unwrap();
    let curve_before = s.env.svm.get_account(&curve_pda(&mint)).unwrap().data;
    let one = both_steps_in_one_v0(&mut s.env, &keeper, &mint, MAX_COMPUTE_UNITS, &table);
    assert!(one.result.is_err());
    assert_eq!(s.env.svm.get_account(&curve_pda(&mint)).unwrap().data, curve_before);
    let pooled = graduate(&mut s.env, &keeper, &mint).expect("the pool step alone");
    assert_eq!(s.env.fetch::<CurveUsdc>(&curve_pda(&mint)).phase, PHASE_POOL_CREATED);
    let lp_amount = s.env.token_amount(&pool.pool_creator_lp);
    let locked = lock(&mut s.env, &keeper, &mint).expect("the lock alone");
    println!(
        "every address dusted: the pool step alone traces {}, the lock alone {}",
        trace_length(&pooled.logs),
        trace_length(&locked.logs)
    );
    assert!(trace_length(&pooled.logs) <= MAX_TRACE && trace_length(&locked.logs) <= MAX_TRACE);
    assert_eq!(s.env.fetch::<CurveUsdc>(&curve_pda(&mint)).phase, PHASE_GRADUATED);
    assert_eq!(s.env.token_amount(&lock_accounts.locked_lp_vault), lp_amount);
    assert_eq!(s.env.token_amount(&lock_accounts.fee_nft_account), 1);
}

// ── What the Fee Key does once it holds it (Raydium's lock, not the program) ────────────────────

/// The lock's `collect_cp_fee(lp_fee_amount)` as the SDK builds it (`collectCpFeeInstruction`,
/// `src/raydium/cpmm/instruction.ts`), the harvest today's keeper sends (`harvestLockLp`): the Fee
/// Key's owner signs, and the pool's two sides reach its ATAs of the two mints.
fn collect_cp_fee_ix(
    owner: &Pubkey,
    fee_nft_mint: &Pubkey,
    pool_state: &Pubkey,
    state: &pepper_curve::raydium::PoolState,
    lp_fee_amount: u64,
) -> Instruction {
    let mut data = vec![8, 30, 51, 199, 209, 184, 247, 133];
    data.extend_from_slice(&lp_fee_amount.to_le_bytes());
    let memo: Pubkey = Pubkey::from_str_const("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr");
    Instruction {
        program_id: RAYDIUM_LP_LOCK_ID,
        accounts: vec![
            AccountMeta::new_readonly(MAINNET_LOCK_AUTHORITY, false),
            AccountMeta::new_readonly(*owner, true),
            AccountMeta::new(ata(owner, fee_nft_mint), false),
            AccountMeta::new(locked_liquidity_pda(fee_nft_mint), false),
            AccountMeta::new_readonly(RAYDIUM_CPMM_ID, false),
            AccountMeta::new_readonly(cpmm_pda(&[CPMM_AUTH_SEED]), false),
            AccountMeta::new(*pool_state, false),
            AccountMeta::new(state.lp_mint, false),
            AccountMeta::new(ata(owner, &state.token_0_mint), false),
            AccountMeta::new(ata(owner, &state.token_1_mint), false),
            AccountMeta::new(state.token_0_vault, false),
            AccountMeta::new(state.token_1_vault, false),
            AccountMeta::new_readonly(state.token_0_mint, false),
            AccountMeta::new_readonly(state.token_1_mint, false),
            AccountMeta::new(ata(&MAINNET_LOCK_AUTHORITY, &state.lp_mint), false),
            AccountMeta::new_readonly(spl_token::ID, false),
            AccountMeta::new_readonly(Pubkey::from_str_const("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"), false),
            AccountMeta::new_readonly(memo, false),
        ],
        data,
    }
}

/// A pool's reserves as CPMM prices them: each vault less the protocol's and the fund's fees it
/// still holds (`PoolState` from byte 341: the protocol's two, then the fund's two, u64 each).
fn lp_reserves(env: &Env, pool_state: &Pubkey, state: &pepper_curve::raydium::PoolState) -> (u128, u128) {
    let data = env.svm.get_account(pool_state).unwrap().data;
    let at = |offset: usize| u128::from(u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap()));
    let (protocol_0, protocol_1, fund_0, fund_1) = (at(341), at(349), at(357), at(365));
    (
        u128::from(env.token_amount(&state.token_0_vault)) - protocol_0 - fund_0,
        u128::from(env.token_amount(&state.token_1_vault)) - protocol_1 - fund_1,
    )
}

/// The LP supply CPMM records (`PoolState.lp_supply`), read afresh.
fn lp_supply(env: &Env, pool_state: &Pubkey) -> u128 {
    u128::from(parse_pool_state(&env.svm.get_account(pool_state).unwrap().data).unwrap().lp_supply)
}

#[test]
fn the_fee_key_harvests_through_the_lock_the_fees_and_never_the_locked_liquidity() {
    // `lp_fee_owner` holds the Fee Key once the lock lands, and harvests through the lock, as
    // today's keeper does on the SOL side (`harvestLockLp`). Recorded on the mainnet binaries: the
    // lock redeems at most the LP its position's share of the fees is worth, whatever it is asked
    // for. With no swap since the pool opened, half the locked LP asked for redeems nothing; after
    // a trader's round trips, the whole locked LP asked for redeems a small slice of both reserves,
    // the LP's share of the fee, and leaves each LP unit worth at least what it was at the seed
    // (√k per LP, the reserves CPMM prices with). So the Fee Key reaches the fees, never the
    // holders' liquidity, on this binary of the lock.
    let mut s = lock_suite();
    let mint = s.mint;
    let pool = pool_of(&s.env.usdc.mint, &mint);
    let keeper = s.keeper.insecure_clone();
    lock(&mut s.env, &keeper, &mint).expect("the lock lands");
    let lock_accounts = lock_of(&s.fee_owner.pubkey(), &mint, &pool.lp_mint);
    let state = parse_pool_state(&s.env.svm.get_account(&pool.pool_state).unwrap().data).unwrap();
    let owner = s.fee_owner.insecure_clone();
    s.env.svm.airdrop(&owner.pubkey(), SOL).unwrap();
    for m in [state.token_0_mint, state.token_1_mint] {
        let open = spl_associated_token_account::instruction::create_associated_token_account_idempotent(
            &owner.pubkey(),
            &owner.pubkey(),
            &m,
            &spl_token::ID,
        );
        s.env.send_paid_by(&owner, &[open], &[]).unwrap();
    }
    let locked = s.env.token_amount(&lock_accounts.locked_lp_vault);
    let reserves = |env: &Env| (env.token_amount(&state.token_0_vault), env.token_amount(&state.token_1_vault));
    let owner_holds = |env: &Env| {
        (env.token_amount(&ata(&owner.pubkey(), &state.token_0_mint)), env.token_amount(&ata(&owner.pubkey(), &state.token_1_mint)))
    };
    let seeded = reserves(&s.env);
    let (x, y) = lp_reserves(&s.env, &pool.pool_state, &state);
    let k_seed = x * y;
    let supply_seed = lp_supply(&s.env, &pool.pool_state);

    // No swap since the pool opened: half the locked LP asked for, nothing redeemed.
    let harvest = collect_cp_fee_ix(&owner.pubkey(), &lock_accounts.fee_nft_mint, &pool.pool_state, &state, locked / 2);
    let result = send_logged_with_budget(&mut s.env, &owner, &[harvest]);
    println!(
        "no fee accrued, half the locked LP asked for: {:?}, the owner received {:?}, the lock's vault {} of {locked}",
        result.as_ref().map(|l| l.units),
        owner_holds(&s.env),
        s.env.token_amount(&lock_accounts.locked_lp_vault)
    );
    assert!(result.is_ok());
    assert_eq!(owner_holds(&s.env), (0, 0));
    assert_eq!(s.env.token_amount(&lock_accounts.locked_lp_vault), locked);

    // A trader's round trips through the pool, once it is open (a swap needs the clock past the
    // pool's `open_time`), its 1% fee left in the reserves; then the whole locked LP asked for.
    s.env.set_time(NOW + 60);
    let trader = s.env.funded_wallet(SOL);
    let trader_usdc = s.env.usdc_account(&trader.pubkey());
    s.env.mint_usdc(&trader_usdc, 2_000 * USDC);
    let trader_token = ata(&trader.pubkey(), &mint);
    s.env.put_token_account(trader_token, mint, trader.pubkey(), 0, COption::None);
    let usdc_mint = s.env.usdc.mint;
    for _ in 0..5 {
        let buy = cpmm_swap_ix(&trader.pubkey(), &pool, &state, (trader_usdc, usdc_mint, pool.usdc_vault), (trader_token, mint, pool.token_vault), 1_000 * USDC);
        send_logged_with_budget(&mut s.env, &trader, &[buy]).expect("a swap into the token");
        let tokens = s.env.token_amount(&trader_token);
        let sell = cpmm_swap_ix(&trader.pubkey(), &pool, &state, (trader_token, mint, pool.token_vault), (trader_usdc, usdc_mint, pool.usdc_vault), tokens);
        send_logged_with_budget(&mut s.env, &trader, &[sell]).expect("a swap back");
    }
    let traded = reserves(&s.env);
    let (x, y) = lp_reserves(&s.env, &pool.pool_state, &state);
    let k_traded = x * y;
    assert!(k_traded > k_seed, "the trades left a fee in the reserves");
    let harvest = collect_cp_fee_ix(&owner.pubkey(), &lock_accounts.fee_nft_mint, &pool.pool_state, &state, locked);
    let result = send_logged_with_budget(&mut s.env, &owner, &[harvest]);
    let vault_after = s.env.token_amount(&lock_accounts.locked_lp_vault);
    let redeemed = locked - vault_after;
    println!(
        "after the round trips, the whole locked LP asked for: {:?}; reserves seeded {seeded:?}, after the trades {traded:?}, after the harvest {:?}; the owner received {:?}; the lock's vault {vault_after} of {locked}, {redeemed} redeemed",
        result.as_ref().map(|l| l.units),
        reserves(&s.env),
        owner_holds(&s.env),
    );
    assert!(result.is_ok());
    // Redeemed: more than nothing, at most the locked position's share of the fee growth, the LP
    // whose redemption brings √k per LP back to the seed's: locked × (1 − √(k_seed / k_traded)).
    let bound = locked as f64 * (1.0 - (k_seed as f64 / k_traded as f64).sqrt());
    assert!(redeemed > 0 && (redeemed as f64) <= bound + 1.0, "redeemed {redeemed}, the fee's share {bound}");
    let (owner_0, owner_1) = owner_holds(&s.env);
    assert!(owner_0 > 0 && owner_1 > 0, "a slice of both reserves");
    // What is left is worth at least the seed per LP unit: the holders' liquidity untouched.
    let (x, y) = lp_reserves(&s.env, &pool.pool_state, &state);
    let supply_after = lp_supply(&s.env, &pool.pool_state);
    assert!(
        ((x * y) as f64).sqrt() / supply_after as f64 >= (k_seed as f64).sqrt() / supply_seed as f64 * (1.0 - 1e-12),
        "√k per LP fell below the seed's"
    );
    // Anyone else, the caller of the lock included, is refused the harvest: an account of the Fee
    // Key's mint that does not hold the Fee Key fails the lock's own constraint on it (Anchor's
    // `ConstraintRaw`, 2003), the lock's vault left as it is.
    for m in [lock_accounts.fee_nft_mint, state.token_0_mint, state.token_1_mint] {
        let open = spl_associated_token_account::instruction::create_associated_token_account_idempotent(
            &keeper.pubkey(),
            &keeper.pubkey(),
            &m,
            &spl_token::ID,
        );
        s.env.send_paid_by(&keeper, &[open], &[]).unwrap();
    }
    assert_eq!(s.env.token_amount(&ata(&keeper.pubkey(), &lock_accounts.fee_nft_mint)), 0);
    let stranger_harvest = collect_cp_fee_ix(&keeper.pubkey(), &lock_accounts.fee_nft_mint, &pool.pool_state, &state, 1);
    expect_refused(send_logged_with_budget(&mut s.env, &keeper, &[stranger_harvest]), anchor(ErrorCode::ConstraintRaw));
    assert_eq!(s.env.token_amount(&lock_accounts.locked_lp_vault), vault_after);
}

/// CPMM's `swap_base_input(amount_in, 0)` as the SDK builds it (`makeSwapCpmmBaseInInstruction`):
/// the 13 metas, each side its (token account, mint, pool vault).
fn cpmm_swap_ix(
    payer: &Pubkey,
    pool: &Pool,
    state: &pepper_curve::raydium::PoolState,
    input: (Pubkey, Pubkey, Pubkey),
    output: (Pubkey, Pubkey, Pubkey),
    amount_in: u64,
) -> Instruction {
    let mut data = vec![143, 190, 90, 218, 196, 30, 51, 222];
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes());
    Instruction {
        program_id: RAYDIUM_CPMM_ID,
        accounts: vec![
            AccountMeta::new_readonly(*payer, true),
            AccountMeta::new_readonly(pool.authority, false),
            AccountMeta::new_readonly(state.amm_config, false),
            AccountMeta::new(pool.pool_state, false),
            AccountMeta::new(input.0, false),
            AccountMeta::new(output.0, false),
            AccountMeta::new(input.2, false),
            AccountMeta::new(output.2, false),
            AccountMeta::new_readonly(spl_token::ID, false),
            AccountMeta::new_readonly(spl_token::ID, false),
            AccountMeta::new_readonly(input.1, false),
            AccountMeta::new_readonly(output.1, false),
            AccountMeta::new(pool.observation, false),
        ],
        data,
    }
}
