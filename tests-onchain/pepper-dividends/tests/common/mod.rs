#![allow(dead_code)]

//! The LiteSVM harness of `pepper_dividends`, run against its built `.so` beside
//! the real `pepper_curve.so` (docs/solana/PEPPER-DIVIDENDS-PROGRAM.md §8).
//!
//! - **Both programs under the upgradeable loader**, as a cluster runs them:
//!   `pepper_dividends` at its id with a ProgramData that records `Env::admin`
//!   as its upgrade authority (what `initialize_config` reads), `pepper_curve` at
//!   `CDPwGW…`, from `PEPPER_DIVIDENDS_SO` and `PEPPER_CURVE_SO`.
//! - **A test USDC** made by the token program itself: a classic mint of 6
//!   decimals with a freeze authority, as Circle's has on both clusters.
//! - **Writers** of a `DividendsConfig`, a `UsdcDistributor` (any amounts and
//!   bitmap), a `MonthRecord` and a funded vault, through the crate's own
//!   serializer, so a test of one instruction starts from the accounts another
//!   would leave, with no other instruction run first.
//! - **`pepper_curve`'s side**: its config and a `Curve` written through its
//!   crate; a `Curve` and a `CurveUsdc` launched by its own `create_curve` and
//!   `create_curve_usdc` (track A's program, merged on 2026-10-26), the real
//!   Metaplex Token Metadata loaded from track A's fixture; and its v1 and v2
//!   months opened, claimed and closed by the real program.
//! - The clock (and `past_review_window`, a month's claims opening a day after
//!   its funding), `send_logged` with `event` for the events a transaction
//!   emitted, and `build_tree`, the app's tree.
//!
//! Every rent is the bank's (`minimum_balance_for_rent_exemption`): LiteSVM's
//! rent is not mainnet's. Setup transactions are paid by `faucet`, so a test
//! reads every other wallet's lamports as the instruction under test left them.

use anchor_lang::solana_program::bpf_loader_upgradeable;
use anchor_lang::solana_program::keccak::hashv;
use anchor_lang::solana_program::program_option::COption;
use anchor_lang::solana_program::program_pack::Pack;
use anchor_lang::{AccountDeserialize, AccountSerialize, InstructionData, Space, ToAccountMetas};
use anchor_spl::associated_token::get_associated_token_address as ata;
use anchor_spl::associated_token::spl_associated_token_account;
use anchor_spl::token::spl_token;
use litesvm::LiteSVM;
use pepper_dividends::constants::{ACCOUNT_VERSION, CONFIG_SEED, DISTRIBUTOR_SEED, MONTH_SEED, MONTH_STATUS_OPEN};
use pepper_dividends::instructions::{ConfigKeys, ConfigUpdate};
use pepper_dividends::state::{DividendsConfig, MonthRecord, UsdcDistributor};
use pepper_dividends::validation::claim_registry_bytes;
use solana_sdk::{
    account::Account,
    clock::Clock,
    instruction::{Instruction, InstructionError},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction, system_program,
    transaction::{Transaction, TransactionError},
};

pub const SOL: u64 = 1_000_000_000;
pub const DAY: i64 = 86_400;
/// 2026-11-01 00:00:00 UTC: every `Env` starts there.
pub const NOV_1_2026: i64 = 1_793_491_200;
/// The month the tests open, YYYYMM.
pub const MONTH: i64 = 202_611;
/// µUSDC in one USDC.
pub const USDC: u64 = 1_000_000;
/// The test USDC's decimals, Circle's.
pub const USDC_DECIMALS: u8 = 6;
/// LiteSVM's fee for each signature a transaction carries (its default, mainnet's base fee).
pub const FEE_PER_SIGNATURE: u64 = 5_000;
/// The system program's `AccountAlreadyInUse`: an `init` of an account that exists.
pub const SYSTEM_ACCOUNT_ALREADY_IN_USE: u32 = 0;
/// SPL Token's `AccountFrozen`: a transfer from or to an account its mint's freeze authority froze.
pub const SPL_TOKEN_ACCOUNT_FROZEN: u32 = 17;

pub fn dividends_so() -> String {
    std::env::var("PEPPER_DIVIDENDS_SO").unwrap_or_else(|_| "../../target/deploy/pepper_dividends.so".to_string())
}

pub fn curve_so() -> String {
    std::env::var("PEPPER_CURVE_SO").unwrap_or_else(|_| "../../target/deploy/pepper_curve.so".to_string())
}

/// Metaplex Token Metadata, which both launches call: the mainnet build track A's suite commits
/// and pins (`tests-onchain/pepper-curve/fixtures/SHA256SUMS`), read from there, never copied.
pub fn metaplex_so() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../pepper-curve/fixtures/mpl_token_metadata.so")
}

/// The compute units the app asks for a launch (`CREATE_CU_LIMIT`, `lib/solana/pepper-curve/launch.ts`):
/// the Metaplex CPI takes a launch past the default 200,000.
pub const CREATE_CU_LIMIT: u32 = 500_000;

/// The compute budget program's `SetComputeUnitLimit`, written by hand as the app's builder sends it.
pub fn compute_unit_limit(units: u32) -> Instruction {
    let mut data = vec![2u8];
    data.extend_from_slice(&units.to_le_bytes());
    Instruction { program_id: Pubkey::from_str_const("ComputeBudget111111111111111111111111111111"), accounts: vec![], data }
}

/// Every launch's token split: 80M tokens on the curve, 18M for the pool, 2% vested.
const LAUNCH_CURVE_SUPPLY: u64 = 80_000_000 * 1_000_000;
const LAUNCH_MIGRATION_SUPPLY: u64 = 18_000_000 * 1_000_000;
const LAUNCH_VESTING_SUPPLY: u64 = 2_000_000 * 1_000_000;
const LAUNCH_NAME: &str = "Pepper Dividends Suite";
/// Ten bytes: the USDC curve's `MAX_USDC_SYMBOL_LEN`, Metaplex's own limit.
const LAUNCH_SYMBOL: &str = "PEPPERDIVS";
const LAUNCH_URI: &str = "https://pepper.fan/api/solana/metadata/pepperdivs";

// ── Errors ───────────────────────────────────────────────────────────────────

/// The custom error code of a `DividendsError`.
pub fn code(e: pepper_dividends::errors::DividendsError) -> u32 {
    anchor_lang::error::ERROR_CODE_OFFSET + e as u32
}

/// The number of one of Anchor's own errors (`ConstraintSeeds` 2006,
/// `InvalidProgramId` 3008, …), which a test pins like the program's.
pub fn anchor_code(e: anchor_lang::error::ErrorCode) -> u32 {
    e.into()
}

pub fn expect_err<T: std::fmt::Debug>(result: Result<T, TransactionError>, expected: u32) {
    match result {
        Err(TransactionError::InstructionError(_, InstructionError::Custom(c))) => {
            assert_eq!(c, expected, "custom error {c}, expected {expected}")
        }
        other => panic!("expected custom error {expected}, got {other:?}"),
    }
}

// ── Instructions and addresses ───────────────────────────────────────────────

/// An instruction of `pepper_dividends`.
pub fn ix<A: ToAccountMetas, D: InstructionData>(accounts: A, data: D) -> Instruction {
    Instruction { program_id: pepper_dividends::ID, accounts: accounts.to_account_metas(None), data: data.data() }
}

/// An instruction of `pepper_curve`.
pub fn curve_ix<A: ToAccountMetas, D: InstructionData>(accounts: A, data: D) -> Instruction {
    Instruction { program_id: pepper_curve::ID, accounts: accounts.to_account_metas(None), data: data.data() }
}

pub fn pda(seeds: &[&[u8]]) -> (Pubkey, u8) {
    Pubkey::find_program_address(seeds, &pepper_dividends::ID)
}
pub fn config_pda() -> Pubkey {
    pda(&[CONFIG_SEED]).0
}
pub fn distributor_pda(mint: &Pubkey, month: i64) -> Pubkey {
    pda(&[DISTRIBUTOR_SEED, mint.as_ref(), &month.to_le_bytes()]).0
}
pub fn month_record_pda(mint: &Pubkey, month: i64) -> Pubkey {
    pda(&[MONTH_SEED, mint.as_ref(), &month.to_le_bytes()]).0
}

pub fn curve_pda_of(seeds: &[&[u8]]) -> (Pubkey, u8) {
    Pubkey::find_program_address(seeds, &pepper_curve::ID)
}
pub fn curve_config_pda() -> Pubkey {
    curve_pda_of(&[pepper_curve::constants::CONFIG_SEED]).0
}
/// `["curve", mint]` under `pepper_curve`: a SOL `Curve` or a `CurveUsdc`.
pub fn curve_pda(mint: &Pubkey) -> Pubkey {
    curve_pda_of(&[pepper_curve::constants::CURVE_SEED, mint.as_ref()]).0
}
/// `["distributor", mint, month]` under `pepper_curve`: its v1 or v2 month, the
/// address `init_distributor` reads as `legacy_distributor`.
pub fn legacy_distributor_pda(mint: &Pubkey, month: i64) -> Pubkey {
    curve_pda_of(&[pepper_curve::constants::DISTRIBUTOR_SEED, mint.as_ref(), &month.to_le_bytes()]).0
}
pub fn claim_status_pda(distributor: &Pubkey, claimant: &Pubkey) -> Pubkey {
    curve_pda_of(&[pepper_curve::constants::CLAIM_SEED, distributor.as_ref(), claimant.as_ref()]).0
}
/// `["usdc_config"]` under `pepper_curve`: the USDC side's config.
pub fn usdc_config_pda() -> Pubkey {
    curve_pda_of(&[pepper_curve::constants::USDC_CONFIG_SEED]).0
}
/// Metaplex's metadata account of `mint`.
pub fn metadata_pda(mint: &Pubkey) -> Pubkey {
    anchor_spl::metadata::mpl_token_metadata::accounts::Metadata::find_pda(mint).0
}

// ── The tree (lib/solana/pepper-curve/merkle.ts) ─────────────────────────────

/// A Merkle tree as the app builds it: sorted-pair keccak, an odd node promoted
/// unchanged, leaves in the given order.
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
        let next: Vec<[u8; 32]> =
            prev.chunks(2).map(|pair| if pair.len() == 2 { hash_pair(&pair[0], &pair[1]) } else { pair[0] }).collect();
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

/// A month's holders, with distinct amounts.
pub fn holders(n: usize) -> Vec<(Keypair, u64)> {
    (0..n).map(|i| (Keypair::new(), USDC + i as u64 * 1_000)).collect()
}

/// This program's leaves for `holders` in a month of `distributor` paid in `usdc_mint`.
pub fn dividend_leaves(distributor: &Pubkey, usdc_mint: &Pubkey, holders: &[(Keypair, u64)]) -> Vec<[u8; 32]> {
    holders
        .iter()
        .enumerate()
        .map(|(i, (holder, amount))| {
            pepper_dividends::math::dividend_leaf(distributor, usdc_mint, i as u32, &holder.pubkey(), *amount)
        })
        .collect()
}

// ── The upgradeable loader (as track A's pepper-curve harness) ───────────────

/// A ProgramData's header, before the ELF: `UpgradeableLoaderState::size_of_programdata_metadata`.
pub const PROGRAM_DATA_HEADER: usize = 45;

/// The ProgramData address of `program_id`: the upgradeable loader's PDA of the program id alone.
pub fn program_data_pda(program_id: &Pubkey) -> Pubkey {
    bpf_loader_upgradeable::get_program_data_address(program_id)
}

/// A ProgramData's bytes as the loader writes them: the bincode of
/// `UpgradeableLoaderState::ProgramData` (the tag 3 as a u32, `slot` as a u64, then the authority
/// as an `Option<Pubkey>`: one byte, then its 32, left zero when there is none, so the header is 45
/// bytes either way), then the ELF.
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
/// address), executable. Returns the ProgramData's address.
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

fn read_so(path: &str, build: &str) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("{path}: {e} (build first: {build})"))
}

// ── The test USDC ────────────────────────────────────────────────────────────

/// The suite's USDC: a classic SPL mint of 6 decimals with a freeze authority, as
/// Circle's USDC has on both clusters, so a frozen account's refusals can be made.
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
            system_instruction::create_account(&payer.pubkey(), &mint.pubkey(), rent, spl_token::state::Mint::LEN as u64, &spl_token::ID),
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

// ── The environment ──────────────────────────────────────────────────────────

/// What a sent transaction left: its logs, where `emit!` writes the events, and
/// the compute units it used.
#[derive(Debug)]
pub struct Sent {
    pub logs: Vec<String>,
    pub compute_units: u64,
}

/// A month of `holders` written into the bank as a funding leaves it (`Env::write_month`).
pub struct Month {
    pub mint: Pubkey,
    pub month: i64,
    pub distributor: Pubkey,
    pub vault: Pubkey,
    pub holders: Vec<(Keypair, u64)>,
    pub tree: Tree,
    pub total: u64,
}

impl Month {
    /// Leaf `index`'s holder, amount and proof.
    pub fn leaf(&self, index: usize) -> (&Keypair, u64, Vec<[u8; 32]>) {
        (&self.holders[index].0, self.holders[index].1, self.tree.proofs[index].clone())
    }
}

/// The accounts of a `claim`, each derived from the month and the claimant unless a test replaces it.
#[derive(Clone, Debug)]
pub struct ClaimAccounts {
    pub distributor: Pubkey,
    pub vault: Pubkey,
    pub claimant: Pubkey,
    pub claimant_usdc: Pubkey,
    pub usdc_mint: Pubkey,
    pub token_program: Pubkey,
}

/// The accounts of a `close_distributor` or a `void_distributor` beside its signer, each the
/// month's own, as its distributor records them (`Env::settle_accounts`), unless a test replaces it.
#[derive(Clone, Debug)]
pub struct SettleAccounts {
    pub distributor: Pubkey,
    pub month_record: Pubkey,
    pub vault: Pubkey,
    pub funder_usdc_account: Pubkey,
    pub rent_payer: Pubkey,
    pub usdc_mint: Pubkey,
    pub token_program: Pubkey,
}

/// The accounts of a `top_up_distributor` beside its two signers, each the month's own, as its
/// distributor records them (`Env::top_up_accounts`), unless a test replaces it.
#[derive(Clone, Debug)]
pub struct TopUpAccounts {
    pub distributor: Pubkey,
    pub vault: Pubkey,
    pub funder_usdc_account: Pubkey,
    pub usdc_mint: Pubkey,
    pub token_program: Pubkey,
}

/// The accounts and arguments of an `init_distributor`, each derived from the month's mint and
/// month (`Env::funding`) unless a test replaces it.
#[derive(Clone, Debug)]
pub struct Funding {
    pub funder: Pubkey,
    pub approver: Pubkey,
    pub rent_payer: Pubkey,
    pub mint: Pubkey,
    pub month: i64,
    pub merkle_root: [u8; 32],
    pub total_amount: u64,
    pub expires_at: i64,
    pub leaf_count: u32,
    pub curve: Pubkey,
    pub legacy_distributor: Pubkey,
    pub funder_usdc: Pubkey,
    pub usdc_mint: Pubkey,
    pub token_program: Pubkey,
}

/// How `Env` loads `pepper_dividends`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramLoad {
    /// Under the upgradeable loader, `Env::admin` its recorded upgrade authority (`Env::new`).
    UpgradeableByAdmin,
    /// Under the upgradeable loader, immutable: a header that records no authority.
    Immutable,
    /// Through LiteSVM's `add_program_from_file`: no ProgramData at all.
    WithoutProgramData,
}

pub struct Env {
    pub svm: LiteSVM,
    /// Pays every setup transaction, so no other wallet's lamports move but by
    /// the instruction under test.
    pub faucet: Keypair,
    /// `pepper_dividends`' upgrade authority, recorded in its ProgramData, and the
    /// config's admin: one key, as the upgrade Squad's vault is both on mainnet.
    pub admin: Keypair,
    pub pauser: Keypair,
    pub approver: Keypair,
    pub closer: Keypair,
    /// `pepper_funder`.
    pub pepper_funder: Keypair,
    /// The dividend payer: `payer_funder`, and the rent payer of every month.
    pub payer: Keypair,
    /// The sponsor: a claim's fee payer, which fronts a holder's missing USDC account.
    pub sponsor: Keypair,
    /// The creator of the SOL `Curve` written at `mint`'s curve address.
    pub creator: Keypair,
    /// `pepper_curve`'s config admin, fee wallet and graduation authority: since
    /// the fee-wallet split, the approver of its v2 months (whose funder is the
    /// curve's creator or the dividend payer) and the only funder of a v1 one.
    pub curve_admin: Keypair,
    /// The token whose months the tests open.
    pub mint: Pubkey,
    pub usdc: TestUsdc,
}

impl Env {
    /// Both programs loaded, the test USDC made, the clock at `NOV_1_2026`,
    /// `pepper_curve`'s config and a SOL `Curve` for `mint` written. No
    /// `DividendsConfig` yet: `write_config` writes one.
    pub fn new() -> Self {
        Self::loaded(ProgramLoad::UpgradeableByAdmin)
    }

    /// As `new`, `pepper_dividends` loaded as `load` says.
    pub fn loaded(load: ProgramLoad) -> Self {
        Self::with_svm(LiteSVM::new(), load)
    }

    /// As `new`, with signature verification off, for a signer whose key the suite
    /// does not hold. The program still sees it as a signer.
    pub fn unverified() -> Self {
        Self::with_svm(LiteSVM::new().with_sigverify(false), ProgramLoad::UpgradeableByAdmin)
    }

    fn with_svm(svm: LiteSVM, load: ProgramLoad) -> Self {
        // Every log line kept: LiteSVM keeps 10 000 bytes by default, and a long
        // transaction would lose the events `event` reads.
        let mut svm = svm.with_log_bytes_limit(None);
        let faucet = Keypair::new();
        svm.airdrop(&faucet.pubkey(), 10_000 * SOL).unwrap();
        let admin = Keypair::new();
        let curve_admin = Keypair::new();
        let dividends = read_so(&dividends_so(), "cargo-build-sbf --manifest-path programs/pepper_dividends/Cargo.toml");
        match load {
            ProgramLoad::UpgradeableByAdmin => {
                load_upgradeable_program(&mut svm, pepper_dividends::ID, &dividends, Some(admin.pubkey()));
            }
            ProgramLoad::Immutable => {
                load_upgradeable_program(&mut svm, pepper_dividends::ID, &dividends, None);
            }
            ProgramLoad::WithoutProgramData => svm.add_program(pepper_dividends::ID, &dividends),
        }
        let curve = read_so(&curve_so(), "cargo-build-sbf --manifest-path programs/pepper_curve/Cargo.toml");
        load_upgradeable_program(&mut svm, pepper_curve::ID, &curve, Some(curve_admin.pubkey()));
        let usdc = TestUsdc::create(&mut svm, &faucet);
        let mut env = Self {
            svm,
            faucet,
            admin,
            pauser: Keypair::new(),
            approver: Keypair::new(),
            closer: Keypair::new(),
            pepper_funder: Keypair::new(),
            payer: Keypair::new(),
            sponsor: Keypair::new(),
            creator: Keypair::new(),
            curve_admin,
            mint: Pubkey::new_unique(),
            usdc,
        };
        for wallet in [
            env.admin.pubkey(),
            env.pauser.pubkey(),
            env.approver.pubkey(),
            env.closer.pubkey(),
            env.pepper_funder.pubkey(),
            env.payer.pubkey(),
            env.sponsor.pubkey(),
            env.creator.pubkey(),
            env.curve_admin.pubkey(),
        ] {
            env.svm.airdrop(&wallet, 100 * SOL).unwrap();
        }
        env.set_time(NOV_1_2026);
        env.put_curve_config();
        let (mint, creator) = (env.mint, env.creator.pubkey());
        env.put_curve(mint, creator);
        env
    }

    // ── Sending ──────────────────────────────────────────────────────────────

    /// Paid by `payer`, signed by it and by `extra`, then the blockhash expired so
    /// an identical transaction can be sent again.
    pub fn send_paid_by(&mut self, payer: &Keypair, ixs: &[Instruction], extra: &[&Keypair]) -> Result<(), TransactionError> {
        self.send_logged_paid_by(payer, ixs, extra).map(|_| ())
    }

    /// As `send_paid_by`, handing back the logs and the compute units.
    pub fn send_logged_paid_by(&mut self, payer: &Keypair, ixs: &[Instruction], extra: &[&Keypair]) -> Result<Sent, TransactionError> {
        let mut signers: Vec<&Keypair> = vec![payer];
        signers.extend(extra.iter().copied().filter(|k| k.pubkey() != payer.pubkey()));
        let tx = Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &signers, self.svm.latest_blockhash());
        let result = self
            .svm
            .send_transaction(tx)
            .map(|meta| Sent { logs: meta.logs, compute_units: meta.compute_units_consumed })
            .map_err(|failed| failed.err);
        self.svm.expire_blockhash();
        result
    }

    /// A setup transaction: paid by the faucet, signed by `extra` too.
    pub fn setup(&mut self, ixs: &[Instruction], extra: &[&Keypair]) {
        let faucet = self.faucet.insecure_clone();
        self.send_paid_by(&faucet, ixs, extra).expect("a setup transaction");
    }

    pub fn set_time(&mut self, unix_timestamp: i64) {
        let mut clock: Clock = self.svm.get_sysvar();
        clock.unix_timestamp = unix_timestamp;
        self.svm.set_sysvar(&clock);
    }

    pub fn now(&self) -> i64 {
        self.svm.get_sysvar::<Clock>().unix_timestamp
    }

    /// The clock moved a review window on (`REVIEW_WINDOW_SECONDS`, a day): a month funded at the
    /// second this runs pays its claims from then.
    pub fn past_review_window(&mut self) {
        let now = self.now();
        self.set_time(now + pepper_dividends::constants::REVIEW_WINDOW_SECONDS);
    }

    // ── Reading the bank ─────────────────────────────────────────────────────

    pub fn lamports(&self, address: &Pubkey) -> u64 {
        self.svm.get_account(address).map(|a| a.lamports).unwrap_or(0)
    }

    pub fn exists(&self, address: &Pubkey) -> bool {
        self.svm.get_account(address).map(|a| a.lamports > 0).unwrap_or(false)
    }

    /// The bytes an address holds: none when it holds no account, or lamports alone.
    pub fn data_len(&self, address: &Pubkey) -> usize {
        self.svm.get_account(address).map(|a| a.data.len()).unwrap_or(0)
    }

    pub fn fetch<T: AccountDeserialize>(&self, address: &Pubkey) -> T {
        let account = self.svm.get_account(address).expect("account exists");
        T::try_deserialize(&mut account.data.as_slice()).unwrap()
    }

    /// A new wallet holding `lamports`.
    pub fn funded_wallet(&mut self, lamports: u64) -> Keypair {
        let wallet = Keypair::new();
        self.svm.airdrop(&wallet.pubkey(), lamports).unwrap();
        wallet
    }

    /// The bank's rent-exempt minimum for `len` bytes.
    pub fn rent(&self, len: usize) -> u64 {
        self.svm.minimum_balance_for_rent_exemption(len)
    }

    /// Writes `data` at `address`, owned by `owner`, rent-exempt at the bank's rent.
    pub fn put(&mut self, address: Pubkey, data: Vec<u8>, owner: Pubkey) {
        let lamports = self.rent(data.len());
        self.svm.set_account(address, Account { lamports, data, owner, executable: false, rent_epoch: 0 }).unwrap();
    }

    // ── USDC ─────────────────────────────────────────────────────────────────

    /// `owner`'s associated USDC account, opened by the faucet when missing.
    pub fn usdc_account(&mut self, owner: &Pubkey) -> Pubkey {
        let open = spl_associated_token_account::instruction::create_associated_token_account_idempotent(
            &self.faucet.pubkey(),
            owner,
            &self.usdc.mint,
            &spl_token::ID,
        );
        self.setup(&[open], &[]);
        ata(owner, &self.usdc.mint)
    }

    /// `owner`'s associated USDC address, whether or not it exists.
    pub fn usdc_ata(&self, owner: &Pubkey) -> Pubkey {
        ata(owner, &self.usdc.mint)
    }

    /// Mints `amount` µUSDC into the token account `to`.
    pub fn mint_usdc(&mut self, to: &Pubkey, amount: u64) {
        let mint_to =
            spl_token::instruction::mint_to(&spl_token::ID, &self.usdc.mint, to, &self.usdc.mint_authority.pubkey(), &[], amount)
                .unwrap();
        let authority = self.usdc.mint_authority.insecure_clone();
        self.setup(&[mint_to], &[&authority]);
    }

    /// Freezes a USDC account, as Circle can freeze any.
    pub fn freeze_usdc(&mut self, account: &Pubkey) {
        let freeze =
            spl_token::instruction::freeze_account(&spl_token::ID, account, &self.usdc.mint, &self.usdc.freeze_authority.pubkey(), &[])
                .unwrap();
        let authority = self.usdc.freeze_authority.insecure_clone();
        self.setup(&[freeze], &[&authority]);
    }

    pub fn thaw_usdc(&mut self, account: &Pubkey) {
        let thaw =
            spl_token::instruction::thaw_account(&spl_token::ID, account, &self.usdc.mint, &self.usdc.freeze_authority.pubkey(), &[])
                .unwrap();
        let authority = self.usdc.freeze_authority.insecure_clone();
        self.setup(&[thaw], &[&authority]);
    }

    /// A token account written into the bank as the token program stores one.
    pub fn put_token_account(&mut self, address: Pubkey, mint: Pubkey, owner: Pubkey, amount: u64, close_authority: COption<Pubkey>) {
        let mut data = vec![0u8; spl_token::state::Account::LEN];
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
        .pack_into_slice(&mut data);
        self.put(address, data, spl_token::ID);
    }

    /// The SPL token account at `address`.
    pub fn token_account(&self, address: &Pubkey) -> spl_token::state::Account {
        let account = self.svm.get_account(address).expect("token account exists");
        assert_eq!(account.owner, spl_token::ID, "{address} is not a token account");
        spl_token::state::Account::unpack(&account.data).unwrap()
    }

    /// What the token account at `address` holds.
    pub fn token_amount(&self, address: &Pubkey) -> u64 {
        self.token_account(address).amount
    }

    /// The test USDC's mint as the token program stores it.
    pub fn usdc_mint_account(&self) -> spl_token::state::Mint {
        spl_token::state::Mint::unpack(&self.svm.get_account(&self.usdc.mint).expect("the USDC mint").data).unwrap()
    }

    // ── This program's accounts, through its crate ───────────────────────────

    /// The config the suite writes: the suite's keys, its USDC, `pepper_curve` as
    /// `curve_program`, claims never paused (0), funding open.
    pub fn dividends_config(&self) -> DividendsConfig {
        DividendsConfig {
            version: ACCOUNT_VERSION,
            bump: pda(&[CONFIG_SEED]).1,
            paused: false,
            claims_paused_until: 0,
            admin: self.admin.pubkey(),
            pauser: self.pauser.pubkey(),
            approver: self.approver.pubkey(),
            closer: self.closer.pubkey(),
            usdc_mint: self.usdc.mint,
            pepper_funder: self.pepper_funder.pubkey(),
            payer_funder: self.payer.pubkey(),
            curve_program: pepper_curve::ID,
            reserved: [0; 128],
        }
    }

    /// Writes `config` at `["config"]`, as the program stores it.
    pub fn put_config(&mut self, config: &DividendsConfig) {
        self.put(config_pda(), account_data(config, DividendsConfig::INIT_SPACE), pepper_dividends::ID);
    }

    /// The keys `initialize_config` takes for the suite's config: `dividends_config()`'s.
    pub fn config_keys(&self) -> ConfigKeys {
        ConfigKeys {
            pauser: self.pauser.pubkey(),
            approver: self.approver.pubkey(),
            closer: self.closer.pubkey(),
            pepper_funder: self.pepper_funder.pubkey(),
            payer_funder: self.payer.pubkey(),
        }
    }

    /// `initialize_config` for this program's ProgramData, the suite's USDC and `pepper_curve`, unsigned.
    pub fn init_config_ix(&self, payer: &Pubkey, upgrade_authority: &Pubkey, admin: &Pubkey, keys: ConfigKeys) -> Instruction {
        let program_data = program_data_pda(&pepper_dividends::ID);
        self.init_config_ix_with(payer, upgrade_authority, &program_data, admin, &self.usdc.mint, &pepper_curve::ID, keys)
    }

    /// `initialize_config` with every account named, unsigned.
    #[allow(clippy::too_many_arguments)]
    pub fn init_config_ix_with(
        &self,
        payer: &Pubkey,
        upgrade_authority: &Pubkey,
        program_data: &Pubkey,
        admin: &Pubkey,
        usdc_mint: &Pubkey,
        curve_program: &Pubkey,
        keys: ConfigKeys,
    ) -> Instruction {
        ix(
            pepper_dividends::accounts::InitializeConfig {
                payer: *payer,
                upgrade_authority: *upgrade_authority,
                program_data: *program_data,
                config: config_pda(),
                admin: *admin,
                usdc_mint: *usdc_mint,
                curve_program: *curve_program,
                system_program: system_program::id(),
            },
            pepper_dividends::instruction::InitializeConfig { keys },
        )
    }

    /// The config born the mainnet way: the admin pays, signs as the upgrade authority and is the admin.
    pub fn init_config(&mut self) -> Result<Sent, TransactionError> {
        let admin = self.admin.insecure_clone();
        let init = self.init_config_ix(&admin.pubkey(), &admin.pubkey(), &admin.pubkey(), self.config_keys());
        self.send_logged_paid_by(&admin, &[init], &[])
    }

    /// `update_config` signed by `admin`, handing the admin to `new_admin` when given, unsigned.
    pub fn update_config_ix(&self, admin: &Pubkey, new_admin: Option<Pubkey>, update: ConfigUpdate) -> Instruction {
        ix(
            pepper_dividends::accounts::UpdateConfig { admin: *admin, config: config_pda(), new_admin },
            pepper_dividends::instruction::UpdateConfig { update },
        )
    }

    /// `set_paused(paused)` signed by `authority`, unsigned.
    pub fn set_paused_ix(&self, authority: &Pubkey, paused: bool) -> Instruction {
        ix(
            pepper_dividends::accounts::Pause { authority: *authority, config: config_pda() },
            pepper_dividends::instruction::SetPaused { paused },
        )
    }

    /// `pause_claims(until)` signed by `authority`, unsigned.
    pub fn pause_claims_ix(&self, authority: &Pubkey, until: i64) -> Instruction {
        ix(
            pepper_dividends::accounts::Pause { authority: *authority, config: config_pda() },
            pepper_dividends::instruction::PauseClaims { until },
        )
    }

    /// `resume_claims()` signed by `authority`, unsigned: it carries no time, so the same
    /// instruction runs whenever a vault executes it.
    pub fn resume_claims_ix(&self, authority: &Pubkey) -> Instruction {
        ix(
            pepper_dividends::accounts::Pause { authority: *authority, config: config_pda() },
            pepper_dividends::instruction::ResumeClaims {},
        )
    }

    /// `hold_claims(until)` of the month at `distributor`, signed by `authority`, unsigned.
    pub fn hold_claims_ix(&self, authority: &Pubkey, distributor: &Pubkey, until: i64) -> Instruction {
        ix(
            pepper_dividends::accounts::Hold { authority: *authority, config: config_pda(), distributor: *distributor },
            pepper_dividends::instruction::HoldClaims { until },
        )
    }

    /// `release_claims()` of the month at `distributor`, signed by `authority`, unsigned: as the
    /// resume, it carries no time.
    pub fn release_claims_ix(&self, authority: &Pubkey, distributor: &Pubkey) -> Instruction {
        ix(
            pepper_dividends::accounts::Hold { authority: *authority, config: config_pda(), distributor: *distributor },
            pepper_dividends::instruction::ReleaseClaims {},
        )
    }

    /// A funding of `self.mint`'s `MONTH` by `funder` over `leaf_count` leaves: the config's approver,
    /// the dividend payer paying the rents, the funder's own USDC ATA, expiring 365 days on.
    pub fn funding(&self, funder: &Pubkey, merkle_root: [u8; 32], total_amount: u64, leaf_count: u32) -> Funding {
        Funding {
            funder: *funder,
            approver: self.approver.pubkey(),
            rent_payer: self.payer.pubkey(),
            mint: self.mint,
            month: MONTH,
            merkle_root,
            total_amount,
            expires_at: self.now() + 365 * DAY,
            leaf_count,
            curve: curve_pda(&self.mint),
            legacy_distributor: legacy_distributor_pda(&self.mint, MONTH),
            funder_usdc: self.usdc_ata(funder),
            usdc_mint: self.usdc.mint,
            token_program: spl_token::ID,
        }
    }

    /// `init_distributor` for `funding`, unsigned. The month's record, distributor and vault are
    /// derived from its mint and month, the vault with its token program.
    pub fn init_distributor_ix(&self, funding: &Funding) -> Instruction {
        let distributor = distributor_pda(&funding.mint, funding.month);
        ix(
            pepper_dividends::accounts::InitDistributor {
                funder: funding.funder,
                approver: funding.approver,
                rent_payer: funding.rent_payer,
                config: config_pda(),
                mint: funding.mint,
                curve: funding.curve,
                legacy_distributor: funding.legacy_distributor,
                month_record: month_record_pda(&funding.mint, funding.month),
                distributor,
                vault: spl_associated_token_account::get_associated_token_address_with_program_id(
                    &distributor,
                    &funding.usdc_mint,
                    &funding.token_program,
                ),
                funder_usdc: funding.funder_usdc,
                usdc_mint: funding.usdc_mint,
                token_program: funding.token_program,
                associated_token_program: spl_associated_token_account::ID,
                system_program: system_program::id(),
            },
            pepper_dividends::instruction::InitDistributor {
                month: funding.month,
                merkle_root: funding.merkle_root,
                total_amount: funding.total_amount,
                expires_at: funding.expires_at,
                leaf_count: funding.leaf_count,
            },
        )
    }

    /// Sends `funding` as Fanex's server does: the dividend payer pays the fee, `funder` and the
    /// config's approver sign.
    pub fn fund(&mut self, funding: &Funding, funder: &Keypair) -> Result<Sent, TransactionError> {
        let (payer, approver) = (self.payer.insecure_clone(), self.approver.insecure_clone());
        let init = self.init_distributor_ix(funding);
        self.send_logged_paid_by(&payer, &[init], &[funder, &approver])
    }

    /// A month of `n` holders of `self.mint`'s `month`, funded by the dividend payer and written into
    /// the bank with its record and its vault, nothing claimed. No holder has a USDC account yet.
    pub fn write_month(&mut self, month: i64, n: usize) -> Month {
        let holders = holders(n);
        self.write_month_of(month, holders)
    }

    /// As `write_month`, for the holders given, in their order.
    pub fn write_month_of(&mut self, month: i64, holders: Vec<(Keypair, u64)>) -> Month {
        let mint = self.mint;
        let distributor = distributor_pda(&mint, month);
        let tree = build_tree(&dividend_leaves(&distributor, &self.usdc.mint, &holders));
        let total = holders.iter().map(|(_, amount)| amount).sum();
        let payer = self.payer.pubkey();
        let written = self.usdc_distributor(mint, month, payer, tree.root, total, holders.len() as u32);
        self.put_funded_month(&written);
        Month { mint, month, distributor, vault: self.usdc_ata(&distributor), holders, tree, total }
    }

    /// A claim's accounts for `claimant` on `month`.
    pub fn claim_accounts(&self, month: &Month, claimant: &Pubkey) -> ClaimAccounts {
        ClaimAccounts {
            distributor: month.distributor,
            vault: month.vault,
            claimant: *claimant,
            claimant_usdc: self.usdc_ata(claimant),
            usdc_mint: self.usdc.mint,
            token_program: spl_token::ID,
        }
    }

    /// `claim` of leaf `index` with `accounts`, unsigned: no account of it signs.
    pub fn claim_ix_with(&self, accounts: &ClaimAccounts, index: u32, amount: u64, proof: Vec<[u8; 32]>) -> Instruction {
        ix(
            pepper_dividends::accounts::Claim {
                config: config_pda(),
                distributor: accounts.distributor,
                vault: accounts.vault,
                claimant: accounts.claimant,
                claimant_usdc: accounts.claimant_usdc,
                usdc_mint: accounts.usdc_mint,
                token_program: accounts.token_program,
            },
            pepper_dividends::instruction::Claim { index, amount, proof },
        )
    }

    /// `claim` of leaf `index` of `month`, by its own holder's accounts, unsigned.
    pub fn claim_ix(&self, month: &Month, index: usize) -> Instruction {
        let (holder, amount, proof) = month.leaf(index);
        let accounts = self.claim_accounts(month, &holder.pubkey());
        self.claim_ix_with(&accounts, index as u32, amount, proof)
    }

    /// The holder's strict ATA creation and its close authority handed to the sponsor, the two
    /// instructions a holder with no USDC account signs before its claim (spec §7).
    pub fn open_for_claim_ixs(&self, holder: &Pubkey) -> [Instruction; 2] {
        let sponsor = self.sponsor.pubkey();
        let create = spl_associated_token_account::instruction::create_associated_token_account(
            &sponsor,
            holder,
            &self.usdc.mint,
            &spl_token::ID,
        );
        let to_sponsor = spl_token::instruction::set_authority(
            &spl_token::ID,
            &self.usdc_ata(holder),
            Some(&sponsor),
            spl_token::instruction::AuthorityType::CloseAccount,
            holder,
            &[],
        )
        .unwrap();
        [create, to_sponsor]
    }

    /// The accounts a close or a void of `mint`'s `month` takes, the funder's USDC account and the
    /// rent payer as its distributor in the bank records them.
    pub fn settle_accounts(&self, mint: &Pubkey, month: i64) -> SettleAccounts {
        let distributor = distributor_pda(mint, month);
        let recorded: UsdcDistributor = self.fetch(&distributor);
        SettleAccounts {
            distributor,
            month_record: month_record_pda(mint, month),
            vault: self.usdc_ata(&distributor),
            funder_usdc_account: recorded.funder_usdc_account,
            rent_payer: recorded.rent_payer,
            usdc_mint: self.usdc.mint,
            token_program: spl_token::ID,
        }
    }

    /// `close_distributor` signed by `authority` with `accounts`, unsigned.
    pub fn close_ix_with(&self, authority: &Pubkey, accounts: &SettleAccounts) -> Instruction {
        ix(
            pepper_dividends::accounts::CloseDistributor {
                authority: *authority,
                config: config_pda(),
                distributor: accounts.distributor,
                month_record: accounts.month_record,
                vault: accounts.vault,
                funder_usdc_account: accounts.funder_usdc_account,
                rent_payer: accounts.rent_payer,
                usdc_mint: accounts.usdc_mint,
                token_program: accounts.token_program,
            },
            pepper_dividends::instruction::CloseDistributor {},
        )
    }

    /// `close_distributor` of `self.mint`'s `month` signed by `authority`, with the accounts its
    /// distributor records, unsigned.
    pub fn close_ix(&self, authority: &Pubkey, month: i64) -> Instruction {
        let accounts = self.settle_accounts(&self.mint, month);
        self.close_ix_with(authority, &accounts)
    }

    /// `void_distributor` signed by `admin` with `accounts`, unsigned.
    pub fn void_ix_with(&self, admin: &Pubkey, accounts: &SettleAccounts) -> Instruction {
        ix(
            pepper_dividends::accounts::VoidDistributor {
                admin: *admin,
                config: config_pda(),
                distributor: accounts.distributor,
                month_record: accounts.month_record,
                vault: accounts.vault,
                funder_usdc_account: accounts.funder_usdc_account,
                rent_payer: accounts.rent_payer,
                usdc_mint: accounts.usdc_mint,
                token_program: accounts.token_program,
            },
            pepper_dividends::instruction::VoidDistributor {},
        )
    }

    /// `void_distributor` of `self.mint`'s `month` signed by `admin`, with the accounts its
    /// distributor records, unsigned.
    pub fn void_ix(&self, admin: &Pubkey, month: i64) -> Instruction {
        let accounts = self.settle_accounts(&self.mint, month);
        self.void_ix_with(admin, &accounts)
    }

    /// `rehome_distributor` of the month at `distributor`, signed by `admin`, unsigned: it carries
    /// no argument, the key it moves the payees to being the config's payer when it runs.
    pub fn rehome_ix(&self, admin: &Pubkey, distributor: &Pubkey) -> Instruction {
        ix(
            pepper_dividends::accounts::RehomeDistributor { admin: *admin, config: config_pda(), distributor: *distributor },
            pepper_dividends::instruction::RehomeDistributor {},
        )
    }

    /// The accounts a top-up of `mint`'s `month` takes beside its two signers, the funder's USDC
    /// account as its distributor in the bank records it.
    pub fn top_up_accounts(&self, mint: &Pubkey, month: i64) -> TopUpAccounts {
        let distributor = distributor_pda(mint, month);
        let recorded: UsdcDistributor = self.fetch(&distributor);
        TopUpAccounts {
            distributor,
            vault: self.usdc_ata(&distributor),
            funder_usdc_account: recorded.funder_usdc_account,
            usdc_mint: self.usdc.mint,
            token_program: spl_token::ID,
        }
    }

    /// `top_up_distributor(amount)` signed by `funder` and `approver` with `accounts`, unsigned.
    pub fn top_up_ix_with(&self, funder: &Pubkey, approver: &Pubkey, accounts: &TopUpAccounts, amount: u64) -> Instruction {
        ix(
            pepper_dividends::accounts::TopUpDistributor {
                funder: *funder,
                approver: *approver,
                config: config_pda(),
                distributor: accounts.distributor,
                vault: accounts.vault,
                funder_usdc_account: accounts.funder_usdc_account,
                usdc_mint: accounts.usdc_mint,
                token_program: accounts.token_program,
            },
            pepper_dividends::instruction::TopUpDistributor { amount },
        )
    }

    /// `top_up_distributor(amount)` of `self.mint`'s `month` by `funder`, the config's approver
    /// co-signing, with the accounts its distributor records, unsigned.
    pub fn top_up_ix(&self, funder: &Pubkey, month: i64, amount: u64) -> Instruction {
        let accounts = self.top_up_accounts(&self.mint, month);
        self.top_up_ix_with(funder, &self.approver.pubkey(), &accounts, amount)
    }

    /// The funder's USDC ATA opened again, as a close's or a void's transaction carries it when the
    /// funder closed theirs: the associated token program's `create_idempotent`, paid by `payer`.
    pub fn reopen_usdc_ix(&self, payer: &Pubkey, owner: &Pubkey) -> Instruction {
        spl_associated_token_account::instruction::create_associated_token_account_idempotent(payer, owner, &self.usdc.mint, &spl_token::ID)
    }

    /// How many signatures a transaction signed by `signers` carries: each key once.
    pub fn signatures(&self, signers: &[&Pubkey]) -> u64 {
        let mut keys: Vec<&Pubkey> = signers.to_vec();
        keys.sort();
        keys.dedup();
        keys.len() as u64
    }

    /// A second classic mint of 6 decimals, with no authority: another USDC as far as a check of
    /// the decimals goes.
    pub fn second_mint(&mut self) -> Pubkey {
        let address = Pubkey::new_unique();
        let mut data = vec![0u8; spl_token::state::Mint::LEN];
        spl_token::state::Mint {
            mint_authority: COption::None,
            supply: 0,
            decimals: USDC_DECIMALS,
            is_initialized: true,
            freeze_authority: COption::None,
        }
        .pack_into_slice(&mut data);
        self.put(address, data, spl_token::ID);
        address
    }

    /// `owner`'s associated account of `mint`, opened by the faucet when missing.
    pub fn token_ata_of(&mut self, owner: &Pubkey, mint: &Pubkey) -> Pubkey {
        let open = spl_associated_token_account::instruction::create_associated_token_account_idempotent(
            &self.faucet.pubkey(),
            owner,
            mint,
            &spl_token::ID,
        );
        self.setup(&[open], &[]);
        ata(owner, mint)
    }

    /// `dividends_config()` written into the bank.
    pub fn write_config(&mut self) -> DividendsConfig {
        let config = self.dividends_config();
        self.put_config(&config);
        config
    }

    /// A month of `mint` as `init_distributor` leaves it once its review window has passed
    /// (`claims_held_until` 0, so its claims open at once): funded by `funder` with `total` µUSDC
    /// over `leaf_count` leaves, nothing claimed, its rents the dividend payer's, the funder's USDC
    /// ATA recorded, both funding flags as the funding records them, expiring a year on. Every
    /// field is public: a test sets any amount, bitmap or flag before writing it.
    pub fn usdc_distributor(&self, mint: Pubkey, month: i64, funder: Pubkey, merkle_root: [u8; 32], total: u64, leaf_count: u32) -> UsdcDistributor {
        UsdcDistributor {
            version: ACCOUNT_VERSION,
            bump: pda(&[DISTRIBUTOR_SEED, mint.as_ref(), &month.to_le_bytes()]).1,
            mint,
            month,
            merkle_root,
            total_amount: total,
            claimed_amount: 0,
            funder,
            funder_usdc_account: self.usdc_ata(&funder),
            rent_payer: self.payer.pubkey(),
            expires_at: self.now() + 365 * DAY,
            leaf_count,
            claims_held_until: 0,
            funded_by_fanex: funder == self.pepper_funder.pubkey() || funder == self.payer.pubkey(),
            rents_by_payer: true,
            reserved: [0; 54],
            claimed: vec![0; claim_registry_bytes(leaf_count)],
        }
    }

    /// Writes `distributor` at its address, sized `UsdcDistributor::space(leaf_count)`.
    pub fn put_distributor(&mut self, distributor: &UsdcDistributor) {
        let space = UsdcDistributor::space(distributor.leaf_count);
        let mut data = Vec::with_capacity(space);
        distributor.try_serialize(&mut data).unwrap();
        assert!(data.len() <= space, "a distributor longer than its space: its bitmap is not ⌈leaf_count / 8⌉ bytes");
        data.resize(space, 0);
        self.put(distributor_pda(&distributor.mint, distributor.month), data, pepper_dividends::ID);
    }

    /// Writes a month's record at `["month", mint, month]`.
    pub fn put_month_record(&mut self, mint: Pubkey, month: i64, status: u8, distributor: Pubkey, funded_at: i64) {
        let record = MonthRecord {
            version: ACCOUNT_VERSION,
            bump: pda(&[MONTH_SEED, mint.as_ref(), &month.to_le_bytes()]).1,
            status,
            distributor,
            funded_at,
            reserved: [0; 24],
        };
        self.put(month_record_pda(&mint, month), account_data(&record, MonthRecord::INIT_SPACE), pepper_dividends::ID);
    }

    /// The distributor's vault: its associated USDC account, its authority the
    /// distributor PDA, holding `amount` µUSDC minted for it, so the USDC's supply
    /// counts them. Returns its address.
    pub fn put_vault(&mut self, distributor: &Pubkey, amount: u64) -> Pubkey {
        let vault = self.usdc_ata(distributor);
        self.put_token_account(vault, self.usdc.mint, *distributor, 0, COption::None);
        if amount > 0 {
            self.mint_usdc(&vault, amount);
        }
        vault
    }

    /// `distributor` written with what a funding leaves beside it: its record,
    /// open since now, and its vault holding what is still unclaimed. Returns the
    /// distributor's address.
    pub fn put_funded_month(&mut self, distributor: &UsdcDistributor) -> Pubkey {
        let address = distributor_pda(&distributor.mint, distributor.month);
        self.put_distributor(distributor);
        let now = self.now();
        self.put_month_record(distributor.mint, distributor.month, MONTH_STATUS_OPEN, address, now);
        self.put_vault(&address, distributor.total_amount - distributor.claimed_amount);
        address
    }

    // ── pepper_curve's accounts, through its crate ───────────────────────────

    /// `pepper_curve`'s config: `curve_admin` as its admin, fee wallet and
    /// graduation authority, unpaused.
    pub fn put_curve_config(&mut self) {
        let admin = self.curve_admin.pubkey();
        let config = pepper_curve::state::GlobalConfig {
            admin,
            fanex_fee_wallet: admin,
            graduation_authority: admin,
            paused: false,
            bump: curve_pda_of(&[pepper_curve::constants::CONFIG_SEED]).1,
        };
        self.put(curve_config_pda(), account_data(&config, pepper_curve::state::GlobalConfig::INIT_SPACE), pepper_curve::ID);
    }

    /// A SOL `Curve` of `mint` at `["curve", mint]`, as `pepper_curve` stores it
    /// (196 bytes), its creator `creator`.
    pub fn put_curve(&mut self, mint: Pubkey, creator: Pubkey) {
        let (address, bump) = curve_pda_of(&[pepper_curve::constants::CURVE_SEED, mint.as_ref()]);
        let curve = pepper_curve::state::Curve {
            mint,
            creator,
            phase: pepper_curve::state::CurvePhase::Bonding,
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
        self.put(address, account_data(&curve, pepper_curve::state::Curve::INIT_SPACE), pepper_curve::ID);
    }

    // ── pepper_curve's launches, by the real program ──────────────────────────

    /// Loads Metaplex Token Metadata at its address from track A's fixture, as a cluster runs it,
    /// once.
    pub fn load_metaplex(&mut self) {
        if self.exists(&anchor_spl::metadata::ID) {
            return;
        }
        let path = metaplex_so();
        self.svm
            .add_program_from_file(anchor_spl::metadata::ID, &path)
            .unwrap_or_else(|e| panic!("{}: {e} (track A's fixture, scripts/solana/dump-litesvm-fixtures.sh)", path.display()));
    }

    /// A SOL `Curve` launched by `pepper_curve`'s own `create_curve` for a fresh mint, `creator`
    /// its launcher and creator, the token's metadata written by the real Metaplex, its update
    /// authority the curve config's admin: an equity launch at the program's smallest pool seed.
    /// Answers the mint.
    pub fn launch_curve(&mut self) -> Pubkey {
        use pepper_curve::constants as c;
        self.load_metaplex();
        let mint = Keypair::new();
        let address = mint.pubkey();
        let curve = curve_pda(&address);
        let update_authority = self.fetch::<pepper_curve::state::GlobalConfig>(&curve_config_pda()).admin;
        let launcher = self.creator.insecure_clone();
        let launch = curve_ix(
            pepper_curve::accounts::CreateCurve {
                launcher: launcher.pubkey(),
                creator: launcher.pubkey(),
                config: curve_config_pda(),
                mint: address,
                curve,
                sol_vault: curve_pda_of(&[c::SOL_VAULT_SEED, address.as_ref()]).0,
                vesting: curve_pda_of(&[c::VESTING_SEED, address.as_ref()]).0,
                token_vault: ata(&curve, &address),
                vesting_vault: curve_pda_of(&[c::VESTING_VAULT_SEED, address.as_ref()]).0,
                metadata: metadata_pda(&address),
                metadata_update_authority: update_authority,
                token_program: spl_token::ID,
                token_metadata_program: anchor_spl::metadata::ID,
                associated_token_program: anchor_spl::associated_token::ID,
                system_program: system_program::id(),
                rent: solana_sdk::sysvar::rent::id(),
            },
            pepper_curve::instruction::CreateCurve {
                params: pepper_curve::instructions::CreateCurveParams {
                    creator_target_lamports: 0,
                    cpmm_seed_lamports: c::MIN_CPMM_SEED_LAMPORTS,
                    creator_split_bps: 0,
                    fanex_split_bps: 0,
                    creator_cap_lamports: 0,
                    fanex_cap_lamports: 0,
                    virtual_sol_reserves: 5 * SOL,
                    virtual_token_reserves: c::TOKEN_TOTAL_SUPPLY,
                    curve_token_supply: LAUNCH_CURVE_SUPPLY,
                    migration_token_supply: LAUNCH_MIGRATION_SUPPLY,
                    vesting_token_supply: LAUNCH_VESTING_SUPPLY,
                    vesting_cliff_seconds: c::VESTING_CLIFF_SECONDS,
                    vesting_duration_seconds: c::VESTING_DURATION_SECONDS,
                    name: LAUNCH_NAME.to_string(),
                    symbol: LAUNCH_SYMBOL.to_string(),
                    uri: LAUNCH_URI.to_string(),
                },
            },
        );
        self.send_paid_by(&launcher, &[compute_unit_limit(CREATE_CU_LIMIT), launch], &[&mint]).expect("create_curve lands");
        address
    }

    /// A `CurveUsdc` launched by `pepper_curve`'s own `create_curve_usdc` for a fresh mint,
    /// `creator` its creator, paid by the sponsor as `UsdcConfig.launcher`: an equity launch, so no
    /// creator USDC account. The `UsdcConfig` is written first, once, as `initialize_usdc_config` and
    /// `set_usdc_raydium` leave it: the suite's USDC, `curve_admin` as its admin (the metadata's
    /// update authority) and pauser, its fee account `curve_admin`'s USDC ATA, opened, and five
    /// Raydium keys apart from one another, which a launch refuses only all-zero (`RaydiumNotSet`).
    /// Answers the mint.
    pub fn launch_curve_usdc(&mut self) -> Pubkey {
        use pepper_curve::constants as c;
        self.load_metaplex();
        let admin = self.curve_admin.pubkey();
        let sponsor = self.sponsor.insecure_clone();
        if !self.exists(&usdc_config_pda()) {
            let fee_usdc_account = self.usdc_account(&admin);
            let config = pepper_curve::state::UsdcConfig {
                version: 1,
                bump: curve_pda_of(&[c::USDC_CONFIG_SEED]).1,
                paused: false,
                admin,
                pauser: admin,
                migration_authority: Pubkey::default(),
                usdc_mint: self.usdc.mint,
                fee_usdc_account,
                lp_fee_owner: admin,
                raydium_cpmm_program: Pubkey::new_unique(),
                raydium_amm_config: Pubkey::new_unique(),
                raydium_create_pool_fee: Pubkey::new_unique(),
                raydium_lock_program: Pubkey::new_unique(),
                raydium_lock_authority: Pubkey::new_unique(),
                launcher: sponsor.pubkey(),
                reserved: [0; 96],
            };
            self.put(usdc_config_pda(), account_data(&config, pepper_curve::state::UsdcConfig::INIT_SPACE), pepper_curve::ID);
        }
        let mint = Keypair::new();
        let address = mint.pubkey();
        let curve = curve_pda(&address);
        let launch = curve_ix(
            pepper_curve::accounts::CreateCurveUsdc {
                payer: sponsor.pubkey(),
                creator: self.creator.pubkey(),
                usdc_config: usdc_config_pda(),
                usdc_mint: self.usdc.mint,
                mint: address,
                curve,
                usdc_vault: curve_pda_of(&[c::USDC_VAULT_SEED, address.as_ref()]).0,
                vesting: curve_pda_of(&[c::VESTING_SEED, address.as_ref()]).0,
                token_vault: ata(&curve, &address),
                vesting_vault: curve_pda_of(&[c::VESTING_VAULT_SEED, address.as_ref()]).0,
                creator_usdc_account: None,
                metadata: metadata_pda(&address),
                metadata_update_authority: admin,
                token_program: spl_token::ID,
                associated_token_program: anchor_spl::associated_token::ID,
                system_program: system_program::id(),
                token_metadata_program: anchor_spl::metadata::ID,
                rent: solana_sdk::sysvar::rent::id(),
            },
            pepper_curve::instruction::CreateCurveUsdc {
                params: pepper_curve::instructions::CreateCurveUsdcParams {
                    creator_target_usdc: 0,
                    graduation_threshold_usdc: c::MIN_GRADUATION_THRESHOLD_USDC,
                    creator_split_bps: 0,
                    fanex_split_bps: 0,
                    creator_cap_usdc: 0,
                    fanex_cap_usdc: 0,
                    virtual_usdc_reserves: 5_000 * USDC,
                    virtual_token_reserves: c::TOKEN_TOTAL_SUPPLY,
                    curve_token_supply: LAUNCH_CURVE_SUPPLY,
                    migration_token_supply: LAUNCH_MIGRATION_SUPPLY,
                    vesting_token_supply: LAUNCH_VESTING_SUPPLY,
                    vesting_cliff_seconds: c::VESTING_CLIFF_SECONDS,
                    vesting_duration_seconds: c::VESTING_DURATION_SECONDS,
                    name: LAUNCH_NAME.to_string(),
                    symbol: LAUNCH_SYMBOL.to_string(),
                    uri: LAUNCH_URI.to_string(),
                },
            },
        );
        self.send_paid_by(&sponsor, &[compute_unit_limit(CREATE_CU_LIMIT), launch], &[&mint]).expect("create_curve_usdc lands");
        address
    }

    // ── pepper_curve's SOL months, by the real program ───────────────────────

    /// SOL-month holders, each funded so their account exists.
    pub fn sol_holders(&mut self, n: usize) -> Vec<(Keypair, u64)> {
        (0..n)
            .map(|i| {
                let holder = Keypair::new();
                self.svm.airdrop(&holder.pubkey(), SOL).unwrap();
                (holder, 10_000_000 + i as u64 * 1_000)
            })
            .collect()
    }

    /// A v2 month of `mint` opened by `pepper_curve`'s `init_distributor_v2`,
    /// funded by the curve's creator (`self.creator`, which the curve must name)
    /// and approved by `curve_admin`, the graduation authority, expiring a year on.
    pub fn open_v2(&mut self, mint: Pubkey, month: i64, holders: &[(Keypair, u64)]) -> Result<Tree, TransactionError> {
        let leaves: Vec<[u8; 32]> = holders
            .iter()
            .enumerate()
            .map(|(i, (holder, amount))| pepper_curve::math::dividend_leaf_v2(i as u32, &holder.pubkey(), *amount))
            .collect();
        let tree = build_tree(&leaves);
        let curve: pepper_curve::state::Curve = self.fetch(&curve_pda(&mint));
        assert_eq!(curve.creator, self.creator.pubkey(), "open_v2 funds a month of a curve whose creator is self.creator");
        let (creator, admin) = (self.creator.insecure_clone(), self.curve_admin.insecure_clone());
        let init = curve_ix(
            pepper_curve::accounts::InitDistributorV2 {
                funder: creator.pubkey(),
                approver: admin.pubkey(),
                config: curve_config_pda(),
                mint,
                curve: curve_pda(&mint),
                distributor: legacy_distributor_pda(&mint, month),
                system_program: system_program::id(),
            },
            pepper_curve::instruction::InitDistributorV2 {
                month,
                merkle_root: tree.root,
                total_amount: holders.iter().map(|(_, a)| a).sum(),
                expires_at: self.now() + 365 * DAY,
                leaf_count: holders.len() as u32,
            },
        );
        self.send_paid_by(&creator, &[init], &[&admin]).map(|_| tree)
    }

    /// Leaf `index` of a v2 month claimed by its holder, paid by `curve_admin`.
    pub fn claim_v2(&mut self, mint: Pubkey, month: i64, holder: &Keypair, index: u32, amount: u64, proof: Vec<[u8; 32]>) -> Result<(), TransactionError> {
        let claim = curve_ix(
            pepper_curve::accounts::ClaimDividendV2 {
                claimant: holder.pubkey(),
                distributor: legacy_distributor_pda(&mint, month),
                system_program: system_program::id(),
            },
            pepper_curve::instruction::ClaimDividendV2 { index, amount, proof },
        );
        let admin = self.curve_admin.insecure_clone();
        self.send_paid_by(&admin, &[claim], &[holder])
    }

    /// A v2 month closed by its funder, the curve's creator.
    pub fn close_v2(&mut self, mint: Pubkey, month: i64) -> Result<(), TransactionError> {
        let creator = self.creator.insecure_clone();
        let close = curve_ix(
            pepper_curve::accounts::CloseDistributorV2 { distributor: legacy_distributor_pda(&mint, month), funder: creator.pubkey() },
            pepper_curve::instruction::CloseDistributorV2 {},
        );
        self.send_paid_by(&creator, &[close], &[])
    }

    /// A v1 month of `mint` opened by `pepper_curve`'s `init_distributor`, funded
    /// by `curve_admin` (its graduation authority, the one v1 funder since the
    /// fee-wallet split), expiring a year on.
    pub fn open_v1(&mut self, mint: Pubkey, month: i64, holders: &[(Keypair, u64)]) -> Result<Tree, TransactionError> {
        let leaves: Vec<[u8; 32]> =
            holders.iter().map(|(holder, amount)| pepper_curve::math::dividend_leaf(&holder.pubkey(), *amount)).collect();
        let tree = build_tree(&leaves);
        let admin = self.curve_admin.insecure_clone();
        let init = curve_ix(
            pepper_curve::accounts::InitDistributor {
                funder: admin.pubkey(),
                config: curve_config_pda(),
                mint,
                curve: curve_pda(&mint),
                distributor: legacy_distributor_pda(&mint, month),
                system_program: system_program::id(),
            },
            pepper_curve::instruction::InitDistributor {
                month,
                merkle_root: tree.root,
                total_amount: holders.iter().map(|(_, a)| a).sum(),
                expires_at: self.now() + 365 * DAY,
            },
        );
        self.send_paid_by(&admin, &[init], &[]).map(|_| tree)
    }

    /// A v1 claim by its holder, `curve_admin` paying the fee and the `ClaimStatus` rent.
    pub fn claim_v1(&mut self, mint: Pubkey, month: i64, holder: &Keypair, amount: u64, proof: Vec<[u8; 32]>) -> Result<(), TransactionError> {
        let distributor = legacy_distributor_pda(&mint, month);
        let admin = self.curve_admin.insecure_clone();
        let claim = curve_ix(
            pepper_curve::accounts::ClaimDividend {
                claimant: holder.pubkey(),
                rent_payer: admin.pubkey(),
                distributor,
                claim_status: claim_status_pda(&distributor, &holder.pubkey()),
                system_program: system_program::id(),
            },
            pepper_curve::instruction::ClaimDividend { amount, proof },
        );
        self.send_paid_by(&admin, &[claim], &[holder])
    }

    /// A v1 month closed by its funder, `curve_admin`.
    pub fn close_v1(&mut self, mint: Pubkey, month: i64) -> Result<(), TransactionError> {
        let admin = self.curve_admin.insecure_clone();
        let close = curve_ix(
            pepper_curve::accounts::CloseDistributor { distributor: legacy_distributor_pda(&mint, month), funder: admin.pubkey() },
            pepper_curve::instruction::CloseDistributor {},
        );
        self.send_paid_by(&admin, &[close], &[])
    }
}

/// `account` as a program stores it: its discriminator and fields, then zeros up
/// to `8 + space`.
pub fn account_data<T: AccountSerialize>(account: &T, space: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(8 + space);
    account.try_serialize(&mut data).unwrap();
    assert!(data.len() <= 8 + space, "an account longer than its space");
    data.resize(8 + space, 0);
    data
}

/// The first `E` among a transaction's logs: `emit!` writes each event as
/// "Program data: " and the base64 of its discriminator and its borsh bytes.
pub fn event<E: anchor_lang::Event>(logs: &[String]) -> E {
    events_of::<E>(logs).into_iter().next().expect("the event was emitted")
}

/// Every `E` among a transaction's logs, in the order they were emitted: an empty
/// list where `event` would panic, so a test can say an event is absent.
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
