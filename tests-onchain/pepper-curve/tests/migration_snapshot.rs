//! The snapshot of the live SOL curves, pinned and read with the program's own types (track M,
//! M5; docs/solana/PEPPER-CURVE-PROGRAM.md §18).
//!
//! `scripts/solana/snapshot-live-curves.ts --write` reads mainnet and writes
//! `migration-fixtures/live_sol_curves.json` with its `SHA256SUMS`. The pin below is written by
//! hand, as `fixtures.rs` pins track A's: a new snapshot stays red here until someone reads what
//! moved and moves the pin, the census asserted below with it. Every curve is decoded twice, by
//! Anchor's `try_deserialize` and at the offsets the migration will read, and the script's own
//! `decoded` blocks are held to both, so the TypeScript decoder, the offsets and the Rust layout
//! cannot drift apart.

mod migration_common;

use anchor_lang::solana_program::program_option::COption;
use anchor_lang::solana_program::program_pack::Pack;
use anchor_lang::{AccountDeserialize, AccountSerialize, Space};
use anchor_spl::associated_token::get_associated_token_address;
use anchor_spl::token::spl_token;
use migration_common::*;
use pepper_curve::constants::{CONFIG_SEED, CURVE_SEED, SOL_VAULT_SEED, VESTING_SEED, VESTING_VAULT_SEED};
use pepper_curve::state::{Curve, CurvePhase, GlobalConfig, SolVault, Vesting};
use serde_json::{json, Value};
use solana_sdk::{hash::hash, pubkey::Pubkey, rent::Rent, sysvar};

/// The snapshot's sha256, read from mainnet on 2026-10-23 at finalized slot 460 477 618.
const PINNED_SHA256: &str = "50067cef756bed254988d6da4bd605c259ce126f749f227a0863483385838dbc";
const PINNED_SLOT: u64 = 460_477_618;

/// The census it holds: the one of 2026-10-23 the migration plan was written on (slot 460 450 059),
/// unchanged at this slot: 13 curves, all Bonding, none paused, `GlobalConfig` unpaused.
const CURVES: usize = 13;
const REAL_SOL_RESERVES: u64 = 11_108_969_639;
/// The curves holding less than LiteSVM's default minimum for 196 bytes (2 255 040): the four
/// launched on the live rent (1 645 920) and MARCO, on a rent between the two (2 051 892).
const CURVES_UNDER_THE_DEFAULT_RENT: usize = 5;

const MAINNET_GENESIS: &str = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";

fn sha256_hex(bytes: &[u8]) -> String {
    hash(bytes).to_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

fn pda(seeds: &[&[u8]]) -> (Pubkey, u8) {
    Pubkey::find_program_address(seeds, &pepper_curve::ID)
}

#[test]
fn the_snapshot_matches_its_pinned_sha256() {
    assert_eq!(sha256_hex(snapshot_text().as_bytes()), PINNED_SHA256, "not the snapshot pinned here");
    assert_eq!(snapshot().slot, PINNED_SLOT);
}

/// The folder's other files, each pinned by what reads it: the route capture by `migration_route.rs`
/// (M2), the executor's messages by `lib/solana/migration/executor-fixture.test.ts` (M14; its Rust
/// replay, `migration_jupiter.rs`, left with the migration's source in N+3) and the server's
/// swap-buys over the capture by `usdc_swap_buy.rs` (phase 5.1's P11b), which a snapshot's `--write`
/// never rewrites.
const PINNED_BY_THEIR_OWN_SUITE: [&str; 3] = ["routes.json", "executor_messages.json", "swap_buy_messages.json"];

#[test]
fn the_sums_file_lists_the_snapshot_and_every_file_in_the_folder_is_pinned() {
    let sums = std::fs::read_to_string(migration_fixture_path("SHA256SUMS")).unwrap();
    assert_eq!(sums, format!("{PINNED_SHA256}  {SNAPSHOT_FILE}\n"));
    for entry in std::fs::read_dir(migration_fixtures_dir()).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        assert!(
            name == "SHA256SUMS" || name == SNAPSHOT_FILE || PINNED_BY_THEIR_OWN_SUITE.contains(&name.as_str()),
            "{name} is in migration-fixtures/ without a pin"
        );
    }
}

#[test]
fn the_snapshot_is_mainnet_and_this_program() {
    let s = snapshot();
    assert_eq!(s.genesis_hash, MAINNET_GENESIS);
    assert_eq!(s.program_id, pepper_curve::ID);
}

/// A `Curve` field's bytes as the struct holds it, and its value as the script's `decoded` block
/// writes it (u64 and i64 as decimal strings, the phase by name).
fn curve_field(c: &Curve, name: &str) -> (Vec<u8>, Value) {
    let u = |v: u64| (v.to_le_bytes().to_vec(), json!(v.to_string()));
    let i = |v: i64| (v.to_le_bytes().to_vec(), json!(v.to_string()));
    let k = |v: &Pubkey| (v.to_bytes().to_vec(), json!(v.to_string()));
    match name {
        "mint" => k(&c.mint),
        "creator" => k(&c.creator),
        "phase" => (vec![c.phase as u8], json!(format!("{:?}", c.phase))),
        "creator_target_lamports" => u(c.creator_target_lamports),
        "cpmm_seed_lamports" => u(c.cpmm_seed_lamports),
        "creator_split_bps" => u(c.creator_split_bps),
        "fanex_split_bps" => u(c.fanex_split_bps),
        "creator_cap_lamports" => u(c.creator_cap_lamports),
        "fanex_cap_lamports" => u(c.fanex_cap_lamports),
        "virtual_sol_reserves" => u(c.virtual_sol_reserves),
        "virtual_token_reserves" => u(c.virtual_token_reserves),
        "real_sol_reserves" => u(c.real_sol_reserves),
        "real_token_reserves" => u(c.real_token_reserves),
        "migration_token_reserves" => u(c.migration_token_reserves),
        "creator_received_lamports" => u(c.creator_received_lamports),
        "fanex_received_lamports" => u(c.fanex_received_lamports),
        "paused" => (vec![c.paused as u8], json!(c.paused)),
        "created_at" => i(c.created_at),
        "graduated_at" => i(c.graduated_at),
        "bump" => (vec![c.bump], json!(c.bump)),
        "sol_vault_bump" => (vec![c.sol_vault_bump], json!(c.sol_vault_bump)),
        other => panic!("no Curve field {other}"),
    }
}

fn reserialized<T: AccountSerialize>(account: &T) -> Vec<u8> {
    let mut out = Vec::new();
    account.try_serialize(&mut out).unwrap();
    out
}

#[test]
fn every_curve_decodes_with_the_rust_layout_at_its_offsets() {
    let mut next = 8;
    for (name, offset, len) in CURVE_FIELDS {
        assert_eq!(offset, next, "{name} does not follow the field before it");
        next = offset + len;
    }
    assert_eq!(next, 8 + Curve::INIT_SPACE, "the offsets end where a Curve does");
    let s = snapshot();
    assert_eq!(s.curves.len(), CURVES);
    for entry in &s.curves {
        let data = &entry.curve.account.data;
        assert_eq!(data.len(), 196, "{}", entry.mint);
        let curve = Curve::try_deserialize(&mut &data[..]).unwrap_or_else(|e| panic!("{}: {e}", entry.mint));
        assert_eq!(&reserialized(&curve), data, "{}: Borsh reads every byte", entry.mint);
        for (name, offset, len) in CURVE_FIELDS {
            let (bytes, value) = curve_field(&curve, name);
            assert_eq!(bytes.len(), len, "{name}'s length");
            assert_eq!(&data[offset..offset + len], &bytes[..], "{}: {name} at {offset}", entry.mint);
            assert_eq!(entry.decoded["curve"][name], value, "{}: the script's {name}", entry.mint);
        }
        assert_eq!(entry.decoded["curve"].as_object().unwrap().len(), CURVE_FIELDS.len(), "{}: no other field", entry.mint);
    }
}

#[test]
fn the_other_accounts_decode_as_the_script_wrote_them() {
    let s = snapshot();
    let config = GlobalConfig::try_deserialize(&mut &s.global_config.account.data[..]).unwrap();
    assert_eq!(reserialized(&config), s.global_config.account.data);
    assert_eq!(
        s.global_config_decoded,
        json!({
            "admin": config.admin.to_string(),
            "fanex_fee_wallet": config.fanex_fee_wallet.to_string(),
            "graduation_authority": config.graduation_authority.to_string(),
            "paused": config.paused,
            "bump": config.bump,
        })
    );
    let rent = rent_from_sysvar_data(&s.rent.account.data);
    assert_eq!(s.rent_decoded["lamports_per_byte_year"], json!(rent.lamports_per_byte_year.to_string()));
    // JSON from JavaScript writes 1.0 as 1: compared as a float.
    assert_eq!(s.rent_decoded["exemption_threshold"].as_f64(), Some(rent.exemption_threshold));
    assert_eq!(s.rent_decoded["burn_percent"], json!(rent.burn_percent));
    assert_eq!(s.rent_decoded.as_object().unwrap().len(), 3);
    let key = |o: COption<Pubkey>| match o {
        COption::Some(k) => json!(k.to_string()),
        COption::None => Value::Null,
    };
    for entry in &s.curves {
        let at = entry.mint;
        let vault = SolVault::try_deserialize(&mut &entry.sol_vault.account.data[..]).unwrap();
        assert_eq!(reserialized(&vault), entry.sol_vault.account.data, "{at}: SolVault");
        assert_eq!(entry.decoded["sol_vault"], json!({ "mint": vault.mint.to_string(), "bump": vault.bump }), "{at}");
        let v = Vesting::try_deserialize(&mut &entry.vesting.account.data[..]).unwrap();
        assert_eq!(reserialized(&v), entry.vesting.account.data, "{at}: Vesting");
        assert_eq!(
            entry.decoded["vesting"],
            json!({
                "mint": v.mint.to_string(),
                "beneficiary": v.beneficiary.to_string(),
                "total_amount": v.total_amount.to_string(),
                "released_amount": v.released_amount.to_string(),
                "start_time": v.start_time.to_string(),
                "cliff_seconds": v.cliff_seconds.to_string(),
                "duration_seconds": v.duration_seconds.to_string(),
                "bump": v.bump,
            }),
            "{at}"
        );
        for (role, account) in [("token_vault", &entry.token_vault), ("vesting_vault", &entry.vesting_vault)] {
            let t = spl_token::state::Account::unpack(&account.account.data).unwrap_or_else(|e| panic!("{at}: {role}: {e}"));
            assert_eq!(
                entry.decoded[role],
                json!({
                    "mint": t.mint.to_string(),
                    "owner": t.owner.to_string(),
                    "amount": t.amount.to_string(),
                    "delegate": key(t.delegate),
                    "state": t.state as u8,
                    "is_native": match t.is_native { COption::Some(n) => json!(n.to_string()), COption::None => Value::Null },
                    "delegated_amount": t.delegated_amount.to_string(),
                    "close_authority": key(t.close_authority),
                }),
                "{at}: {role}"
            );
        }
        let m = spl_token::state::Mint::unpack(&entry.mint_account.account.data).unwrap();
        assert_eq!(
            entry.decoded["mint"],
            json!({
                "mint_authority": key(m.mint_authority),
                "supply": m.supply.to_string(),
                "decimals": m.decimals,
                "is_initialized": m.is_initialized,
                "freeze_authority": key(m.freeze_authority),
            }),
            "{at}: mint"
        );
    }
}

#[test]
fn every_account_sits_where_create_curve_puts_it() {
    let s = snapshot();
    assert_eq!(s.global_config.address, pda(&[CONFIG_SEED]).0);
    assert_eq!(s.global_config.account.owner, pepper_curve::ID);
    assert_eq!(s.rent.address, sysvar::rent::ID);
    assert_eq!(s.rent.account.owner, sysvar::ID);
    for entry in &s.curves {
        let at = entry.mint;
        let mint = entry.mint.as_ref();
        let curve = Curve::try_deserialize(&mut &entry.curve.account.data[..]).unwrap();
        assert_eq!(curve.mint, entry.mint, "{at}");
        let (curve_pda, curve_bump) = pda(&[CURVE_SEED, mint]);
        assert_eq!((entry.curve.address, curve.bump), (curve_pda, curve_bump), "{at}: the curve");
        let (vault_pda, vault_bump) = pda(&[SOL_VAULT_SEED, mint]);
        assert_eq!((entry.sol_vault.address, curve.sol_vault_bump), (vault_pda, vault_bump), "{at}: the SolVault");
        assert_eq!(entry.vesting.address, pda(&[VESTING_SEED, mint]).0, "{at}: the Vesting");
        assert_eq!(entry.vesting_vault.address, pda(&[VESTING_VAULT_SEED, mint]).0, "{at}: the vesting vault");
        assert_eq!(entry.token_vault.address, get_associated_token_address(&curve_pda, &entry.mint), "{at}: the token vault");
        assert_eq!(entry.mint_account.address, entry.mint, "{at}: the mint");
        for a in [&entry.curve, &entry.sol_vault, &entry.vesting] {
            assert_eq!(a.account.owner, pepper_curve::ID, "{at}: {} is the program's", a.address);
        }
        for a in [&entry.token_vault, &entry.vesting_vault, &entry.mint_account] {
            assert_eq!(a.account.owner, spl_token::ID, "{at}: {} is the token program's", a.address);
        }
        for a in entry.accounts() {
            assert!(!a.account.executable, "{at}: {}", a.address);
        }
        for vault in [&entry.token_vault, &entry.vesting_vault] {
            let t = spl_token::state::Account::unpack(&vault.account.data).unwrap();
            assert_eq!((t.mint, t.owner), (entry.mint, curve_pda), "{at}: {} is the curve's", vault.address);
        }
    }
}

#[test]
fn the_census_is_the_one_of_2026_10_23() {
    let s = snapshot();
    let config = GlobalConfig::try_deserialize(&mut &s.global_config.account.data[..]).unwrap();
    assert!(!config.paused, "GlobalConfig is paused");
    let curves: Vec<Curve> = s.curves.iter().map(|e| Curve::try_deserialize(&mut &e.curve.account.data[..]).unwrap()).collect();
    assert_eq!(curves.len(), CURVES);
    assert!(curves.iter().all(|c| c.phase == CurvePhase::Bonding), "a curve past Bonding");
    assert!(curves.iter().all(|c| !c.paused), "a paused curve");
    assert!(curves.iter().all(|c| c.graduated_at == 0));
    let real: u64 = curves.iter().map(|c| c.real_sol_reserves).sum();
    assert_eq!(real, REAL_SOL_RESERVES);
    assert_eq!(
        s.census,
        json!({
            "curves": CURVES,
            "bonding": CURVES,
            "graduating": 0,
            "graduated": 0,
            "paused": 0,
            "global_paused": false,
            "real_sol_reserves": REAL_SOL_RESERVES.to_string(),
        })
    );
    let mut mints: Vec<String> = s.curves.iter().map(|e| e.mint.to_string()).collect();
    let listed = mints.clone();
    mints.sort();
    assert_eq!(listed, mints, "the curves by mint");
}

/// What the migration relies on: the vaults hold at least what the curve owes (a gift only adds),
/// the reserve is payable above the vault's rent at the live rent, and no mint keeps a mint or
/// freeze authority (usdc-curve design §8: the mint and the holders' accounts never move).
#[test]
fn the_vaults_hold_what_each_curve_owes() {
    let s = snapshot();
    let rent = live_rent();
    for entry in &s.curves {
        let at = entry.mint;
        let curve = Curve::try_deserialize(&mut &entry.curve.account.data[..]).unwrap();
        let tokens = spl_token::state::Account::unpack(&entry.token_vault.account.data).unwrap();
        assert!(tokens.amount >= curve.real_token_reserves + curve.migration_token_reserves, "{at}: the token vault");
        let vesting = Vesting::try_deserialize(&mut &entry.vesting.account.data[..]).unwrap();
        let vested = spl_token::state::Account::unpack(&entry.vesting_vault.account.data).unwrap();
        assert!(vested.amount >= vesting.total_amount - vesting.released_amount, "{at}: the vesting vault");
        assert!(
            entry.sol_vault.account.lamports >= rent.minimum_balance(entry.sol_vault.account.data.len()) + curve.real_sol_reserves,
            "{at}: the SolVault pays its reserve above its rent"
        );
        let mint = spl_token::state::Mint::unpack(&entry.mint_account.account.data).unwrap();
        assert_eq!((mint.mint_authority, mint.freeze_authority), (COption::None, COption::None), "{at}: the mint's authorities");
        assert_eq!(mint.decimals, pepper_curve::constants::TOKEN_DECIMALS, "{at}");
    }
}

/// The live rent goes into LiteSVM as the sysvar mainnet serves (the bank's own Rent account
/// bytes equal the snapshot's), and every snapshot account loads; on LiteSVM's default rent five
/// curves would sit under the minimum.
#[test]
fn the_snapshot_loads_into_litesvm_on_the_live_rent() {
    let s = snapshot();
    let rent = live_rent();
    assert_eq!(rent.lamports_per_byte_year, LIVE_LAMPORTS_PER_BYTE_YEAR);
    assert_eq!(rent.exemption_threshold, LIVE_EXEMPTION_THRESHOLD);
    let mut svm = live_svm();
    load_snapshot(&mut svm, &s);
    assert_eq!(svm.get_account(&sysvar::rent::ID).unwrap().data, s.rent.account.data, "the Rent sysvar's bytes");
    let in_bank = svm.get_sysvar::<Rent>();
    assert_eq!(
        (in_bank.lamports_per_byte_year, in_bank.exemption_threshold, in_bank.burn_percent),
        (rent.lamports_per_byte_year, rent.exemption_threshold, rent.burn_percent)
    );
    // The figures the migration's rent top-up is written against (plan: 1 645 920 and 2 448 560).
    assert_eq!(svm.minimum_balance_for_rent_exemption(196), 1_645_920);
    assert_eq!(svm.minimum_balance_for_rent_exemption(354), 2_448_560);
    assert_eq!(svm.minimum_balance_for_rent_exemption(41), 858_520);
    assert_eq!(svm.minimum_balance_for_rent_exemption(165), 1_488_440);
    for entry in &s.curves {
        for a in entry.accounts() {
            assert_eq!(svm.get_account(&a.address).as_ref(), Some(&a.account), "{}: {} loaded", entry.mint, a.address);
            assert!(a.account.lamports >= rent.minimum_balance(a.account.data.len()), "{}: {} under the live rent", entry.mint, a.address);
        }
    }
    let default_minimum = Rent::default().minimum_balance(196);
    assert_eq!(default_minimum, 2_255_040);
    let under = s.curves.iter().filter(|e| e.curve.account.lamports < default_minimum).count();
    assert_eq!(under, CURVES_UNDER_THE_DEFAULT_RENT);
    let held: std::collections::BTreeSet<u64> = s.curves.iter().map(|e| e.curve.account.lamports).collect();
    assert_eq!(held.into_iter().collect::<Vec<_>>(), vec![1_645_920, 2_051_892, 2_255_040], "the three rents the curves hold");
}
