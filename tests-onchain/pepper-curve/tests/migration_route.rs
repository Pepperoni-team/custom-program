//! A route through a PDA, by CPI (upgrade N+1, track M's M2: docs/solana/PEPPER-CURVE-PROGRAM.md
//! «Migration (upgrade N+1)»; usdc-curve design §10.10).
//!
//! `migrate_to_usdc` will sell a curve's SOL reserve through Jupiter v6 by CPI, signing for a PDA
//! of its own whose WSOL account holds the reserve. Before any of it is written, this suite lands
//! real routes that way: `migration-fixtures/routes.json` holds one route from Jupiter's `/build`
//! per allowed venue (Orca's Whirlpool, Raydium's CLMM, Meteora's DLMM), a one-hop SOL → USDC sell
//! of CICCO1's reserve with `taker` the probe's swap PDA and `payer` its executor, and every
//! account those routes read, all at one mainnet slot with its Clock
//! (`scripts/solana/migration-route-fixtures.ts`; read by `tests/migration_capture`, which
//! `migration_jupiter.rs` shares). The probe (`tests-onchain/migration-route-probe`, never deployed)
//! forwards the route's data verbatim by `invoke_signed`.
//!
//! The programs are the binaries `fixtures.rs` pins, the token program mainnet's p-token
//! (`P_TOKEN_SO`) unless a test says otherwise, every LiteSVM under mainnet's Rent sysvar. The
//! numbers pinned here are the capture's: a re-capture moves them on purpose, with the doc.

mod common;
mod migration_capture;
mod migration_common;

use anchor_lang::solana_program::program_pack::Pack;
use anchor_spl::associated_token::{get_associated_token_address as ata, spl_associated_token_account};
use anchor_spl::token::spl_token;
use common::*;
use litesvm::LiteSVM;
use migration_capture::*;
use migration_common::*;
use solana_sdk::{
    account::Account,
    compute_budget::{self, ComputeBudgetInstruction},
    hash::hash,
    instruction::{AccountMeta, Instruction, InstructionError},
    message::Message,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    signer::keypair::keypair_from_seed,
    system_program,
    transaction::{Transaction, TransactionError},
};

/// The probe's address and its executor's key, both sha256 of a label (the capture script's).
const PROBE_LABEL: &str = "pepper:migration-route-probe";
/// The swap PDA's seed, `pepper_curve`'s `["migration_swap", mint]` (the probe's `SWAP_SEED`).
const SWAP_SEED: &[u8] = b"migration_swap";

/// The probe's build: `MIGRATION_ROUTE_PROBE_SO` (`scripts/ci-onchain-tests.sh` sets it), else
/// where `cargo-build-sbf --manifest-path tests-onchain/migration-route-probe/Cargo.toml` writes it.
fn probe_so() -> String {
    std::env::var("MIGRATION_ROUTE_PROBE_SO")
        .unwrap_or_else(|_| "../migration-route-probe/target/deploy/migration_route_probe.so".to_string())
}

fn probe_id() -> Pubkey {
    Pubkey::new_from_array(hash(PROBE_LABEL.as_bytes()).to_bytes())
}

fn executor() -> Keypair {
    keypair_from_seed(&hash(format!("{PROBE_LABEL}:executor").as_bytes()).to_bytes()).unwrap()
}

fn probe_pda(seed: &[u8], mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[seed, mint.as_ref()], &probe_id()).0
}

/// The mainnet bank at the capture's slot, ready to swap: the four programs and the probe, the
/// accounts and the Clock, the executor funded, the taker's two token accounts made by `/build`'s
/// own setup instructions, its WSOL account holding the reserve, and the destination, a USDC
/// account of another PDA (the curve's `usdc_vault` in the migration). Signatures are verified;
/// each test clones it for each send.
struct RouteEnv {
    svm: LiteSVM,
    cap: Capture,
    executor: Keypair,
}

fn route_env(token: TokenProgram) -> RouteEnv {
    let cap = capture();
    let mut svm = mainnet_rent_svm();
    // Loaded at slot 0, then the Clock moves on.
    load_mainnet_programs(&mut svm, token);
    let probe = std::fs::read(probe_so()).unwrap_or_else(|e| panic!("{}: {e} (scripts/ci-onchain-tests.sh builds it)", probe_so()));
    load_upgradeable_program(&mut svm, probe_id(), &probe, None);
    load_capture(&mut svm, &cap);

    let executor = executor();
    assert_eq!(executor.pubkey(), cap.executor, "the capture's payer is the executor's key");
    svm.airdrop(&executor.pubkey(), 10 * SOL).unwrap();
    // `/build`'s setup instructions as they came, signed by the payer alone.
    let tx = Transaction::new_signed_with_payer(&cap.routes[0].setup, Some(&executor.pubkey()), &[&executor], svm.latest_blockhash());
    svm.send_transaction(tx).unwrap_or_else(|e| panic!("the setup: {:?}\n{}", e.err, e.meta.pretty_logs()));
    wrap(&mut svm, &executor, &cap.taker_wsol, RESERVE_LAMPORTS);
    // The destination, owned by another PDA of the probe's, as `usdc_vault` is the curve's.
    let vault = spl_token::state::Account {
        mint: USDC_MINT,
        owner: probe_pda(b"curve", &cap.seed_mint),
        amount: 0,
        state: spl_token::state::AccountState::Initialized,
        ..Default::default()
    };
    let mut data = vec![0; spl_token::state::Account::LEN];
    vault.pack_into_slice(&mut data);
    let lamports = svm.minimum_balance_for_rent_exemption(data.len());
    svm.set_account(cap.destination, Account { lamports, data, owner: spl_token::ID, executable: false, rent_epoch: 0 })
        .unwrap();
    assert_eq!(token_amount(&svm, &cap.taker_wsol), RESERVE_LAMPORTS);
    RouteEnv { svm, cap, executor }
}

/// The probe's instruction for `swap`: the swap PDA, the mint, the router, the probe, then the
/// route's accounts with no signer among them (the probe signs for the PDA), `nest` levels of the
/// probe calling itself above the route, `unchecked` for `invoke_signed_unchecked`.
fn probe_instruction(cap: &Capture, swap: &Instruction, nest: u8, unchecked: bool) -> Instruction {
    let mut accounts = vec![
        AccountMeta::new_readonly(cap.taker, false),
        AccountMeta::new_readonly(cap.seed_mint, false),
        AccountMeta::new_readonly(swap.program_id, false),
        AccountMeta::new_readonly(probe_id(), false),
    ];
    accounts.extend(swap.accounts.iter().map(|a| AccountMeta { is_signer: false, ..a.clone() }));
    let mut data = vec![nest, unchecked as u8];
    data.extend_from_slice(&swap.data);
    Instruction { program_id: probe_id(), accounts, data }
}

/// What a landed route measured.
#[derive(Debug)]
struct Landed {
    /// The transaction's, the compute-unit-limit instruction's 150 included.
    compute_units: u64,
    /// What Jupiter's outermost invocation reported, its venue's and token program's CPIs inside.
    jupiter_compute_units: u64,
    /// Every instruction below the top level, its program and stack height (the top level is 1),
    /// from the runtime's inner instructions.
    invoked: Vec<(Pubkey, u8)>,
}

impl Landed {
    fn deepest(&self) -> u8 {
        self.invoked.iter().map(|(_, height)| *height).max().unwrap_or(1)
    }
}

/// `ix` sent after a compute unit limit of 1 400 000, paid and signed by `payer` alone: at top
/// level a PDA marked a signer passes only a LiteSVM with its signature check off.
fn send_route(svm: &mut LiteSVM, payer: &Keypair, ix: Instruction) -> Result<Landed, (TransactionError, Vec<String>)> {
    let limit = ComputeBudgetInstruction::set_compute_unit_limit(MAX_COMPUTE_UNITS);
    let message = Message::new(&[limit, ix], Some(&payer.pubkey()));
    let mut tx = Transaction::new_unsigned(message.clone());
    tx.partial_sign(&[payer], svm.latest_blockhash());
    match svm.send_transaction(tx) {
        Ok(meta) => Ok(Landed {
            compute_units: meta.compute_units_consumed,
            jupiter_compute_units: consumed(&meta.logs, &JUPITER_V6_ID),
            invoked: meta
                .inner_instructions
                .iter()
                .flatten()
                .map(|inner| (message.account_keys[inner.instruction.program_id_index as usize], inner.stack_height))
                .collect(),
        }),
        Err(failed) => Err((failed.err, failed.meta.logs)),
    }
}

/// Every account a send of `swap` through the probe touches but the fee payer, each read now.
fn snapshot(svm: &LiteSVM, cap: &Capture, swap: &Instruction) -> Vec<(Pubkey, Option<Account>)> {
    let mut keys: Vec<Pubkey> = [cap.taker, cap.seed_mint, cap.taker_wsol, cap.taker_usdc, cap.destination, probe_id()]
        .into_iter()
        .chain(swap.accounts.iter().map(|a| a.pubkey))
        .collect();
    keys.sort();
    keys.dedup();
    keys.into_iter().map(|k| (k, svm.get_account(&k))).collect()
}

fn query(route: &CapturedRoute) -> Vec<(String, String)> {
    route.json["query"]
        .as_str()
        .unwrap()
        .split('&')
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap();
            (k.to_string(), v.replace('+', " "))
        })
        .collect()
}

/// The capture is the committed one, read at one slot: its Clock's, the three pools among its
/// accounts with the binaries `fixtures.rs` pins, each route one hop of CICCO1's reserve.
#[test]
fn the_capture_is_pinned_and_read_at_one_slot() {
    let cap = capture();
    let sha: String = hash(&cap.bytes).to_bytes().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(sha, ROUTES_SHA256, "routes.json is not the capture pinned here");
    assert_eq!(cap.json["slot"].as_u64(), Some(CAPTURE_SLOT));
    assert_eq!(cap.clock.slot, CAPTURE_SLOT, "the Clock was read in the same call as the pools");
    assert_eq!(cap.json["inAmount"].as_str(), Some(RESERVE_LAMPORTS.to_string().as_str()));
    assert_eq!(key(&cap.json["probeProgram"]), probe_id());
    for (program, slot) in PINNED_DEPLOY_SLOTS {
        assert_eq!(cap.json["programsDeployedAt"][program.to_string()].as_u64(), Some(slot), "{program}");
    }
    assert_eq!(cap.routes.len(), VENUES.len());
    let held: Vec<Pubkey> = cap.accounts.iter().map(|(k, _)| *k).collect();
    let absent: Vec<Pubkey> = cap.json["absent"].as_array().unwrap().iter().map(key).collect();
    for (route, venue) in cap.routes.iter().zip(&VENUES) {
        assert_eq!(route.json["venue"].as_str(), Some(venue.label));
        let plan = route.json["routePlan"].as_array().unwrap();
        assert_eq!(plan.len(), 1, "{}: one pool", venue.label);
        assert_eq!(plan[0]["swapInfo"]["label"].as_str(), Some(venue.label));
        assert_eq!(key(&plan[0]["swapInfo"]["ammKey"]), venue.pool);
        assert_eq!(plan[0]["percent"].as_u64(), Some(100));
        let pool = cap.accounts.iter().find(|(k, _)| *k == venue.pool).unwrap_or_else(|| panic!("{}: its pool", venue.label));
        assert_eq!(pool.1.owner, venue.program, "{}: the pool is the venue's", venue.label);
        assert_eq!(amount(&route.json["inAmount"]), RESERVE_LAMPORTS);
        assert_eq!(amount(&route.json["outAmount"]), venue.quoted_out);
        // Every account the route names is in the bank: captured, absent on mainnet too (Whirlpool's
        // oracle, which a pool with no adaptive fee never created), a program the suite loads, or
        // the probe's side.
        let loaded = [JUPITER_V6_ID, ORCA_WHIRLPOOL_ID, RAYDIUM_CLMM_ID, METEORA_DLMM_ID, spl_token::ID];
        for meta in &route.swap.accounts {
            let k = meta.pubkey;
            let probe_side = [cap.taker, cap.taker_wsol, cap.taker_usdc, cap.destination].contains(&k);
            assert!(
                held.contains(&k) || absent.contains(&k) || loaded.contains(&k) || probe_side,
                "{}: {k} is in no part of the bank",
                venue.label
            );
        }
    }
}

/// What `/build` answers for a PDA taker with another payer: Jupiter v6's `route_v2` (no shared
/// accounts, and `/build` has no switch for them), one hop at 1 bp with no fee, the PDA its only
/// signer, the payer in no account of the swap, and two setup instructions the payer alone signs.
#[test]
fn build_returns_route_v2_for_an_off_curve_taker() {
    let cap = capture();
    assert!(!cap.taker.is_on_curve(), "the taker is a PDA, off the curve");
    assert_eq!(cap.taker, probe_pda(SWAP_SEED, &cap.seed_mint));
    assert_eq!(cap.taker_wsol, ata(&cap.taker, &WSOL_MINT));
    assert_eq!(cap.taker_usdc, ata(&cap.taker, &USDC_MINT));
    assert_eq!(cap.destination, probe_pda(b"usdc_vault", &cap.seed_mint));
    for (route, venue) in cap.routes.iter().zip(&VENUES) {
        let asked = query(route);
        let param = |name: &str| asked.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()).unwrap_or_default();
        assert_eq!(param("taker"), cap.taker.to_string());
        assert_eq!(param("payer"), cap.executor.to_string());
        assert_eq!(param("amount"), RESERVE_LAMPORTS.to_string());
        assert_eq!(param("slippageBps"), "1");
        assert_eq!(param("wrapAndUnwrapSol"), "false");
        assert_eq!(param("destinationTokenAccount"), cap.destination.to_string());
        assert_eq!(param("forJitoBundle"), "true");
        assert_eq!(param("dexes"), venue.label);

        let swap = &route.swap;
        assert_eq!(swap.program_id, JUPITER_V6_ID, "{}", venue.label);
        assert_eq!(swap.data[..8], discriminator("route_v2"), "{}", venue.label);
        for other in R10_ROUTES.iter().filter(|name| **name != "route_v2") {
            assert_ne!(swap.data[..8], discriminator(other));
        }
        assert_eq!(
            decode_route_v2(&swap.data),
            RouteV2 {
                in_amount: RESERVE_LAMPORTS,
                quoted_out_amount: venue.quoted_out,
                slippage_bps: 1,
                platform_fee_bps: 0,
                positive_slippage_bps: 0,
                steps: vec![(venue.swap.to_vec(), 10_000, 0, 1)],
            },
            "{}",
            venue.label
        );
        assert_eq!(amount(&route.json["otherAmountThreshold"]), one_bp_threshold(venue.quoted_out), "{}", venue.label);

        // route_v2's ten accounts, then the hop's, the venue's program first.
        assert_eq!(swap.accounts.len(), venue.accounts, "{}", venue.label);
        let fixed: Vec<(Pubkey, bool, bool)> = swap.accounts[..10].iter().map(|a| (a.pubkey, a.is_signer, a.is_writable)).collect();
        assert_eq!(
            fixed,
            vec![
                (cap.taker, true, false),
                (cap.taker_wsol, false, true),
                (cap.taker_usdc, false, true),
                (WSOL_MINT, false, false),
                (USDC_MINT, false, false),
                (spl_token::ID, false, false),
                (spl_token::ID, false, false),
                (cap.destination, false, true),
                (JUPITER_EVENT_AUTHORITY, false, false),
                (JUPITER_V6_ID, false, false),
            ],
            "{}",
            venue.label
        );
        assert_eq!(swap.accounts[10].pubkey, venue.program, "{}", venue.label);
        let signers: Vec<usize> = swap.accounts.iter().enumerate().filter(|(_, a)| a.is_signer).map(|(i, _)| i).collect();
        assert_eq!(signers, vec![0], "{}: the taker alone signs", venue.label);
        assert!(swap.accounts.iter().all(|a| a.pubkey != cap.executor), "{}: the payer is in no account", venue.label);

        // The setup: the taker's two token accounts, created idempotently, the payer the only signer.
        assert_eq!(route.setup, cap.routes[0].setup, "{}: the same setup for every venue", venue.label);
        for (ix, (account, mint)) in route.setup.iter().zip([(cap.taker_wsol, WSOL_MINT), (cap.taker_usdc, USDC_MINT)]) {
            assert_eq!(ix.program_id, spl_associated_token_account::ID);
            assert_eq!(ix.data, vec![1], "CreateIdempotent");
            let metas: Vec<(Pubkey, bool, bool)> = ix.accounts.iter().map(|a| (a.pubkey, a.is_signer, a.is_writable)).collect();
            assert_eq!(
                metas,
                vec![
                    (cap.executor, true, true),
                    (account, false, true),
                    (cap.taker, false, false),
                    (mint, false, false),
                    (system_program::ID, false, false),
                    (spl_token::ID, false, false),
                ]
            );
        }
        // A price for the compute units and no limit (`/build`'s guide: simulate for it), no
        // cleanup with `wrapAndUnwrapSol=false`, nothing else.
        let budget: Vec<Instruction> = route.json["computeBudgetInstructions"].as_array().unwrap().iter().map(instruction).collect();
        assert_eq!(budget.len(), 1);
        assert_eq!(budget[0].program_id, compute_budget::ID);
        assert_eq!((budget[0].data[0], budget[0].data.len()), (3, 9), "SetComputeUnitPrice alone");
        assert!(route.json["cleanupInstruction"].is_null());
        assert_eq!(route.json["otherInstructions"].as_array().map(Vec::len), Some(0));
    }
}

/// Each route lands at top level as `/build` built it (the PDA's signature waived: LiteSVM's check
/// off, the only way a PDA signs a top-level instruction), at exactly its quote: the pools were read
/// at the slot it was quoted against.
#[test]
fn each_route_lands_at_top_level() {
    let env = route_env(TokenProgram::Mainnet);
    for (route, venue) in env.cap.routes.iter().zip(&VENUES) {
        let mut svm = env.svm.clone().with_sigverify(false);
        let landed = send_route(&mut svm, &env.executor, route.swap.clone()).unwrap_or_else(|e| panic!("{}: {e:?}", venue.label));
        assert_eq!(token_amount(&svm, &env.cap.taker_wsol), 0, "{}", venue.label);
        assert_eq!(token_amount(&svm, &env.cap.taker_usdc), 0, "{}", venue.label);
        assert_eq!(token_amount(&svm, &env.cap.destination), venue.quoted_out, "{}", venue.label);
        assert_eq!(landed.jupiter_compute_units, venue.jupiter_units_p_token, "{}", venue.label);
        assert_eq!(landed.compute_units, venue.jupiter_units_p_token + COMPUTE_BUDGET_UNITS, "{}", venue.label);
        assert!(landed.invoked.contains(&(venue.program, 2)) && landed.invoked.contains(&(spl_token::ID, 3)), "{}: {:?}", venue.label, landed.invoked);
        assert_eq!(landed.deepest(), 3, "{}", venue.label);
    }
}

/// Each route lands by CPI, the probe signing for its PDA by `invoke_signed` with signatures
/// checked, at the same output and the same compute units inside Jupiter, the whole transaction at
/// `cpi_units`: the probe at 1, Jupiter at 2, the venue at 3, the token program at 4, the deepest,
/// whatever the venue (Jupiter's event and its last transfer at 3, Meteora's event at 4).
#[test]
fn each_route_lands_by_cpi_from_the_pda() {
    let env = route_env(TokenProgram::Mainnet);
    for (route, venue) in env.cap.routes.iter().zip(&VENUES) {
        let mut svm = env.svm.clone();
        let ix = probe_instruction(&env.cap, &route.swap, 0, false);
        let landed = send_route(&mut svm, &env.executor, ix).unwrap_or_else(|e| panic!("{}: {e:?}", venue.label));
        assert_eq!(token_amount(&svm, &env.cap.taker_wsol), 0, "{}", venue.label);
        assert_eq!(token_amount(&svm, &env.cap.taker_usdc), 0, "{}", venue.label);
        assert_eq!(token_amount(&svm, &env.cap.destination), venue.quoted_out, "{}", venue.label);
        assert_eq!(landed.jupiter_compute_units, venue.jupiter_units_p_token, "{}", venue.label);
        for expected in [(JUPITER_V6_ID, 2), (venue.program, 3), (spl_token::ID, 4)] {
            assert!(landed.invoked.contains(&expected), "{}: {expected:?} in {:?}", venue.label, landed.invoked);
        }
        assert_eq!(landed.deepest(), 4, "{}", venue.label);
        assert_eq!(landed.compute_units, venue.cpi_units, "{}: {landed:?}", venue.label);
    }
}

/// Mainnet allows five instructions deep, as LiteSVM does (`MAX_INSTRUCTION_STACK_DEPTH`; SIMD-0268's
/// eight CPI levels have no feature account on mainnet, read on 2026-10-23): one caller above the
/// migration, a Squads vault transaction executing it, still lands the route, at 5; a second is
/// refused (`CallDepth`) with nothing moved.
#[test]
fn the_route_has_one_level_to_spare() {
    let env = route_env(TokenProgram::Mainnet);
    for (route, venue) in env.cap.routes.iter().zip(&VENUES) {
        let mut svm = env.svm.clone();
        let landed = send_route(&mut svm, &env.executor, probe_instruction(&env.cap, &route.swap, 1, false))
            .unwrap_or_else(|e| panic!("{}: {e:?}", venue.label));
        assert_eq!(token_amount(&svm, &env.cap.destination), venue.quoted_out, "{}", venue.label);
        assert_eq!(landed.deepest(), 5, "{}", venue.label);

        let mut svm = env.svm.clone();
        let before = snapshot(&svm, &env.cap, &route.swap);
        let (err, _) = send_route(&mut svm, &env.executor, probe_instruction(&env.cap, &route.swap, 2, false))
            .expect_err("six levels deep");
        assert_eq!(err, TransactionError::InstructionError(1, InstructionError::CallDepth), "{}", venue.label);
        assert_eq!(snapshot(&svm, &env.cap, &route.swap), before, "{}: nothing moved", venue.label);
    }
}

/// Rick's 1 bp, held by Jupiter itself: a quote raised 2 bp over what the pool pays is refused
/// (`SlippageToleranceExceeded`, 6001) with nothing moved, the executor charged its fee alone.
#[test]
fn a_quote_raised_2_bp_is_refused_with_nothing_moved() {
    let env = route_env(TokenProgram::Mainnet);
    for (route, venue) in env.cap.routes.iter().zip(&VENUES) {
        let raised = with_quoted_out(&route.swap, venue.quoted_out + (venue.quoted_out * 2).div_ceil(10_000));
        let mut svm = env.svm.clone();
        let before = snapshot(&svm, &env.cap, &raised);
        let paid_before = svm.get_balance(&env.executor.pubkey()).unwrap();
        let (err, _) = send_route(&mut svm, &env.executor, probe_instruction(&env.cap, &raised, 0, false))
            .expect_err("a quote 2 bp over the pool's output");
        assert_eq!(
            err,
            TransactionError::InstructionError(1, InstructionError::Custom(JUPITER_SLIPPAGE_TOLERANCE_EXCEEDED)),
            "{}",
            venue.label
        );
        assert_eq!(snapshot(&svm, &env.cap, &raised), before, "{}: nothing moved", venue.label);
        assert_eq!(svm.get_balance(&env.executor.pubkey()).unwrap(), paid_before - FEE_ONE_SIGNATURE, "{}", venue.label);
        assert_eq!(token_amount(&svm, &env.cap.taker_wsol), RESERVE_LAMPORTS, "{}", venue.label);
    }
}

/// The tolerance to the µUSDC: Jupiter's own check is `/build`'s `otherAmountThreshold`,
/// `quoted − ⌊quoted / 10 000⌋` at 1 bp. The largest quote whose threshold the pool's output meets
/// lands; one µUSDC more, whose threshold `⌊quoted × 9 999 / 10 000⌋` would still meet, is refused.
#[test]
fn jupiters_check_is_the_threshold_build_states() {
    let env = route_env(TokenProgram::Mainnet);
    for (route, venue) in env.cap.routes.iter().zip(&VENUES) {
        // The largest quote whose threshold the pool's output still meets.
        let mut largest = venue.quoted_out + venue.quoted_out / 9_999 + 2;
        while one_bp_threshold(largest) > venue.quoted_out {
            largest -= 1;
        }
        // So the refusal below tells the two roundings apart: `⌊quoted × 9 999 / 10 000⌋` would accept it.
        assert!((largest + 1) * 9_999 / 10_000 <= venue.quoted_out, "{}", venue.label);
        let mut svm = env.svm.clone();
        send_route(&mut svm, &env.executor, probe_instruction(&env.cap, &with_quoted_out(&route.swap, largest), 0, false))
            .unwrap_or_else(|e| panic!("{}: the largest quote {largest}: {e:?}", venue.label));
        assert_eq!(token_amount(&svm, &env.cap.destination), venue.quoted_out, "{}", venue.label);
        let mut svm = env.svm.clone();
        let (err, _) = send_route(&mut svm, &env.executor, probe_instruction(&env.cap, &with_quoted_out(&route.swap, largest + 1), 0, false))
            .expect_err("a µUSDC over the threshold");
        assert_eq!(
            err,
            TransactionError::InstructionError(1, InstructionError::Custom(JUPITER_SLIPPAGE_TOLERANCE_EXCEEDED)),
            "{}",
            venue.label
        );
    }
}

/// What the route moves: exactly `in_amount` out of the taker's WSOL account and exactly its output
/// into the destination, through the taker's USDC account, which it leaves as it found it: a gift
/// in either account stays there.
#[test]
fn the_route_moves_exactly_its_input_and_its_output() {
    let env = route_env(TokenProgram::Mainnet);
    const WSOL_GIFT: u64 = 1_000_000;
    const USDC_GIFT: u64 = 1_000;
    for (route, venue) in env.cap.routes.iter().zip(&VENUES) {
        let mut svm = env.svm.clone();
        let stranger = Keypair::new();
        svm.airdrop(&stranger.pubkey(), SOL).unwrap();
        wrap(&mut svm, &stranger, &env.cap.taker_wsol, WSOL_GIFT);
        let mut usdc = svm.get_account(&env.cap.taker_usdc).unwrap();
        let mut state = spl_token::state::Account::unpack(&usdc.data).unwrap();
        state.amount = USDC_GIFT;
        state.pack_into_slice(&mut usdc.data);
        svm.set_account(env.cap.taker_usdc, usdc).unwrap();

        send_route(&mut svm, &env.executor, probe_instruction(&env.cap, &route.swap, 0, false))
            .unwrap_or_else(|e| panic!("{}: {e:?}", venue.label));
        assert_eq!(token_amount(&svm, &env.cap.taker_wsol), WSOL_GIFT, "{}", venue.label);
        assert_eq!(token_amount(&svm, &env.cap.taker_usdc), USDC_GIFT, "{}", venue.label);
        assert_eq!(token_amount(&svm, &env.cap.destination), venue.quoted_out, "{}", venue.label);
    }
}

/// The taker's USDC account is the route's transit, so it must exist though the output goes to the
/// destination: without it Jupiter refuses the route (`InvalidTokenAccount`, 6025), nothing moved.
#[test]
fn the_takers_usdc_account_must_exist() {
    let env = route_env(TokenProgram::Mainnet);
    for (route, venue) in env.cap.routes.iter().zip(&VENUES) {
        let mut svm = env.svm.clone();
        svm.set_account(env.cap.taker_usdc, Account::default()).unwrap();
        let before = snapshot(&svm, &env.cap, &route.swap);
        let (err, _) = send_route(&mut svm, &env.executor, probe_instruction(&env.cap, &route.swap, 0, false))
            .expect_err("no USDC account for the taker");
        assert_eq!(
            err,
            TransactionError::InstructionError(1, InstructionError::Custom(JUPITER_INVALID_TOKEN_ACCOUNT)),
            "{}",
            venue.label
        );
        assert_eq!(snapshot(&svm, &env.cap, &route.swap), before, "{}: nothing moved", venue.label);
    }
}

/// On LiteSVM's own SPL Token 3.5.0 the routes land the same, at more compute units than on the
/// p-token mainnet runs: why this suite loads p-token.
#[test]
fn p_token_is_what_the_compute_units_are_measured_on() {
    let env = route_env(TokenProgram::Bundled);
    for (route, venue) in env.cap.routes.iter().zip(&VENUES) {
        let mut svm = env.svm.clone();
        let landed = send_route(&mut svm, &env.executor, probe_instruction(&env.cap, &route.swap, 0, false))
            .unwrap_or_else(|e| panic!("{}: {e:?}", venue.label));
        assert_eq!(token_amount(&svm, &env.cap.destination), venue.quoted_out, "{}", venue.label);
        assert_eq!(landed.jupiter_compute_units, venue.jupiter_units_spl_token, "{}", venue.label);
        assert!(venue.jupiter_units_spl_token > venue.jupiter_units_p_token);
    }
}

/// What the CPI itself costs, the probe's part of the totals by CPI: its PDA's derivation (bump
/// 253 under the probe's id, three tries of `create_program_address` at 1 500 compute units each)
/// and the CPI, in which `invoke_signed`'s check that no account it passes is borrowed takes 3 082
/// to 4 399 compute units at these 26 to 34 accounts, a search of the account infos for each meta.
/// `invoke_signed_unchecked` skips it and lands the same route.
#[test]
fn invoke_signed_unchecked_lands_the_same_route_for_less() {
    let env = route_env(TokenProgram::Mainnet);
    let (_, bump) = Pubkey::find_program_address(&[SWAP_SEED, env.cap.seed_mint.as_ref()], &probe_id());
    assert_eq!(bump, 253);
    for (route, venue) in env.cap.routes.iter().zip(&VENUES) {
        let mut svm = env.svm.clone();
        let checked = send_route(&mut svm, &env.executor, probe_instruction(&env.cap, &route.swap, 0, false))
            .unwrap_or_else(|e| panic!("{}: {e:?}", venue.label));
        let mut svm = env.svm.clone();
        let unchecked = send_route(&mut svm, &env.executor, probe_instruction(&env.cap, &route.swap, 0, true))
            .unwrap_or_else(|e| panic!("{}: {e:?}", venue.label));
        assert_eq!(token_amount(&svm, &env.cap.destination), venue.quoted_out, "{}", venue.label);
        assert_eq!(unchecked.jupiter_compute_units, checked.jupiter_compute_units, "{}", venue.label);
        assert_eq!(unchecked.invoked, checked.invoked, "{}", venue.label);
        assert_eq!(checked.compute_units, venue.cpi_units, "{}", venue.label);
        assert_eq!(checked.compute_units - unchecked.compute_units, venue.borrow_check_units, "{}", venue.label);
    }
}
