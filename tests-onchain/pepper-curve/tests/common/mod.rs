#![allow(dead_code)]

//! A LiteSVM harness for `pepper_curve`, run against the built `.so`
//! (docs/solana/PEPPER-CURVE-PROGRAM.md §13). The `Curve` a distributor's funder
//! gate and a trade read is written straight into the bank: creating it through
//! `create_curve` needs the Metaplex program, which neither touches.
//! `metaplex_create_curve.rs` does create one, with Metaplex loaded from the
//! mainnet fixtures (`load_fixture_program`).
//!
//! The config's three keys are three wallets (the fee-wallet split, 2026-10-24),
//! so a constraint that reads the wrong one fails here: `admin`, `fee_wallet`,
//! `graduation_authority`. `Env::one_key()` makes them one, as mainnet's config is
//! until its cut-over.
//!
//! The USDC curve (usdc-curve design §10.11): every `Env` makes a test USDC
//! through the token program itself, a 6-decimal classic mint with a freeze
//! authority as Circle's USDC has on both clusters (`TestUsdc`), and
//! `write_usdc_config` and `write_curve_usdc` write a `UsdcConfig` and a
//! `CurveUsdc` with its two vaults into the bank, as the program stores them,
//! so a suite needs no launch first (`usdc_config.rs` holds the config's bytes
//! to what `initialize_usdc_config` writes). `vectors` reads the golden vectors
//! into the math crate's own types, and `send_logged` with `event` hands back
//! the events a transaction emitted.
//!
//! `Env::upgradeable` loads the program under the upgradeable loader, with the
//! ProgramData that records its upgrade authority, and `load_fixture_account`
//! writes an account fixture (the CLI's JSON) at its address.

use anchor_lang::solana_program::keccak::hashv;
use anchor_lang::solana_program::program_option::COption;
use anchor_lang::solana_program::program_pack::Pack;
use anchor_lang::{
    AccountDeserialize, AccountSerialize, AnchorDeserialize, Discriminator, Event, InstructionData, Space,
    ToAccountMetas,
};
use anchor_spl::associated_token::get_associated_token_address as ata;
use anchor_spl::associated_token::spl_associated_token_account;
use anchor_spl::token::spl_token;
use pepper_curve::constants::*;
use pepper_curve::state::{Curve, CurvePhase, CurveUsdc, SolVault, UsdcConfig};
use pepper_curve_math::{quote_buy, quote_sell, BuyQuote, CurveState, MathError, SellQuote};
use litesvm::LiteSVM;
use serde_json::Value;
use solana_sdk::{
    account::Account,
    clock::Clock,
    instruction::{AccountMeta, Instruction, InstructionError},
    message::Message,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction, system_program,
    transaction::{Transaction, TransactionError},
};

pub const SOL: u64 = 1_000_000_000;
pub const DAY: i64 = 86_400;
/// 2026-10-05 00:00:00 UTC.
pub const OCT_5_2026: i64 = 1_791_158_400;
pub const MONTH: i64 = 202_611;

/// µUSDC in one USDC.
pub const USDC: u64 = 1_000_000;
/// The test USDC's decimals, Circle's.
pub const USDC_DECIMALS: u8 = 6;

/// Anchor's own code for an account of the wrong type: the check that keeps a
/// v1 instruction off a v2 distributor and the reverse.
pub const ACCOUNT_DISCRIMINATOR_MISMATCH: u32 = 3002;

/// Anchor's own code for a `Signer` account that did not sign: Fanex named as
/// the approver of a month without Fanex's signature.
pub const ACCOUNT_NOT_SIGNER: u32 = 3010;

/// Anchor's own code for an account that does not exist yet: a waiver asked for
/// before any launch guard authority was named.
pub const ACCOUNT_NOT_INITIALIZED: u32 = 3012;

pub fn program_so() -> String {
    std::env::var("PEPPER_CURVE_SO").unwrap_or_else(|_| "../../target/deploy/pepper_curve.so".to_string())
}

/// Metaplex Token Metadata, which `create_curve` calls.
pub const METAPLEX_SO: &str = "mpl_token_metadata.so";
/// Raydium's CPMM, the graduation's pool.
pub const RAYDIUM_CPMM_SO: &str = "raydium_cpmm.so";
pub const RAYDIUM_CPMM_ID: Pubkey = Pubkey::from_str_const("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C");
/// Raydium's LP lock (closed source), the graduation's lock.
pub const RAYDIUM_LP_LOCK_SO: &str = "raydium_lp_lock.so";
pub const RAYDIUM_LP_LOCK_ID: Pubkey = Pubkey::from_str_const("LockrWmn6K5twhz3y9w1dQERbmgSaRkfnTeTKbpofwE");
/// Mainnet's 1% CPMM tier, index 1 (spec §10.12).
pub const MAINNET_AMM_CONFIG: Pubkey = Pubkey::from_str_const("G95xxie3XbkCqtE39GgQ9Ggc7xBC8Uceve7HFDEFApkc");
/// The account CPMM takes its pool-creation fee into, a constant of its code.
pub const MAINNET_CREATE_POOL_FEE: Pubkey = Pubkey::from_str_const("DNXgeM9EiiaAbaWvwjHj9fQQLAX5ZsfHyvmYUNRAdNC8");
/// The LP lock's authority PDA, `["lock_cp_authority_seed"]` under the lock.
pub const MAINNET_LOCK_AUTHORITY: Pubkey = Pubkey::from_str_const("3f7GcQFG397GAaEnv51zR6tsTVihYRydnydDD1cXekxH");

/// The third-party programs and accounts dumped from mainnet by
/// `scripts/solana/dump-litesvm-fixtures.sh`, committed, each pinned by
/// `fixtures.rs`. ⚠️ Third-party code: never in a public mirror.
pub fn fixtures_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

pub fn fixture_path(name: &str) -> std::path::PathBuf {
    fixtures_dir().join(name)
}

/// Loads the program fixture `name` at `program_id`, the bytes mainnet runs.
/// LiteSVM loads it as a non-upgradeable program, with no ProgramData.
pub fn load_fixture_program(svm: &mut LiteSVM, program_id: Pubkey, name: &str) {
    let path = fixture_path(name);
    svm.add_program_from_file(program_id, &path)
        .unwrap_or_else(|e| panic!("{}: {e} (scripts/solana/dump-litesvm-fixtures.sh)", path.display()));
}

/// The custom error code of a `FanexError`.
pub fn code(e: pepper_curve::errors::FanexError) -> u32 {
    anchor_lang::error::ERROR_CODE_OFFSET + e as u32
}

pub fn expect_err<T: std::fmt::Debug>(result: Result<T, TransactionError>, expected: u32) {
    match result {
        Err(TransactionError::InstructionError(_, InstructionError::Custom(c))) => {
            assert_eq!(c, expected, "custom error {c}, expected {expected}")
        }
        other => panic!("expected custom error {expected}, got {other:?}"),
    }
}

/// The events of type `E` a transaction emitted, in order, read from its logs.
pub fn events<E: Event + AnchorDeserialize>(logs: &[String]) -> Vec<E> {
    use anchor_lang::__private::base64::{engine::general_purpose::STANDARD, Engine as _};
    logs.iter()
        .filter_map(|line| line.strip_prefix("Program data: "))
        .filter_map(|data| STANDARD.decode(data).ok())
        .filter(|bytes| bytes.starts_with(E::DISCRIMINATOR))
        .map(|bytes| E::try_from_slice(&bytes[E::DISCRIMINATOR.len()..]).unwrap())
        .collect()
}

pub fn ix<A: ToAccountMetas, D: InstructionData>(accounts: A, data: D) -> Instruction {
    Instruction { program_id: pepper_curve::ID, accounts: accounts.to_account_metas(None), data: data.data() }
}

pub fn pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &pepper_curve::ID).0
}
pub fn config_pda() -> Pubkey {
    pda(&[CONFIG_SEED])
}
pub fn curve_pda(mint: &Pubkey) -> Pubkey {
    pda(&[CURVE_SEED, mint.as_ref()])
}
pub fn distributor_pda(mint: &Pubkey, month: i64) -> Pubkey {
    pda(&[DISTRIBUTOR_SEED, mint.as_ref(), &month.to_le_bytes()])
}
pub fn claim_status_pda(distributor: &Pubkey, claimant: &Pubkey) -> Pubkey {
    pda(&[CLAIM_SEED, distributor.as_ref(), claimant.as_ref()])
}
pub fn transit_pda(source: &Pubkey) -> Pubkey {
    pda(&[PAYOUT_TRANSIT_SEED, source.as_ref()])
}
pub fn launch_guard_pda() -> Pubkey {
    pda(&[LAUNCH_GUARD_SEED])
}
pub fn waiver_pda(mint: &Pubkey) -> Pubkey {
    pda(&[LAUNCH_GUARD_WAIVER_SEED, mint.as_ref()])
}
pub fn usdc_config_pda() -> Pubkey {
    pda(&[USDC_CONFIG_SEED])
}
/// A USDC curve's reserve: a token account at its own PDA, not an ATA (§10.2).
pub fn usdc_vault_pda(mint: &Pubkey) -> Pubkey {
    pda(&[USDC_VAULT_SEED, mint.as_ref()])
}

/// A Merkle tree as `lib/solana/pepper-curve/merkle.ts` builds it: sorted-pair
/// keccak, an odd node promoted unchanged, leaves in the given order.
pub struct Tree {
    pub root: [u8; 32],
    pub proofs: Vec<Vec<[u8; 32]>>,
}

fn hash_pair(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    if a <= b {
        hashv(&[a, b]).0
    } else {
        hashv(&[b, a]).0
    }
}

pub fn build_tree(leaves: &[[u8; 32]]) -> Tree {
    let mut layers: Vec<Vec<[u8; 32]>> = vec![leaves.to_vec()];
    while layers.last().unwrap().len() > 1 {
        let prev = layers.last().unwrap();
        let next: Vec<[u8; 32]> = prev
            .chunks(2)
            .map(|pair| if pair.len() == 2 { hash_pair(&pair[0], &pair[1]) } else { pair[0] })
            .collect();
        layers.push(next);
    }
    let proofs = (0..leaves.len())
        .map(|leaf| {
            let mut proof = Vec::new();
            let mut idx = leaf;
            for layer in &layers[..layers.len() - 1] {
                let sibling = if idx % 2 == 0 { idx + 1 } else { idx - 1 };
                if sibling < layer.len() {
                    proof.push(layer[sibling]);
                }
                idx /= 2;
            }
            proof
        })
        .collect();
    Tree { root: layers.last().unwrap()[0], proofs }
}

/// A month's holders, funded so their accounts exist, and the v2 tree over them.
/// A curve's raise: the shares of a buy its creator and Fanex take, up to their caps.
#[derive(Clone, Copy)]
pub struct Raise {
    pub creator_split_bps: u64,
    pub fanex_split_bps: u64,
    pub creator_cap_lamports: u64,
    pub fanex_cap_lamports: u64,
}

impl Raise {
    /// A revenue launch's shape: creator 3,760 and Fanex 240 bps, capped at
    /// 15.04 and 0.96 SOL of a 16 SOL target.
    pub fn revenue() -> Self {
        Self {
            creator_split_bps: 3_760,
            fanex_split_bps: 240,
            creator_cap_lamports: 15_040_000_000,
            fanex_cap_lamports: 960_000_000,
        }
    }
}

pub struct V2Month {
    pub holders: Vec<(Keypair, u64)>,
    pub tree: Tree,
    pub total: u64,
}

impl V2Month {
    pub fn proof(&self, index: usize) -> Vec<[u8; 32]> {
        self.tree.proofs[index].clone()
    }
}

/// The suite's USDC: a classic SPL mint of 6 decimals with a freeze authority,
/// as Circle's USDC has on both clusters (usdc-curve design §10.11), so a frozen
/// account's refusals can be produced. `pepper_raise`'s harness makes its mint
/// without one.
pub struct TestUsdc {
    pub mint: Pubkey,
    pub mint_authority: Keypair,
    pub freeze_authority: Keypair,
}

impl TestUsdc {
    /// Made by the token program itself, paid by `payer`: the system program's
    /// `create_account`, then `initialize_mint2`.
    fn create(svm: &mut LiteSVM, payer: &Keypair) -> Self {
        let mint = Keypair::new();
        let mint_authority = Keypair::new();
        let freeze_authority = Keypair::new();
        let rent = svm.minimum_balance_for_rent_exemption(spl_token::state::Mint::LEN);
        let ixs = [
            system_instruction::create_account(
                &payer.pubkey(),
                &mint.pubkey(),
                rent,
                spl_token::state::Mint::LEN as u64,
                &spl_token::ID,
            ),
            spl_token::instruction::initialize_mint2(
                &spl_token::ID,
                &mint.pubkey(),
                &mint_authority.pubkey(),
                Some(&freeze_authority.pubkey()),
                USDC_DECIMALS,
            )
            .unwrap(),
        ];
        let tx = Transaction::new_signed_with_payer(&ixs, Some(&payer.pubkey()), &[payer, &mint], svm.latest_blockhash());
        svm.send_transaction(tx).expect("the test USDC mint");
        svm.expire_blockhash();
        Self { mint: mint.pubkey(), mint_authority, freeze_authority }
    }
}

pub struct Env {
    pub svm: LiteSVM,
    /// Deploys, administers (`config.admin`) and pays every fee: the Fanex
    /// sponsor of the production claim.
    pub admin: Keypair,
    /// `config.fanex_fee_wallet`, receive-only since 2026-10-24: `buy` and `sell`
    /// pay it, nothing asks for its signature. It holds 1 SOL, so it exists, as
    /// the production one must before the config names it.
    pub fee_wallet: Keypair,
    /// `config.graduation_authority`, the server's key: since 2026-10-24 the
    /// approver of every v2 month and the only funder of a v1 one.
    pub graduation_authority: Keypair,
    /// The curve's creator, a v2 month's funder next to the dividend payer.
    pub creator: Keypair,
    pub mint: Pubkey,
    /// The suite's USDC, made with every `Env`.
    pub usdc: TestUsdc,
    /// The `pauser` `usdc_config` names: a hot key that may only pause.
    pub pauser: Keypair,
}

impl Env {
    pub fn new() -> Self {
        Self::with_svm(LiteSVM::new(), false)
    }

    /// As `new`, the admin also the fee wallet and the graduation authority: the
    /// one key mainnet's config holds until its cut-over (docs/ops/KEY-ROTATION.md §1e).
    pub fn one_key() -> Self {
        Self::with_svm(LiteSVM::new(), true)
    }

    /// As `new`, with signature verification off, for a signer whose key the
    /// suite does not hold: the dividend payer, a fixed address
    /// (`DIVIDEND_PAYER`). The program still sees it as a signer, which is all
    /// its funder gate reads. Send through `send_unverified`.
    pub fn unverified() -> Self {
        Self::with_svm(LiteSVM::new().with_sigverify(false), false)
    }

    fn with_svm(svm: LiteSVM, one_key: bool) -> Self {
        // Every log line kept: LiteSVM keeps 10 000 bytes by default, and a long
        // transaction (a launch through Metaplex) would lose the events `event` reads.
        let mut svm = svm.with_log_bytes_limit(None);
        svm.add_program_from_file(pepper_curve::ID, program_so())
            .expect("build first: cargo-build-sbf --manifest-path programs/pepper_curve/Cargo.toml");
        let admin = Keypair::new();
        svm.airdrop(&admin.pubkey(), 1_000 * SOL).unwrap();
        let (fee_wallet, graduation_authority) = if one_key {
            (admin.insecure_clone(), admin.insecure_clone())
        } else {
            let (fee_wallet, graduation_authority) = (Keypair::new(), Keypair::new());
            svm.airdrop(&fee_wallet.pubkey(), SOL).unwrap();
            svm.airdrop(&graduation_authority.pubkey(), 100 * SOL).unwrap();
            (fee_wallet, graduation_authority)
        };
        let creator = Keypair::new();
        svm.airdrop(&creator.pubkey(), 100 * SOL).unwrap();
        let usdc = TestUsdc::create(&mut svm, &admin);
        let mut env = Self {
            svm,
            admin,
            fee_wallet,
            graduation_authority,
            creator,
            mint: Pubkey::new_unique(),
            usdc,
            pauser: Keypair::new(),
        };
        env.set_time(OCT_5_2026);
        env.init_config();
        env.write_curve();
        env
    }

    /// Signs with the admin (fee payer) plus `extra`, then expires the blockhash
    /// so an identical transaction can be sent again.
    pub fn send(&mut self, ixs: &[Instruction], extra: &[&Keypair]) -> Result<(), TransactionError> {
        let admin = self.admin.insecure_clone();
        self.send_paid_by(&admin, ixs, extra)
    }

    /// As `send`, for an `Env::unverified()`: the admin pays and signs, `extra`
    /// sign, and every other signer the instructions name keeps an empty
    /// signature.
    pub fn send_unverified(&mut self, ixs: &[Instruction], extra: &[&Keypair]) -> Result<(), TransactionError> {
        let admin = self.admin.insecure_clone();
        let mut signers: Vec<&Keypair> = vec![&admin];
        signers.extend(extra.iter().copied().filter(|k| k.pubkey() != admin.pubkey()));
        let mut tx = Transaction::new_unsigned(Message::new(ixs, Some(&admin.pubkey())));
        tx.partial_sign(&signers, self.svm.latest_blockhash());
        let result = self.svm.send_transaction(tx).map(|_| ()).map_err(|failed| failed.err);
        self.svm.expire_blockhash();
        result
    }

    /// As `send`, with another fee payer: a transaction the admin (the Fanex
    /// wallet) does not sign at all, as a creator would send one.
    pub fn send_paid_by(&mut self, payer: &Keypair, ixs: &[Instruction], extra: &[&Keypair]) -> Result<(), TransactionError> {
        self.send_logged_paid_by(payer, ixs, extra).map(|_| ())
    }

    /// As `send`, handing back the transaction's logs, where `emit!` writes its
    /// events (`event`, `events_of`).
    pub fn send_logged(&mut self, ixs: &[Instruction], extra: &[&Keypair]) -> Result<Vec<String>, TransactionError> {
        let admin = self.admin.insecure_clone();
        self.send_logged_paid_by(&admin, ixs, extra)
    }

    /// As `send_paid_by`, handing back the transaction's logs.
    pub fn send_logged_paid_by(
        &mut self,
        payer: &Keypair,
        ixs: &[Instruction],
        extra: &[&Keypair],
    ) -> Result<Vec<String>, TransactionError> {
        let mut signers: Vec<&Keypair> = vec![payer];
        signers.extend(extra.iter().copied().filter(|k| k.pubkey() != payer.pubkey()));
        let tx = Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &signers, self.svm.latest_blockhash());
        let result = self.svm.send_transaction(tx).map(|meta| meta.logs).map_err(|failed| failed.err);
        self.svm.expire_blockhash();
        result
    }

    /// Loads Metaplex Token Metadata at its address, from its fixture.
    pub fn load_metaplex(&mut self) {
        load_fixture_program(&mut self.svm, anchor_spl::metadata::ID, METAPLEX_SO);
    }

    pub fn set_time(&mut self, unix_timestamp: i64) {
        let mut clock: Clock = self.svm.get_sysvar();
        clock.unix_timestamp = unix_timestamp;
        self.svm.set_sysvar(&clock);
    }

    pub fn lamports(&self, address: &Pubkey) -> u64 {
        self.svm.get_account(address).map(|a| a.lamports).unwrap_or(0)
    }

    pub fn exists(&self, address: &Pubkey) -> bool {
        self.svm.get_account(address).map(|a| a.lamports > 0).unwrap_or(false)
    }

    pub fn fetch<T: AccountDeserialize>(&self, address: &Pubkey) -> T {
        let account = self.svm.get_account(address).expect("account exists");
        T::try_deserialize(&mut account.data.as_slice()).unwrap()
    }

    fn init_config(&mut self) {
        let init = ix(
            pepper_curve::accounts::InitializeConfig {
                admin: self.admin.pubkey(),
                config: config_pda(),
                system_program: system_program::id(),
            },
            pepper_curve::instruction::InitializeConfig {
                fanex_fee_wallet: self.fee_wallet.pubkey(),
                graduation_authority: self.graduation_authority.pubkey(),
            },
        );
        self.send(&[init], &[]).unwrap();
    }

    /// The curve the funder gate reads, written as the program would store it.
    fn write_curve(&mut self) {
        let (address, bump) = Pubkey::find_program_address(&[CURVE_SEED, self.mint.as_ref()], &pepper_curve::ID);
        let curve = Curve {
            mint: self.mint,
            creator: self.creator.pubkey(),
            phase: CurvePhase::Bonding,
            creator_target_lamports: 0,
            cpmm_seed_lamports: 0,
            creator_split_bps: 0,
            fanex_split_bps: 0,
            creator_cap_lamports: 0,
            fanex_cap_lamports: 0,
            virtual_sol_reserves: 0,
            virtual_token_reserves: 0,
            real_sol_reserves: 0,
            real_token_reserves: 0,
            migration_token_reserves: 0,
            creator_received_lamports: 0,
            fanex_received_lamports: 0,
            paused: false,
            created_at: 0,
            graduated_at: 0,
            bump,
            sol_vault_bump: 0,
        };
        self.put(address, account_data(&curve, Curve::INIT_SPACE), pepper_curve::ID);
    }

    /// A curve `buy` can trade, launched at `created_at`, written as the program
    /// would store it: a real SPL mint at `self.mint`, the token vault holding
    /// what the curve sells and migrates, the SOL vault, and the reserves of a
    /// fresh launch. No raise split (an equity-shaped curve), so after the 1%
    /// fee every lamport buys from the curve: 5 virtual SOL against the whole
    /// supply, about 0.05 SOL for 1% of it.
    pub fn write_tradeable_curve(&mut self, created_at: i64) {
        self.write_tradeable_curve_raising(created_at, None);
    }

    /// As `write_tradeable_curve`, with `raise`'s shares taken off every buy
    /// before the curve's (a revenue curve's shape), none with `None`.
    pub fn write_tradeable_curve_raising(&mut self, created_at: i64, raise: Option<Raise>) {
        let raise = raise.unwrap_or(Raise { creator_split_bps: 0, fanex_split_bps: 0, creator_cap_lamports: 0, fanex_cap_lamports: 0 });
        let curve_address = curve_pda(&self.mint);
        let (sol_vault, sol_vault_bump) = Pubkey::find_program_address(&[SOL_VAULT_SEED, self.mint.as_ref()], &pepper_curve::ID);
        let (_, curve_bump) = Pubkey::find_program_address(&[CURVE_SEED, self.mint.as_ref()], &pepper_curve::ID);
        let real_token_reserves = 80_000_000 * 1_000_000;
        let migration_token_reserves = 14_000_000 * 1_000_000;

        self.put_launched_mint(self.mint);
        self.put_token_account(
            ata(&curve_address, &self.mint),
            self.mint,
            curve_address,
            real_token_reserves + migration_token_reserves,
            COption::None,
        );

        let mut sol_vault_data = Vec::new();
        SolVault { mint: self.mint, bump: sol_vault_bump }.try_serialize(&mut sol_vault_data).unwrap();
        self.put(sol_vault, sol_vault_data, pepper_curve::ID);

        let curve = Curve {
            mint: self.mint,
            creator: self.creator.pubkey(),
            phase: CurvePhase::Bonding,
            creator_target_lamports: raise.creator_cap_lamports + raise.fanex_cap_lamports,
            cpmm_seed_lamports: 24 * SOL,
            creator_split_bps: raise.creator_split_bps,
            fanex_split_bps: raise.fanex_split_bps,
            creator_cap_lamports: raise.creator_cap_lamports,
            fanex_cap_lamports: raise.fanex_cap_lamports,
            virtual_sol_reserves: 5 * SOL,
            virtual_token_reserves: TOKEN_TOTAL_SUPPLY,
            real_sol_reserves: 0,
            real_token_reserves,
            migration_token_reserves,
            creator_received_lamports: 0,
            fanex_received_lamports: 0,
            paused: false,
            created_at,
            graduated_at: 0,
            bump: curve_bump,
            sol_vault_bump,
        };
        self.put(curve_address, account_data(&curve, Curve::INIT_SPACE), pepper_curve::ID);
    }

    /// Writes `data` at `address`, owned by `owner`, rent-exempt.
    pub fn put(&mut self, address: Pubkey, data: Vec<u8>, owner: Pubkey) {
        let lamports = self.svm.minimum_balance_for_rent_exemption(data.len());
        self.svm
            .set_account(address, Account { lamports, data, owner, executable: false, rent_epoch: 0 })
            .unwrap();
    }

    /// A launched token's mint at `address`, as a launch leaves it: the whole
    /// supply minted, no mint or freeze authority.
    pub fn put_launched_mint(&mut self, address: Pubkey) {
        let mut mint = vec![0u8; spl_token::state::Mint::LEN];
        spl_token::state::Mint {
            mint_authority: COption::None,
            supply: TOKEN_TOTAL_SUPPLY,
            decimals: TOKEN_DECIMALS,
            is_initialized: true,
            freeze_authority: COption::None,
        }
        .pack_into_slice(&mut mint);
        self.put(address, mint, spl_token::ID);
    }

    /// An initialized SPL token account at `address`: `amount` of `mint`, its
    /// authority `owner`, no delegate: a seller's tokens of a written curve's
    /// mint, whose authority a launch revoked, can only be written.
    pub fn put_token_account(&mut self, address: Pubkey, mint: Pubkey, owner: Pubkey, amount: u64, close_authority: COption<Pubkey>) {
        let mut account = vec![0u8; spl_token::state::Account::LEN];
        spl_token::state::Account {
            mint,
            owner,
            amount,
            delegate: COption::None,
            state: spl_token::state::AccountState::Initialized,
            is_native: COption::None,
            delegated_amount: 0,
            close_authority,
        }
        .pack_into_slice(&mut account);
        self.put(address, account, spl_token::ID);
    }

    /// A new wallet holding `lamports`.
    pub fn funded_wallet(&mut self, lamports: u64) -> Keypair {
        let wallet = Keypair::new();
        self.svm.airdrop(&wallet.pubkey(), lamports).unwrap();
        wallet
    }

    /// `buy(sol_in, 0)` on `self.mint`, signed and paid by `buyer`.
    pub fn buy(&mut self, buyer: &Keypair, sol_in: u64) -> Result<(), TransactionError> {
        self.buy_with_extra(buyer, sol_in, &[])
    }

    /// `set_launch_guard_authority(authority)`, signed by `signer` as the admin.
    pub fn set_launch_guard_authority_by(&mut self, signer: &Keypair, authority: &Pubkey) -> Result<(), TransactionError> {
        let set = ix(
            pepper_curve::accounts::SetLaunchGuardAuthority {
                config: config_pda(),
                admin: signer.pubkey(),
                launch_guard: launch_guard_pda(),
                system_program: system_program::id(),
            },
            pepper_curve::instruction::SetLaunchGuardAuthority { authority: *authority },
        );
        let signer = signer.insecure_clone();
        self.send_paid_by(&signer, &[set], &[])
    }

    /// `waive_launch_guard()` for `mint`, paid by `payer` (the launcher) and
    /// signed by `authority`.
    pub fn waive(&mut self, payer: &Keypair, authority: &Keypair, mint: &Pubkey) -> Result<(), TransactionError> {
        let waive = ix(
            pepper_curve::accounts::WaiveLaunchGuard {
                payer: payer.pubkey(),
                launch_guard: launch_guard_pda(),
                authority: authority.pubkey(),
                mint: *mint,
                curve: curve_pda(mint),
                waiver: waiver_pda(mint),
                system_program: system_program::id(),
            },
            pepper_curve::instruction::WaiveLaunchGuard {},
        );
        let payer = payer.insecure_clone();
        self.send_paid_by(&payer, &[waive], &[authority])
    }

    /// As `buy`, with `extra` appended to the instruction's accounts, read-only:
    /// a waiver, or something passed as one.
    pub fn buy_with_extra(&mut self, buyer: &Keypair, sol_in: u64, extra: &[Pubkey]) -> Result<(), TransactionError> {
        let mut buy = self.buy_ix(&buyer.pubkey(), sol_in, &self.fee_wallet.pubkey());
        buy.accounts.extend(extra.iter().map(|key| AccountMeta::new_readonly(*key, false)));
        let buyer = buyer.insecure_clone();
        self.send_paid_by(&buyer, &[buy], &[])
    }

    /// `buy(sol_in, 0)` on `self.mint`, unsigned, Fanex's share paid to `fanex`.
    pub fn buy_ix(&self, buyer: &Pubkey, sol_in: u64, fanex: &Pubkey) -> Instruction {
        let curve = curve_pda(&self.mint);
        ix(
            pepper_curve::accounts::Buy {
                buyer: *buyer,
                config: config_pda(),
                curve,
                mint: self.mint,
                sol_vault: pda(&[SOL_VAULT_SEED, self.mint.as_ref()]),
                token_vault: ata(&curve, &self.mint),
                buyer_token_account: ata(buyer, &self.mint),
                creator: self.creator.pubkey(),
                fanex: *fanex,
                token_program: spl_token::ID,
                associated_token_program: anchor_spl::associated_token::ID,
                system_program: system_program::id(),
            },
            pepper_curve::instruction::Buy { sol_in, min_tokens_out: 0 },
        )
    }

    /// `sell(token_in, 0)` on `self.mint`, unsigned, the fee paid to `fanex`.
    pub fn sell_ix(&self, seller: &Pubkey, token_in: u64, fanex: &Pubkey) -> Instruction {
        let curve = curve_pda(&self.mint);
        ix(
            pepper_curve::accounts::Sell {
                seller: *seller,
                config: config_pda(),
                curve,
                mint: self.mint,
                sol_vault: pda(&[SOL_VAULT_SEED, self.mint.as_ref()]),
                token_vault: ata(&curve, &self.mint),
                seller_token_account: ata(seller, &self.mint),
                fanex: *fanex,
                token_program: spl_token::ID,
                system_program: system_program::id(),
            },
            pepper_curve::instruction::Sell { token_in, min_sol_out: 0 },
        )
    }

    /// The tokens of `self.mint` in `owner`'s associated account, 0 without one.
    pub fn token_balance(&self, owner: &Pubkey) -> u64 {
        self.svm
            .get_account(&ata(owner, &self.mint))
            .map(|account| spl_token::state::Account::unpack(&account.data).unwrap().amount)
            .unwrap_or(0)
    }

    /// `update_config(new_admin, new_fee_wallet, new_graduation_authority)`, signed by `signer` as the admin.
    pub fn update_config_by(
        &mut self,
        signer: &Keypair,
        new_admin: Option<Pubkey>,
        new_fee_wallet: Option<Pubkey>,
        new_graduation_authority: Option<Pubkey>,
    ) -> Result<(), TransactionError> {
        let update = ix(
            pepper_curve::accounts::UpdateConfig { config: config_pda(), admin: signer.pubkey() },
            pepper_curve::instruction::UpdateConfig {
                new_admin,
                new_fanex_fee_wallet: new_fee_wallet,
                new_graduation_authority,
            },
        );
        let signer = signer.insecure_clone();
        self.send_paid_by(&signer, &[update], &[])
    }

    pub fn set_paused(&mut self, paused: bool) {
        let pause = ix(
            pepper_curve::accounts::UpdateConfig { config: config_pda(), admin: self.admin.pubkey() },
            pepper_curve::instruction::SetPaused { paused },
        );
        self.send(&[pause], &[]).unwrap();
    }

    /// `n` holders with distinct amounts, each account funded so it exists, and
    /// the v2 tree over them in that order.
    pub fn v2_month(&mut self, n: usize) -> V2Month {
        let holders: Vec<(Keypair, u64)> = (0..n)
            .map(|i| {
                let kp = Keypair::new();
                self.svm.airdrop(&kp.pubkey(), SOL).unwrap();
                (kp, 10_000_000 + i as u64 * 1_000)
            })
            .collect();
        let leaves: Vec<[u8; 32]> = holders
            .iter()
            .enumerate()
            .map(|(i, (kp, amount))| pepper_curve::math::dividend_leaf_v2(i as u32, &kp.pubkey(), *amount))
            .collect();
        let total = holders.iter().map(|(_, a)| a).sum();
        V2Month { holders, tree: build_tree(&leaves), total }
    }

    /// Funds `MONTH` as v2 with Fanex's approval: the graduation authority signs
    /// as the approver, the admin pays.
    pub fn init_v2(
        &mut self,
        funder: &Keypair,
        root: [u8; 32],
        total: u64,
        expires_at: i64,
        leaf_count: u32,
    ) -> Result<(), TransactionError> {
        let approver = self.graduation_authority.insecure_clone();
        let ix = self.init_v2_ix(funder, &approver.pubkey(), root, total, expires_at, leaf_count);
        let funder = funder.insecure_clone();
        let mut signers = vec![&funder];
        if approver.pubkey() != funder.pubkey() {
            signers.push(&approver);
        }
        self.send(&[ix], &signers)
    }

    /// `init_distributor_v2` naming `approver`, unsigned: the caller decides who signs.
    pub fn init_v2_ix(
        &self,
        funder: &Keypair,
        approver: &Pubkey,
        root: [u8; 32],
        total: u64,
        expires_at: i64,
        leaf_count: u32,
    ) -> Instruction {
        self.init_v2_ix_by(&funder.pubkey(), approver, root, total, expires_at, leaf_count)
    }

    /// As `init_v2_ix`, for a funder named by its address alone.
    pub fn init_v2_ix_by(
        &self,
        funder: &Pubkey,
        approver: &Pubkey,
        root: [u8; 32],
        total: u64,
        expires_at: i64,
        leaf_count: u32,
    ) -> Instruction {
        ix(
            pepper_curve::accounts::InitDistributorV2 {
                funder: *funder,
                approver: *approver,
                config: config_pda(),
                mint: self.mint,
                curve: curve_pda(&self.mint),
                distributor: distributor_pda(&self.mint, MONTH),
                system_program: system_program::id(),
            },
            pepper_curve::instruction::InitDistributorV2 { month: MONTH, merkle_root: root, total_amount: total, expires_at, leaf_count },
        )
    }

    /// `init_distributor` (v1) for `MONTH`, funded by `funder`, expiring a year on.
    pub fn init_v1(&mut self, funder: &Keypair, root: [u8; 32], total: u64) -> Result<(), TransactionError> {
        self.init_v1_expiring(funder, root, total, OCT_5_2026 + 365 * DAY)
    }

    /// As `init_v1`, expiring at `expires_at`.
    pub fn init_v1_expiring(
        &mut self,
        funder: &Keypair,
        root: [u8; 32],
        total: u64,
        expires_at: i64,
    ) -> Result<(), TransactionError> {
        let init = self.init_v1_ix_by(&funder.pubkey(), root, total, expires_at);
        let funder = funder.insecure_clone();
        self.send(&[init], &[&funder])
    }

    /// `init_distributor` (v1) for `MONTH`, unsigned, for a funder named by its address alone.
    pub fn init_v1_ix_by(&self, funder: &Pubkey, root: [u8; 32], total: u64, expires_at: i64) -> Instruction {
        ix(
            pepper_curve::accounts::InitDistributor {
                funder: *funder,
                config: config_pda(),
                mint: self.mint,
                curve: curve_pda(&self.mint),
                distributor: distributor_pda(&self.mint, MONTH),
                system_program: system_program::id(),
            },
            pepper_curve::instruction::InitDistributor { month: MONTH, merkle_root: root, total_amount: total, expires_at },
        )
    }

    /// Funds `month` as a v2 distributor, by the creator with the graduation
    /// authority's approval, expiring a year on: no config key funds a v2 month
    /// since 2026-10-24, and the creator's key is the one the suite holds.
    pub fn fund_v2(&mut self, month: &V2Month) {
        let creator = self.creator.insecure_clone();
        let expires = OCT_5_2026 + 365 * DAY;
        self.init_v2(&creator, month.tree.root, month.total, expires, month.holders.len() as u32).unwrap();
    }

    pub fn claim_v2(
        &mut self,
        claimant: &Keypair,
        index: u32,
        amount: u64,
        proof: Vec<[u8; 32]>,
        through_transit: bool,
    ) -> Result<(), TransactionError> {
        let distributor = distributor_pda(&self.mint, MONTH);
        let mut claim = ix(
            pepper_curve::accounts::ClaimDividendV2 {
                claimant: claimant.pubkey(),
                distributor,
                system_program: system_program::id(),
            },
            pepper_curve::instruction::ClaimDividendV2 { index, amount, proof },
        );
        if through_transit {
            claim.accounts.push(AccountMeta::new(transit_pda(&distributor), false));
        }
        let claimant = claimant.insecure_clone();
        self.send(&[claim], &[&claimant])
    }

    pub fn close_v2(&mut self, funder: &Keypair) -> Result<(), TransactionError> {
        let close = ix(
            pepper_curve::accounts::CloseDistributorV2 { distributor: distributor_pda(&self.mint, MONTH), funder: funder.pubkey() },
            pepper_curve::instruction::CloseDistributorV2 {},
        );
        let funder = funder.insecure_clone();
        let extra: Vec<&Keypair> = if funder.pubkey() == self.admin.pubkey() { vec![] } else { vec![&funder] };
        self.send(&[close], &extra)
    }

    /// A v1 month over v1 leaves, funded by the graduation authority, its one funder.
    pub fn fund_v1(&mut self, holders: &[(Keypair, u64)]) -> Tree {
        let leaves: Vec<[u8; 32]> = holders
            .iter()
            .map(|(kp, amount)| pepper_curve::math::dividend_leaf(&kp.pubkey(), *amount))
            .collect();
        let tree = build_tree(&leaves);
        let total = holders.iter().map(|(_, a)| a).sum();
        let authority = self.graduation_authority.insecure_clone();
        self.init_v1(&authority, tree.root, total).unwrap();
        tree
    }

    /// `n` funded holders with distinct amounts, for a v1 month.
    pub fn v1_holders(&mut self, n: usize) -> Vec<(Keypair, u64)> {
        (0..n)
            .map(|i| {
                let kp = Keypair::new();
                self.svm.airdrop(&kp.pubkey(), SOL).unwrap();
                (kp, 10_000_000 + i as u64 * 1_000)
            })
            .collect()
    }

    /// `set_distributor_funder(new_funder)` on `MONTH`'s distributor, `funder`
    /// named as the signer, unsigned: the caller decides who signs.
    pub fn set_funder_ix(&self, funder: &Pubkey, new_funder: &Pubkey) -> Instruction {
        ix(
            pepper_curve::accounts::SetDistributorFunder { funder: *funder, distributor: distributor_pda(&self.mint, MONTH) },
            pepper_curve::instruction::SetDistributorFunder { new_funder: *new_funder },
        )
    }

    /// `close_distributor` (v1) on `MONTH`, unsigned, for a funder named by its address alone.
    pub fn close_v1_ix_by(&self, funder: &Pubkey) -> Instruction {
        ix(
            pepper_curve::accounts::CloseDistributor { distributor: distributor_pda(&self.mint, MONTH), funder: *funder },
            pepper_curve::instruction::CloseDistributor {},
        )
    }

    /// `close_claim_status` for `claimant`'s v1 claim on `MONTH`, the deposit to `rent_recipient`.
    pub fn close_claim_status_ix(&self, claimant: &Pubkey, rent_recipient: &Pubkey) -> Instruction {
        let distributor = distributor_pda(&self.mint, MONTH);
        ix(
            pepper_curve::accounts::CloseClaimStatus {
                claim_status: claim_status_pda(&distributor, claimant),
                distributor,
                rent_recipient: *rent_recipient,
            },
            pepper_curve::instruction::CloseClaimStatus {},
        )
    }

    /// A v1 claim, the admin as `rent_payer` (the Fanex sponsor).
    pub fn claim_v1(&mut self, claimant: &Keypair, amount: u64, proof: Vec<[u8; 32]>) -> Result<(), TransactionError> {
        let distributor = distributor_pda(&self.mint, MONTH);
        let claim = ix(
            pepper_curve::accounts::ClaimDividend {
                claimant: claimant.pubkey(),
                rent_payer: self.admin.pubkey(),
                distributor,
                claim_status: claim_status_pda(&distributor, &claimant.pubkey()),
                system_program: system_program::id(),
            },
            pepper_curve::instruction::ClaimDividend { amount, proof },
        );
        let claimant = claimant.insecure_clone();
        self.send(&[claim], &[&claimant])
    }
}

/// `account` as the program stores it: its discriminator and fields, then
/// zeros up to `8 + space`.
pub fn account_data<T: AccountSerialize>(account: &T, space: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(8 + space);
    account.try_serialize(&mut data).unwrap();
    assert!(data.len() <= 8 + space, "an account longer than its space");
    data.resize(8 + space, 0);
    data
}

/// Tokens a written USDC curve keeps for its pool: `write_tradeable_curve`'s 14M.
pub const USDC_CURVE_MIGRATION_TOKENS: u64 = 14_000_000 * 1_000_000;

// ── The USDC curve (usdc-curve design §10.2, §10.11) ─────────────────────────
impl Env {
    /// `owner`'s associated USDC account, opened by the admin when missing.
    pub fn usdc_account(&mut self, owner: &Pubkey) -> Pubkey {
        let open = spl_associated_token_account::instruction::create_associated_token_account_idempotent(
            &self.admin.pubkey(),
            owner,
            &self.usdc.mint,
            &spl_token::ID,
        );
        self.send(&[open], &[]).unwrap();
        ata(owner, &self.usdc.mint)
    }

    /// Mints `amount` µUSDC into the token account `to`.
    pub fn mint_usdc(&mut self, to: &Pubkey, amount: u64) {
        let mint_to =
            spl_token::instruction::mint_to(&spl_token::ID, &self.usdc.mint, to, &self.usdc.mint_authority.pubkey(), &[], amount)
                .unwrap();
        let authority = self.usdc.mint_authority.insecure_clone();
        self.send(&[mint_to], &[&authority]).unwrap();
    }

    /// `freeze_account` on a USDC account, signed by `authority`.
    pub fn freeze_usdc_by(&mut self, authority: &Keypair, account: &Pubkey) -> Result<(), TransactionError> {
        let freeze =
            spl_token::instruction::freeze_account(&spl_token::ID, account, &self.usdc.mint, &authority.pubkey(), &[]).unwrap();
        self.send(&[freeze], &[authority])
    }

    /// Freezes a USDC account, as Circle can freeze any.
    pub fn freeze_usdc(&mut self, account: &Pubkey) {
        let authority = self.usdc.freeze_authority.insecure_clone();
        self.freeze_usdc_by(&authority, account).unwrap();
    }

    pub fn thaw_usdc(&mut self, account: &Pubkey) {
        let thaw = spl_token::instruction::thaw_account(
            &spl_token::ID,
            account,
            &self.usdc.mint,
            &self.usdc.freeze_authority.pubkey(),
            &[],
        )
        .unwrap();
        let authority = self.usdc.freeze_authority.insecure_clone();
        self.send(&[thaw], &[&authority]).unwrap();
    }

    /// The SPL token account at `address`, of any mint.
    pub fn token_account(&self, address: &Pubkey) -> spl_token::state::Account {
        let account = self.svm.get_account(address).expect("token account exists");
        assert_eq!(account.owner, spl_token::ID, "{address} is not a token account");
        spl_token::state::Account::unpack(&account.data).unwrap()
    }

    /// What the token account at `address` holds.
    pub fn token_amount(&self, address: &Pubkey) -> u64 {
        self.token_account(address).amount
    }

    /// The `UsdcConfig` the suite writes: the admin as its `admin` (the
    /// Squad's vault in production), its `launcher` (the sponsor) and its
    /// `lp_fee_owner`; `self.pauser`; no migration authority; the suite's USDC,
    /// and the fee wallet's USDC account for the fees, the one
    /// `initialize_usdc_config` takes (owned by `config.fanex_fee_wallet`, a key of
    /// its own since the fee-wallet split); the five Raydium fields all-zero, as
    /// `initialize_usdc_config` leaves them until `set_usdc_raydium`.
    pub fn usdc_config(&self) -> UsdcConfig {
        let (_, bump) = Pubkey::find_program_address(&[USDC_CONFIG_SEED], &pepper_curve::ID);
        let admin = self.admin.pubkey();
        UsdcConfig {
            version: 1,
            bump,
            paused: false,
            admin,
            pauser: self.pauser.pubkey(),
            migration_authority: Pubkey::default(),
            usdc_mint: self.usdc.mint,
            fee_usdc_account: ata(&self.fee_wallet.pubkey(), &self.usdc.mint),
            lp_fee_owner: admin,
            raydium_cpmm_program: Pubkey::default(),
            raydium_amm_config: Pubkey::default(),
            raydium_create_pool_fee: Pubkey::default(),
            raydium_lock_program: Pubkey::default(),
            raydium_lock_authority: Pubkey::default(),
            launcher: admin,
            reserved: [0; 96],
        }
    }

    /// `usdc_config()` naming mainnet's Raydium, as `set_usdc_raydium` leaves it: what a launch
    /// needs, which refuses while any of the five is unset (`RaydiumNotSet`, 2026-10-24).
    pub fn usdc_config_with_raydium(&self) -> UsdcConfig {
        let mut config = self.usdc_config();
        config.raydium_cpmm_program = RAYDIUM_CPMM_ID;
        config.raydium_amm_config = MAINNET_AMM_CONFIG;
        config.raydium_create_pool_fee = MAINNET_CREATE_POOL_FEE;
        config.raydium_lock_program = RAYDIUM_LP_LOCK_ID;
        config.raydium_lock_authority = MAINNET_LOCK_AUTHORITY;
        config
    }

    /// Writes `config` at `["usdc_config"]`, as the program would store it.
    pub fn put_usdc_config(&mut self, config: &UsdcConfig) {
        self.put(usdc_config_pda(), account_data(config, UsdcConfig::INIT_SPACE), pepper_curve::ID);
    }

    /// `usdc_config()` written into the bank, its fee account (the fee wallet's ATA) opened and
    /// hardened, as `initialize_usdc_config` takes it since the internal audit (2026-10-29).
    pub fn write_usdc_config(&mut self) -> UsdcConfig {
        self.hardened_fee_account();
        let config = self.usdc_config();
        self.put_usdc_config(&config);
        config
    }

    /// `SetAuthority(CloseAccount → to)` on the token account `account`, signed by `authority`, its
    /// owner or its close authority as SPL Token requires; the admin pays.
    pub fn set_close_authority(&mut self, authority: &Keypair, account: &Pubkey, to: Option<&Pubkey>) -> Result<(), TransactionError> {
        let set = spl_token::instruction::set_authority(
            &spl_token::ID,
            account,
            to,
            spl_token::instruction::AuthorityType::CloseAccount,
            &authority.pubkey(),
            &[],
        )
        .unwrap();
        let authority = authority.insecure_clone();
        if authority.pubkey() == self.admin.pubkey() {
            self.send(&[set], &[])
        } else {
            self.send(&[set], &[&authority])
        }
    }

    /// `wallet`'s USDC ATA opened and hardened as the USDC config's fee account must be (the
    /// internal audit, 2026-10-29): its close authority the `usdc_config` PDA, which no instruction
    /// signs for, set by the wallet.
    pub fn hardened_usdc_account(&mut self, wallet: &Keypair) -> Pubkey {
        let account = self.usdc_account(&wallet.pubkey());
        self.set_close_authority(wallet, &account, Some(&usdc_config_pda())).expect("the fee account hardened");
        account
    }

    /// The fee wallet's USDC ATA, opened and hardened (`hardened_usdc_account`).
    pub fn hardened_fee_account(&mut self) -> Pubkey {
        let fee_wallet = self.fee_wallet.insecure_clone();
        self.hardened_usdc_account(&fee_wallet)
    }

    /// A `CurveUsdc` at `mint`, in Bonding, every counter from `state` (the
    /// golden vectors' shape) in its USDC field: the creator's target the sum of
    /// the two caps, the creator's USDC account their associated one when the
    /// curve takes a creator split (all-zero otherwise, §10.2), and
    /// `USDC_CURVE_MIGRATION_TOKENS` for the pool.
    pub fn curve_usdc(&self, mint: Pubkey, state: &CurveState) -> CurveUsdc {
        let (_, bump) = Pubkey::find_program_address(&[CURVE_SEED, mint.as_ref()], &pepper_curve::ID);
        let (_, usdc_vault_bump) = Pubkey::find_program_address(&[USDC_VAULT_SEED, mint.as_ref()], &pepper_curve::ID);
        let creator = self.creator.pubkey();
        let creator_split_bps = u16::try_from(state.creator_split_bps).expect("a creator split that fits a u16");
        CurveUsdc {
            version: 1,
            bump,
            usdc_vault_bump,
            phase: PHASE_BONDING,
            paused: false,
            migrated: false,
            mint,
            creator,
            creator_usdc_account: if creator_split_bps > 0 { ata(&creator, &self.usdc.mint) } else { Pubkey::default() },
            creator_split_bps,
            fanex_split_bps: u16::try_from(state.fanex_split_bps).expect("a Fanex split that fits a u16"),
            creator_target_usdc: state.creator_cap + state.fanex_cap,
            graduation_threshold_usdc: state.graduation_threshold,
            creator_cap_usdc: state.creator_cap,
            fanex_cap_usdc: state.fanex_cap,
            virtual_usdc_reserves: state.virtual_reserve,
            virtual_token_reserves: state.virtual_token_reserves,
            real_usdc_reserves: state.real_reserve,
            real_token_reserves: state.real_token_reserves,
            migration_token_reserves: USDC_CURVE_MIGRATION_TOKENS,
            creator_received_usdc: state.creator_received,
            fanex_received_usdc: state.fanex_received,
            created_at: state.created_at,
            graduated_at: 0,
            migrated_at: 0,
            reserved: [0; 128],
        }
    }

    /// Writes `curve` at `["curve", mint]` with what it holds, as the program
    /// would store them: its mint, as a launch leaves it; `token_vault` (the
    /// curve PDA's associated account) holding the tokens for sale and the
    /// pool's; `usdc_vault` (the PDA `["usdc_vault", mint]`, of the suite's
    /// USDC), its authority the curve PDA, holding `real_usdc_reserves` minted
    /// for it, so the USDC's supply counts them; and, when the curve names a
    /// creator USDC account that does not exist yet, that account as
    /// `create_curve_usdc` leaves it: the creator's, closable by the launcher
    /// alone (§10.3.4), the written `UsdcConfig`'s, else `usdc_config()`'s.
    pub fn put_curve_usdc(&mut self, curve: &CurveUsdc) {
        let address = curve_pda(&curve.mint);
        self.put_launched_mint(curve.mint);
        self.put_token_account(
            ata(&address, &curve.mint),
            curve.mint,
            address,
            curve.real_token_reserves + curve.migration_token_reserves,
            COption::None,
        );
        let usdc_vault = usdc_vault_pda(&curve.mint);
        self.put_token_account(usdc_vault, self.usdc.mint, address, 0, COption::None);
        if curve.real_usdc_reserves > 0 {
            self.mint_usdc(&usdc_vault, curve.real_usdc_reserves);
        }
        if curve.creator_usdc_account != Pubkey::default() && !self.exists(&curve.creator_usdc_account) {
            let launcher = if self.exists(&usdc_config_pda()) {
                self.fetch::<UsdcConfig>(&usdc_config_pda()).launcher
            } else {
                self.usdc_config().launcher
            };
            self.put_token_account(curve.creator_usdc_account, self.usdc.mint, curve.creator, 0, COption::Some(launcher));
        }
        self.put(address, account_data(curve, CurveUsdc::INIT_SPACE), pepper_curve::ID);
    }

    /// `curve_usdc(state)` written with `put_curve_usdc` at a fresh mint:
    /// `Env::new` already writes a SOL `Curve` at `self.mint`'s address, which
    /// stays, so a test holds a curve of each kind. `self.mint` does not move.
    pub fn write_curve_usdc(&mut self, state: &CurveState) -> CurveUsdc {
        let curve = self.curve_usdc(Pubkey::new_unique(), state);
        self.put_curve_usdc(&curve);
        curve
    }
}

/// The first `E` among a transaction's logs: `emit!` writes each event as
/// "Program data: " and the base64 of its discriminator and its borsh bytes.
pub fn event<E: anchor_lang::Event>(logs: &[String]) -> E {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    logs.iter()
        .filter_map(|line| line.strip_prefix("Program data: "))
        .filter_map(|data| STANDARD.decode(data).ok())
        .find_map(|bytes| {
            let body = bytes.strip_prefix(E::DISCRIMINATOR)?;
            Some(E::try_from_slice(body).expect("the event decodes"))
        })
        .expect("the event was emitted")
}

/// Every `E` among a transaction's logs, in the order they were emitted: an
/// empty list where `event` would panic, so a test can say an event is absent.
pub fn events_of<E: anchor_lang::Event>(logs: &[String]) -> Vec<E> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    logs.iter()
        .filter_map(|line| line.strip_prefix("Program data: "))
        .filter_map(|data| STANDARD.decode(data).ok())
        .filter_map(|bytes| {
            let body = bytes.strip_prefix(E::DISCRIMINATOR)?;
            Some(E::try_from_slice(body).expect("the event decodes"))
        })
        .collect()
}

// ── The golden vectors (usdc-curve design §10.9) ─────────────────────────────

/// Written by the math crate's own test, which fails when the file and its
/// functions disagree.
pub const VECTORS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../programs/pepper_curve_math/vectors/curve-v1.json");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorOp {
    /// `amount` µUSDC in, at the clock `now`.
    Buy { amount: u64, now: i64 },
    /// `amount` token base units in.
    Sell { amount: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorOutcome {
    Bought(BuyQuote),
    Sold(SellQuote),
    Refused(MathError),
}

/// One case of `curve-v1.json`: a curve's state, an operation on it, and what
/// `quote_buy` or `quote_sell` returned for it.
#[derive(Clone, Debug)]
pub struct Vector {
    pub name: String,
    pub state: CurveState,
    pub op: VectorOp,
    pub outcome: VectorOutcome,
}

impl Vector {
    /// What the crate returns for this case now.
    pub fn quote(&self) -> VectorOutcome {
        match self.op {
            VectorOp::Buy { amount, now } => {
                quote_buy(&self.state, amount, now).map_or_else(VectorOutcome::Refused, VectorOutcome::Bought)
            }
            VectorOp::Sell { amount } => quote_sell(&self.state, amount).map_or_else(VectorOutcome::Refused, VectorOutcome::Sold),
        }
    }
}

/// Every case of `curve-v1.json`, in the file's order, read into the math
/// crate's own types. Amounts are strings in the file (a JSON number cannot
/// hold every u64), clocks are numbers; an object with a key the reader does
/// not know, or without one it needs, fails the read.
pub fn vectors() -> Vec<Vector> {
    let text = std::fs::read_to_string(VECTORS_PATH).unwrap_or_else(|e| panic!("{VECTORS_PATH}: {e}"));
    let file: Value = serde_json::from_str(&text).expect("curve-v1.json is JSON");
    assert_keys(&file, &["version", "source", "cases"], "the file");
    assert_eq!(file["version"], 1, "curve-v1.json's version");
    file["cases"].as_array().expect("an array of cases").iter().map(read_vector).collect()
}

/// The vector named `name`.
pub fn vector(name: &str) -> Vector {
    vectors().into_iter().find(|v| v.name == name).unwrap_or_else(|| panic!("no vector named {name:?}"))
}

fn read_vector(case: &Value) -> Vector {
    let name = case["name"].as_str().expect("a case's name").to_string();
    let amount_in = amount(case, "amount");
    let op = match case["op"].as_str() {
        Some("buy") => VectorOp::Buy {
            amount: amount_in,
            now: case["now"].as_i64().unwrap_or_else(|| panic!("{name}: a buy's clock")),
        },
        Some("sell") => VectorOp::Sell { amount: amount_in },
        other => panic!("{name}: op {other:?}"),
    };
    let ending = if case.get("error").is_some() { "error" } else { "result" };
    let keys: &[&str] = match op {
        VectorOp::Buy { .. } => &["name", "op", "amount", "now", "state", ending],
        VectorOp::Sell { .. } => &["name", "op", "amount", "state", ending],
    };
    assert_keys(case, keys, &name);
    let outcome = match (op, ending) {
        (_, "error") => VectorOutcome::Refused(math_error(case["error"].as_str().unwrap_or_default(), &name)),
        (VectorOp::Buy { .. }, _) => {
            let r = &case["result"];
            assert_keys(
                r,
                &[
                    "charged",
                    "tokens_out",
                    "platform_fee",
                    "creator_amount",
                    "fanex_amount",
                    "curve_amount",
                    "launch_window",
                    "graduates",
                    "after",
                ],
                &name,
            );
            VectorOutcome::Bought(BuyQuote {
                charged: amount(r, "charged"),
                tokens_out: amount(r, "tokens_out"),
                platform_fee: amount(r, "platform_fee"),
                creator_amount: amount(r, "creator_amount"),
                fanex_amount: amount(r, "fanex_amount"),
                curve_amount: amount(r, "curve_amount"),
                launch_window: r["launch_window"].as_bool().unwrap_or_else(|| panic!("{name}: launch_window")),
                graduates: r["graduates"].as_bool().unwrap_or_else(|| panic!("{name}: graduates")),
                after: read_state(&r["after"], &name),
            })
        }
        (VectorOp::Sell { .. }, _) => {
            let r = &case["result"];
            assert_keys(r, &["reserve_out", "platform_fee", "proceeds", "after"], &name);
            VectorOutcome::Sold(SellQuote {
                reserve_out: amount(r, "reserve_out"),
                platform_fee: amount(r, "platform_fee"),
                proceeds: amount(r, "proceeds"),
                after: read_state(&r["after"], &name),
            })
        }
    };
    Vector { state: read_state(&case["state"], &name), name, op, outcome }
}

fn read_state(state: &Value, name: &str) -> CurveState {
    assert_keys(
        state,
        &[
            "virtual_reserve",
            "virtual_token_reserves",
            "real_reserve",
            "real_token_reserves",
            "creator_split_bps",
            "fanex_split_bps",
            "creator_cap",
            "fanex_cap",
            "creator_received",
            "fanex_received",
            "graduation_threshold",
            "created_at",
        ],
        name,
    );
    CurveState {
        virtual_reserve: amount(state, "virtual_reserve"),
        virtual_token_reserves: amount(state, "virtual_token_reserves"),
        real_reserve: amount(state, "real_reserve"),
        real_token_reserves: amount(state, "real_token_reserves"),
        creator_split_bps: amount(state, "creator_split_bps"),
        fanex_split_bps: amount(state, "fanex_split_bps"),
        creator_cap: amount(state, "creator_cap"),
        fanex_cap: amount(state, "fanex_cap"),
        creator_received: amount(state, "creator_received"),
        fanex_received: amount(state, "fanex_received"),
        graduation_threshold: amount(state, "graduation_threshold"),
        created_at: state["created_at"].as_i64().unwrap_or_else(|| panic!("{name}: created_at")),
    }
}

/// A u64 the file writes as a decimal string.
fn amount(object: &Value, key: &str) -> u64 {
    let text = object[key].as_str().unwrap_or_else(|| panic!("{key}: a decimal string"));
    text.parse().unwrap_or_else(|e| panic!("{key} = {text:?}: {e}"))
}

fn math_error(name: &str, case: &str) -> MathError {
    [MathError::MathOverflow, MathError::ZeroAmount, MathError::CurveComplete, MathError::InsufficientReserves]
        .into_iter()
        .find(|e| e.name() == name)
        .unwrap_or_else(|| panic!("{case}: an error the crate does not name, {name:?}"))
}

/// `object`'s keys are exactly `expected`, in any order.
fn assert_keys(object: &Value, expected: &[&str], what: &str) {
    let mut keys: Vec<&str> = object.as_object().unwrap_or_else(|| panic!("{what}: an object")).keys().map(String::as_str).collect();
    let mut expected = expected.to_vec();
    keys.sort_unstable();
    expected.sort_unstable();
    assert_eq!(keys, expected, "{what}: its keys");
}

// ── The program under the upgradeable loader, the fixture accounts (usdc-curve design §10.11) ──
//
// `Env::new` loads the program with `add_program_from_file`, as LiteSVM's loader v2 runs one: no
// ProgramData, so no upgrade authority. `set_usdc_raydium` reads that authority, so its suite
// loads the program as a cluster runs it (`Env::upgradeable`).

use anchor_lang::solana_program::bpf_loader_upgradeable;

/// A ProgramData's header, before the ELF: `UpgradeableLoaderState::size_of_programdata_metadata`.
pub const PROGRAM_DATA_HEADER: usize = 45;

/// The ProgramData address of `program_id`: the upgradeable loader's PDA of the program id alone.
pub fn program_data_pda(program_id: &Pubkey) -> Pubkey {
    bpf_loader_upgradeable::get_program_data_address(program_id)
}

/// A ProgramData's bytes as the loader writes them: the bincode of
/// `UpgradeableLoaderState::ProgramData` (the tag 3 as a u32, `slot` as a u64, then the authority
/// as an `Option<Pubkey>`: one byte, then its 32, left zero here when there is none, so the header
/// is 45 bytes either way, as the loader reserves it), then the ELF. A cluster's `--final` writes
/// the 13 bytes of the None header over a key's and leaves that key's bytes after the option byte:
/// `usdc_raydium.rs` makes that one through the loader's own `SetAuthority`. Written by hand: Anchor's
/// `ProgramData` serializer writes nothing, and the suite has no bincode of its own.
pub fn program_data_bytes(slot: u64, upgrade_authority: Option<Pubkey>, elf: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(PROGRAM_DATA_HEADER + elf.len());
    data.extend_from_slice(&3u32.to_le_bytes());
    data.extend_from_slice(&slot.to_le_bytes());
    match upgrade_authority {
        Some(authority) => {
            data.push(1);
            data.extend_from_slice(authority.as_ref());
        }
        None => data.extend_from_slice(&[0u8; 33]),
    }
    assert_eq!(data.len(), PROGRAM_DATA_HEADER);
    data.extend_from_slice(elf);
    data
}

/// Loads `elf` at `program_id` under the upgradeable loader, as a deploy leaves it, replacing
/// whatever ran there: the ProgramData first, since LiteSVM compiles the ELF from it when the
/// Program account arrives, then the Program account (the tag 2 as a u32 and the ProgramData's
/// address, 36 bytes), executable. `upgrade_authority` `None` is an immutable program whose header
/// never recorded a key (`program_data_bytes`). Returns the ProgramData's address.
pub fn load_upgradeable_program(svm: &mut LiteSVM, program_id: Pubkey, elf: &[u8], upgrade_authority: Option<Pubkey>) -> Pubkey {
    let loader = bpf_loader_upgradeable::id();
    let program_data = program_data_pda(&program_id);
    let slot = svm.get_sysvar::<Clock>().slot;
    let data = program_data_bytes(slot, upgrade_authority, elf);
    let lamports = svm.minimum_balance_for_rent_exemption(data.len());
    svm.set_account(program_data, Account { lamports, data, owner: loader, executable: false, rent_epoch: 0 }).unwrap();
    let mut program = 2u32.to_le_bytes().to_vec();
    program.extend_from_slice(program_data.as_ref());
    let lamports = svm.minimum_balance_for_rent_exemption(program.len());
    svm.set_account(program_id, Account { lamports, data: program, owner: loader, executable: true, rent_epoch: 0 })
        .expect("the program loads from its ProgramData");
    program_data
}

impl Env {
    /// As `new`, then the program loaded again under the upgradeable loader, `upgrade_authority`
    /// recorded in its ProgramData (`None`: immutable), as both clusters run it. Every other suite
    /// takes `new`, whose program has no ProgramData.
    pub fn upgradeable(upgrade_authority: Option<Pubkey>) -> Self {
        let mut env = Self::new();
        let so = program_so();
        let elf = std::fs::read(&so).unwrap_or_else(|e| panic!("{so}: {e}"));
        load_upgradeable_program(&mut env.svm, pepper_curve::ID, &elf, upgrade_authority);
        env
    }

    /// As `upgradeable`, the admin the upgrade authority: a cluster before the hand-over to the
    /// vault, where the admin's key both administers and upgrades the program.
    pub fn upgradeable_by_admin() -> Self {
        let mut env = Self::new();
        let so = program_so();
        let elf = std::fs::read(&so).unwrap_or_else(|e| panic!("{so}: {e}"));
        let admin = env.admin.pubkey();
        load_upgradeable_program(&mut env.svm, pepper_curve::ID, &elf, Some(admin));
        env
    }
}

/// `initialize_usdc_config` as it is signed since the internal audit (2026-10-29): `payer` pays,
/// `upgrade_authority` signs as the program's ProgramData records it, `admin` becomes
/// `UsdcConfig.admin`, `fee` is the fee wallet's hardened USDC ATA.
pub fn init_usdc_config_ix(
    payer: &Pubkey,
    upgrade_authority: &Pubkey,
    admin: &Pubkey,
    usdc_mint: &Pubkey,
    fee: &Pubkey,
    params: &pepper_curve::instructions::InitializeUsdcConfigParams,
) -> Instruction {
    ix(
        pepper_curve::accounts::InitializeUsdcConfig {
            payer: *payer,
            upgrade_authority: *upgrade_authority,
            program_data: program_data_pda(&pepper_curve::ID),
            admin: *admin,
            global_config: config_pda(),
            usdc_config: usdc_config_pda(),
            usdc_mint: *usdc_mint,
            fee_usdc_account: *fee,
            system_program: system_program::id(),
        },
        pepper_curve::instruction::InitializeUsdcConfig { params: params.clone() },
    )
}

impl Env {

    /// The account fixture `name` written at its address (`fixture_account`); returns the address.
    pub fn load_fixture_account(&mut self, name: &str) -> Pubkey {
        let (address, account) = fixture_account(name);
        self.svm.set_account(address, account).unwrap();
        address
    }
}

/// An account fixture of `fixtures/`, as the CLI writes it (`solana account --output json`): its
/// address, lamports, data (base64), owner, `executable` and `rentEpoch`. The `space` it states
/// is held to the data's length.
pub fn fixture_account(name: &str) -> (Pubkey, Account) {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let path = fixture_path(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let file: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
    let pubkey = |value: &Value, what: &str| -> Pubkey {
        value.as_str().and_then(|s| s.parse().ok()).unwrap_or_else(|| panic!("{name}: {what}"))
    };
    let address = pubkey(&file["pubkey"], "pubkey");
    let account = &file["account"];
    assert_eq!(account["data"][1], "base64", "{name}: the data's encoding");
    let data = STANDARD.decode(account["data"][0].as_str().unwrap_or_default()).unwrap_or_else(|e| panic!("{name}: data, {e}"));
    assert_eq!(account["space"].as_u64(), Some(data.len() as u64), "{name}: space");
    let account = Account {
        lamports: account["lamports"].as_u64().unwrap_or_else(|| panic!("{name}: lamports")),
        data,
        owner: pubkey(&account["owner"], "owner"),
        executable: account["executable"].as_bool().unwrap_or_else(|| panic!("{name}: executable")),
        rent_epoch: account["rentEpoch"].as_u64().unwrap_or_else(|| panic!("{name}: rentEpoch")),
    };
    (address, account)
}
