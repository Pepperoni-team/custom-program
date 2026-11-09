#![allow(dead_code)]

//! M2's capture of real Jupiter routes, shared by the suites that run them (track M;
//! docs/solana/PEPPER-CURVE-PROGRAM.md §18.2, §18.10): `migration_route.rs` lands them through the route
//! probe (M2), `usdc_swap_buy.rs` on the bank they read (phase 5.1's P11b); `migration_jupiter.rs` ran
//! them through `migrate_to_usdc` (M11) until N+3 dropped the migration from the source. A suite takes
//! it with `mod migration_capture;` beside `mod common;` and `mod migration_common;`.
//!
//! `migration-fixtures/routes.json` holds one route from Jupiter's `/build` per allowed venue (Orca's
//! Whirlpool, Raydium's CLMM, Meteora's DLMM), a one-hop SOL → USDC sell of CICCO1's reserve with
//! `taker` the probe's swap PDA and `payer` its executor, and every account those routes read, all at
//! one mainnet slot with its Clock (`scripts/solana/migration-route-fixtures.ts`). The programs are the
//! binaries `fixtures.rs` pins, under the upgradeable loader as mainnet holds them. The numbers pinned
//! here are the capture's: a re-capture moves them on purpose, with the doc.

use crate::common::*;
use crate::migration_common::*;
use anchor_lang::solana_program::program_pack::Pack;
use anchor_spl::token::spl_token;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use litesvm::LiteSVM;
use serde_json::Value;
use solana_sdk::{
    account::Account,
    clock::Clock,
    hash::hash,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction, sysvar,
    transaction::Transaction,
};

pub const ROUTES_JSON: &str = "routes.json";
/// The capture of 2026-10-23, mainnet slot 460 486 282: a re-capture stays red here until its
/// numbers below are read and moved on purpose.
pub const ROUTES_SHA256: &str = "aee418e34c6b3ea82c4121e651dbc5a8d182bd7b770b02f8266815f3903ab276";
pub const CAPTURE_SLOT: u64 = 460_486_282;
/// CICCO1's real reserve in the census of 2026-10-23, the furthest live curve: what every route sells.
pub const RESERVE_LAMPORTS: u64 = 2_614_777_778;

pub const USDC_MINT: Pubkey = Pubkey::from_str_const("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
pub const WSOL_MINT: Pubkey = spl_token::native_mint::ID;
pub const JUPITER_EVENT_AUTHORITY: Pubkey = Pubkey::from_str_const("D8cy77BBepLMngZx6ZukaTff5hCt1HrWyKk3Hnd9oitf");
/// Jupiter's `SlippageToleranceExceeded` (its IDL's 6001).
pub const JUPITER_SLIPPAGE_TOLERANCE_EXCEEDED: u32 = 6001;
/// Jupiter's `InvalidTokenAccount` (its IDL's 6025).
pub const JUPITER_INVALID_TOKEN_ACCOUNT: u32 = 6025;
/// The limit `/build`'s guide asks a simulation for, the most a transaction may take.
pub const MAX_COMPUTE_UNITS: u32 = 1_400_000;
/// What the compute-unit-limit instruction itself costs, a builtin's 150.
pub const COMPUTE_BUDGET_UNITS: u64 = 150;
/// LiteSVM's fee for one signature, mainnet's.
pub const FEE_ONE_SIGNATURE: u64 = 5_000;
/// The four Jupiter instructions track M's plan lets the migration's CPI send (R10).
pub const R10_ROUTES: [&str; 4] = ["route", "route_v2", "shared_accounts_route", "shared_accounts_route_v2"];

/// One venue's route as the capture holds it and as it measured, in the capture's order.
pub struct Venue {
    pub label: &'static str,
    pub program: Pubkey,
    pub pool: Pubkey,
    /// The step's `Swap` in `route_v2`'s plan, its variant and fields (Jupiter v6's IDL, read from
    /// its IDL account on 2026-10-23: 17 `Whirlpool { a_to_b }`, 26 `RaydiumClmm`, 38 `MeteoraDlmm`).
    pub swap: &'static [u8],
    /// The swap instruction's accounts, `route_v2`'s 10 and the hop's.
    pub accounts: usize,
    pub quoted_out: u64,
    /// Compute units inside Jupiter's invocation, the venue's and the token program's CPIs
    /// included, on p-token and on LiteSVM's SPL Token 3.5.0. Jupiter's binary moves them: on its
    /// redeploy of slot 462802085 (`fixtures.rs`, 2026-10-30) 1 more over Whirlpool and 2 more over
    /// the CLMM and the DLMM than on the capture's binary of slot 459479624 (37 434, 53 310 and
    /// 41 645 on p-token), every output the same to the µUSDC.
    pub jupiter_units_p_token: u64,
    pub jupiter_units_spl_token: u64,
    /// The whole transaction by CPI from the probe, on p-token, the limit's 150 included, and how
    /// much of it `invoke_signed`'s borrow check takes: the probe's share is its own build's, which
    /// `ci-onchain-tests.sh` makes with Agave 3.1.14 (CI's too), so another SBF toolchain moves
    /// these two on purpose, with the doc; Jupiter's binary moves the first by its own units.
    pub cpi_units: u64,
    pub borrow_check_units: u64,
}

pub const VENUES: [Venue; 3] = [
    Venue {
        label: "Whirlpool",
        program: ORCA_WHIRLPOOL_ID,
        pool: Pubkey::from_str_const("Czfq3xZZDmsdGdUyrNLtRhGc47cXcZtLG4crryfu44zE"),
        swap: &[17, 1],
        accounts: 22,
        quoted_out: 273_254_815,
        jupiter_units_p_token: 37_435,
        jupiter_units_spl_token: 51_759,
        cpi_units: 53_871,
        borrow_check_units: 3_082,
    },
    Venue {
        label: "Raydium CLMM",
        program: RAYDIUM_CLMM_ID,
        pool: Pubkey::from_str_const("3ucNos4NbumPLZNWztqGHNFFgkHeRMBQAVemeeomsUxv"),
        swap: &[26],
        accounts: 25,
        quoted_out: 273_253_801,
        jupiter_units_p_token: 53_312,
        jupiter_units_spl_token: 67_636,
        cpi_units: 71_106,
        borrow_check_units: 3_623,
    },
    Venue {
        label: "Meteora DLMM",
        program: METEORA_DLMM_ID,
        pool: Pubkey::from_str_const("3msVd34R5KxonDzyNSV5nT19UtUeJ2RF1NaQhvVPNLxL"),
        swap: &[38],
        accounts: 30,
        quoted_out: 273_277_978,
        jupiter_units_p_token: 41_647,
        jupiter_units_spl_token: 58_972,
        cpi_units: 60_878,
        borrow_check_units: 4_399,
    },
];

/// The deploy slot of each binary the capture ran on, which it read back after its accounts: the
/// pools ran these very binaries at the capture's slot, and `fixtures.rs` pins the three venues'.
/// Jupiter's fixture is its redeploy of slot 462802085 since 2026-10-30, on which the capture's
/// routes land at the same outputs (docs/solana/PEPPER-CURVE-PROGRAM.md §18.1).
pub const PINNED_DEPLOY_SLOTS: [(Pubkey, u64); 4] = [
    (JUPITER_V6_ID, 459_479_624),
    (ORCA_WHIRLPOOL_ID, 457_152_296),
    (RAYDIUM_CLMM_ID, 459_932_378),
    (METEORA_DLMM_ID, 423_977_638),
];

pub fn discriminator(name: &str) -> [u8; 8] {
    hash(format!("global:{name}").as_bytes()).to_bytes()[..8].try_into().unwrap()
}

pub fn key(v: &Value) -> Pubkey {
    Pubkey::from_str_const(v.as_str().unwrap_or_else(|| panic!("not a key: {v}")))
}

pub fn amount(v: &Value) -> u64 {
    v.as_str().and_then(|s| s.parse().ok()).unwrap_or_else(|| panic!("not an amount: {v}"))
}

pub fn fixture_bytes(name: &str) -> Vec<u8> {
    let path = fixture_path(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e} (scripts/solana/dump-litesvm-fixtures.sh)", path.display()))
}

pub fn instruction(v: &Value) -> Instruction {
    Instruction {
        program_id: key(&v["programId"]),
        accounts: v["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| AccountMeta {
                pubkey: key(&a["pubkey"]),
                is_signer: a["isSigner"].as_bool().unwrap(),
                is_writable: a["isWritable"].as_bool().unwrap(),
            })
            .collect(),
        data: STANDARD.decode(v["data"].as_str().unwrap()).unwrap(),
    }
}

/// One venue's route as `/build` returned it, with the request that asked for it.
pub struct CapturedRoute {
    pub json: Value,
    pub setup: Vec<Instruction>,
    pub swap: Instruction,
}

/// The capture: the probe's side of the routes, the routes, the accounts at the slot.
pub struct Capture {
    pub json: Value,
    pub bytes: Vec<u8>,
    pub seed_mint: Pubkey,
    pub taker: Pubkey,
    pub taker_wsol: Pubkey,
    pub taker_usdc: Pubkey,
    pub destination: Pubkey,
    pub executor: Pubkey,
    pub routes: Vec<CapturedRoute>,
    pub clock: Clock,
    pub accounts: Vec<(Pubkey, Account)>,
}

pub fn capture() -> Capture {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migration-fixtures").join(ROUTES_JSON);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e} (scripts/solana/migration-route-fixtures.ts)", path.display()));
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    let mut clock = None;
    let mut accounts = vec![];
    for a in json["accounts"].as_array().unwrap() {
        let address = key(&a["pubkey"]);
        let data = STANDARD.decode(a["data"].as_str().unwrap()).unwrap();
        if address == sysvar::clock::ID {
            clock = Some(decode_clock(&data));
            continue;
        }
        let account = Account {
            lamports: a["lamports"].as_u64().unwrap(),
            data,
            owner: key(&a["owner"]),
            executable: a["executable"].as_bool().unwrap(),
            rent_epoch: u64::MAX,
        };
        accounts.push((address, account));
    }
    let routes = json["routes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| CapturedRoute {
            json: r.clone(),
            setup: r["setupInstructions"].as_array().unwrap().iter().map(instruction).collect(),
            swap: instruction(&r["swapInstruction"]),
        })
        .collect();
    Capture {
        seed_mint: key(&json["seedMint"]),
        taker: key(&json["taker"]),
        taker_wsol: key(&json["takerWsolAccount"]),
        taker_usdc: key(&json["takerUsdcAccount"]),
        destination: key(&json["destination"]),
        executor: key(&json["executor"]),
        routes,
        clock: clock.expect("the Clock at the slot"),
        accounts,
        json,
        bytes,
    }
}

/// The Clock sysvar's bytes: slot, epoch start timestamp, epoch, leader schedule epoch, unix
/// timestamp, each eight bytes little-endian.
pub fn decode_clock(data: &[u8]) -> Clock {
    assert_eq!(data.len(), 40);
    let word = |i: usize| <[u8; 8]>::try_from(&data[i * 8..i * 8 + 8]).unwrap();
    Clock {
        slot: u64::from_le_bytes(word(0)),
        epoch_start_timestamp: i64::from_le_bytes(word(1)),
        epoch: u64::from_le_bytes(word(2)),
        leader_schedule_epoch: u64::from_le_bytes(word(3)),
        unix_timestamp: i64::from_le_bytes(word(4)),
    }
}

/// `route_v2`'s arguments after its discriminator (Jupiter v6's IDL): `in_amount: u64,
/// quoted_out_amount: u64, slippage_bps: u16, platform_fee_bps: u16, positive_slippage_bps: u16,
/// route_plan: Vec<RoutePlanStepV2 { swap: Swap, bps: u16, input_index: u8, output_index: u8 }>`.
#[derive(Debug, PartialEq)]
pub struct RouteV2 {
    pub in_amount: u64,
    pub quoted_out_amount: u64,
    pub slippage_bps: u16,
    pub platform_fee_bps: u16,
    pub positive_slippage_bps: u16,
    /// Each step: its `Swap` (variant and fields), its share in basis points, its input and output.
    pub steps: Vec<(Vec<u8>, u16, u8, u8)>,
}

/// Where `in_amount` and `quoted_out_amount` sit in `route_v2`'s data.
pub const IN_AMOUNT: std::ops::Range<usize> = 8..16;
pub const QUOTED_OUT: std::ops::Range<usize> = 16..24;

pub fn decode_route_v2(data: &[u8]) -> RouteV2 {
    assert_eq!(data[..8], discriminator("route_v2"), "not route_v2");
    let u64_at = |i: usize| u64::from_le_bytes(data[i..i + 8].try_into().unwrap());
    let u16_at = |i: usize| u16::from_le_bytes(data[i..i + 2].try_into().unwrap());
    let len = u32::from_le_bytes(data[30..34].try_into().unwrap());
    let mut at = 34;
    let mut steps = vec![];
    for _ in 0..len {
        // Only the three venues the migration allows; another variant is a route it would not take.
        let fields = match data[at] {
            17 => 1,
            26 | 38 => 0,
            other => panic!("Swap variant {other}: not a venue the migration allows"),
        };
        let swap = data[at..at + 1 + fields].to_vec();
        at += 1 + fields;
        steps.push((swap, u16_at(at), data[at + 2], data[at + 3]));
        at += 4;
    }
    assert_eq!(at, data.len(), "route_v2's data runs past its route plan");
    RouteV2 {
        in_amount: u64_at(8),
        quoted_out_amount: u64_at(16),
        slippage_bps: u16_at(24),
        platform_fee_bps: u16_at(26),
        positive_slippage_bps: u16_at(28),
        steps,
    }
}

/// The swap instruction with another `quoted_out_amount`, the rest of its data and accounts as
/// `/build` wrote them.
pub fn with_quoted_out(swap: &Instruction, quoted_out: u64) -> Instruction {
    let mut swap = swap.clone();
    swap.data[QUOTED_OUT].copy_from_slice(&quoted_out.to_le_bytes());
    swap
}

/// The least output Jupiter lands for a quote, at 1 bp: `/build`'s `otherAmountThreshold`.
pub fn one_bp_threshold(quoted_out: u64) -> u64 {
    quoted_out - quoted_out / 10_000
}

/// Which token program runs at `Tokenkeg…`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TokenProgram {
    /// Mainnet's: p-token, dumped and pinned (`P_TOKEN_SO`).
    Mainnet,
    /// LiteSVM 0.6.1's own, SPL Token 3.5.0, which every other suite runs on.
    Bundled,
}

/// Jupiter and the three venues, and p-token at `Tokenkeg…` unless `token` keeps LiteSVM's own, each
/// as mainnet holds it, under the upgradeable loader (a 36-byte Program account), never as LiteSVM's
/// `add_program` writes one (the whole ELF in the program's own account), which a CPI passing it pays
/// for by the byte. Loaded at the bank's slot, before the capture's Clock moves it on: a program runs
/// from the slot after its deploy.
pub fn load_mainnet_programs(svm: &mut LiteSVM, token: TokenProgram) {
    for (id, so) in [
        (JUPITER_V6_ID, JUPITER_V6_SO),
        (ORCA_WHIRLPOOL_ID, ORCA_WHIRLPOOL_SO),
        (RAYDIUM_CLMM_ID, RAYDIUM_CLMM_SO),
        (METEORA_DLMM_ID, METEORA_DLMM_SO),
    ] {
        load_upgradeable_program(svm, id, &fixture_bytes(so), None);
    }
    if token == TokenProgram::Mainnet {
        load_upgradeable_program(svm, spl_token::ID, &fixture_bytes(P_TOKEN_SO), None);
    }
}

/// The capture's Clock and every account it read, written into the bank: the pools as they stood at
/// the slot their routes were quoted against.
pub fn load_capture(svm: &mut LiteSVM, cap: &Capture) {
    svm.set_sysvar(&cap.clock);
    for (address, account) in &cap.accounts {
        svm.set_account(*address, account.clone()).unwrap();
    }
}

/// `lamports` wrapped into the WSOL account `wsol`: a System transfer, then `sync_native`.
pub fn wrap(svm: &mut LiteSVM, payer: &Keypair, wsol: &Pubkey, lamports: u64) {
    let ixs = [
        system_instruction::transfer(&payer.pubkey(), wsol, lamports),
        spl_token::instruction::sync_native(&spl_token::ID, wsol).unwrap(),
    ];
    let tx = Transaction::new_signed_with_payer(&ixs, Some(&payer.pubkey()), &[payer], svm.latest_blockhash());
    svm.send_transaction(tx).unwrap_or_else(|e| panic!("the wrap: {:?}", e.err));
}

pub fn token_amount(svm: &LiteSVM, address: &Pubkey) -> u64 {
    let account = svm.get_account(address).unwrap_or_else(|| panic!("{address}: no account"));
    spl_token::state::Account::unpack(&account.data).unwrap().amount
}

/// The compute units `program`'s outermost invocation reported, the last line it wrote.
pub fn consumed(logs: &[String], program: &Pubkey) -> u64 {
    let prefix = format!("Program {program} consumed ");
    let line = logs.iter().rev().find_map(|line| line.strip_prefix(&prefix)).unwrap_or_else(|| panic!("{program} never ran: {logs:?}"));
    line.split(' ').next().unwrap().parse().unwrap()
}
