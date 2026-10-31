//! The USDC side's config and its two pauses, run against the built program
//! (docs/solana/PEPPER-CURVE-PROGRAM.md §5, §5.1; usdc-curve design §10.3.1 to
//! §10.3.3).
//!
//! `initialize_usdc_config` is signed by the program's upgrade authority as its
//! ProgramData records it (the internal audit, 2026-10-29: `GlobalConfig.admin`
//! until then), names its first admin, a system-owned account, takes a 6-decimal
//! USDC mint and the hardened USDC ATA of the fee wallet `GlobalConfig` names (not
//! frozen, closable by the config's PDA alone), and leaves the five Raydium fields
//! all-zero; `update_usdc_config` is signed by `UsdcConfig.admin`, holds a new fee
//! account to the same rule and refuses an all-zero admin. The pauser or the admin
//! pauses, only the admin unpauses, and a curve's pause is bound by its seeds.
//! Every refusal is read by its code.

mod common;

use anchor_lang::error::ErrorCode;
use anchor_lang::solana_program::program_pack::Pack;
use anchor_lang::Space;
use anchor_spl::associated_token::get_associated_token_address as ata;
use anchor_spl::associated_token::spl_associated_token_account;
use anchor_spl::token::spl_token;
use common::*;
use pepper_curve::errors::FanexError;
use pepper_curve::events::{UsdcConfigUpdated, UsdcPauseSet};
use pepper_curve::instructions::InitializeUsdcConfigParams;
use pepper_curve::state::{CurveUsdc, GlobalConfig, UsdcConfig};
use solana_sdk::{
    account::Account,
    instruction::Instruction,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction,
};

/// What the system program answers an `init` of an account already there:
/// `SystemError::AccountAlreadyInUse`.
const ACCOUNT_ALREADY_IN_USE: u32 = 0;

/// Anchor's own code for a `SystemAccount` the system program does not own.
const ACCOUNT_NOT_SYSTEM_OWNED: u32 = 3011;

fn anchor(code: ErrorCode) -> u32 {
    code.into()
}

// ── Instructions ─────────────────────────────────────────────────────────────

/// `initialize_usdc_config` paid and signed by `authority`, as the upgrade authority it claims to be,
/// naming `admin` as the config's first admin.
fn init_ix(authority: &Pubkey, admin: &Pubkey, usdc_mint: &Pubkey, fee_usdc_account: &Pubkey, params: &InitializeUsdcConfigParams) -> Instruction {
    init_usdc_config_ix(authority, authority, admin, usdc_mint, fee_usdc_account, params)
}

/// The five keys `update_usdc_config` takes, each `None` to leave it.
#[derive(Clone, Copy, Default)]
struct Update {
    admin: Option<Pubkey>,
    pauser: Option<Pubkey>,
    migration_authority: Option<Pubkey>,
    lp_fee_owner: Option<Pubkey>,
    launcher: Option<Pubkey>,
}

fn update_ix(signer: &Pubkey, update: Update, new_fee_usdc_account: Option<Pubkey>) -> Instruction {
    ix(
        pepper_curve::accounts::UpdateUsdcConfig {
            admin: *signer,
            usdc_config: usdc_config_pda(),
            global_config: config_pda(),
            new_fee_usdc_account,
        },
        pepper_curve::instruction::UpdateUsdcConfig {
            new_admin: update.admin,
            new_pauser: update.pauser,
            new_migration_authority: update.migration_authority,
            new_lp_fee_owner: update.lp_fee_owner,
            new_launcher: update.launcher,
        },
    )
}

fn pause_ix(signer: &Pubkey, paused: bool) -> Instruction {
    ix(
        pepper_curve::accounts::SetUsdcPaused { authority: *signer, usdc_config: usdc_config_pda() },
        pepper_curve::instruction::SetUsdcPaused { paused },
    )
}

fn curve_pause_ix(signer: &Pubkey, curve: &Pubkey, paused: bool) -> Instruction {
    ix(
        pepper_curve::accounts::SetCurveUsdcPaused { authority: *signer, usdc_config: usdc_config_pda(), curve: *curve },
        pepper_curve::instruction::SetCurveUsdcPaused { paused },
    )
}

// ── Setup ────────────────────────────────────────────────────────────────────

/// Four keys of their own, so a key written to the wrong field shows.
fn distinct_params() -> InitializeUsdcConfigParams {
    InitializeUsdcConfigParams {
        pauser: Pubkey::new_unique(),
        migration_authority: Pubkey::new_unique(),
        lp_fee_owner: Pubkey::new_unique(),
        launcher: Pubkey::new_unique(),
    }
}

/// The keys `Env::usdc_config` writes: the suite's pauser, no migration
/// authority, the admin as the LP fee owner and the launcher.
fn harness_params(env: &Env) -> InitializeUsdcConfigParams {
    let admin = env.admin.pubkey();
    InitializeUsdcConfigParams {
        pauser: env.pauser.pubkey(),
        migration_authority: Pubkey::default(),
        lp_fee_owner: admin,
        launcher: admin,
    }
}

/// A classic SPL mint of `decimals`, made by the token program, its authority the admin.
fn make_mint(env: &mut Env, decimals: u8) -> Pubkey {
    let mint = Keypair::new();
    let admin = env.admin.pubkey();
    let rent = env.svm.minimum_balance_for_rent_exemption(spl_token::state::Mint::LEN);
    let ixs = [
        system_instruction::create_account(&admin, &mint.pubkey(), rent, spl_token::state::Mint::LEN as u64, &spl_token::ID),
        spl_token::instruction::initialize_mint2(&spl_token::ID, &mint.pubkey(), &admin, None, decimals).unwrap(),
    ];
    env.send(&ixs, &[&mint]).unwrap();
    mint.pubkey()
}

/// `owner`'s associated account of `mint`, opened by the admin.
fn open_ata(env: &mut Env, owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    let open = spl_associated_token_account::instruction::create_associated_token_account_idempotent(
        &env.admin.pubkey(),
        owner,
        mint,
        &spl_token::ID,
    );
    env.send(&[open], &[]).unwrap();
    ata(owner, mint)
}

/// A USDC account of `owner` that is not its ATA, opened by the admin at a fresh keypair's address.
fn second_usdc_account(env: &mut Env, owner: &Pubkey) -> Pubkey {
    let account = Keypair::new();
    let admin = env.admin.pubkey();
    let rent = env.svm.minimum_balance_for_rent_exemption(spl_token::state::Account::LEN);
    let open = [
        system_instruction::create_account(&admin, &account.pubkey(), rent, spl_token::state::Account::LEN as u64, &spl_token::ID),
        spl_token::instruction::initialize_account3(&spl_token::ID, &account.pubkey(), &env.usdc.mint, owner).unwrap(),
    ];
    env.send(&open, &[&account]).unwrap();
    account.pubkey()
}

/// Names `wallet` as `GlobalConfig.fanex_fee_wallet`, through the SOL `update_config`.
fn set_fee_wallet(env: &mut Env, wallet: &Pubkey) {
    let update = ix(
        pepper_curve::accounts::UpdateConfig { config: config_pda(), admin: env.admin.pubkey() },
        pepper_curve::instruction::UpdateConfig { new_admin: None, new_fanex_fee_wallet: Some(*wallet), new_graduation_authority: None },
    );
    env.send(&[update], &[]).unwrap();
    assert_eq!(env.fetch::<GlobalConfig>(&config_pda()).fanex_fee_wallet, *wallet);
}

/// A new fee wallet named in `GlobalConfig`, holding SOL, and its hardened USDC ATA.
fn new_fee_wallet(env: &mut Env) -> (Keypair, Pubkey) {
    let wallet = env.funded_wallet(SOL);
    set_fee_wallet(env, &wallet.pubkey());
    let account = env.hardened_usdc_account(&wallet);
    (wallet, account)
}

/// Sends `ix` paid and signed by `signer` alone: the admin signs nothing.
fn send_by(env: &mut Env, signer: &Keypair, ix: Instruction) -> Result<Vec<String>, solana_sdk::transaction::TransactionError> {
    let signer = signer.insecure_clone();
    env.send_logged_paid_by(&signer, &[ix], &[])
}

fn unit(result: Result<Vec<String>, solana_sdk::transaction::TransactionError>) -> Result<(), solana_sdk::transaction::TransactionError> {
    result.map(|_| ())
}

/// The config created through `initialize_usdc_config` by the admin as the program's upgrade
/// authority, naming itself the config's admin, with the harness's keys and the fee wallet's
/// hardened USDC ATA for the fees (a key apart from the admin since the fee-wallet split). Its
/// pauser holds SOL, so it pays its own transactions.
fn initialized() -> Env {
    let mut env = Env::upgradeable_by_admin();
    let admin = env.admin.pubkey();
    let fee = env.hardened_fee_account();
    let params = harness_params(&env);
    env.send(&[init_ix(&admin, &admin, &env.usdc.mint, &fee, &params)], &[]).unwrap();
    let pauser = env.pauser.pubkey();
    env.svm.airdrop(&pauser, SOL).unwrap();
    env
}

fn config(env: &Env) -> UsdcConfig {
    env.fetch(&usdc_config_pda())
}

fn raw_config(env: &Env) -> Vec<u8> {
    env.svm.get_account(&usdc_config_pda()).unwrap().data
}

fn keys_of(config: &UsdcConfig) -> UsdcConfigUpdated {
    UsdcConfigUpdated {
        admin: config.admin,
        pauser: config.pauser,
        migration_authority: config.migration_authority,
        fee_usdc_account: config.fee_usdc_account,
        lp_fee_owner: config.lp_fee_owner,
        launcher: config.launcher,
    }
}

fn assert_keys(event: &UsdcConfigUpdated, expected: &UsdcConfigUpdated) {
    assert_eq!(
        [event.admin, event.pauser, event.migration_authority, event.fee_usdc_account, event.lp_fee_owner, event.launcher],
        [expected.admin, expected.pauser, expected.migration_authority, expected.fee_usdc_account, expected.lp_fee_owner, expected.launcher],
    );
}

fn assert_pause_event(logs: &[String], curve: Pubkey, paused: bool, by: Pubkey) {
    let events = events_of::<UsdcPauseSet>(logs);
    assert_eq!(events.len(), 1);
    assert_eq!((events[0].curve, events[0].paused, events[0].by), (curve, paused, by));
}

// ── initialize_usdc_config ───────────────────────────────────────────────────

#[test]
fn the_upgrade_authority_initializes_the_config_naming_its_admin_with_each_key_in_its_own_field() {
    let mut env = Env::upgradeable_by_admin();
    let authority = env.admin.pubkey();
    let fee = env.hardened_fee_account();
    let params = distinct_params();
    // The admin named is an account of its own, not the signer: the vault on mainnet.
    let usdc_admin = Pubkey::new_unique();

    let logs = env.send_logged(&[init_ix(&authority, &usdc_admin, &env.usdc.mint, &fee, &params)], &[]).unwrap();

    let account = env.svm.get_account(&usdc_config_pda()).unwrap();
    assert_eq!((account.owner, account.data.len()), (pepper_curve::ID, 8 + UsdcConfig::INIT_SPACE));
    assert_eq!(account.lamports, env.svm.minimum_balance_for_rent_exemption(491));
    let c = config(&env);
    let bump = Pubkey::find_program_address(&[pepper_curve::constants::USDC_CONFIG_SEED], &pepper_curve::ID).1;
    assert_eq!((c.version, c.bump, c.paused), (1, bump, false));
    assert_eq!(c.admin, usdc_admin, "the admin is the account named, never the signer");
    assert_eq!(c.pauser, params.pauser);
    assert_eq!(c.migration_authority, params.migration_authority);
    assert_eq!(c.usdc_mint, env.usdc.mint);
    assert_eq!(c.fee_usdc_account, fee);
    assert_eq!(c.lp_fee_owner, params.lp_fee_owner);
    assert_eq!(c.launcher, params.launcher);
    // The five Raydium fields stay all-zero until `set_usdc_raydium`, and so does `reserved`.
    for field in [c.raydium_cpmm_program, c.raydium_amm_config, c.raydium_create_pool_fee, c.raydium_lock_program, c.raydium_lock_authority] {
        assert_eq!(field, Pubkey::default());
    }
    assert_eq!(c.reserved, [0u8; 96]);

    let event: UsdcConfigUpdated = event(&logs);
    assert_keys(&event, &keys_of(&c));
    assert_eq!(event.launcher, params.launcher);
    assert_eq!(events_of::<UsdcConfigUpdated>(&logs).len(), 1);
}

#[test]
fn initialized_with_the_harness_keys_it_stores_what_write_usdc_config_writes() {
    // The suites that write the config into the bank stand on the same bytes.
    let env = initialized();
    let written = account_data(&env.usdc_config(), UsdcConfig::INIT_SPACE);
    assert_eq!(raw_config(&env), written);
}

#[test]
fn refuses_an_initialize_by_the_global_admin_once_the_upgrade_authority_is_the_vault() {
    // The internal audit (2026-10-29): with the upgrade authority on the vault and
    // `GlobalConfig.admin` still the hot key, the hot key could write every USDC role, the mint and
    // the migration authority. It no longer can; the vault can.
    let vault = Keypair::new();
    let mut env = Env::upgradeable(Some(vault.pubkey()));
    env.svm.airdrop(&vault.pubkey(), SOL).unwrap();
    let hot = env.admin.insecure_clone();
    assert_eq!(env.fetch::<GlobalConfig>(&config_pda()).admin, hot.pubkey());
    let fee = env.hardened_fee_account();
    let mut params = distinct_params();
    params.migration_authority = hot.pubkey();

    let by_hot = init_ix(&hot.pubkey(), &hot.pubkey(), &env.usdc.mint, &fee, &params);
    expect_err(unit(send_by(&mut env, &hot, by_hot)), code(FanexError::Unauthorized));
    assert!(!env.exists(&usdc_config_pda()));

    // The vault pays, signs as the upgrade authority and names itself the admin.
    let by_vault = init_ix(&vault.pubkey(), &vault.pubkey(), &env.usdc.mint, &fee, &distinct_params());
    send_by(&mut env, &vault, by_vault).unwrap();
    assert_eq!(config(&env).admin, vault.pubkey());
}

#[test]
fn refuses_an_initialize_by_anyone_but_the_upgrade_authority() {
    let mut env = Env::upgradeable_by_admin();
    let fee = env.hardened_fee_account();
    let stranger = env.funded_wallet(SOL);
    let init = init_ix(&stranger.pubkey(), &stranger.pubkey(), &env.usdc.mint, &fee, &distinct_params());
    expect_err(unit(send_by(&mut env, &stranger, init)), code(FanexError::Unauthorized));
    // The upgrade authority named but not signing: Anchor's `AccountNotSigner`.
    let admin = env.admin.pubkey();
    let mut unsigned = init_usdc_config_ix(&stranger.pubkey(), &admin, &admin, &env.usdc.mint, &fee, &distinct_params());
    unsigned.accounts[1].is_signer = false;
    expect_err(unit(send_by(&mut env, &stranger, unsigned)), ACCOUNT_NOT_SIGNER);
    assert!(!env.exists(&usdc_config_pda()));
}

#[test]
fn refuses_every_key_where_no_upgrade_authority_is_recorded() {
    // An immutable program, and one loaded without the upgradeable loader (no ProgramData at all):
    // `set_usdc_raydium`'s gate, so nobody creates the config.
    for mut env in [Env::upgradeable(None), Env::new()] {
        let fee = env.hardened_fee_account();
        let admin = env.admin.insecure_clone();
        let init = init_ix(&admin.pubkey(), &admin.pubkey(), &env.usdc.mint, &fee, &distinct_params());
        expect_err(unit(send_by(&mut env, &admin, init)), code(FanexError::Unauthorized));
        assert!(!env.exists(&usdc_config_pda()));
    }
}

#[test]
fn refuses_forged_program_data_and_another_program_s() {
    let mut env = Env::upgradeable_by_admin();
    let fee = env.hardened_fee_account();
    let stranger = env.funded_wallet(SOL);
    // A ProgramData header naming the stranger, at the right address under another owner.
    let address = program_data_pda(&pepper_curve::ID);
    let original = env.svm.get_account(&address).unwrap();
    let mut forged = original.clone();
    forged.data[..PROGRAM_DATA_HEADER].copy_from_slice(&program_data_bytes(0, Some(stranger.pubkey()), &[]));
    forged.owner = Pubkey::new_unique();
    env.svm.set_account(address, forged).unwrap();
    let init = init_ix(&stranger.pubkey(), &stranger.pubkey(), &env.usdc.mint, &fee, &distinct_params());
    expect_err(unit(send_by(&mut env, &stranger, init)), code(FanexError::Unauthorized));
    env.svm.set_account(address, original).unwrap();

    // The stranger's own program's ProgramData, recording the stranger: at another address.
    let theirs = program_data_pda(&Pubkey::new_unique());
    let data = program_data_bytes(0, Some(stranger.pubkey()), &[0; 64]);
    let lamports = env.svm.minimum_balance_for_rent_exemption(data.len());
    let loader = anchor_lang::solana_program::bpf_loader_upgradeable::id();
    env.svm.set_account(theirs, Account { lamports, data, owner: loader, executable: false, rent_epoch: 0 }).unwrap();
    let mut init = init_ix(&stranger.pubkey(), &stranger.pubkey(), &env.usdc.mint, &fee, &distinct_params());
    init.accounts[2].pubkey = theirs;
    expect_err(unit(send_by(&mut env, &stranger, init)), anchor(ErrorCode::ConstraintSeeds));
    assert!(!env.exists(&usdc_config_pda()));
}

#[test]
fn refuses_an_admin_the_system_program_does_not_own() {
    // A Squads multisig account is its program's and never signs: an admin handed to it is lost.
    // The all-zero key is the system program's own account, which the native loader owns.
    let mut env = Env::upgradeable_by_admin();
    let fee = env.hardened_fee_account();
    let admin = env.admin.pubkey();
    let init = init_ix(&admin, &config_pda(), &env.usdc.mint, &fee, &distinct_params());
    expect_err(env.send(&[init], &[]), ACCOUNT_NOT_SYSTEM_OWNED);
    // All-zero: refused whichever check meets it first, the owner's or the handler's.
    let init = init_ix(&admin, &Pubkey::default(), &env.usdc.mint, &fee, &distinct_params());
    let refused = env.send(&[init], &[]).unwrap_err();
    let codes = [ACCOUNT_NOT_SYSTEM_OWNED, code(FanexError::AdminUnset)];
    assert!(
        matches!(refused, solana_sdk::transaction::TransactionError::InstructionError(_, solana_sdk::instruction::InstructionError::Custom(c)) if codes.contains(&c)),
        "{refused:?}"
    );
    assert!(!env.exists(&usdc_config_pda()));
}

#[test]
fn refuses_an_initialize_that_hands_the_fee_key_to_nobody() {
    // An all-zero `lp_fee_owner` would send every graduated pool's Fee Key, and every fee its
    // locked LP earns, to an address nobody signs for (2026-10-24).
    let mut env = Env::upgradeable_by_admin();
    let admin = env.admin.insecure_clone();
    let fee = env.hardened_fee_account();
    let mut params = distinct_params();
    params.lp_fee_owner = Pubkey::default();
    let init = init_ix(&admin.pubkey(), &admin.pubkey(), &env.usdc.mint, &fee, &params);
    expect_err(unit(send_by(&mut env, &admin, init)), code(FanexError::LpFeeOwnerUnset));
    assert!(!env.exists(&usdc_config_pda()));
}

#[test]
fn refuses_an_update_that_hands_the_fee_key_to_nobody() {
    let mut env = initialized();
    let before = raw_config(&env);
    let admin = env.admin.insecure_clone();
    let unset = Update { lp_fee_owner: Some(Pubkey::default()), ..Update::default() };
    expect_err(unit(send_by(&mut env, &admin, update_ix(&admin.pubkey(), unset, None))), code(FanexError::LpFeeOwnerUnset));
    // With every other key at once, nothing is written either.
    let all = Update {
        admin: Some(Pubkey::new_unique()),
        pauser: Some(Pubkey::new_unique()),
        migration_authority: Some(Pubkey::new_unique()),
        lp_fee_owner: Some(Pubkey::default()),
        launcher: Some(Pubkey::new_unique()),
    };
    expect_err(unit(send_by(&mut env, &admin, update_ix(&admin.pubkey(), all, None))), code(FanexError::LpFeeOwnerUnset));
    assert_eq!(raw_config(&env), before);
    // Another owner, set: taken.
    let owner = Pubkey::new_unique();
    let set = Update { lp_fee_owner: Some(owner), ..Update::default() };
    send_by(&mut env, &admin, update_ix(&admin.pubkey(), set, None)).unwrap();
    assert_eq!(config(&env).lp_fee_owner, owner);
}

#[test]
fn refuses_an_update_that_hands_the_admin_to_nobody() {
    // The internal audit (2026-10-29): only the admin unpauses, and every migrated curve is born
    // paused. A vault's off-curve key is taken.
    let mut env = initialized();
    let before = raw_config(&env);
    let admin = env.admin.insecure_clone();
    let unset = Update { admin: Some(Pubkey::default()), ..Update::default() };
    expect_err(unit(send_by(&mut env, &admin, update_ix(&admin.pubkey(), unset, None))), code(FanexError::AdminUnset));
    assert_eq!(raw_config(&env), before);
    let vault = Pubkey::find_program_address(&[b"multisig", b"vault"], &Pubkey::new_unique()).0;
    let hand_over = Update { admin: Some(vault), ..Update::default() };
    send_by(&mut env, &admin, update_ix(&admin.pubkey(), hand_over, None)).unwrap();
    assert_eq!(config(&env).admin, vault);
}

#[test]
fn refuses_a_second_initialize_and_keeps_the_first() {
    let mut env = initialized();
    let before = raw_config(&env);
    let admin = env.admin.pubkey();
    let fee = ata(&env.fee_wallet.pubkey(), &env.usdc.mint);

    let again = init_ix(&admin, &admin, &env.usdc.mint, &fee, &distinct_params());
    expect_err(env.send(&[again], &[]), ACCOUNT_ALREADY_IN_USE);
    assert_eq!(raw_config(&env), before);
}

#[test]
fn refuses_a_usdc_mint_whose_decimals_are_not_six() {
    let mut env = Env::upgradeable_by_admin();
    let admin = env.admin.pubkey();
    for decimals in [0, 5, 9] {
        let mint = make_mint(&mut env, decimals);
        let fee_wallet = env.fee_wallet.pubkey();
        let fee = open_ata(&mut env, &fee_wallet, &mint);
        let init = init_ix(&admin, &admin, &mint, &fee, &distinct_params());
        expect_err(env.send(&[init], &[]), anchor(ErrorCode::ConstraintMintDecimals));
    }
    assert!(!env.exists(&usdc_config_pda()));
}

#[test]
fn refuses_a_fee_account_of_another_mint() {
    let mut env = Env::upgradeable_by_admin();
    let admin = env.admin.pubkey();
    // Another six-decimal mint: the decimals pass, the fee account's mint does not.
    let other = make_mint(&mut env, 6);
    let fee_wallet = env.fee_wallet.pubkey();
    let fee = open_ata(&mut env, &fee_wallet, &other);

    let init = init_ix(&admin, &admin, &env.usdc.mint, &fee, &distinct_params());
    expect_err(env.send(&[init], &[]), anchor(ErrorCode::ConstraintTokenMint));
    assert!(!env.exists(&usdc_config_pda()));
}

#[test]
fn refuses_a_fee_account_the_fee_wallet_does_not_own() {
    let mut env = Env::upgradeable_by_admin();
    let admin = env.admin.pubkey();
    let stranger = Pubkey::new_unique();
    let fee = env.usdc_account(&stranger);

    let init = init_ix(&admin, &admin, &env.usdc.mint, &fee, &distinct_params());
    expect_err(env.send(&[init], &[]), code(FanexError::AccountMismatch));
    assert!(!env.exists(&usdc_config_pda()));
}

#[test]
fn refuses_a_fee_account_its_owner_could_close_or_that_is_frozen_or_not_its_ata() {
    // The internal audit (2026-10-29): the fee wallet, stolen or cleaning up, could close its fee
    // account and stop every USDC trade. Each of the fee wallet's own USDC accounts below is
    // refused: one that is not its ATA, its ATA with no close authority, with the wallet as close
    // authority, or hardened but frozen.
    let mut env = Env::upgradeable_by_admin();
    let admin = env.admin.pubkey();
    let fee_wallet = env.fee_wallet.insecure_clone();
    let refused = code(FanexError::FeeUsdcAccountInvalid);
    let not_ata = second_usdc_account(&mut env, &fee_wallet.pubkey());
    env.set_close_authority(&fee_wallet, &not_ata, Some(&usdc_config_pda())).unwrap();
    let init = init_ix(&admin, &admin, &env.usdc.mint, &not_ata, &distinct_params());
    expect_err(env.send(&[init], &[]), refused);

    let fee = env.usdc_account(&fee_wallet.pubkey());
    let init = |env: &Env| init_ix(&admin, &admin, &env.usdc.mint, &fee, &distinct_params());
    let open = init(&env);
    expect_err(env.send(&[open], &[]), refused);
    env.set_close_authority(&fee_wallet, &fee, Some(&fee_wallet.pubkey())).unwrap();
    let owners = init(&env);
    expect_err(env.send(&[owners], &[]), refused);
    env.set_close_authority(&fee_wallet, &fee, Some(&usdc_config_pda())).unwrap();
    env.freeze_usdc(&fee);
    let frozen = init(&env);
    expect_err(env.send(&[frozen], &[]), refused);
    assert!(!env.exists(&usdc_config_pda()));

    // Thawed, the hardened ATA is taken.
    env.thaw_usdc(&fee);
    let hardened = init(&env);
    env.send(&[hardened], &[]).unwrap();
    assert_eq!(config(&env).fee_usdc_account, fee);
}

#[test]
fn the_fee_account_is_the_fee_wallet_s_whoever_signs() {
    // `GlobalConfig.fanex_fee_wallet` decides, not the signer: with another
    // wallet named, the admin's own USDC account is refused and the wallet's taken.
    let mut env = Env::upgradeable_by_admin();
    let admin = env.admin.insecure_clone();
    let wallet = env.funded_wallet(SOL);
    set_fee_wallet(&mut env, &wallet.pubkey());
    let admins = env.hardened_usdc_account(&admin);
    let wallets = env.hardened_usdc_account(&wallet);

    let init = init_ix(&admin.pubkey(), &admin.pubkey(), &env.usdc.mint, &admins, &distinct_params());
    expect_err(env.send(&[init], &[]), code(FanexError::AccountMismatch));

    let init = init_ix(&admin.pubkey(), &admin.pubkey(), &env.usdc.mint, &wallets, &distinct_params());
    env.send(&[init], &[]).unwrap();
    assert_eq!(config(&env).fee_usdc_account, wallets);
}

// ── update_usdc_config ───────────────────────────────────────────────────────

#[test]
fn an_update_with_nothing_given_leaves_every_byte() {
    let mut env = initialized();
    let before = raw_config(&env);
    let admin = env.admin.pubkey();

    let logs = env.send_logged(&[update_ix(&admin, Update::default(), None)], &[]).unwrap();
    assert_eq!(raw_config(&env), before);
    // The event still states the keys, as they stand.
    assert_keys(&event::<UsdcConfigUpdated>(&logs), &keys_of(&config(&env)));
}

#[test]
fn an_update_writes_each_key_it_is_given_and_no_other() {
    let mut env = initialized();
    let admin = env.admin.pubkey();
    // A new fee wallet's hardened ATA: the only other account the rule takes.
    let (_, second_fee) = new_fee_wallet(&mut env);

    type Set = fn(&mut UsdcConfig, Pubkey);
    let pauser = Pubkey::new_unique();
    let migration = Pubkey::new_unique();
    let lp_fee_owner = Pubkey::new_unique();
    let launcher = Pubkey::new_unique();
    let cases: [(&str, Update, Option<Pubkey>, Pubkey, Set); 5] = [
        ("pauser", Update { pauser: Some(pauser), ..Default::default() }, None, pauser, |c, k| c.pauser = k),
        ("migration_authority", Update { migration_authority: Some(migration), ..Default::default() }, None, migration, |c, k| {
            c.migration_authority = k
        }),
        ("lp_fee_owner", Update { lp_fee_owner: Some(lp_fee_owner), ..Default::default() }, None, lp_fee_owner, |c, k| {
            c.lp_fee_owner = k
        }),
        ("launcher", Update { launcher: Some(launcher), ..Default::default() }, None, launcher, |c, k| c.launcher = k),
        ("fee_usdc_account", Update::default(), Some(second_fee), second_fee, |c, k| c.fee_usdc_account = k),
    ];
    for (name, update, fee, key, set) in cases {
        let mut expected = config(&env);
        set(&mut expected, key);
        let logs = env.send_logged(&[update_ix(&admin, update, fee)], &[]).unwrap();
        assert_eq!(raw_config(&env), account_data(&expected, UsdcConfig::INIT_SPACE), "{name}");
        assert_keys(&event::<UsdcConfigUpdated>(&logs), &keys_of(&expected));
    }

    // All at once, the admin last: the mint and the Raydium fields have no argument.
    let multisig = Pubkey::new_unique();
    let all = Update {
        admin: Some(multisig),
        pauser: Some(Pubkey::new_unique()),
        migration_authority: Some(Pubkey::default()),
        lp_fee_owner: Some(Pubkey::new_unique()),
        launcher: Some(Pubkey::new_unique()),
    };
    env.send(&[update_ix(&admin, all, Some(second_fee))], &[]).unwrap();
    let c = config(&env);
    assert_eq!(
        [c.admin, c.pauser, c.migration_authority, c.fee_usdc_account, c.lp_fee_owner, c.launcher],
        [multisig, all.pauser.unwrap(), Pubkey::default(), second_fee, all.lp_fee_owner.unwrap(), all.launcher.unwrap()],
    );
    assert_eq!((c.version, c.paused, c.usdc_mint), (1, false, env.usdc.mint));
    for field in [c.raydium_cpmm_program, c.raydium_amm_config, c.raydium_create_pool_fee, c.raydium_lock_program, c.raydium_lock_authority] {
        assert_eq!(field, Pubkey::default());
    }
}

#[test]
fn a_handed_over_admin_signs_and_the_old_one_is_refused() {
    let mut env = initialized();
    let old = env.admin.insecure_clone();
    let multisig = env.funded_wallet(SOL);
    let hand_over = Update { admin: Some(multisig.pubkey()), ..Default::default() };
    env.send(&[update_ix(&old.pubkey(), hand_over, None)], &[]).unwrap();
    assert_eq!(config(&env).admin, multisig.pubkey());

    let launcher = Pubkey::new_unique();
    let change = Update { launcher: Some(launcher), ..Default::default() };
    expect_err(unit(send_by(&mut env, &old, update_ix(&old.pubkey(), change, None))), code(FanexError::Unauthorized));
    assert_ne!(config(&env).launcher, launcher);

    send_by(&mut env, &multisig, update_ix(&multisig.pubkey(), change, None)).unwrap();
    assert_eq!(config(&env).launcher, launcher);

    // The pauses read the admin of now: the old one unpauses nothing, the new one does.
    send_by(&mut env, &multisig, pause_ix(&multisig.pubkey(), true)).unwrap();
    expect_err(unit(send_by(&mut env, &old, pause_ix(&old.pubkey(), false))), code(FanexError::NotPauser));
    send_by(&mut env, &multisig, pause_ix(&multisig.pubkey(), false)).unwrap();
    assert!(!config(&env).paused);
}

#[test]
fn refuses_an_update_by_anyone_but_the_usdc_admin() {
    let mut env = initialized();
    let before = raw_config(&env);
    let stranger = env.funded_wallet(SOL);
    let take_over = Update { admin: Some(stranger.pubkey()), ..Default::default() };
    expect_err(unit(send_by(&mut env, &stranger, update_ix(&stranger.pubkey(), take_over, None))), code(FanexError::Unauthorized));
    // The pauser is no admin either.
    let pauser = env.pauser.insecure_clone();
    expect_err(unit(send_by(&mut env, &pauser, update_ix(&pauser.pubkey(), take_over, None))), code(FanexError::Unauthorized));
    assert_eq!(raw_config(&env), before);
}

#[test]
fn an_update_holds_the_new_fee_account_to_the_init_s_rule() {
    let mut env = initialized();
    let before = raw_config(&env);
    let admin = env.admin.pubkey();

    // Of another mint, owned by the fee wallet.
    let other = make_mint(&mut env, 6);
    let fee_wallet = env.fee_wallet.insecure_clone();
    let other_fee = open_ata(&mut env, &fee_wallet.pubkey(), &other);
    expect_err(env.send(&[update_ix(&admin, Update::default(), Some(other_fee))], &[]), anchor(ErrorCode::ConstraintTokenMint));

    // Of USDC, owned by anyone else.
    let strangers = env.usdc_account(&Pubkey::new_unique());
    expect_err(env.send(&[update_ix(&admin, Update::default(), Some(strangers))], &[]), code(FanexError::AccountMismatch));

    // The fee wallet's own, but not its ATA, or its ATA unhardened or frozen (the internal audit).
    let refused = code(FanexError::FeeUsdcAccountInvalid);
    let not_ata = second_usdc_account(&mut env, &fee_wallet.pubkey());
    env.set_close_authority(&fee_wallet, &not_ata, Some(&usdc_config_pda())).unwrap();
    expect_err(env.send(&[update_ix(&admin, Update::default(), Some(not_ata))], &[]), refused);
    let wallet = env.funded_wallet(SOL);
    set_fee_wallet(&mut env, &wallet.pubkey());
    let wallets = env.usdc_account(&wallet.pubkey());
    expect_err(env.send(&[update_ix(&admin, Update::default(), Some(wallets))], &[]), refused);
    env.set_close_authority(&wallet, &wallets, Some(&usdc_config_pda())).unwrap();
    env.freeze_usdc(&wallets);
    expect_err(env.send(&[update_ix(&admin, Update::default(), Some(wallets))], &[]), refused);
    env.thaw_usdc(&wallets);
    assert_eq!(raw_config(&env), before);

    // A new fee wallet in `GlobalConfig`: its hardened ATA is taken, and the old wallet's account is
    // refused from then on.
    env.send(&[update_ix(&admin, Update::default(), Some(wallets))], &[]).unwrap();
    assert_eq!(config(&env).fee_usdc_account, wallets);
    let olds = ata(&fee_wallet.pubkey(), &env.usdc.mint);
    expect_err(env.send(&[update_ix(&admin, Update::default(), Some(olds))], &[]), code(FanexError::AccountMismatch));
    assert_eq!(config(&env).fee_usdc_account, wallets);
}

#[test]
fn the_fee_wallet_cannot_close_its_hardened_fee_account() {
    // The internal audit (2026-10-29): its owner can sweep it, never close it, so no key but the
    // program's own PDA, which signs for nothing, could stop the swaps through it.
    let mut env = initialized();
    let fee_wallet = env.fee_wallet.insecure_clone();
    let fee = config(&env).fee_usdc_account;
    let close = spl_token::instruction::close_account(&spl_token::ID, &fee, &fee_wallet.pubkey(), &fee_wallet.pubkey(), &[]).unwrap();
    expect_err(env.send(&[close], &[&fee_wallet]), spl_token::error::TokenError::OwnerMismatch as u32);
    assert!(env.exists(&fee));
    // Nor hand the close to itself again: only the close authority moves it.
    env.set_close_authority(&fee_wallet, &fee, Some(&fee_wallet.pubkey())).unwrap_err();
    assert_eq!(env.token_account(&fee).close_authority, anchor_lang::solana_program::program_option::COption::Some(usdc_config_pda()));
}

// ── set_usdc_paused ──────────────────────────────────────────────────────────

#[test]
fn the_pauser_pauses_but_never_unpauses() {
    let mut env = initialized();
    let pauser = env.pauser.insecure_clone();

    // Writing `false` is an unpause whatever the flag holds: refused onto an unpaused config too.
    expect_err(unit(send_by(&mut env, &pauser, pause_ix(&pauser.pubkey(), false))), code(FanexError::NotPauser));

    let logs = send_by(&mut env, &pauser, pause_ix(&pauser.pubkey(), true)).unwrap();
    assert!(config(&env).paused);
    assert_pause_event(&logs, Pubkey::default(), true, pauser.pubkey());
    // Pausing again is no error.
    send_by(&mut env, &pauser, pause_ix(&pauser.pubkey(), true)).unwrap();

    expect_err(unit(send_by(&mut env, &pauser, pause_ix(&pauser.pubkey(), false))), code(FanexError::NotPauser));
    assert!(config(&env).paused);

    let admin = env.admin.insecure_clone();
    let logs = send_by(&mut env, &admin, pause_ix(&admin.pubkey(), false)).unwrap();
    assert!(!config(&env).paused);
    assert_pause_event(&logs, Pubkey::default(), false, admin.pubkey());
}

#[test]
fn the_admin_pauses_and_unpauses() {
    let mut env = initialized();
    let admin = env.admin.insecure_clone();
    let logs = send_by(&mut env, &admin, pause_ix(&admin.pubkey(), true)).unwrap();
    assert!(config(&env).paused);
    assert_pause_event(&logs, Pubkey::default(), true, admin.pubkey());
    send_by(&mut env, &admin, pause_ix(&admin.pubkey(), false)).unwrap();
    assert!(!config(&env).paused);
}

#[test]
fn a_stranger_neither_pauses_nor_unpauses() {
    let mut env = initialized();
    let stranger = env.funded_wallet(SOL);
    expect_err(unit(send_by(&mut env, &stranger, pause_ix(&stranger.pubkey(), true))), code(FanexError::NotPauser));
    assert!(!config(&env).paused);

    let admin = env.admin.insecure_clone();
    send_by(&mut env, &admin, pause_ix(&admin.pubkey(), true)).unwrap();
    expect_err(unit(send_by(&mut env, &stranger, pause_ix(&stranger.pubkey(), false))), code(FanexError::NotPauser));
    assert!(config(&env).paused);
}

#[test]
fn a_paused_config_still_takes_its_admin_instructions() {
    // Neither pause instruction nor the update reads a pause flag: a pause never
    // locks the config, or the admin out of its own unpause.
    let mut env = initialized();
    let admin = env.admin.insecure_clone();
    let pauser = env.pauser.insecure_clone();
    send_by(&mut env, &pauser, pause_ix(&pauser.pubkey(), true)).unwrap();

    let launcher = Pubkey::new_unique();
    send_by(&mut env, &admin, update_ix(&admin.pubkey(), Update { launcher: Some(launcher), ..Default::default() }, None)).unwrap();
    assert_eq!(config(&env).launcher, launcher);
    assert!(config(&env).paused);

    let curve = env.write_curve_usdc(&vectors()[0].state);
    let address = curve_pda(&curve.mint);
    send_by(&mut env, &pauser, curve_pause_ix(&pauser.pubkey(), &address, true)).unwrap();
    send_by(&mut env, &admin, curve_pause_ix(&admin.pubkey(), &address, false)).unwrap();
    assert!(!env.fetch::<CurveUsdc>(&address).paused);

    send_by(&mut env, &admin, pause_ix(&admin.pubkey(), false)).unwrap();
    assert!(!config(&env).paused);
}

#[test]
fn each_side_reads_only_its_own_pause() {
    // The console's kill-switch has to flip both (usdc-curve design §10.3.3).
    let mut env = initialized();
    env.write_tradeable_curve(OCT_5_2026 - DAY);
    let admin = env.admin.insecure_clone();

    send_by(&mut env, &admin, pause_ix(&admin.pubkey(), true)).unwrap();
    assert!(!env.fetch::<GlobalConfig>(&config_pda()).paused);
    let buyer = env.funded_wallet(SOL);
    env.buy(&buyer, SOL / 100).unwrap();
    send_by(&mut env, &admin, pause_ix(&admin.pubkey(), false)).unwrap();

    env.set_paused(true);
    assert!(!config(&env).paused);
    expect_err(env.buy(&buyer, SOL / 100), code(FanexError::GloballyPaused));
}

// ── set_curve_usdc_paused ────────────────────────────────────────────────────

#[test]
fn a_curve_s_pause_follows_the_same_rule_and_reaches_that_curve_alone() {
    let mut env = initialized();
    let paused_one = env.write_curve_usdc(&vectors()[0].state);
    let other = env.write_curve_usdc(&vectors()[0].state);
    let address = curve_pda(&paused_one.mint);
    let pauser = env.pauser.insecure_clone();
    let admin = env.admin.insecure_clone();
    let stranger = env.funded_wallet(SOL);

    expect_err(unit(send_by(&mut env, &stranger, curve_pause_ix(&stranger.pubkey(), &address, true))), code(FanexError::NotPauser));
    expect_err(unit(send_by(&mut env, &pauser, curve_pause_ix(&pauser.pubkey(), &address, false))), code(FanexError::NotPauser));

    let logs = send_by(&mut env, &pauser, curve_pause_ix(&pauser.pubkey(), &address, true)).unwrap();
    assert_pause_event(&logs, address, true, pauser.pubkey());
    let mut expected = paused_one.clone();
    expected.paused = true;
    let stored = env.svm.get_account(&address).unwrap().data;
    assert_eq!(stored, account_data(&expected, CurveUsdc::INIT_SPACE), "the flag and nothing else");
    assert!(!env.fetch::<CurveUsdc>(&curve_pda(&other.mint)).paused);
    assert!(!config(&env).paused);

    expect_err(unit(send_by(&mut env, &pauser, curve_pause_ix(&pauser.pubkey(), &address, false))), code(FanexError::NotPauser));
    expect_err(unit(send_by(&mut env, &stranger, curve_pause_ix(&stranger.pubkey(), &address, false))), code(FanexError::NotPauser));
    assert!(env.fetch::<CurveUsdc>(&address).paused);

    let logs = send_by(&mut env, &admin, curve_pause_ix(&admin.pubkey(), &address, false)).unwrap();
    assert_pause_event(&logs, address, false, admin.pubkey());
    assert_eq!(env.svm.get_account(&address).unwrap().data, account_data(&paused_one, CurveUsdc::INIT_SPACE));
}

#[test]
fn refuses_a_sol_curve_by_its_discriminator() {
    let mut env = initialized();
    // `Env::new` wrote a SOL `Curve` at `["curve", env.mint]`.
    let sol_curve = curve_pda(&env.mint);
    let admin = env.admin.insecure_clone();
    expect_err(unit(send_by(&mut env, &admin, curve_pause_ix(&admin.pubkey(), &sol_curve, true))), ACCOUNT_DISCRIMINATOR_MISMATCH);
    assert!(!env.fetch::<pepper_curve::state::Curve>(&sol_curve).paused);
}

#[test]
fn refuses_a_usdc_curve_anywhere_but_its_own_seeds() {
    let mut env = initialized();
    let curve = env.write_curve_usdc(&vectors()[0].state);
    let admin = env.admin.insecure_clone();
    let bytes = account_data(&curve, CurveUsdc::INIT_SPACE);

    // The same bytes at an address of no seeds, and at another mint's curve address.
    let forged = Pubkey::new_unique();
    let elsewhere = curve_pda(&Pubkey::new_unique());
    for address in [forged, elsewhere] {
        env.put(address, bytes.clone(), pepper_curve::ID);
        expect_err(unit(send_by(&mut env, &admin, curve_pause_ix(&admin.pubkey(), &address, true))), anchor(ErrorCode::ConstraintSeeds));
        assert!(!env.fetch::<CurveUsdc>(&address).paused);
    }
    // At its own address it is taken.
    send_by(&mut env, &admin, curve_pause_ix(&admin.pubkey(), &curve_pda(&curve.mint), true)).unwrap();
}
