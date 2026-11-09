#![allow(dead_code)]

//! What the migration's suites share (upgrade N+1, docs/solana/PEPPER-CURVE-PROGRAM.md §18), and since
//! N+3 dropped the migration from the source the suites left standing on its fixtures: `migration_programs.rs`,
//! `migration_route.rs`, `migration_snapshot.rs` and `usdc_swap_buy.rs`. A suite takes it with
//! `mod migration_common;`, beside `mod common;`, which it leaves as track A wrote it:
//! - mainnet's Rent sysvar, which every LiteSVM of theirs runs under, and the four third-party
//!   programs of the swap and the token program mainnet runs, at their mainnet addresses with the
//!   fixtures `dump-litesvm-fixtures.sh` reads and `fixtures.rs` pins (M1, M2);
//! - the live SOL curves of mainnet, as `scripts/solana/snapshot-live-curves.ts` committed them (M5).
//!
//! The snapshot is chain data only, every account in the Solana CLI's JSON as `fixture_account`
//! reads it (its address, lamports, data in base64, owner, `executable`, `rentEpoch`, `space`), at
//! one finalized slot: each `Curve` with its `SolVault`, `Vesting`, token vault, vesting vault and
//! mint, `GlobalConfig` and the Rent sysvar. `migration_snapshot.rs` pins its sha256.
//!
//! ⚠️ The live rent, never LiteSVM's default. LiteSVM 0.6 starts on `Rent::default()` (3,480
//! lamports per byte-year, threshold 2.0), where mainnet charges 5,080 at 1.0 (read 2026-10-23):
//! five live curves hold less than the default's minimum for 196 bytes, so a suite on the default
//! would refuse states mainnet holds and price the migration's top-up wrong. `load_snapshot` sets
//! the snapshot's own Rent sysvar first, and `live_svm` hands back a bank that has it.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use litesvm::LiteSVM;
use serde_json::Value;
use solana_sdk::{account::Account, pubkey::Pubkey, rent::Rent};

pub const JUPITER_V6_ID: Pubkey = Pubkey::from_str_const("JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4");
pub const JUPITER_V6_SO: &str = "jupiter_v6.so";
pub const ORCA_WHIRLPOOL_ID: Pubkey = Pubkey::from_str_const("whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc");
pub const ORCA_WHIRLPOOL_SO: &str = "orca_whirlpool.so";
pub const RAYDIUM_CLMM_ID: Pubkey = Pubkey::from_str_const("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK");
pub const RAYDIUM_CLMM_SO: &str = "raydium_clmm.so";
pub const METEORA_DLMM_ID: Pubkey = Pubkey::from_str_const("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo");
pub const METEORA_DLMM_SO: &str = "meteora_dlmm.so";
/// The token program mainnet runs at `Tokenkeg…` since slot 419 472 000: p-token, the pinocchio
/// rewrite, immutable, where LiteSVM 0.6.1 bundles SPL Token 3.5.0 at that address.
pub const P_TOKEN_SO: &str = "p_token.so";

/// Mainnet's Rent sysvar, read on 2026-10-23 (finalized slot 460474650): 5 080
/// lamports per byte-year, an exemption threshold of 1.0, half of it burnt.
/// LiteSVM's default (3 480 × 2.0) asks 37% more for every account.
pub const MAINNET_RENT: Rent = Rent { lamports_per_byte_year: 5_080, exemption_threshold: 1.0, burn_percent: 50 };

/// A LiteSVM under `MAINNET_RENT`, held to the minimums mainnet answered the
/// same day for a SOL `Curve` (196 bytes) and a `CurveUsdc` (354).
pub fn mainnet_rent_svm() -> LiteSVM {
    let mut svm = LiteSVM::new();
    svm.set_sysvar(&MAINNET_RENT);
    assert_eq!(svm.minimum_balance_for_rent_exemption(196), 1_645_920);
    assert_eq!(svm.minimum_balance_for_rent_exemption(354), 2_448_560);
    svm
}

pub const SNAPSHOT_FILE: &str = "live_sol_curves.json";
/// The fixture's format, `SNAPSHOT_FORMAT_VERSION` in `lib/solana/migration/live-curve-snapshot.ts`.
pub const SNAPSHOT_FORMAT: u64 = 1;

/// The live rent the snapshot holds, read on 2026-10-23 and pinned by `migration_snapshot.rs`.
/// ⚠️ Rent moves: SIMD-0437's last steps (Agave 4.4, around November 2026) bring the rate to 696,
/// so a snapshot taken at the window may hold another rent. A suite takes `live_rent()`, never these
/// two constants; they move with the pin.
pub const LIVE_LAMPORTS_PER_BYTE_YEAR: u64 = 5_080;
pub const LIVE_EXEMPTION_THRESHOLD: f64 = 1.0;

/// `Curve`, 196 bytes: (field, offset, length), in declaration order from byte 8, no gap
/// (`programs/pepper_curve/src/state.rs`; `CURVE_FIELDS` in `live-curve-snapshot.ts`).
pub const CURVE_FIELDS: [(&str, usize, usize); 21] = [
    ("mint", 8, 32),
    ("creator", 40, 32),
    ("phase", 72, 1),
    ("creator_target_lamports", 73, 8),
    ("cpmm_seed_lamports", 81, 8),
    ("creator_split_bps", 89, 8),
    ("fanex_split_bps", 97, 8),
    ("creator_cap_lamports", 105, 8),
    ("fanex_cap_lamports", 113, 8),
    ("virtual_sol_reserves", 121, 8),
    ("virtual_token_reserves", 129, 8),
    ("real_sol_reserves", 137, 8),
    ("real_token_reserves", 145, 8),
    ("migration_token_reserves", 153, 8),
    ("creator_received_lamports", 161, 8),
    ("fanex_received_lamports", 169, 8),
    ("paused", 177, 1),
    ("created_at", 178, 8),
    ("graduated_at", 186, 8),
    ("bump", 194, 1),
    ("sol_vault_bump", 195, 1),
];

pub fn migration_fixtures_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migration-fixtures")
}

pub fn migration_fixture_path(name: &str) -> std::path::PathBuf {
    migration_fixtures_dir().join(name)
}

/// One account of the snapshot, at its address.
#[derive(Clone, Debug)]
pub struct SnapshotAccount {
    pub address: Pubkey,
    pub account: Account,
}

/// One curve's accounts, and the `decoded` block the script wrote beside them.
#[derive(Clone, Debug)]
pub struct SnapshotCurve {
    pub mint: Pubkey,
    pub curve: SnapshotAccount,
    pub sol_vault: SnapshotAccount,
    pub vesting: SnapshotAccount,
    pub token_vault: SnapshotAccount,
    pub vesting_vault: SnapshotAccount,
    pub mint_account: SnapshotAccount,
    pub decoded: Value,
}

impl SnapshotCurve {
    pub fn accounts(&self) -> [&SnapshotAccount; 6] {
        [&self.curve, &self.sol_vault, &self.vesting, &self.token_vault, &self.vesting_vault, &self.mint_account]
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub genesis_hash: String,
    pub program_id: Pubkey,
    pub slot: u64,
    pub rent: SnapshotAccount,
    pub rent_decoded: Value,
    pub global_config: SnapshotAccount,
    pub global_config_decoded: Value,
    pub census: Value,
    pub curves: Vec<SnapshotCurve>,
}

fn pubkey(value: &Value, what: &str) -> Pubkey {
    value.as_str().and_then(|s| s.parse().ok()).unwrap_or_else(|| panic!("{what}: not a public key"))
}

/// One account in the CLI's JSON: `{"pubkey", "account": {...}}`, its `space` held to the data.
fn cli_account(value: &Value, what: &str) -> SnapshotAccount {
    let address = pubkey(&value["pubkey"], &format!("{what}.pubkey"));
    let account = &value["account"];
    assert_eq!(account["data"][1], "base64", "{what}: the data's encoding");
    let data = STANDARD
        .decode(account["data"][0].as_str().unwrap_or_default())
        .unwrap_or_else(|e| panic!("{what}: data, {e}"));
    assert_eq!(account["space"].as_u64(), Some(data.len() as u64), "{what}: space");
    let account = Account {
        lamports: account["lamports"].as_u64().unwrap_or_else(|| panic!("{what}: lamports")),
        data,
        owner: pubkey(&account["owner"], &format!("{what}.owner")),
        executable: account["executable"].as_bool().unwrap_or_else(|| panic!("{what}: executable")),
        rent_epoch: account["rentEpoch"].as_u64().unwrap_or_else(|| panic!("{what}: rentEpoch")),
    };
    SnapshotAccount { address, account }
}

pub fn snapshot_text() -> String {
    let path = migration_fixture_path(SNAPSHOT_FILE);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The committed snapshot, parsed.
pub fn snapshot() -> Snapshot {
    let file: Value = serde_json::from_str(&snapshot_text()).unwrap_or_else(|e| panic!("{SNAPSHOT_FILE}: {e}"));
    assert_eq!(file["format"].as_u64(), Some(SNAPSHOT_FORMAT), "the snapshot's format");
    let curves = file["curves"]
        .as_array()
        .expect("curves")
        .iter()
        .map(|entry| {
            let mint = pubkey(&entry["mint"], "mint");
            let account = |role: &str| cli_account(&entry["accounts"][role], &format!("{mint}.{role}"));
            SnapshotCurve {
                mint,
                curve: account("curve"),
                sol_vault: account("sol_vault"),
                vesting: account("vesting"),
                token_vault: account("token_vault"),
                vesting_vault: account("vesting_vault"),
                mint_account: account("mint"),
                decoded: entry["decoded"].clone(),
            }
        })
        .collect();
    Snapshot {
        genesis_hash: file["genesis_hash"].as_str().expect("genesis_hash").to_string(),
        program_id: pubkey(&file["program_id"], "program_id"),
        slot: file["slot"].as_u64().expect("slot"),
        rent: cli_account(&file["rent"], "rent"),
        rent_decoded: file["rent"]["decoded"].clone(),
        global_config: cli_account(&file["global_config"], "global_config"),
        global_config_decoded: file["global_config"]["decoded"].clone(),
        census: file["census"].clone(),
        curves,
    }
}

/// The Rent sysvar's 17 bytes as bincode writes them: u64, f64, u8.
pub fn rent_from_sysvar_data(data: &[u8]) -> Rent {
    assert_eq!(data.len(), 17, "the Rent sysvar's size");
    Rent {
        lamports_per_byte_year: u64::from_le_bytes(data[0..8].try_into().unwrap()),
        exemption_threshold: f64::from_le_bytes(data[8..16].try_into().unwrap()),
        burn_percent: data[16],
    }
}

/// Mainnet's rent, as the snapshot read it.
pub fn live_rent() -> Rent {
    rent_from_sysvar_data(&snapshot().rent.account.data)
}

/// A LiteSVM bank on the live rent.
pub fn live_svm() -> LiteSVM {
    let mut svm = LiteSVM::new();
    svm.set_sysvar(&live_rent());
    svm
}

/// Writes the live rent, `GlobalConfig` and every curve's accounts into the bank, at their addresses.
pub fn load_snapshot(svm: &mut LiteSVM, snapshot: &Snapshot) {
    svm.set_sysvar(&rent_from_sysvar_data(&snapshot.rent.account.data));
    svm.set_account(snapshot.global_config.address, snapshot.global_config.account.clone()).unwrap();
    for curve in &snapshot.curves {
        for a in curve.accounts() {
            svm.set_account(a.address, a.account.clone()).unwrap();
        }
    }
}
