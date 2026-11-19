//! Pepper's own messages on MetaDAO's real launchpad v0.7 (docs/solana/EXTERNAL-RAISES.md §21).
//!
//! The program is mainnet's, dumped by `scripts/solana/dump-launchpad-fixture.sh` and pinned below by hand: a re-dump
//! that brings another binary (an upgrade on mainnet) stays red until someone reads what changed and moves the pin on
//! purpose. The launches, their vaults and records, and every message are written by Pepper's TypeScript
//! (`lib/external-raises/adapters/metadao-litesvm-fixture.ts`, held equal to the committed `scenarios.json` by its
//! test): the commit message the co-sign route builds, the claim and refund messages the settlement cron builds, each
//! with the figures Pepper's own math says the program must move. So this suite fails the day the program refuses a
//! message Pepper sends, or moves another amount than Pepper's math, in the commit or in a delivery.

use std::{fs, path::PathBuf, str::FromStr};

use base64::{engine::general_purpose::STANDARD, Engine};
use litesvm::LiteSVM;
use serde::Deserialize;
use solana_sdk::{
    account::Account,
    clock::Clock,
    hash::hash,
    message::VersionedMessage,
    pubkey::Pubkey,
    signature::Keypair,
    transaction::{TransactionError, VersionedTransaction},
};

/// The v0.7 program's ProgramData after its header, dumped on 2026-11-15: last deployed at mainnet slot 409688798,
/// 800000 bytes.
const PINNED_SHA256: &str = "46827be9884c02165d415d52f5babaa37f974921721978e2c642bd4ab4e8220c";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    program_id: String,
    keys: Keys,
    scenarios: Vec<Scenario>,
}

#[derive(Deserialize)]
struct Keys {
    sponsor: Vec<u8>,
    funder: Vec<u8>,
}

#[derive(Deserialize)]
struct Scenario {
    name: String,
    accounts: Vec<FixtureAccount>,
    steps: Vec<Step>,
}

#[derive(Deserialize)]
struct FixtureAccount {
    address: String,
    owner: String,
    lamports: u64,
    data: String,
}

#[derive(Deserialize)]
struct Step {
    what: String,
    clock: i64,
    message: String,
    signers: Vec<String>,
    outcome: String,
    tokens: Vec<TokenAmount>,
    record: Option<Record>,
}

#[derive(Deserialize)]
struct TokenAmount {
    address: String,
    amount: String,
}

#[derive(Deserialize)]
struct Record {
    address: String,
    committed: String,
    approved: String,
    claimed: bool,
    refunded: bool,
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

/// SHA-256: Solana's `hash` is SHA-256.
fn sha256_hex(bytes: &[u8]) -> String {
    hash(bytes).to_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

fn load() -> Fixture {
    let text = fs::read_to_string(fixtures_dir().join("scenarios.json")).expect("fixtures/scenarios.json: run scripts/solana/write-launchpad-fixture.ts");
    serde_json::from_str(&text).expect("fixtures/scenarios.json does not parse")
}

fn key(text: &str) -> Pubkey {
    Pubkey::from_str(text).unwrap_or_else(|_| panic!("not a key: {text}"))
}

fn keypair(bytes: &[u8]) -> Keypair {
    #[allow(deprecated)]
    Keypair::from_bytes(bytes).expect("a 64-byte test key")
}

/// A token account's amount: the SPL layout's u64 at offset 64.
fn token_amount(svm: &LiteSVM, address: &str) -> u64 {
    let account = svm.get_account(&key(address)).unwrap_or_else(|| panic!("no token account {address}"));
    u64::from_le_bytes(account.data[64..72].try_into().unwrap())
}

/// A `FundingRecord` as v0.7 lays it out: the 8-byte discriminator, `pda_bump`, `funder`, `launch`, then
/// `committed_amount` at 73, `is_tokens_claimed` at 81, `is_usdc_refunded` at 82, `approved_amount` at 83.
fn check_record(svm: &LiteSVM, expected: &Record, label: &str) {
    let data = svm.get_account(&key(&expected.address)).unwrap_or_else(|| panic!("{label}: no funding record")).data;
    let u64_at = |at: usize| u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
    assert_eq!(u64_at(73), expected.committed.parse::<u64>().unwrap(), "{label}: committed_amount");
    assert_eq!(data[81] == 1, expected.claimed, "{label}: is_tokens_claimed");
    assert_eq!(data[82] == 1, expected.refunded, "{label}: is_usdc_refunded");
    assert_eq!(u64_at(83), expected.approved.parse::<u64>().unwrap(), "{label}: approved_amount");
}

fn run(prefix: &str) {
    let fixture = load();
    let scenario = fixture.scenarios.iter().find(|s| s.name.starts_with(prefix)).unwrap_or_else(|| panic!("no scenario «{prefix}»"));
    let program_id = key(&fixture.program_id);
    let mut svm = LiteSVM::new().with_log_bytes_limit(None);
    svm.add_program_from_file(program_id, fixtures_dir().join("launchpad_v0_7.so")).expect("the launchpad binary loads");
    for a in &scenario.accounts {
        let account = Account { lamports: a.lamports, data: STANDARD.decode(&a.data).unwrap(), owner: key(&a.owner), executable: false, rent_epoch: 0 };
        svm.set_account(key(&a.address), account).unwrap();
    }
    let sponsor = keypair(&fixture.keys.sponsor);
    let funder = keypair(&fixture.keys.funder);

    for (i, step) in scenario.steps.iter().enumerate() {
        let label = format!("{} · step {} ({})", scenario.name, i + 1, step.what);
        let mut clock: Clock = svm.get_sysvar();
        clock.unix_timestamp = step.clock;
        svm.set_sysvar(&clock);
        // A new blockhash per step, so a message sent twice is a new transaction, never «already processed».
        svm.expire_blockhash();
        let mut message: VersionedMessage = bincode::deserialize(&STANDARD.decode(&step.message).unwrap()).expect("a v0 message");
        let last = message.instructions().len() - 1;
        message.set_recent_blockhash(svm.latest_blockhash());
        let signers: Vec<&Keypair> = step
            .signers
            .iter()
            .map(|name| match name.as_str() {
                "sponsor" => &sponsor,
                "funder" => &funder,
                other => panic!("{label}: unknown signer {other}"),
            })
            .collect();
        let tx = VersionedTransaction::try_new(message, &signers).expect("the named keys sign the message");
        let result = svm.send_transaction(tx);
        match step.outcome.as_str() {
            "ok" => {
                if let Err(failed) = &result {
                    panic!("{label}: refused with {:?}\n{}", failed.err, failed.meta.logs.join("\n"));
                }
            }
            "refused" => match &result {
                // Refused by the launchpad's own instruction, the last: never by anything before it.
                Err(failed) => match &failed.err {
                    TransactionError::InstructionError(at, _) => assert_eq!(*at as usize, last, "{label}: refused by instruction {at}, not the launchpad's"),
                    other => panic!("{label}: refused before the program ran: {other:?}"),
                },
                Ok(_) => panic!("{label}: the program took it"),
            },
            other => panic!("{label}: unknown outcome {other}"),
        }
        for t in &step.tokens {
            assert_eq!(token_amount(&svm, &t.address), t.amount.parse::<u64>().unwrap(), "{label}: the amount in {}", t.address);
        }
        if let Some(record) = &step.record {
            check_record(&svm, record, &label);
        }
    }
}

#[test]
fn the_binary_is_the_one_pinned() {
    let bytes = fs::read(fixtures_dir().join("launchpad_v0_7.so")).expect("fixtures/launchpad_v0_7.so: run scripts/solana/dump-launchpad-fixture.sh");
    assert_eq!(sha256_hex(&bytes), PINNED_SHA256, "another binary: read what changed on mainnet before moving the pin");
}

#[test]
fn a_live_launch_takes_the_commit_message_and_refuses_it_past_its_window() {
    run("live:");
}

#[test]
fn a_complete_launch_delivers_pepper_s_allocation_and_refund_once() {
    run("complete:");
}

#[test]
fn a_refunding_launch_sends_the_whole_commitment_back_once() {
    run("refunding:");
}
