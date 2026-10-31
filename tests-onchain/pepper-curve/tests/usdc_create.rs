//! `create_curve_usdc` against the built program (docs/solana/PEPPER-CURVE-PROGRAM.md §5, §6, §10,
//! §13; usdc-curve design §10.3.4, §10.11). A launch that lands runs the real Metaplex program from
//! its mainnet fixture; every refusal runs without it, which the metadata program checked by its
//! address alone allows: a check that let a bad launch through would reach the missing program and
//! fail with another error than the one expected.
//!
//! The `UsdcConfig` is written into the bank (`usdc_config.rs` holds those bytes to what
//! `initialize_usdc_config` writes) with three keys apart: the sponsor as `launcher`, the only payer;
//! a key standing for the Squad's vault as `admin`, which never signs, the metadata's update
//! authority; and `Env::admin`, the `GlobalConfig` admin and fee wallet, which a SOL launch names
//! instead.

mod common;

use anchor_lang::solana_program::program_option::COption;
use anchor_lang::solana_program::program_pack::Pack;
use anchor_lang::Space;
use anchor_spl::associated_token::get_associated_token_address as ata;
use anchor_spl::associated_token::spl_associated_token_account;
use anchor_spl::metadata::mpl_token_metadata::{self, accounts::Metadata};
use anchor_spl::token::spl_token;
use common::*;
use pepper_curve::constants::*;
use pepper_curve::errors::FanexError;
use pepper_curve::events::CurveCreatedUsdc;
use pepper_curve::instructions::set_vesting_beneficiary::fan_led_placeholder;
use pepper_curve::instructions::CreateCurveUsdcParams;
use pepper_curve::state::{CurveUsdc, GlobalConfig, UsdcConfig, Vesting};
use pepper_curve::validation::{MAX_VESTING_CLIFF_SECONDS, MAX_VESTING_DURATION_SECONDS};
use solana_sdk::{
    hash::Hash,
    instruction::{Instruction, InstructionError},
    message::{v0, VersionedMessage},
    packet::PACKET_DATA_SIZE,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_program,
    transaction::{Transaction, TransactionError, VersionedTransaction},
};

const COMPUTE_BUDGET_ID: Pubkey = Pubkey::from_str_const("ComputeBudget111111111111111111111111111111");
const RENT_SYSVAR_ID: Pubkey = Pubkey::from_str_const("SysvarRent111111111111111111111111111111111");

/// The compute units and the priority fee the app asks for a SOL launch (`CREATE_CU_LIMIT`,
/// `CREATE_CU_PRICE`, `lib/solana/pepper-curve/launch.ts`), which sends both ahead of the launch
/// in a v0 message: the launches below are sent so, so their sizes are the ones a builder meets.
const CREATE_CU_LIMIT: u32 = 500_000;
const CREATE_CU_PRICE: u64 = 50_000;

/// Anchor's own code for an account that is not where its `address` constraint says; the harness's
/// `ACCOUNT_NOT_INITIALIZED` (3012) is its code for one that does not exist.
const CONSTRAINT_ADDRESS: u32 = 2012;

const NAME: &str = "Pepper Fixture Launch";
/// Ten bytes: `MAX_USDC_SYMBOL_LEN`, Metaplex's own limit.
const SYMBOL: &str = "PEPPERTEST";
/// The uri the create route builds, `${canonicalOrigin()}/api/solana/metadata/<symbol>`.
const URI: &str = "https://pepper.fan/api/solana/metadata/peppertest";

/// 80M tokens on the curve, 18M for the pool, 2% vested.
const CURVE_SUPPLY: u64 = 80_000_000 * 1_000_000;
const MIGRATION_SUPPLY: u64 = 18_000_000 * 1_000_000;
const VESTING_SUPPLY: u64 = 2_000_000 * 1_000_000;

/// An equity launch: no raise split, every µUSDC of a buy after the fee goes to the curve.
fn equity() -> CreateCurveUsdcParams {
    CreateCurveUsdcParams {
        creator_target_usdc: 0,
        graduation_threshold_usdc: MIN_GRADUATION_THRESHOLD_USDC,
        creator_split_bps: 0,
        fanex_split_bps: 0,
        creator_cap_usdc: 0,
        fanex_cap_usdc: 0,
        virtual_usdc_reserves: 5_000 * USDC,
        virtual_token_reserves: TOKEN_TOTAL_SUPPLY,
        curve_token_supply: CURVE_SUPPLY,
        migration_token_supply: MIGRATION_SUPPLY,
        vesting_token_supply: VESTING_SUPPLY,
        vesting_cliff_seconds: VESTING_CLIFF_SECONDS,
        vesting_duration_seconds: VESTING_DURATION_SECONDS,
        name: NAME.to_string(),
        symbol: SYMBOL.to_string(),
        uri: URI.to_string(),
    }
}

/// A revenue launch at the smallest target: 1,600 USDC, 94% of it the creator's and 6% Fanex's, its
/// threshold the floor's 2,400, so a split of 37.6% and 2.4% of each buy, the caps over the target
/// plus the threshold, as the launch builder derives it (ECONOMIC-MODEL §4.2), until the caps fill.
fn revenue() -> CreateCurveUsdcParams {
    CreateCurveUsdcParams {
        creator_target_usdc: MIN_CREATOR_TARGET_USDC,
        creator_split_bps: 3_760,
        fanex_split_bps: 240,
        creator_cap_usdc: 1_504 * USDC,
        fanex_cap_usdc: 96 * USDC,
        ..equity()
    }
}

/// A revenue launch's raise as the launch builder derives it from `target`
/// (`lib/solana/pepper-curve/usdc-launch-params.ts`, phase 5.1's P20): the threshold
/// `max(2,400, min(80,000, ⌊1.5 × target⌋))` USDC, the creator's cap ⌊94% × target⌋ and Fanex's the
/// rest, each split its cap over `target + threshold` in bps, rounded to the nearest as the builder's
/// `Math.round` does.
fn builder_revenue(target: u64) -> CreateCurveUsdcParams {
    let threshold = (target * 3 / 2).clamp(MIN_GRADUATION_THRESHOLD_USDC, CAP_GRADUATION_THRESHOLD_USDC);
    let creator_cap = target * 94 / 100;
    let fanex_cap = target - creator_cap;
    let total = (target + threshold) as f64;
    let bps = |cap: u64| ((cap as f64) * 10_000.0 / total).round() as u16;
    CreateCurveUsdcParams {
        creator_target_usdc: target,
        graduation_threshold_usdc: threshold,
        creator_split_bps: bps(creator_cap),
        fanex_split_bps: bps(fanex_cap),
        creator_cap_usdc: creator_cap,
        fanex_cap_usdc: fanex_cap,
        ..equity()
    }
}

fn compute_unit_limit(units: u32) -> Instruction {
    let mut data = vec![2u8];
    data.extend_from_slice(&units.to_le_bytes());
    Instruction { program_id: COMPUTE_BUDGET_ID, accounts: vec![], data }
}

fn compute_unit_price(micro_lamports: u64) -> Instruction {
    let mut data = vec![3u8];
    data.extend_from_slice(&micro_lamports.to_le_bytes());
    Instruction { program_id: COMPUTE_BUDGET_ID, accounts: vec![], data }
}

/// The two compute-budget instructions a launch opens with, as the app's builder sends them.
fn budget() -> [Instruction; 2] {
    [compute_unit_limit(CREATE_CU_LIMIT), compute_unit_price(CREATE_CU_PRICE)]
}

/// `ixs` as the app's builder compiles them (`compileToV0Message`, no lookup table), paid by
/// `fee_payer` and signed by `signers`, the fee payer among them.
fn v0_transaction(ixs: &[Instruction], fee_payer: &Pubkey, signers: &[&Keypair], blockhash: Hash) -> VersionedTransaction {
    let message = v0::Message::try_compile(fee_payer, ixs, &[], blockhash).expect("a v0 message");
    VersionedTransaction::try_new(VersionedMessage::V0(message), signers).expect("every signer given")
}

/// The bytes a signed transaction takes on the wire: its signatures' count (one byte under 128),
/// the signatures, then the message, a v0 one with its version prefix and its empty lookup list.
fn wire_size(tx: &VersionedTransaction) -> usize {
    1 + 64 * tx.signatures.len() + tx.message.serialize().len()
}

/// The compute units `pepper_curve`'s own instruction took, read off its log line.
fn program_units(logs: &[String]) -> u64 {
    let prefix = format!("Program {} consumed ", pepper_curve::ID);
    logs.iter()
        .filter_map(|line| line.strip_prefix(&prefix))
        .filter_map(|rest| rest.split_whitespace().next()?.parse().ok())
        .last()
        .expect("pepper_curve's consumed line")
}

struct Landed {
    units: u64,
    logs: Vec<String>,
    size: usize,
}

struct Launch {
    env: Env,
    /// `UsdcConfig.launcher`: the sponsor, who pays and signs every launch.
    sponsor: Keypair,
    /// `UsdcConfig.admin`: the Squad's vault in production, the metadata's update authority. It never signs a launch.
    usdc_admin: Pubkey,
    config: UsdcConfig,
}

/// An `Env` with the `UsdcConfig` written, its three keys apart, mainnet's Raydium named (a launch
/// refuses without, `RaydiumNotSet`), and Metaplex loaded when asked.
fn launch_env(metaplex: bool) -> Launch {
    let mut env = Env::new();
    if metaplex {
        env.load_metaplex();
    }
    let sponsor = env.funded_wallet(100 * SOL);
    let usdc_admin = Pubkey::new_unique();
    let fee_wallet = env.fee_wallet.pubkey();
    env.usdc_account(&fee_wallet);
    let mut config = env.usdc_config_with_raydium();
    config.admin = usdc_admin;
    config.launcher = sponsor.pubkey();
    env.put_usdc_config(&config);
    Launch { env, sponsor, usdc_admin, config }
}

impl Launch {
    fn creator(&self) -> Pubkey {
        self.env.creator.pubkey()
    }

    /// The creator's associated USDC account.
    fn creator_ata(&self) -> Pubkey {
        ata(&self.creator(), &self.config.usdc_mint)
    }

    /// The launch's accounts as the builder sends them, for `mint` and `creator`.
    fn accounts(&self, mint: &Pubkey, creator: &Pubkey, creator_usdc_account: Option<Pubkey>) -> pepper_curve::accounts::CreateCurveUsdc {
        let curve = curve_pda(mint);
        pepper_curve::accounts::CreateCurveUsdc {
            payer: self.sponsor.pubkey(),
            creator: *creator,
            usdc_config: usdc_config_pda(),
            usdc_mint: self.config.usdc_mint,
            mint: *mint,
            curve,
            usdc_vault: usdc_vault_pda(mint),
            vesting: pda(&[VESTING_SEED, mint.as_ref()]),
            token_vault: ata(&curve, mint),
            vesting_vault: pda(&[VESTING_VAULT_SEED, mint.as_ref()]),
            creator_usdc_account,
            metadata: Metadata::find_pda(mint).0,
            metadata_update_authority: self.usdc_admin,
            token_program: spl_token::ID,
            associated_token_program: anchor_spl::associated_token::ID,
            system_program: system_program::id(),
            token_metadata_program: mpl_token_metadata::ID,
            rent: RENT_SYSVAR_ID,
        }
    }

    fn create_ix(&self, mint: &Pubkey, creator: &Pubkey, creator_usdc_account: Option<Pubkey>, params: CreateCurveUsdcParams) -> Instruction {
        ix(self.accounts(mint, creator, creator_usdc_account), pepper_curve::instruction::CreateCurveUsdc { params })
    }

    /// The creator's USDC ATA opened at the sponsor's cost and handed the sponsor its close
    /// authority, which the creator signs: the two instructions a launch builder adds for an ATA
    /// that is missing or not yet hardened (§10.3.4).
    fn harden_ixs(&self) -> [Instruction; 2] {
        let sponsor = self.sponsor.pubkey();
        [
            spl_associated_token_account::instruction::create_associated_token_account_idempotent(
                &sponsor,
                &self.creator(),
                &self.config.usdc_mint,
                &spl_token::ID,
            ),
            spl_token::instruction::set_authority(
                &spl_token::ID,
                &self.creator_ata(),
                Some(&sponsor),
                spl_token::instruction::AuthorityType::CloseAccount,
                &self.creator(),
                &[],
            )
            .unwrap(),
        ]
    }

    /// The whole revenue launch as one transaction: the compute budget, the creator's ATA opened
    /// and hardened, then the launch.
    fn revenue_launch_ixs(&self, mint: &Pubkey, params: CreateCurveUsdcParams) -> Vec<Instruction> {
        let mut ixs = budget().to_vec();
        ixs.extend(self.harden_ixs());
        ixs.push(self.create_ix(mint, &self.creator(), Some(self.creator_ata()), params));
        ixs
    }

    /// `ixs` paid by `fee_payer` and signed by `extra` too, sent as a v0 transaction, the blockhash
    /// expired after.
    fn send_paid_by(&mut self, fee_payer: &Keypair, ixs: &[Instruction], extra: &[&Keypair]) -> Result<Landed, TransactionError> {
        let mut signers: Vec<&Keypair> = vec![fee_payer];
        signers.extend(extra.iter().copied().filter(|k| k.pubkey() != fee_payer.pubkey()));
        let tx = v0_transaction(ixs, &fee_payer.pubkey(), &signers, self.env.svm.latest_blockhash());
        let size = wire_size(&tx);
        let result = self.env.svm.send_transaction(tx);
        self.env.svm.expire_blockhash();
        result.map(|meta| Landed { units: meta.compute_units_consumed, logs: meta.logs, size }).map_err(|failed| failed.err)
    }

    /// `ixs` paid by the sponsor.
    fn send(&mut self, ixs: &[Instruction], extra: &[&Keypair]) -> Result<Landed, TransactionError> {
        let sponsor = self.sponsor.insecure_clone();
        self.send_paid_by(&sponsor, ixs, extra)
    }

    /// The launch of `params` on a fresh mint for `creator`, paid by the sponsor, refused with
    /// `code`, leaving nothing behind: the instruction's `init`s are undone with it.
    fn assert_refused(&mut self, creator: &Pubkey, creator_usdc_account: Option<Pubkey>, params: CreateCurveUsdcParams, code: u32) {
        let mint = Keypair::new();
        let launch = self.create_ix(&mint.pubkey(), creator, creator_usdc_account, params);
        let result = self.send(&[launch], &[&mint]).map(|_| ());
        expect_err(result, code);
        assert!(!self.env.exists(&mint.pubkey()));
        assert!(!self.env.exists(&curve_pda(&mint.pubkey())));
        assert!(!self.env.exists(&usdc_vault_pda(&mint.pubkey())));
    }

    /// The creator's USDC ATA as a launch leaves it, written into the bank: theirs, empty,
    /// closable by the sponsor alone.
    fn put_hardened_ata(&mut self) -> Pubkey {
        let (address, usdc, creator, sponsor) = (self.creator_ata(), self.config.usdc_mint, self.creator(), self.sponsor.pubkey());
        self.env.put_token_account(address, usdc, creator, 0, COption::Some(sponsor));
        address
    }

    /// A revenue launch's refusal, the creator's hardened ATA passed, so only `params` is wrong.
    fn assert_revenue_refused(&mut self, params: CreateCurveUsdcParams, code: u32) {
        let (creator, account) = (self.creator(), self.put_hardened_ata());
        self.assert_refused(&creator, Some(account), params, code);
    }
}

// ── Launches that land ──────────────────────────────────────────────────────

#[test]
fn an_equity_launch_writes_the_curve_its_vaults_its_vesting_and_the_metaplex_metadata() {
    let mut launch = launch_env(true);
    let mint = Keypair::new();
    let creator = launch.creator();
    let mut ixs = budget().to_vec();
    ixs.push(launch.create_ix(&mint.pubkey(), &creator, None, equity()));
    let landed = launch.send(&ixs, &[&mint]).expect("the launch lands");
    println!(
        "create_curve_usdc, equity: {} compute units in all, {} in pepper_curve; {} bytes",
        landed.units,
        program_units(&landed.logs),
        landed.size
    );
    assert!(landed.units < CREATE_CU_LIMIT as u64);
    let mint = mint.pubkey();
    let curve_address = curve_pda(&mint);

    // Every field of the curve, in its 354 bytes.
    assert_eq!(launch.env.svm.get_account(&curve_address).unwrap().data.len(), 8 + CurveUsdc::INIT_SPACE);
    assert_eq!(8 + CurveUsdc::INIT_SPACE, 354);
    let curve: CurveUsdc = launch.env.fetch(&curve_address);
    let (_, bump) = Pubkey::find_program_address(&[CURVE_SEED, mint.as_ref()], &pepper_curve::ID);
    let (_, usdc_vault_bump) = Pubkey::find_program_address(&[USDC_VAULT_SEED, mint.as_ref()], &pepper_curve::ID);
    assert_eq!(curve.version, 1);
    assert_eq!(curve.bump, bump);
    assert_eq!(curve.usdc_vault_bump, usdc_vault_bump);
    assert_eq!(curve.phase, PHASE_BONDING);
    assert!(!curve.paused);
    assert!(!curve.migrated);
    assert_eq!(curve.mint, mint);
    assert_eq!(curve.creator, creator);
    assert_eq!(curve.creator_usdc_account, Pubkey::default(), "no split, no creator account");
    assert_eq!((curve.creator_split_bps, curve.fanex_split_bps), (0, 0));
    assert_eq!(curve.creator_target_usdc, 0);
    assert_eq!(curve.graduation_threshold_usdc, MIN_GRADUATION_THRESHOLD_USDC);
    assert_eq!((curve.creator_cap_usdc, curve.fanex_cap_usdc), (0, 0));
    assert_eq!(curve.virtual_usdc_reserves, 5_000 * USDC);
    assert_eq!(curve.virtual_token_reserves, TOKEN_TOTAL_SUPPLY);
    assert_eq!(curve.real_usdc_reserves, 0);
    assert_eq!(curve.real_token_reserves, CURVE_SUPPLY);
    assert_eq!(curve.migration_token_reserves, MIGRATION_SUPPLY);
    assert_eq!((curve.creator_received_usdc, curve.fanex_received_usdc), (0, 0));
    assert_eq!(curve.created_at, OCT_5_2026);
    assert_eq!((curve.graduated_at, curve.migrated_at), (0, 0));
    assert_eq!(curve.reserved, [0; 128]);

    // The supply in the two vaults, both the curve PDA's; the USDC vault empty, at the address its
    // stored bump gives back: no USDC moves at launch.
    let token_vault = launch.env.token_account(&ata(&curve_address, &mint));
    assert_eq!((token_vault.mint, token_vault.owner, token_vault.amount), (mint, curve_address, CURVE_SUPPLY + MIGRATION_SUPPLY));
    let vesting_vault = launch.env.token_account(&pda(&[VESTING_VAULT_SEED, mint.as_ref()]));
    assert_eq!((vesting_vault.mint, vesting_vault.owner, vesting_vault.amount), (mint, curve_address, VESTING_SUPPLY));
    let usdc_vault_address = usdc_vault_pda(&mint);
    assert_eq!(
        Pubkey::create_program_address(&[USDC_VAULT_SEED, mint.as_ref(), &[curve.usdc_vault_bump]], &pepper_curve::ID).unwrap(),
        usdc_vault_address
    );
    let usdc_vault = launch.env.token_account(&usdc_vault_address);
    assert_eq!((usdc_vault.mint, usdc_vault.owner, usdc_vault.amount), (launch.config.usdc_mint, curve_address, 0));
    assert_eq!(usdc_vault.close_authority, COption::None);
    assert_eq!(launch.env.token_amount(&launch.config.fee_usdc_account), 0);

    // The mint: the whole supply, no mint authority left and none to freeze.
    let mint_state = spl_token::state::Mint::unpack(&launch.env.svm.get_account(&mint).unwrap().data).unwrap();
    assert_eq!(mint_state.mint_authority, COption::None);
    assert_eq!(mint_state.freeze_authority, COption::None);
    assert_eq!(mint_state.supply, TOKEN_TOTAL_SUPPLY);
    assert_eq!(mint_state.decimals, TOKEN_DECIMALS);

    // The metadata, its update authority `UsdcConfig.admin`, not the fee wallet a SOL launch names.
    let account = launch.env.svm.get_account(&Metadata::find_pda(&mint).0).expect("the metadata exists");
    assert_eq!(account.owner, mpl_token_metadata::ID);
    let metadata = Metadata::safe_deserialize(&account.data).unwrap();
    assert_eq!(metadata.mint, mint);
    assert_eq!(metadata.name.trim_end_matches('\0'), NAME);
    assert_eq!(metadata.symbol.trim_end_matches('\0'), SYMBOL);
    assert_eq!(metadata.uri.trim_end_matches('\0'), URI);
    assert_eq!(metadata.update_authority, launch.usdc_admin);
    let fee_wallet = launch.env.fetch::<GlobalConfig>(&config_pda()).fanex_fee_wallet;
    assert_ne!(metadata.update_authority, fee_wallet);
    assert!(metadata.is_mutable);
    assert_eq!(metadata.seller_fee_basis_points, 0);
    assert_eq!(metadata.creators, None);

    // The vesting as today's: the creator's, nothing released, counting from the launch.
    let vesting: Vesting = launch.env.fetch(&pda(&[VESTING_SEED, mint.as_ref()]));
    let (_, vesting_bump) = Pubkey::find_program_address(&[VESTING_SEED, mint.as_ref()], &pepper_curve::ID);
    assert_eq!(vesting.mint, mint);
    assert_eq!(vesting.beneficiary, creator);
    assert_eq!(vesting.total_amount, VESTING_SUPPLY);
    assert_eq!(vesting.released_amount, 0);
    assert_eq!(vesting.start_time, OCT_5_2026);
    assert_eq!(vesting.cliff_seconds, VESTING_CLIFF_SECONDS);
    assert_eq!(vesting.duration_seconds, VESTING_DURATION_SECONDS);
    assert_eq!(vesting.bump, vesting_bump);

    let created: CurveCreatedUsdc = event(&landed.logs);
    assert_eq!(created.mint, mint);
    assert_eq!(created.creator, creator);
    assert_eq!(created.creator_target_usdc, 0);
    assert_eq!(created.graduation_threshold_usdc, MIN_GRADUATION_THRESHOLD_USDC);
}

/// The revenue launch as one transaction, the creator's ATA opened and hardened in it, so the
/// creator signs; its compute units and its size against the 1,232 bytes of a packet, recorded in
/// PEPPER-CURVE-PROGRAM.md §13.
#[test]
fn a_revenue_launch_opens_and_hardens_the_creators_usdc_ata_in_its_own_transaction() {
    let mut launch = launch_env(true);
    let mint = Keypair::new();
    let creator = launch.env.creator.insecure_clone();
    assert!(!launch.env.exists(&launch.creator_ata()));
    let ixs = launch.revenue_launch_ixs(&mint.pubkey(), revenue());
    let landed = launch.send(&ixs, &[&mint, &creator]).expect("the launch lands");
    println!(
        "create_curve_usdc, revenue, with the ATA create and the SetAuthority: {} compute units in all, {} in pepper_curve; {} bytes of {}",
        landed.units,
        program_units(&landed.logs),
        landed.size,
        PACKET_DATA_SIZE
    );
    assert!(landed.units < CREATE_CU_LIMIT as u64, "{} compute units", landed.units);
    assert!(landed.size <= PACKET_DATA_SIZE, "{} bytes", landed.size);

    let curve: CurveUsdc = launch.env.fetch(&curve_pda(&mint.pubkey()));
    assert_eq!(curve.creator, creator.pubkey());
    assert_eq!(curve.creator_usdc_account, launch.creator_ata());
    assert_eq!((curve.creator_split_bps, curve.fanex_split_bps), (3_760, 240));
    assert_eq!(curve.creator_target_usdc, MIN_CREATOR_TARGET_USDC);
    assert_eq!((curve.creator_cap_usdc, curve.fanex_cap_usdc), (1_504 * USDC, 96 * USDC));
    let account = launch.env.token_account(&launch.creator_ata());
    assert_eq!(account.owner, creator.pubkey());
    assert_eq!(account.close_authority, COption::Some(launch.sponsor.pubkey()));
    let created: CurveCreatedUsdc = event(&landed.logs);
    assert_eq!(created.creator_target_usdc, MIN_CREATOR_TARGET_USDC);

    // The same transaction with the longest strings the program takes: 32, 10 and 200 bytes.
    let longest = CreateCurveUsdcParams {
        name: "N".repeat(MAX_NAME_LEN),
        symbol: "S".repeat(MAX_USDC_SYMBOL_LEN),
        uri: format!("https://pepper.fan/{}", "u".repeat(MAX_URI_LEN - 19)),
        ..revenue()
    };
    assert_eq!(longest.uri.len(), MAX_URI_LEN);
    // The app's own uri with the longest name and symbol: the launch as the server builds it.
    let app_longest = CreateCurveUsdcParams {
        name: "N".repeat(MAX_NAME_LEN),
        symbol: "S".repeat(MAX_USDC_SYMBOL_LEN),
        uri: format!("https://pepper.fan/api/solana/metadata/{}", "S".repeat(MAX_USDC_SYMBOL_LEN)),
        ..revenue()
    };
    let size_of = |params: CreateCurveUsdcParams| {
        let other = Keypair::new();
        let ixs = launch.revenue_launch_ixs(&other.pubkey(), params);
        wire_size(&v0_transaction(
            &ixs,
            &launch.sponsor.pubkey(),
            &[&launch.sponsor, &other, &creator],
            launch.env.svm.latest_blockhash(),
        ))
    };
    let (app, max) = (size_of(app_longest), size_of(longest));
    println!("the same launch: {app} bytes with the app's uri and the longest name and symbol, {max} with every string at its cap");
    assert!(app <= PACKET_DATA_SIZE, "{app} bytes");
    // Every string at its cap does not fit one packet: the launch builder splits the ATA's two
    // instructions into a transaction of their own when it must (usdc-curve plan, T15).
    assert!(max > PACKET_DATA_SIZE, "{max} bytes");
}

#[test]
fn an_ata_already_hardened_is_taken_without_the_creators_signature() {
    let mut launch = launch_env(true);
    // Hardened before the launch, as a raise leaves it: the sponsor its close authority.
    let creator = launch.env.creator.insecure_clone();
    let harden = launch.harden_ixs();
    launch.send(&harden, &[&creator]).expect("the ATA hardened");

    let mint = Keypair::new();
    let mut ixs = budget().to_vec();
    ixs.push(launch.create_ix(&mint.pubkey(), &creator.pubkey(), Some(launch.creator_ata()), revenue()));
    let landed = launch.send(&ixs, &[&mint]).expect("the launch lands, signed by the sponsor and the mint alone");
    println!("create_curve_usdc, revenue, the ATA hardened before: {} bytes", landed.size);
    let curve: CurveUsdc = launch.env.fetch(&curve_pda(&mint.pubkey()));
    assert_eq!(curve.creator_usdc_account, launch.creator_ata());
}

/// A fan-led launch: the keyless placeholder as the creator and the vesting's beneficiary, no split.
/// Once the curve exists, `waive_launch_guard` refuses its mint: it reads only that an account of
/// the program sits at `["curve", mint]`, which a `CurveUsdc` is.
#[test]
fn a_fan_led_launch_lands_and_the_launch_guard_waiver_is_refused_after_it() {
    let mut launch = launch_env(true);
    let mint = Keypair::new();
    let placeholder = fan_led_placeholder();
    launch.try_launch_with_mint(&mint, &placeholder).expect("the fan-led launch lands");
    let curve: CurveUsdc = launch.env.fetch(&curve_pda(&mint.pubkey()));
    assert_eq!(curve.creator, placeholder);
    let vesting: Vesting = launch.env.fetch(&pda(&[VESTING_SEED, mint.pubkey().as_ref()]));
    assert_eq!(vesting.beneficiary, placeholder);

    let admin = launch.env.admin.insecure_clone();
    let authority = Keypair::new();
    launch.env.set_launch_guard_authority_by(&admin, &authority.pubkey()).unwrap();
    let sponsor = launch.sponsor.insecure_clone();
    let refused = launch.env.waive(&sponsor, &authority, &mint.pubkey());
    expect_err(refused, code(FanexError::LaunchGuardAfterLaunch));
    assert!(!launch.env.exists(&waiver_pda(&mint.pubkey())));
}

impl Launch {
    /// An equity launch on `mint` for `creator`, through Metaplex.
    fn try_launch_with_mint(&mut self, mint: &Keypair, creator: &Pubkey) -> Result<Landed, TransactionError> {
        let mut ixs = budget().to_vec();
        ixs.push(self.create_ix(&mint.pubkey(), creator, None, equity()));
        self.send(&ixs, &[mint])
    }
}

// ── Refusals, without Metaplex loaded ───────────────────────────────────────

/// Without Metaplex, a launch every check lets through fails at the CPI alone, with no code of the
/// program: so in the refusals below, each code is the check's own, raised before the CPI.
#[test]
fn without_metaplex_a_valid_launch_fails_only_at_the_metaplex_cpi() {
    let mut launch = launch_env(false);
    let mint = Keypair::new();
    let create = launch.create_ix(&mint.pubkey(), &launch.creator(), None, equity());
    let tx = Transaction::new_signed_with_payer(
        &[create],
        Some(&launch.sponsor.pubkey()),
        &[&launch.sponsor, &mint],
        launch.env.svm.latest_blockhash(),
    );
    let failed = launch.env.svm.send_transaction(tx).expect_err("no Metaplex to call");
    println!("without Metaplex: {:?}\n{}", failed.err, failed.meta.logs.join("\n"));
    match failed.err {
        TransactionError::InstructionError(_, InstructionError::Custom(c)) => {
            panic!("a custom error {c}: a check refused a valid launch")
        }
        TransactionError::InstructionError(_, _) => {}
        other => panic!("expected the Metaplex CPI to fail, got {other:?}"),
    }
    // Both mints went through, so every check had passed: the call after them is Metaplex's.
    let mints = failed.meta.logs.iter().filter(|line| line.as_str() == "Program log: Instruction: MintTo").count();
    assert_eq!(mints, 2, "{}", failed.meta.logs.join("\n"));
}

#[test]
fn a_launch_paid_by_any_key_but_the_launcher_is_refused() {
    let mut launch = launch_env(false);
    let creator = launch.creator();
    // A stranger, then the `GlobalConfig` admin and fee wallet: neither is `UsdcConfig.launcher`.
    let stranger = launch.env.funded_wallet(100 * SOL);
    let admin = launch.env.admin.insecure_clone();
    for payer in [stranger, admin] {
        let mint = Keypair::new();
        let mut accounts = launch.accounts(&mint.pubkey(), &creator, None);
        accounts.payer = payer.pubkey();
        let create = ix(accounts, pepper_curve::instruction::CreateCurveUsdc { params: equity() });
        let refused = launch.send_paid_by(&payer, &[create], &[&mint]).map(|_| ());
        expect_err(refused, code(FanexError::Unauthorized));
        assert!(!launch.env.exists(&curve_pda(&mint.pubkey())));
    }
}

#[test]
fn a_paused_usdc_side_refuses_every_launch() {
    let mut launch = launch_env(false);
    let mut config = launch.config.clone();
    config.paused = true;
    launch.env.put_usdc_config(&config);
    let creator = launch.creator();
    launch.assert_refused(&creator, None, equity(), code(FanexError::GloballyPaused));
    launch.assert_revenue_refused(revenue(), code(FanexError::GloballyPaused));
}

#[test]
fn no_curve_launches_while_any_raydium_address_is_unset() {
    // 2026-10-24: a curve launched before `set_usdc_raydium` would reach its threshold and wait on
    // `RaydiumNotSet`, so the launch refuses first, each of the five apart, after the pause.
    let mut launch = launch_env(false);
    let creator = launch.creator();
    type Unset = fn(&mut UsdcConfig);
    let cases: [(&str, Unset); 5] = [
        ("raydium_cpmm_program", |c| c.raydium_cpmm_program = Pubkey::default()),
        ("raydium_amm_config", |c| c.raydium_amm_config = Pubkey::default()),
        ("raydium_create_pool_fee", |c| c.raydium_create_pool_fee = Pubkey::default()),
        ("raydium_lock_program", |c| c.raydium_lock_program = Pubkey::default()),
        ("raydium_lock_authority", |c| c.raydium_lock_authority = Pubkey::default()),
    ];
    for (name, unset) in cases {
        let mut config = launch.config.clone();
        unset(&mut config);
        launch.env.put_usdc_config(&config);
        println!("unset: {name}");
        launch.assert_refused(&creator, None, equity(), code(FanexError::RaydiumNotSet));
        launch.assert_revenue_refused(revenue(), code(FanexError::RaydiumNotSet));
    }
    // The pause is read first.
    let mut config = launch.config.clone();
    config.raydium_lock_program = Pubkey::default();
    config.paused = true;
    launch.env.put_usdc_config(&config);
    launch.assert_refused(&creator, None, equity(), code(FanexError::GloballyPaused));
}

#[test]
fn strings_over_their_caps_are_refused() {
    let mut launch = launch_env(false);
    let creator = launch.creator();
    let too_long = code(FanexError::StringTooLong);
    // An 11- or 12-byte symbol passes the SOL launch's cap; Metaplex refuses it, so this one does.
    for symbol in ["PEPPERTEST1", "PEPPERTEST12"] {
        assert!(symbol.len() <= MAX_SYMBOL_LEN);
        launch.assert_refused(&creator, None, CreateCurveUsdcParams { symbol: symbol.to_string(), ..equity() }, too_long);
    }
    let name = "N".repeat(MAX_NAME_LEN + 1);
    launch.assert_refused(&creator, None, CreateCurveUsdcParams { name, ..equity() }, too_long);
    let uri = "u".repeat(MAX_URI_LEN + 1);
    launch.assert_refused(&creator, None, CreateCurveUsdcParams { uri, ..equity() }, too_long);
}

#[test]
fn splits_that_leave_the_curve_no_share_are_refused() {
    let mut launch = launch_env(false);
    for (creator_split_bps, fanex_split_bps) in [(9_580, 420), (0, 10_000), (u16::MAX, u16::MAX)] {
        let params = CreateCurveUsdcParams { creator_split_bps, fanex_split_bps, ..revenue() };
        launch.assert_revenue_refused(params, code(FanexError::InvalidSplitBps));
    }
}

#[test]
fn a_raise_under_1_600_usdc_is_refused() {
    let mut launch = launch_env(false);
    let params = CreateCurveUsdcParams {
        creator_target_usdc: MIN_CREATOR_TARGET_USDC - 1,
        creator_cap_usdc: 1_504 * USDC - 1,
        ..revenue()
    };
    launch.assert_revenue_refused(params, code(FanexError::CreatorTargetTooLow));
    // A Fanex split alone is a raise too.
    let fanex_only = CreateCurveUsdcParams {
        creator_target_usdc: 0,
        creator_split_bps: 0,
        creator_cap_usdc: 0,
        fanex_cap_usdc: 0,
        ..revenue()
    };
    let creator = launch.creator();
    launch.assert_refused(&creator, None, fanex_only, code(FanexError::CreatorTargetTooLow));
}

#[test]
fn the_fan_led_placeholder_takes_no_creator_split() {
    let mut launch = launch_env(false);
    launch.assert_refused(&fan_led_placeholder(), None, revenue(), code(FanexError::PlaceholderCannotRaise));
}

#[test]
fn a_threshold_outside_2_400_to_80_000_usdc_is_refused() {
    let mut launch = launch_env(false);
    let creator = launch.creator();
    for graduation_threshold_usdc in [0, MIN_GRADUATION_THRESHOLD_USDC - 1, CAP_GRADUATION_THRESHOLD_USDC + 1] {
        let params = CreateCurveUsdcParams { graduation_threshold_usdc, ..equity() };
        launch.assert_refused(&creator, None, params, code(FanexError::CpmmSeedOutOfRange));
    }
}

#[test]
fn caps_that_miss_the_target_are_refused_even_past_u64() {
    let mut launch = launch_env(false);
    for (creator_cap_usdc, fanex_cap_usdc, creator_target_usdc) in [
        (1_504 * USDC, 96 * USDC - 2, MIN_CREATOR_TARGET_USDC),
        (1_504 * USDC, 96 * USDC + 2, MIN_CREATOR_TARGET_USDC),
        // A sum past u64: refused as caps, where a SOL launch answers `MathOverflow`.
        (u64::MAX, 1, u64::MAX),
    ] {
        let params = CreateCurveUsdcParams { creator_cap_usdc, fanex_cap_usdc, creator_target_usdc, ..revenue() };
        launch.assert_revenue_refused(params, code(FanexError::InvalidCaps));
    }
}

#[test]
fn a_supply_that_misses_the_total_or_the_virtual_reserve_is_refused() {
    let mut launch = launch_env(false);
    let creator = launch.creator();
    let refused = code(FanexError::InvalidSupplyAllocation);
    launch.assert_refused(&creator, None, CreateCurveUsdcParams { curve_token_supply: CURVE_SUPPLY + 1, ..equity() }, refused);
    launch.assert_refused(&creator, None, CreateCurveUsdcParams { migration_token_supply: MIGRATION_SUPPLY - 1, ..equity() }, refused);
    // A virtual token reserve under the tokens for sale.
    launch.assert_refused(&creator, None, CreateCurveUsdcParams { virtual_token_reserves: CURVE_SUPPLY - 1, ..equity() }, refused);
    // A sum past u64, as today: `MathOverflow`.
    let overflowing = CreateCurveUsdcParams { curve_token_supply: u64::MAX, ..equity() };
    launch.assert_refused(&creator, None, overflowing, code(FanexError::MathOverflow));
}

#[test]
fn a_launch_with_no_tokens_for_the_pool_or_none_for_sale_is_refused() {
    // The internal audit (2026-10-29): with no migration tokens a sold-out curve seeds a pool with
    // no token side, which CPMM refuses, so its holders' USDC waits in Graduating for good.
    let mut launch = launch_env(false);
    let creator = launch.creator();
    let refused = code(FanexError::InvalidSupplyAllocation);
    let no_pool = CreateCurveUsdcParams { curve_token_supply: CURVE_SUPPLY + MIGRATION_SUPPLY, migration_token_supply: 0, ..equity() };
    launch.assert_refused(&creator, None, no_pool, refused);
    let no_sale = CreateCurveUsdcParams { curve_token_supply: 0, migration_token_supply: CURVE_SUPPLY + MIGRATION_SUPPLY, ..equity() };
    launch.assert_refused(&creator, None, no_sale, refused);
}

#[test]
fn a_raise_off_the_94_6_relation_or_past_the_target_ceiling_is_refused() {
    // The internal audit (2026-10-29): with the launcher's key alone, 99.99% of each buy to the
    // creator, its target 10^12 USDC, the threshold 80,000.
    let mut launch = launch_env(false);
    let refused = code(FanexError::LaunchTermsOutOfBounds);
    let take = CreateCurveUsdcParams {
        creator_split_bps: 9_999,
        fanex_split_bps: 0,
        creator_target_usdc: 1_000_000_000_000_000_000,
        creator_cap_usdc: 1_000_000_000_000_000_000,
        fanex_cap_usdc: 0,
        graduation_threshold_usdc: CAP_GRADUATION_THRESHOLD_USDC,
        ..revenue()
    };
    launch.assert_revenue_refused(take, refused);
    // One µUSDC past the ceiling, its terms on the relation.
    launch.assert_revenue_refused(builder_revenue(MAX_CREATOR_TARGET_USDC + 1), refused);
    // A threshold off the target's formula, the splits two bps off, the caps off 94/6.
    let good = builder_revenue(10_000 * USDC);
    let off = [
        CreateCurveUsdcParams { graduation_threshold_usdc: good.graduation_threshold_usdc + 1, ..good.clone() },
        CreateCurveUsdcParams { creator_split_bps: good.creator_split_bps + 2, ..good.clone() },
        CreateCurveUsdcParams { fanex_split_bps: good.fanex_split_bps + 2, ..good.clone() },
        CreateCurveUsdcParams { creator_cap_usdc: good.creator_cap_usdc - 2, fanex_cap_usdc: good.fanex_cap_usdc + 2, ..good.clone() },
    ];
    for params in off {
        launch.assert_revenue_refused(params, refused);
    }
}

#[test]
fn an_equity_launch_naming_a_target_or_a_cap_is_refused() {
    // A target with no split: the holders' raise scale would read a raise never made.
    let mut launch = launch_env(false);
    let creator = launch.creator();
    let refused = code(FanexError::LaunchTermsOutOfBounds);
    for params in [
        CreateCurveUsdcParams { creator_target_usdc: MIN_CREATOR_TARGET_USDC, creator_cap_usdc: 1_504 * USDC, fanex_cap_usdc: 96 * USDC, ..equity() },
        CreateCurveUsdcParams { creator_cap_usdc: 1, creator_target_usdc: 1, ..equity() },
    ] {
        launch.assert_refused(&creator, None, params, refused);
    }
}

#[test]
fn the_app_s_own_launches_land_from_the_floor_to_the_ceiling() {
    // The launch builder's terms at the economic model's rows: 1,600 USDC (37.60% to the creator),
    // 80,000 (the threshold at its cap, 47.00%) and the 100,000 ceiling (52.22%).
    let mut launch = launch_env(true);
    let creator = launch.env.creator.insecure_clone();
    let harden = launch.harden_ixs();
    launch.send(&harden, &[&creator]).expect("the ATA hardened");
    for (target, creator_bps) in [(MIN_CREATOR_TARGET_USDC, 3_760), (80_000 * USDC, 4_700), (MAX_CREATOR_TARGET_USDC, 5_222)] {
        let params = builder_revenue(target);
        assert_eq!(params.creator_split_bps, creator_bps, "target {target}");
        let mint = Keypair::new();
        let mut ixs = budget().to_vec();
        ixs.push(launch.create_ix(&mint.pubkey(), &creator.pubkey(), Some(launch.creator_ata()), params.clone()));
        launch.send(&ixs, &[&mint]).unwrap_or_else(|e| panic!("target {target}: {e:?}"));
        let curve: CurveUsdc = launch.env.fetch(&curve_pda(&mint.pubkey()));
        assert_eq!((curve.creator_target_usdc, curve.creator_split_bps), (target, creator_bps));
        assert_eq!(curve.graduation_threshold_usdc, params.graduation_threshold_usdc);
    }
}

#[test]
fn a_vesting_outside_its_bounds_is_refused() {
    let mut launch = launch_env(false);
    let creator = launch.creator();
    let refused = code(FanexError::InvalidVestingAllocation);
    // 1% vested, under the 2% floor; 16%, over the 15% ceiling: the supply still adds up.
    for vesting_token_supply in [1_000_000 * 1_000_000, 16_000_000 * 1_000_000] {
        let params = CreateCurveUsdcParams {
            curve_token_supply: TOKEN_TOTAL_SUPPLY - MIGRATION_SUPPLY - vesting_token_supply,
            vesting_token_supply,
            ..equity()
        };
        launch.assert_refused(&creator, None, params, refused);
    }
    for (vesting_cliff_seconds, vesting_duration_seconds) in
        [(-1, 0), (MAX_VESTING_CLIFF_SECONDS + 1, 0), (0, -1), (0, MAX_VESTING_DURATION_SECONDS + 1)]
    {
        let params = CreateCurveUsdcParams { vesting_cliff_seconds, vesting_duration_seconds, ..equity() };
        launch.assert_refused(&creator, None, params, refused);
    }
}

#[test]
fn a_zero_virtual_usdc_reserve_is_refused() {
    let mut launch = launch_env(false);
    let creator = launch.creator();
    launch.assert_refused(&creator, None, CreateCurveUsdcParams { virtual_usdc_reserves: 0, ..equity() }, code(FanexError::ZeroAmount));
}

#[test]
fn the_creators_usdc_account_must_be_their_hardened_ata_with_a_split_and_absent_without() {
    let mut launch = launch_env(false);
    let invalid = code(FanexError::CreatorUsdcAccountInvalid);
    let (creator, usdc, sponsor) = (launch.creator(), launch.config.usdc_mint, launch.sponsor.pubkey());
    let address = launch.creator_ata();

    // With a split and no account.
    launch.assert_refused(&creator, None, revenue(), invalid);

    // A token account of the creator, hardened, away from their ATA.
    let elsewhere = Pubkey::new_unique();
    launch.env.put_token_account(elsewhere, usdc, creator, 0, COption::Some(sponsor));
    launch.assert_refused(&creator, Some(elsewhere), revenue(), invalid);

    // At the ATA's address: handed to another owner, of another mint, closable by anyone but the
    // sponsor.
    let other_mint = Pubkey::new_unique();
    launch.env.put_launched_mint(other_mint);
    for (mint, owner, close_authority) in [
        (usdc, Pubkey::new_unique(), COption::Some(sponsor)),
        (other_mint, creator, COption::Some(sponsor)),
        (usdc, creator, COption::None),
        (usdc, creator, COption::Some(creator)),
        (usdc, creator, COption::Some(Pubkey::new_unique())),
    ] {
        launch.env.put_token_account(address, mint, owner, 0, close_authority);
        launch.assert_refused(&creator, Some(address), revenue(), invalid);
    }

    // Hardened, then frozen by the USDC's freeze authority.
    launch.put_hardened_ata();
    launch.env.freeze_usdc(&address);
    launch.assert_refused(&creator, Some(address), revenue(), invalid);
    launch.env.thaw_usdc(&address);

    // No split: even the creator's hardened ATA is refused.
    launch.assert_refused(&creator, Some(address), equity(), invalid);
}

#[test]
fn a_creators_ata_not_opened_yet_is_refused_by_anchor() {
    let mut launch = launch_env(false);
    let (creator, address) = (launch.creator(), launch.creator_ata());
    assert!(!launch.env.exists(&address));
    launch.assert_refused(&creator, Some(address), revenue(), ACCOUNT_NOT_INITIALIZED);
}

#[test]
fn accounts_other_than_the_configs_are_refused() {
    let mut launch = launch_env(false);
    let creator = launch.creator();
    let mismatch = code(FanexError::AccountMismatch);
    let send = |launch: &mut Launch, edit: &dyn Fn(&mut pepper_curve::accounts::CreateCurveUsdc)| {
        let mint = Keypair::new();
        let mut accounts = launch.accounts(&mint.pubkey(), &creator, None);
        edit(&mut accounts);
        let create = ix(accounts, pepper_curve::instruction::CreateCurveUsdc { params: equity() });
        launch.send(&[create], &[&mint]).map(|_| ())
    };

    // Another 6-decimal mint as the USDC.
    let other_usdc = Pubkey::new_unique();
    launch.env.put_launched_mint(other_usdc);
    expect_err(send(&mut launch, &|a| a.usdc_mint = other_usdc), mismatch);

    // The fee wallet as the update authority, as a SOL launch names it.
    let fee_wallet = launch.env.fetch::<GlobalConfig>(&config_pda()).fanex_fee_wallet;
    expect_err(send(&mut launch, &|a| a.metadata_update_authority = fee_wallet), mismatch);

    // Any program but Metaplex's address in its place, checked by address alone.
    expect_err(send(&mut launch, &|a| a.token_metadata_program = spl_token::ID), CONSTRAINT_ADDRESS);
}

#[test]
fn no_launch_before_the_usdc_config_exists() {
    let mut launch = launch_env(false);
    launch.env.svm.set_account(usdc_config_pda(), Default::default()).unwrap();
    assert!(!launch.env.exists(&usdc_config_pda()));
    let creator = launch.creator();
    launch.assert_refused(&creator, None, equity(), ACCOUNT_NOT_INITIALIZED);
}
