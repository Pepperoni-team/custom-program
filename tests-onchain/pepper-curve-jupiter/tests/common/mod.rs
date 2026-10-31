//! The parity suite's harness (USDC curve design §10.5), shared by its two files.
//!
//! - **The kit's own check**, `assert_pool_parity`: the SDK's `quote.out_amount` against what the
//!   program delivers to the destination, in LiteSVM, the user's source funded with exactly the
//!   amount asked.
//! - **What this suite adds**, [`assert_swaps_move_their_quote`]: the same swap again, the source's
//!   change against `quote.in_amount` (a buy partly filled at graduation charges less than it was
//!   given), the fee and creator accounts against `fee_amount`, the two vaults, and the curve's
//!   counters against the math crate's `after`, with the slippage bound at the quote itself.
//!
//! The program is the one built from this tree: the kit loads `tests/fixtures/pepper.so`
//! (`Amm::label()` lowercased), which `scripts/ci-onchain-tests.sh` copies there from its build.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use jupiter_amm_interface::{
    Amm, AmmContext, ClockRef, FeeMode, KeyedAccount, QuoteParams, SwapAndAccountMetas, SwapMode,
    SwapParams,
};
use jupiter_amm_test_kit::{PoolSnapshot, PoolTest};
use litesvm::LiteSVM;
use pepper_curve_jupiter::layout::{
    associated_token_address, find_curve_address, find_usdc_vault_address, CurveAddresses,
    CurveUsdc, UsdcConfig, ASSOCIATED_TOKEN_PROGRAM_ID, PHASE_BONDING, PROGRAM_ID,
    TOKEN_PROGRAM_ID, USDC_CONFIG_ADDRESS, USDC_CONFIG_SEED,
};
use pepper_curve_jupiter::quote::Direction;
use pepper_curve_jupiter::swap::direction_of;
use pepper_curve_jupiter::{swap_instruction_data, PepperCurveAmm};
use solana_account::Account;
use solana_clock::Clock;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::{pubkey, Pubkey};
use solana_signer::Signer;
use solana_transaction::Transaction;

pub const CLOCK_SYSVAR: Pubkey = pubkey!("SysvarC1ock11111111111111111111111111111111");
const SYSVAR_OWNER: Pubkey = pubkey!("Sysvar1111111111111111111111111111111111111");
const SYSTEM_PROGRAM: Pubkey = pubkey!("11111111111111111111111111111111");
const JUPITER_PROGRAM_ID: Pubkey = pubkey!("JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4");

/// The program the kit loads, by its convention: `tests/fixtures/<label>.so`.
pub const PROGRAM_SO: &str = "tests/fixtures/pepper.so";

const VECTORS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../programs/pepper_curve_math/vectors/curve-v1.json"
);

/// The kit's encoder: the instruction's data from the `Placeholder` the SDK returned, with no
/// minimum (asserting the exact amount out is the kit's job).
pub fn encode(swap: &jupiter_amm_interface::Swap, in_amount: u64) -> Vec<u8> {
    swap_instruction_data(swap, in_amount, 0).expect("a Pepper swap")
}

// ── The golden vectors ───────────────────────────────────────────────────────

pub fn vector_cases() -> Vec<serde_json::Value> {
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(VECTORS).expect("the golden vectors"))
            .expect("the vectors parse");
    json["cases"].as_array().expect("cases").clone()
}

pub fn vector(name: &str) -> serde_json::Value {
    vector_cases()
        .into_iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no vector named {name}"))
}

pub fn u64_of(value: &serde_json::Value) -> u64 {
    value
        .as_str()
        .expect("a u64 as a string")
        .parse()
        .expect("a u64")
}

// ── Accounts as the program and SPL Token store them ─────────────────────────

/// Rent-exempt lamports for `len` bytes at the default rent (3,480 lamports a byte-year, two years,
/// 128 bytes of overhead): accounts as a launch leaves them.
pub fn rent_exempt(len: usize) -> u64 {
    (128 + len as u64) * 3_480 * 2
}

fn account(owner: Pubkey, data: Vec<u8>) -> Account {
    Account {
        lamports: rent_exempt(data.len()),
        data,
        owner,
        executable: false,
        rent_epoch: 0,
    }
}

fn coption_key(data: &mut [u8], at: usize, key: Option<Pubkey>) {
    if let Some(key) = key {
        data[at..at + 4].copy_from_slice(&1u32.to_le_bytes());
        data[at + 4..at + 36].copy_from_slice(key.as_ref());
    }
}

/// A classic SPL mint (82 bytes).
pub fn mint_account(
    supply: u64,
    decimals: u8,
    mint_authority: Option<Pubkey>,
    freeze_authority: Option<Pubkey>,
) -> Account {
    let mut data = vec![0u8; 82];
    coption_key(&mut data, 0, mint_authority);
    data[36..44].copy_from_slice(&supply.to_le_bytes());
    data[44] = decimals;
    data[45] = 1;
    coption_key(&mut data, 46, freeze_authority);
    account(TOKEN_PROGRAM_ID, data)
}

/// A classic SPL token account (165 bytes), initialized, no delegate.
pub fn token_account(
    mint: Pubkey,
    owner: Pubkey,
    amount: u64,
    close_authority: Option<Pubkey>,
) -> Account {
    let mut data = vec![0u8; 165];
    data[..32].copy_from_slice(mint.as_ref());
    data[32..64].copy_from_slice(owner.as_ref());
    data[64..72].copy_from_slice(&amount.to_le_bytes());
    data[108] = 1;
    coption_key(&mut data, 129, close_authority);
    account(TOKEN_PROGRAM_ID, data)
}

pub fn token_amount(account: &Account) -> u64 {
    u64::from_le_bytes(account.data[64..72].try_into().unwrap())
}

fn clock_account(now: i64) -> Account {
    let clock = Clock {
        slot: 400_000_000,
        epoch_start_timestamp: now - 3_600,
        epoch: 900,
        leader_schedule_epoch: 901,
        unix_timestamp: now,
    };
    Account {
        lamports: 1_169_280,
        data: bincode::serialize(&clock).unwrap(),
        owner: SYSVAR_OWNER,
        executable: false,
        rent_epoch: 0,
    }
}

/// A key of its own for each role in a pool built by hand.
fn role(pool: u8, role: u8) -> Pubkey {
    let mut bytes = [pool; 32];
    bytes[0] = 0x50;
    bytes[1] = role;
    Pubkey::new_from_array(bytes)
}

/// The test USDC mint: 6 decimals, a mint and a freeze authority, as Circle's has on both clusters.
pub const USDC_MINT: Pubkey = pubkey!("3KVFue8ug46TW2itUWis5hzEYfUqgraJCNy1RXT2Pfy1");
const FEE_WALLET: Pubkey = pubkey!("AFjgLSW1o3bMDyfFyBiSb62W6oawMXectCEwWMFnSYNC");

/// A curve built by hand from a golden vector's state, with every account a swap on it reads, as
/// `create_curve_usdc` and the trades so far would have left them (USDC curve design §10.5: the
/// fixtures before any deploy).
pub struct VectorPool {
    pub id: u8,
    pub curve: CurveUsdc,
    pub now: i64,
}

impl VectorPool {
    /// `state` in the vectors' shape (the crate's `CurveState`), at the chain time `now`.
    pub fn new(id: u8, state: &serde_json::Value, now: i64) -> Self {
        let mint = role(id, 1);
        let creator = role(id, 2);
        let creator_split_bps = u64_of(&state["creator_split_bps"]) as u16;
        let creator_usdc_account = if creator_split_bps > 0 {
            associated_token_address(&creator, &USDC_MINT)
        } else {
            Pubkey::default()
        };
        let curve = CurveUsdc {
            bump: find_curve_address(&mint).1,
            usdc_vault_bump: find_usdc_vault_address(&mint).1,
            phase: PHASE_BONDING,
            paused: false,
            migrated: false,
            mint,
            creator,
            creator_usdc_account,
            creator_split_bps,
            fanex_split_bps: u64_of(&state["fanex_split_bps"]) as u16,
            creator_target_usdc: u64_of(&state["creator_cap"]) + u64_of(&state["fanex_cap"]),
            graduation_threshold_usdc: u64_of(&state["graduation_threshold"]),
            creator_cap_usdc: u64_of(&state["creator_cap"]),
            fanex_cap_usdc: u64_of(&state["fanex_cap"]),
            virtual_usdc_reserves: u64_of(&state["virtual_reserve"]),
            virtual_token_reserves: u64_of(&state["virtual_token_reserves"]),
            real_usdc_reserves: u64_of(&state["real_reserve"]),
            real_token_reserves: u64_of(&state["real_token_reserves"]),
            // A launch's migration supply, kept in the token vault beside the curve's.
            migration_token_reserves: 18_000_000_000_000,
            creator_received_usdc: u64_of(&state["creator_received"]),
            fanex_received_usdc: u64_of(&state["fanex_received"]),
            created_at: state["created_at"].as_i64().expect("created_at"),
            graduated_at: 0,
            migrated_at: 0,
        };
        Self { id, curve, now }
    }

    pub fn addresses(&self) -> CurveAddresses {
        CurveAddresses::of(&self.curve).expect("the curve's bumps")
    }

    pub fn config(&self) -> UsdcConfig {
        UsdcConfig {
            bump: Pubkey::find_program_address(&[USDC_CONFIG_SEED], &PROGRAM_ID).1,
            paused: false,
            admin: role(0, 10),
            pauser: role(0, 11),
            migration_authority: role(0, 12),
            usdc_mint: USDC_MINT,
            fee_usdc_account: associated_token_address(&FEE_WALLET, &USDC_MINT),
            lp_fee_owner: role(0, 13),
            raydium_cpmm_program: role(0, 14),
            raydium_amm_config: role(0, 15),
            raydium_create_pool_fee: role(0, 16),
            raydium_lock_program: role(0, 17),
            raydium_lock_authority: role(0, 18),
            launcher: role(0, 19),
        }
    }

    /// Every account a quote and a swap read, the clock included.
    pub fn accounts(&self) -> HashMap<Pubkey, Account> {
        let curve = &self.curve;
        let config = self.config();
        let addresses = self.addresses();
        let mut accounts = HashMap::from([
            (
                addresses.curve,
                account(PROGRAM_ID, curve.to_account_data()),
            ),
            (
                USDC_CONFIG_ADDRESS,
                account(PROGRAM_ID, config.to_account_data()),
            ),
            (
                USDC_MINT,
                mint_account(
                    10_000_000_000_000_000,
                    6,
                    Some(role(0, 20)),
                    Some(role(0, 21)),
                ),
            ),
            // The token's whole supply minted at launch, its mint authority then revoked, no freeze.
            (curve.mint, mint_account(100_000_000_000_000, 6, None, None)),
            (
                addresses.usdc_vault,
                token_account(USDC_MINT, addresses.curve, curve.real_usdc_reserves, None),
            ),
            (
                addresses.token_vault,
                token_account(
                    curve.mint,
                    addresses.curve,
                    curve.real_token_reserves + curve.migration_token_reserves,
                    None,
                ),
            ),
            (
                config.fee_usdc_account,
                token_account(USDC_MINT, FEE_WALLET, 0, None),
            ),
            (CLOCK_SYSVAR, clock_account(self.now)),
        ]);
        if curve.creator_usdc_account != Pubkey::default() {
            // Hardened as the launch leaves it: closable by the launcher alone.
            accounts.insert(
                curve.creator_usdc_account,
                token_account(USDC_MINT, curve.creator, 0, Some(config.launcher)),
            );
        }
        accounts
    }

    /// The pool as the kit takes it: its snapshot written to a directory of its own.
    pub fn pool_test(&self) -> PoolTest {
        let dir = std::env::temp_dir().join(format!(
            "pepper-curve-jupiter-{}-{}",
            std::process::id(),
            self.id
        ));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        PoolSnapshot::from_accounts(self.accounts())
            .save_dir(&dir)
            .expect("write the snapshot");
        let mut test = PoolTest::new(self.addresses().curve);
        test.fixtures_dir = dir;
        test
    }
}

// ── The SDK on a snapshot ────────────────────────────────────────────────────

/// The `<pubkey>.bin` files of a snapshot directory, as the kit writes and reads them.
pub fn load_snapshot(dir: &Path) -> HashMap<Pubkey, Account> {
    let mut accounts = HashMap::new();
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {dir:?}: {e}")) {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("bin") {
            continue;
        }
        let key: Pubkey = path.file_stem().unwrap().to_str().unwrap().parse().unwrap();
        let account: Account = bincode::deserialize(&std::fs::read(&path).unwrap()).unwrap();
        accounts.insert(key, account);
    }
    accounts
}

pub fn snapshot_clock(accounts: &HashMap<Pubkey, Account>) -> Clock {
    accounts
        .get(&CLOCK_SYSVAR)
        .map(|a| bincode::deserialize(&a.data).unwrap())
        .unwrap_or_default()
}

/// The SDK built and updated from a snapshot, as Jupiter builds it from the chain.
pub fn amm_from(accounts: &HashMap<Pubkey, Account>, pool: Pubkey) -> PepperCurveAmm {
    let context = AmmContext {
        clock_ref: ClockRef::from(snapshot_clock(accounts)),
    };
    let keyed = KeyedAccount {
        key: pool,
        account: accounts[&pool].clone(),
        params: None,
    };
    let mut amm = PepperCurveAmm::from_keyed_account(&keyed, &context).expect("from_keyed_account");
    let provider: HashMap<Pubkey, Box<Account>> = accounts
        .iter()
        .map(|(k, a)| (*k, Box::new(a.clone())))
        .collect();
    amm.update(&provider).expect("update");
    assert!(amm.is_active(), "the snapshot's curve is active");
    amm
}

pub fn program_bytes() -> Vec<u8> {
    std::fs::read(PROGRAM_SO).unwrap_or_else(|e| {
        panic!(
            "{PROGRAM_SO}: {e}. The suite runs on pepper_curve built from this tree: scripts/ci-onchain-tests.sh \
             copies its build there (docs/solana/PEPPER-CURVE-PROGRAM.md §14)"
        )
    })
}

fn svm_with(accounts: &HashMap<Pubkey, Account>, program: &[u8]) -> LiteSVM {
    let mut svm = LiteSVM::new();
    svm.add_program(PROGRAM_ID, program)
        .expect("load pepper_curve");
    for (key, account) in accounts {
        if !account.executable {
            svm.set_account(*key, account.clone())
                .expect("set a snapshot account");
        }
    }
    svm
}

/// The wallet's associated token account of `mint`, opened through the associated-token program as
/// a real user's is, then given `amount`: the kit's own way (its `create_funded_ata`).
fn funded_ata(svm: &mut LiteSVM, mint: &Pubkey, wallet: &Keypair, amount: u64) -> Pubkey {
    let owner = wallet.pubkey();
    let ata = associated_token_address(&owner, mint);
    let create = Instruction {
        program_id: ASSOCIATED_TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(owner, true),
            AccountMeta::new(ata, false),
            AccountMeta::new_readonly(owner, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
        ],
        data: vec![0],
    };
    let tx = Transaction::new_signed_with_payer(
        &[create],
        Some(&owner),
        &[wallet],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).expect("open the ATA");
    if amount > 0 {
        let mut account = svm.get_account(&ata).unwrap();
        account.data[64..72].copy_from_slice(&amount.to_le_bytes());
        svm.set_account(ata, account).unwrap();
    }
    ata
}

fn balance(svm: &LiteSVM, key: &Pubkey) -> u64 {
    svm.get_account(key).map(|a| token_amount(&a)).unwrap_or(0)
}

/// Each of the test's swaps again, in a fresh LiteSVM from the same snapshot, a fresh user funded
/// with exactly the amount asked, the instruction's minimum the quote itself: what leaves the source
/// is `in_amount`, what reaches the destination `out_amount`, what the fee and creator accounts gain
/// `fee_amount`, the vaults move by the rest, and the curve's counters land where the math crate's
/// `after` puts them.
pub fn assert_swaps_move_their_quote(test: &PoolTest) -> Vec<jupiter_amm_interface::Quote> {
    let accounts = load_snapshot(&test.fixtures_dir);
    let amm = amm_from(&accounts, test.pool);
    let program = program_bytes();
    let config = *amm.config().unwrap();
    let curve = *amm.curve();
    let addresses = *amm.addresses();
    let now = snapshot_clock(&accounts).unix_timestamp;
    let mut quotes = Vec::new();
    for swap in &test.swaps {
        let mut svm = svm_with(&accounts, &program);
        let user = Keypair::new();
        svm.airdrop(&user.pubkey(), 100_000_000_000).unwrap();
        let source = funded_ata(&mut svm, &swap.input_mint, &user, swap.amount);
        let destination = funded_ata(&mut svm, &swap.output_mint, &user, 0);

        let quote = amm
            .quote(&QuoteParams {
                amount: swap.amount,
                input_mint: swap.input_mint,
                output_mint: swap.output_mint,
                swap_mode: SwapMode::ExactIn,
                fee_mode: FeeMode::Normal,
            })
            .expect("quote");
        let SwapAndAccountMetas {
            swap: carried,
            account_metas,
        } = amm
            .get_swap_and_account_metas(&SwapParams {
                swap_mode: SwapMode::ExactIn,
                in_amount: swap.amount,
                out_amount: quote.out_amount,
                source_mint: swap.input_mint,
                destination_mint: swap.output_mint,
                source_token_account: source,
                destination_token_account: destination,
                token_transfer_authority: user.pubkey(),
                user: user.pubkey(),
                payer: user.pubkey(),
                quote_mint_to_referrer: None,
                jupiter_program_id: &JUPITER_PROGRAM_ID,
                missing_dynamic_accounts_as_default: false,
            })
            .expect("metas");
        let direction = direction_of(&carried).expect("a Pepper swap");
        let creator_account = account_metas
            .get(8)
            .map(|m| m.pubkey)
            .filter(|k| direction == Direction::Buy && *k != PROGRAM_ID);
        let watched = [
            source,
            destination,
            config.fee_usdc_account,
            addresses.usdc_vault,
            addresses.token_vault,
        ];
        let before: Vec<u64> = watched.iter().map(|k| balance(&svm, k)).collect();
        let creator_before = creator_account.map(|k| balance(&svm, &k)).unwrap_or(0);

        let ix = Instruction {
            program_id: amm.program_id(),
            accounts: account_metas,
            data: swap_instruction_data(&carried, swap.amount, quote.out_amount).unwrap(),
        };
        let tx = Transaction::new_signed_with_payer(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
                ix,
            ],
            Some(&user.pubkey()),
            &[&user],
            svm.latest_blockhash(),
        );
        if let Err(failed) = svm.send_transaction(tx) {
            panic!(
                "the swap failed: {:?}\n{}",
                failed.err,
                failed.meta.logs.join("\n")
            );
        }

        let after: Vec<u64> = watched.iter().map(|k| balance(&svm, k)).collect();
        let creator_gain = creator_account
            .map(|k| balance(&svm, &k) - creator_before)
            .unwrap_or(0);
        let fee_gain = after[2] - before[2];
        let label = format!("{direction:?} of {} on {}", swap.amount, test.pool);
        assert_eq!(
            before[0] - after[0],
            quote.in_amount,
            "{label}: the source's change is in_amount"
        );
        assert_eq!(
            after[0],
            swap.amount - quote.in_amount,
            "{label}: the source keeps what was not charged"
        );
        assert_eq!(
            after[1] - before[1],
            quote.out_amount,
            "{label}: the destination's change is out_amount"
        );
        assert_eq!(
            fee_gain + creator_gain,
            quote.fee_amount,
            "{label}: the fee and creator accounts gain fee_amount"
        );
        let curve_after =
            CurveUsdc::decode(&svm.get_account(&addresses.curve).unwrap().data).unwrap();
        match direction {
            Direction::Buy => {
                let expected =
                    pepper_curve_math::quote_buy(&curve.swap_state(), swap.amount, now).unwrap();
                assert_eq!(
                    after[3] - before[3],
                    quote.in_amount - quote.fee_amount,
                    "{label}: usdc_vault takes the curve's amount"
                );
                assert_eq!(
                    before[4] - after[4],
                    quote.out_amount,
                    "{label}: token_vault pays the tokens"
                );
                assert_eq!(
                    curve_after.swap_state(),
                    expected.after,
                    "{label}: the counters are the crate's"
                );
                let phase = if expected.graduates { 1 } else { 0 };
                assert_eq!(curve_after.phase, phase, "{label}: the phase");
            }
            Direction::Sell => {
                let expected =
                    pepper_curve_math::quote_sell(&curve.swap_state(), swap.amount).unwrap();
                assert_eq!(
                    before[3] - after[3],
                    quote.out_amount + quote.fee_amount,
                    "{label}: usdc_vault pays the gross"
                );
                assert_eq!(
                    after[4] - before[4],
                    quote.in_amount,
                    "{label}: token_vault takes the tokens"
                );
                assert_eq!(
                    curve_after.swap_state(),
                    expected.after,
                    "{label}: the counters are the crate's"
                );
                assert_eq!(curve_after.phase, 0, "{label}: a sale never graduates");
            }
        }
        quotes.push(quote);
    }
    quotes
}

/// Where the committed devnet snapshots live, the kit's default for `pool`.
pub fn devnet_fixtures(pool: &Pubkey) -> PathBuf {
    PathBuf::from("tests/fixtures/accounts").join(pool.to_string())
}
