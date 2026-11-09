//! The third-party fixtures, pinned (docs/solana/PEPPER-CURVE-PROGRAM.md §13,
//! usdc-curve design §10.11; Rick, 2026-10-22).
//!
//! `scripts/solana/dump-litesvm-fixtures.sh` reads them from mainnet and writes
//! `fixtures/SHA256SUMS`. The pins below are written by hand: a re-dump that
//! brings another binary (an upgrade on mainnet) stays red here until someone
//! reads what changed and moves the pin on purpose. The create-pool fee
//! receiver's balance moves with every pool created on mainnet, so its pin
//! moves at every re-dump.

mod common;

use common::*;
use litesvm::LiteSVM;
use solana_sdk::{
    hash::hash,
    instruction::{Instruction, InstructionError},
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};

/// Each fixture's sha256, dumped on 2026-10-22 (mainnet slots 460113619 to
/// 460113637).
const PINNED: [(&str, &str); 10] = [
    // Metaplex Token Metadata, last deployed at slot 380725176, immutable.
    ("mpl_token_metadata.so", "31f0a627dba051a938de650464e55cc5397a4be0fd496929c1f9cf02fe5e9011"),
    // Raydium CPMM, last deployed at slot 452376460.
    ("raydium_cpmm.so", "b6dbea8cefc2e7d490775ce6115e34a2ba480c0853c359e0bb4043b9381ef09a"),
    // The CPMM's `amm_config` index 1: trade fee 10 000 (1%), create-pool fee 0.15 SOL.
    ("raydium_cpmm_amm_config.json", "253df3fb506d295a6cf1b13f0e63a17c90a583ee606262b1286e1d373453449b"),
    // The CPMM's create-pool fee receiver, a wrapped-SOL token account.
    ("raydium_cpmm_create_pool_fee.json", "a2cb3116e839d9a6c41ab6299a9b36ae7f14e9074587d0d82226ac109d0ede69"),
    // Raydium's LP lock, last deployed at slot 362025476.
    ("raydium_lp_lock.so", "efd657a986796e1b204ebae198e691d4137d5f89a401693b83f7fb1a142f802d"),
    // The migration's (upgrade N+1, `migration_programs.rs`), dumped on 2026-10-23, finalized
    // before mainnet slot 460474650. Jupiter v6, re-dumped on 2026-10-30 (finalized mainnet slots
    // 463171612 to 463171696): last deployed at slot 462802085, the same 2892224 bytes, its ELF
    // 2047544 (2036392 in the dump of slot 459479624, sha256 0394bfa4…4d30), one `Swap` variant
    // appended (`BisonFiMarketBackedV2`, 203). The venues below read back unchanged.
    ("jupiter_v6.so", "53d53f447f594158a259cbdca1f5e613caf40bc14ce759ffd5953580dc2af8dd"),
    // Orca's Whirlpool, last deployed at slot 457152296.
    ("orca_whirlpool.so", "c422b22f3eefe86dd55878a4f505b9c68c26e241dc96af1573ca032b24b8ee3f"),
    // Raydium's CLMM, last deployed at slot 459932378.
    ("raydium_clmm.so", "e9ae00f7bdfa3b104595e68b3e328d108029e16af3ba4fb1f3968210178ffc5c"),
    // Meteora's DLMM, last deployed at slot 423977638.
    ("meteora_dlmm.so", "d296c6771cec945601027613ca637c1be6721044c5859009d41c453477844c1f"),
    // The token program mainnet runs at Tokenkeg's address, p-token, deployed at slot 419472000,
    // immutable, dumped on 2026-10-23 for the migration route's compute units (`migration_route.rs`).
    ("p_token.so", "8190d3f7ceb6cb7a7a8d8924bff89f9f611e15ce1f806f2b6237f3311a98f697"),
];

/// SHA-256: Solana's `hash` is SHA-256.
fn sha256_hex(bytes: &[u8]) -> String {
    hash(bytes).to_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn each_fixture_matches_its_pinned_sha256() {
    for (name, pinned) in PINNED {
        let path = fixture_path(name);
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(sha256_hex(&bytes), pinned, "{name} is not the binary pinned here");
    }
}

#[test]
fn the_sums_file_lists_exactly_the_pinned_hashes() {
    let sums = std::fs::read_to_string(fixture_path("SHA256SUMS")).unwrap();
    let listed: Vec<(String, String)> = sums
        .lines()
        .map(|line| {
            let (sha, name) = line.split_once("  ").unwrap_or_else(|| panic!("not a sha256sum line: {line:?}"));
            (name.to_string(), sha.to_string())
        })
        .collect();
    let mut pinned: Vec<(String, String)> = PINNED.iter().map(|(n, s)| (n.to_string(), s.to_string())).collect();
    pinned.sort();
    assert_eq!(listed, pinned);
}

#[test]
fn every_file_in_the_folder_is_pinned() {
    for entry in std::fs::read_dir(fixtures_dir()).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        assert!(
            name == "SHA256SUMS" || PINNED.iter().any(|(pinned, _)| *pinned == name),
            "{name} is in fixtures/ without a pin"
        );
    }
}

/// Both Raydium binaries run in this LiteSVM, not only load: an instruction
/// they do not know reaches their own code, which refuses it with a custom
/// error (Anchor's `InstructionFallbackNotFound`), where a binary the runtime
/// could not load fails before any code runs.
#[test]
fn the_raydium_programs_run_in_litesvm() {
    for (program_id, name) in [(RAYDIUM_CPMM_ID, RAYDIUM_CPMM_SO), (RAYDIUM_LP_LOCK_ID, RAYDIUM_LP_LOCK_SO)] {
        let mut svm = LiteSVM::new();
        load_fixture_program(&mut svm, program_id, name);
        let payer = Keypair::new();
        svm.airdrop(&payer.pubkey(), SOL).unwrap();
        let unknown = Instruction { program_id, accounts: vec![], data: vec![0; 8] };
        let tx = Transaction::new_signed_with_payer(&[unknown], Some(&payer.pubkey()), &[&payer], svm.latest_blockhash());
        let err = svm.send_transaction(tx).expect_err("an unknown instruction is refused").err;
        assert!(
            matches!(err, TransactionError::InstructionError(0, InstructionError::Custom(_))),
            "{name}: {err:?}"
        );
    }
}
