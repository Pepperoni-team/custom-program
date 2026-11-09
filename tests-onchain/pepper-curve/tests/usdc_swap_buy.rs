//! A USDC curve buy paid by a swap of the wallet's SOL, through the real Jupiter v6 over mainnet's
//! pools (phase 5.1's P11b; docs/solana/SPONSORED-TRANSACTIONS.md §3.5), run against the built program
//! under mainnet's rent (`migration_common::live_rent`).
//!
//! The bank is mainnet's at M2's capture (`tests/migration_capture`): Jupiter, Orca's Whirlpool,
//! Raydium's CLMM, Meteora's DLMM and p-token as mainnet runs them, under the upgradeable loader; every
//! account the three captured routes read, and the Clock of their slot; Circle's USDC the config's
//! mint. The messages are the server's own (`migration-fixtures/swap_buy_messages.json`, written by
//! `scripts/solana/usdc-swap-buy-fixtures.ts` with the builders `buildUsdcSwapBuy` composes, and held
//! there to the policy the co-sign runs): the capture's route of 2.614777778 SOL into USDC, retargeted
//! to a throwaway buyer, at the swap-buy's 0.5%, on a lookup table of the route's pool accounts as
//! Jupiter's hold them; the buyer's wrapped SOL opened, wrapped, synced and closed back to it around the
//! route; then `buy_exact_in` with the USDC the route guarantees and the top-up, the commission's and the two
//! openings'. The heaviest buy a SOL holder makes: its token account and its USDC account both opened by the
//! sponsor, both the buyer's to keep (Giuseppe's rule, Rick 2026-11-05: the wallet pays every account a curve
//! trade opens).
//!
//! - Over each venue it lands, signed by the throwaway sponsor (the fee payer) and buyer: the buyer
//!   spends exactly the route's input in SOL and nothing else, its wrapped-SOL account gone; the curve's
//!   `usdc_vault` receives the curve's share of the buy, the fee account its commission (the 1% alone from $10,
//!   the flat $0.15 under it: each of the capture's venues past it) and the two openings, the buyer
//!   the tokens the server's quote said, and keeps what the route paid beyond its guarantee as USDC; the
//!   sponsor pays the network fee and fronts the two deposits, both accounts the buyer's, no close
//!   authority handed on. The
//!   compute units stay within the swap-buy's limit and the wire within the packet.
//! - A route asked for more than the pool pays is Jupiter's own refusal (6001), the whole transaction
//!   with it: the buyer keeps its SOL, holds no token, and the curve receives nothing.

mod common;
mod migration_capture;
mod migration_common;

use anchor_lang::solana_program::program_option::COption;
use anchor_spl::associated_token::get_associated_token_address as ata;
use anchor_spl::token::spl_token;
use common::*;
use litesvm::LiteSVM;
use migration_capture::*;
use migration_common::*;
use pepper_curve_math::CurveState;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::Value;
use solana_sdk::{
    account::Account,
    hash::{hash, Hash},
    instruction::{CompiledInstruction, InstructionError},
    message::{v0, v0::MessageAddressTableLookup, MessageHeader, VersionedMessage},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    signer::keypair::keypair_from_seed,
    transaction::{TransactionError, VersionedTransaction},
};
use std::str::FromStr;

const FIXTURE: &str = "swap_buy_messages.json";
/// The fixture `usdc-swap-buy-fixtures.ts` wrote from the capture of 2026-10-23: a rewrite moves it on purpose
/// (2026-11-06: the network's $0.05 beside the 1% on every curve trade, so each venue's top-up is its
/// openings and that $0.05; 2026-11-05: the higher of 1% and $0.10, its openings alone).
const FIXTURE_SHA256: &str = "4b13b1c64a4ff6f821d738c41acfa8135c70a82de491fce4d4c7e813d998c49c";
const LOOKUP_TABLE_PROGRAM: Pubkey = Pubkey::from_str_const("AddressLookupTab1e1111111111111111111111111");
/// A transaction's ceiling on the wire, its signatures included.
const PACKET_DATA_SIZE: usize = 1_232;
/// A classic SPL token account's size, whose rent each deposit is.
const TOKEN_ACCOUNT_LEN: usize = 165;

fn pubkey_of(v: &Value) -> Pubkey {
    Pubkey::from_str(v.as_str().unwrap()).unwrap()
}

fn base64_of(v: &Value) -> Vec<u8> {
    STANDARD.decode(v.as_str().unwrap()).unwrap()
}

fn u64_of(v: &Value) -> u64 {
    v.as_str().unwrap().parse().unwrap()
}

fn indexes_of(v: &Value) -> Vec<u8> {
    v.as_array().unwrap().iter().map(|x| u8::try_from(x.as_u64().unwrap()).unwrap()).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The server's v0 message as the fixture holds it, held to its bytes (its blockhash all zeros).
fn message_of(v: &Value) -> v0::Message {
    let h = indexes_of(&v["header"]);
    let message = v0::Message {
        header: MessageHeader { num_required_signatures: h[0], num_readonly_signed_accounts: h[1], num_readonly_unsigned_accounts: h[2] },
        account_keys: v["account_keys"].as_array().unwrap().iter().map(pubkey_of).collect(),
        recent_blockhash: Hash::default(),
        instructions: v["instructions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|ix| CompiledInstruction {
                program_id_index: u8::try_from(ix["program_id_index"].as_u64().unwrap()).unwrap(),
                accounts: indexes_of(&ix["accounts"]),
                data: base64_of(&ix["data"]),
            })
            .collect(),
        address_table_lookups: v["address_table_lookups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| MessageAddressTableLookup {
                account_key: pubkey_of(&l["account_key"]),
                writable_indexes: indexes_of(&l["writable_indexes"]),
                readonly_indexes: indexes_of(&l["readonly_indexes"]),
            })
            .collect(),
    };
    assert_eq!(VersionedMessage::V0(message.clone()).serialize(), base64_of(&v["serialized"]), "the server's message, byte for byte");
    message
}

/// A lookup table's account as the program writes it: the 56-byte meta (`LookupTable`, never
/// deactivated, last extended at slot 0 so every address is active), then the addresses.
fn put_lookup_table(svm: &mut LiteSVM, address: Pubkey, authority: &Pubkey, addresses: &[Pubkey]) {
    let mut data = Vec::with_capacity(56 + 32 * addresses.len());
    data.extend_from_slice(&1u32.to_le_bytes());
    data.extend_from_slice(&u64::MAX.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes());
    data.push(0);
    data.push(1);
    data.extend_from_slice(authority.as_ref());
    data.extend_from_slice(&0u16.to_le_bytes());
    assert_eq!(data.len(), 56);
    for a in addresses {
        data.extend_from_slice(a.as_ref());
    }
    let lamports = svm.minimum_balance_for_rent_exemption(data.len());
    svm.set_account(address, Account { lamports, data, owner: LOOKUP_TABLE_PROGRAM, executable: false, rent_epoch: 0 }).unwrap();
}

struct Fixture {
    json: Value,
    sponsor: Keypair,
    buyer: Keypair,
}

fn fixture() -> Fixture {
    let bytes = std::fs::read(migration_fixture_path(FIXTURE)).expect("scripts/solana/usdc-swap-buy-fixtures.ts");
    assert_eq!(hex(&hash(&bytes).to_bytes()), FIXTURE_SHA256, "the fixture this suite pins");
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["routes_sha256"].as_str().unwrap(), ROUTES_SHA256, "built from this capture");
    let seed = |name: &str| [u8::try_from(json[name].as_u64().unwrap()).unwrap(); 32];
    let sponsor = keypair_from_seed(&seed("sponsor_seed")).unwrap();
    let buyer = keypair_from_seed(&seed("buyer_seed")).unwrap();
    assert_eq!((sponsor.pubkey(), buyer.pubkey()), (pubkey_of(&json["sponsor"]), pubkey_of(&json["buyer"])));
    Fixture { json, sponsor, buyer }
}

/// Mainnet's bank at the capture's slot, the fixture's curve and fee account in it: the programs as
/// mainnet runs them, loaded before the capture's Clock moves the slot on; the capture's accounts;
/// Circle's USDC the config's mint, the fee wallet's USDC account the fixture's; a fresh equity
/// `CurveUsdc` at the fixture's mint, its launch long past; the sponsor funded, the buyer holding SOL
/// alone (no USDC account, no token account).
fn bench(cap: &Capture, fx: &Fixture, buyer_lamports: u64) -> Env {
    let mut env = Env::new();
    env.svm.set_sysvar(&live_rent());
    load_mainnet_programs(&mut env.svm, TokenProgram::Mainnet);
    load_capture(&mut env.svm, cap);
    assert_eq!(env.svm.get_sysvar::<solana_sdk::clock::Clock>().slot, CAPTURE_SLOT);
    assert_eq!(env.svm.get_sysvar::<solana_sdk::clock::Clock>().unix_timestamp, fx.json["now"].as_i64().unwrap());
    env.usdc.mint = USDC_MINT;
    assert_eq!(pubkey_of(&fx.json["usdc_mint"]), USDC_MINT);

    let fee_owner = pubkey_of(&fx.json["fee_owner"]);
    let fee_account = pubkey_of(&fx.json["fee_usdc_account"]);
    assert_eq!(fee_account, ata(&fee_owner, &USDC_MINT));
    env.put_token_account(fee_account, USDC_MINT, fee_owner, 0, COption::None);
    let mut config = env.usdc_config();
    config.fee_usdc_account = fee_account;
    env.put_usdc_config(&config);

    let c = &fx.json["curve"];
    let state = CurveState {
        virtual_reserve: u64_of(&c["virtual_usdc_reserves"]),
        virtual_token_reserves: u64_of(&c["virtual_token_reserves"]),
        real_reserve: u64_of(&c["real_usdc_reserves"]),
        real_token_reserves: u64_of(&c["real_token_reserves"]),
        creator_split_bps: 0,
        fanex_split_bps: 0,
        creator_cap: 0,
        fanex_cap: 0,
        creator_received: 0,
        fanex_received: 0,
        graduation_threshold: u64_of(&c["graduation_threshold_usdc"]),
        created_at: fx.json["created_at"].as_i64().unwrap(),
    };
    let curve = env.curve_usdc(pubkey_of(&fx.json["mint"]), &state);
    env.put_curve_usdc(&curve);

    env.svm.airdrop(&fx.sponsor.pubkey(), 10 * SOL).unwrap();
    env.svm.airdrop(&fx.buyer.pubkey(), buyer_lamports).unwrap();
    env
}

/// The fixture's message for venue `v`, its table in the bank, its blockhash the bank's, signed by the sponsor and the buyer.
fn signed(env: &mut Env, fx: &Fixture, v: &Value, edit: impl FnOnce(&mut v0::Message)) -> VersionedTransaction {
    let table = &v["table"];
    let addresses: Vec<Pubkey> = table["addresses"].as_array().unwrap().iter().map(pubkey_of).collect();
    put_lookup_table(&mut env.svm, pubkey_of(&table["address"]), &pubkey_of(&fx.json["fee_owner"]), &addresses);
    let mut message = message_of(&v["message"]);
    message.recent_blockhash = env.svm.latest_blockhash();
    edit(&mut message);
    VersionedTransaction::try_new(VersionedMessage::V0(message), &[&fx.sponsor, &fx.buyer]).expect("signed")
}

fn amount_or_none(env: &Env, address: &Pubkey) -> Option<u64> {
    env.svm.get_account(address).filter(|a| a.owner == spl_token::ID && !a.data.is_empty()).map(|_| env.token_amount(address))
}

#[test]
fn a_buy_paid_by_the_wallet_s_sol_lands_over_each_venue_the_curve_receiving_usdc() {
    let cap = capture();
    let fx = fixture();
    let venues = fx.json["venues"].as_array().unwrap().clone();
    assert_eq!(venues.len(), VENUES.len());
    let mint = pubkey_of(&fx.json["mint"]);
    let buyer = fx.buyer.pubkey();
    let sponsor = fx.sponsor.pubkey();
    for (i, v) in venues.iter().enumerate() {
        assert_eq!(v["venue"].as_str().unwrap(), VENUES[i].label);
        assert_eq!(u64_of(&v["quoted_out_usdc"]), VENUES[i].quoted_out, "{}: the capture's quote", VENUES[i].label);
        let in_lamports = u64_of(&v["in_lamports"]);
        assert_eq!(in_lamports, RESERVE_LAMPORTS, "the route the capture sells");
        // The buyer holds the route's input, its wrapped-SOL account's rent and one SOL more.
        let mut env = bench(&cap, &fx, in_lamports + 3 * SOL);
        let rent = env.svm.minimum_balance_for_rent_exemption(TOKEN_ACCOUNT_LEN);
        let usdc_vault = usdc_vault_pda(&mint);
        let fee_account = pubkey_of(&fx.json["fee_usdc_account"]);
        let buyer_wsol = ata(&buyer, &WSOL_MINT);
        let buyer_usdc = ata(&buyer, &USDC_MINT);
        let buyer_token = ata(&buyer, &mint);
        assert!(amount_or_none(&env, &buyer_usdc).is_none() && amount_or_none(&env, &buyer_token).is_none(), "a wallet holding SOL alone");
        let (buyer_before, sponsor_before) = (env.lamports(&buyer), env.lamports(&sponsor));
        let (vault_before, fee_before) = (env.token_amount(&usdc_vault), env.token_amount(&fee_account));

        let tx = signed(&mut env, &fx, v, |_| {});
        // Two signatures and their compact length, then the message.
        let wire = 1 + 2 * 64 + tx.message.serialize().len();
        assert_eq!(wire as u64, v["bytes"].as_u64().unwrap(), "{}: the size the server measured", VENUES[i].label);
        assert!(wire <= PACKET_DATA_SIZE);
        let landed = env
            .svm
            .send_transaction(tx)
            .unwrap_or_else(|e| panic!("{}: {:?}\n{}", VENUES[i].label, e.err, e.meta.pretty_logs()));

        // The buyer: exactly the route's input in SOL, nothing else; its wrapped SOL closed back to it.
        assert_eq!(buyer_before - env.lamports(&buyer), in_lamports, "{}: SOL is spent only as the swap's input", VENUES[i].label);
        assert!(amount_or_none(&env, &buyer_wsol).is_none(), "{}: the wrapped-SOL account closed", VENUES[i].label);
        // The tokens the server's quote said, within the buyer's tolerance.
        let tokens = env.token_amount(&buyer_token);
        assert_eq!(tokens, u64_of(&v["tokens_out"]), "{}: the tokens quoted", VENUES[i].label);
        assert!(tokens >= u64_of(&v["min_tokens_out"]));
        // The curve receives its share of the USDC the route paid; the fee account the 1% and its top-up.
        let curve_in = env.token_amount(&usdc_vault) - vault_before;
        assert_eq!(curve_in, u64_of(&v["curve_usdc"]), "{}: the curve receives USDC", VENUES[i].label);
        assert!(curve_in > 0);
        assert_eq!(env.token_amount(&fee_account) - fee_before, u64_of(&v["platform_fee_usdc"]) + u64_of(&v["top_up_usdc"]));
        // What the route paid beyond what it guaranteed stays the buyer's, as USDC: the quote at the slot, exactly.
        assert_eq!(env.token_amount(&buyer_usdc), u64_of(&v["quoted_out_usdc"]) - u64_of(&v["total_usdc"]), "{}: the change", VENUES[i].label);
        // The sponsor: the network fee and the two deposits it fronts. Both accounts are the buyer's, no close
        // authority handed on, their openings carried by the top-up (Giuseppe's rule, Rick 2026-11-05).
        let deposits = 2 * rent;
        let spent = sponsor_before - env.lamports(&sponsor);
        assert!(spent >= deposits, "{}: the sponsor paid the deposits", VENUES[i].label);
        let fee = spent - deposits;
        assert!(fee <= v["fee_lamports"].as_u64().unwrap(), "{}: a fee within the one priced", VENUES[i].label);
        for account in [buyer_token, buyer_usdc] {
            assert_eq!(env.token_account(&account).close_authority, COption::None, "{}: the account the buyer paid for is the buyer's", VENUES[i].label);
        }
        assert!(
            u64_of(&v["top_up_usdc"]) >= u64_of(&v["openings_usdc"]) && u64_of(&v["openings_usdc"]) > 0,
            "{}: the top-up carries the two openings",
            VENUES[i].label
        );
        assert!(landed.compute_units_consumed <= fx.json["compute_unit_limit"].as_u64().unwrap());
        println!(
            "{}: the server's swap-buy, {} bytes ({} accounts looked up), {} compute units, fee {} lamports; {} lamports of SOL into {} µUSDC, {} to the curve, {} tokens out",
            VENUES[i].label,
            wire,
            v["looked_up"],
            landed.compute_units_consumed,
            fee,
            in_lamports,
            v["quoted_out_usdc"].as_str().unwrap(),
            curve_in,
            tokens
        );
    }
}

#[test]
fn a_route_asked_for_more_than_the_pool_pays_fails_whole_the_buyer_keeping_its_sol() {
    let cap = capture();
    let fx = fixture();
    let v = fx.json["venues"][0].clone();
    let in_lamports = u64_of(&v["in_lamports"]);
    let mut env = bench(&cap, &fx, in_lamports + 3 * SOL);
    let mint = pubkey_of(&fx.json["mint"]);
    let buyer = fx.buyer.pubkey();
    let buyer_before = env.lamports(&buyer);
    let vault_before = env.token_amount(&usdc_vault_pda(&mint));
    // The route's quote 1% above what the pool pays: its minimum, the quote less 0.5%, is out of reach.
    let mut route_index = None;
    let tx = signed(&mut env, &fx, &v, |m| {
        let jupiter = m.account_keys.iter().position(|k| *k == JUPITER_V6_ID).unwrap();
        let (index, ix) = m.instructions.iter_mut().enumerate().find(|(_, ix)| usize::from(ix.program_id_index) == jupiter).unwrap();
        let quoted = u64::from_le_bytes(ix.data[QUOTED_OUT].try_into().unwrap());
        ix.data[QUOTED_OUT].copy_from_slice(&(quoted + quoted / 100).to_le_bytes());
        route_index = Some(index);
    });
    let failed = env.svm.send_transaction(tx).expect_err("the route cannot pay its quote");
    let expected = u8::try_from(route_index.unwrap()).unwrap();
    assert_eq!(failed.err, TransactionError::InstructionError(expected, InstructionError::Custom(JUPITER_SLIPPAGE_TOLERANCE_EXCEEDED)));
    assert_eq!(env.lamports(&buyer), buyer_before, "the buyer keeps its SOL");
    assert!(amount_or_none(&env, &ata(&buyer, &mint)).is_none(), "no token account, no token");
    assert_eq!(env.token_amount(&usdc_vault_pda(&mint)), vault_before, "the curve receives nothing");
}
