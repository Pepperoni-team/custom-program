//! `initialize_config` and `update_config` (spec §4.1, §4.2; track B's B5a).
//!
//! The config is born only by the upgrade authority this program's ProgramData records: on
//! mainnet the upgrade Squad's vault, which also pays the rent and becomes the admin, one account
//! in three roles. A stranger, an immutable program (its header recording no authority, or the
//! loader's own `--final` leaving the former key's bytes behind the option byte), a program with
//! no ProgramData, and another program's ProgramData are refused. The admin must be an account the
//! system program owns, as a vault is (never the multisig account), the mint a classic 6-decimal
//! one, `curve_program` an executable of the upgradeable loader, and the approver never a funder.
//! Every refusal is read by its code, with the bank left as it was.

mod common;

use anchor_lang::error::ErrorCode;
use anchor_lang::solana_program::bpf_loader_upgradeable;
use anchor_lang::solana_program::program_option::COption;
use anchor_lang::solana_program::program_pack::Pack;
use anchor_lang::Space;
use anchor_spl::token::spl_token;
use common::*;
use pepper_dividends::errors::DividendsError;
use pepper_dividends::events::ConfigUpdated;
use pepper_dividends::instructions::{ConfigKeys, ConfigUpdate};
use pepper_dividends::state::DividendsConfig;
use solana_sdk::{pubkey::Pubkey, signature::Signer};

/// The Squads v4 program, which owns every multisig account.
const SQUADS_V4: Pubkey = Pubkey::from_str_const("SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf");

fn config_bytes(env: &Env) -> Option<Vec<u8>> {
    env.svm.get_account(&config_pda()).filter(|a| a.lamports > 0).map(|a| a.data)
}

/// `ConfigUpdated` as a config holding `config` logs it.
fn updated(config: &DividendsConfig) -> (Pubkey, Pubkey, Pubkey, Pubkey, Pubkey, Pubkey) {
    (config.admin, config.pauser, config.approver, config.closer, config.pepper_funder, config.payer_funder)
}

fn logged(event: &ConfigUpdated) -> (Pubkey, Pubkey, Pubkey, Pubkey, Pubkey, Pubkey) {
    (event.admin, event.pauser, event.approver, event.closer, event.pepper_funder, event.payer_funder)
}

#[test]
fn the_vault_as_payer_upgrade_authority_and_admin_initializes_the_config() {
    // The mainnet shape: one account signs as the payer and as the upgrade authority, and is named
    // the admin. It pays the rent, read from the bank, and the fee of its one signature.
    let mut env = Env::new();
    let admin = env.admin.insecure_clone();
    let before = env.lamports(&admin.pubkey());
    let init = env.init_config_ix(&admin.pubkey(), &admin.pubkey(), &admin.pubkey(), env.config_keys());
    let sent = env.send_logged_paid_by(&admin, &[init], &[]).unwrap();

    let account = env.svm.get_account(&config_pda()).unwrap();
    assert_eq!((account.owner, account.data.len()), (pepper_dividends::ID, 403));
    assert_eq!(account.lamports, env.rent(403));
    assert_eq!(before - env.lamports(&admin.pubkey()), env.rent(403) + FEE_PER_SIGNATURE);
    // Every field as the harness writes it for the other suites, `claims_paused_until` at 0.
    let config: DividendsConfig = env.fetch(&config_pda());
    assert_eq!(account.data, account_data(&env.dividends_config(), DividendsConfig::INIT_SPACE));
    assert_eq!((config.version, config.paused, config.claims_paused_until), (1, false, 0));
    assert_eq!((config.usdc_mint, config.curve_program), (env.usdc.mint, pepper_curve::ID));
    assert_eq!(config.reserved, [0; 128]);
    assert_eq!(logged(&event::<ConfigUpdated>(&sent.logs)), updated(&config));
}

#[test]
fn a_payer_apart_from_the_upgrade_authority_pays_the_rent_alone() {
    // The admin named need not sign, nor even exist yet: a vault that holds no lamports is still
    // an account of the system program.
    let mut env = Env::new();
    let (payer, authority) = (env.sponsor.insecure_clone(), env.admin.insecure_clone());
    let vault = Pubkey::new_unique();
    let (payer_before, authority_before) = (env.lamports(&payer.pubkey()), env.lamports(&authority.pubkey()));
    let init = env.init_config_ix(&payer.pubkey(), &authority.pubkey(), &vault, env.config_keys());
    env.send_paid_by(&payer, &[init], &[&authority]).unwrap();
    assert_eq!(payer_before - env.lamports(&payer.pubkey()), env.rent(403) + 2 * FEE_PER_SIGNATURE);
    assert_eq!(env.lamports(&authority.pubkey()), authority_before);
    assert_eq!(env.fetch::<DividendsConfig>(&config_pda()).admin, vault);
}

#[test]
fn only_the_recorded_upgrade_authority_initializes_the_config() {
    let mut env = Env::new();
    let stranger = env.funded_wallet(SOL);
    // pepper_curve's upgrade authority, the pauser and a stranger: none is this program's.
    for signer in [stranger, env.curve_admin.insecure_clone(), env.pauser.insecure_clone()] {
        let init = env.init_config_ix(&signer.pubkey(), &signer.pubkey(), &signer.pubkey(), env.config_keys());
        expect_err(env.send_paid_by(&signer, &[init], &[]), code(DividendsError::Unauthorized));
    }
    assert_eq!(config_bytes(&env), None);
}

#[test]
fn an_immutable_program_takes_no_config() {
    // Two immutable programs: a header that records no authority, and one the loader itself made
    // immutable, as `set-upgrade-authority --final` leaves one on a cluster: its `SetAuthority`
    // writes the 13 bytes of the None header over the 45, so the former key's bytes stay after the
    // option byte. The former authority is refused with everyone else.
    let mut made_final = Env::new();
    let former = made_final.admin.insecure_clone();
    let to_none = bpf_loader_upgradeable::set_upgrade_authority(&pepper_dividends::ID, &former.pubkey(), None);
    made_final.send_paid_by(&former, &[to_none], &[]).unwrap();
    let header = made_final.svm.get_account(&program_data_pda(&pepper_dividends::ID)).unwrap().data[..PROGRAM_DATA_HEADER].to_vec();
    assert_eq!(header[12], 0, "the loader recorded no authority");
    assert_eq!(&header[13..], former.pubkey().as_ref(), "the former key's bytes stay after the option byte");

    for mut env in [Env::loaded(ProgramLoad::Immutable), made_final] {
        let admin = env.admin.insecure_clone();
        let init = env.init_config_ix(&admin.pubkey(), &admin.pubkey(), &admin.pubkey(), env.config_keys());
        expect_err(env.send_paid_by(&admin, &[init], &[]), code(DividendsError::Unauthorized));
        assert_eq!(config_bytes(&env), None);
    }
}

#[test]
fn a_program_with_no_program_data_takes_no_config() {
    // LiteSVM's `add_program_from_file` gives the program no ProgramData: nothing the loader owns
    // sits at its address, so no key is its upgrade authority.
    let mut env = Env::loaded(ProgramLoad::WithoutProgramData);
    assert!(!env.exists(&program_data_pda(&pepper_dividends::ID)));
    let admin = env.admin.insecure_clone();
    let init = env.init_config_ix(&admin.pubkey(), &admin.pubkey(), &admin.pubkey(), env.config_keys());
    expect_err(env.send_paid_by(&admin, &[init], &[]), code(DividendsError::Unauthorized));
    assert_eq!(config_bytes(&env), None);
}

#[test]
fn another_programs_program_data_is_refused_by_its_seeds() {
    // pepper_curve's ProgramData records `curve_admin`, who signs: the address is not this
    // program's ProgramData, whoever its authority is.
    let mut env = Env::new();
    let curve_admin = env.curve_admin.insecure_clone();
    let init = env.init_config_ix_with(
        &curve_admin.pubkey(),
        &curve_admin.pubkey(),
        &program_data_pda(&pepper_curve::ID),
        &curve_admin.pubkey(),
        &env.usdc.mint,
        &pepper_curve::ID,
        env.config_keys(),
    );
    expect_err(env.send_paid_by(&curve_admin, &[init], &[]), anchor_code(ErrorCode::ConstraintSeeds));
    assert_eq!(config_bytes(&env), None);
}

#[test]
fn a_second_initialize_config_is_refused_and_the_first_stays() {
    let mut env = Env::new();
    env.init_config().unwrap();
    let first = config_bytes(&env).unwrap();
    let admin = env.admin.insecure_clone();
    let mut keys = env.config_keys();
    keys.pauser = Pubkey::new_unique();
    let again = env.init_config_ix(&admin.pubkey(), &admin.pubkey(), &admin.pubkey(), keys);
    // The system program refuses to create an account that already exists: its AccountAlreadyInUse.
    expect_err(env.send_paid_by(&admin, &[again], &[]), SYSTEM_ACCOUNT_ALREADY_IN_USE);
    assert_eq!(config_bytes(&env).unwrap(), first);
}

/// A classic SPL Token mint of `decimals`, with no authority, at a new address.
fn put_mint(env: &mut Env, decimals: u8, owner: Pubkey) -> Pubkey {
    let address = Pubkey::new_unique();
    let mut data = vec![0u8; spl_token::state::Mint::LEN];
    spl_token::state::Mint {
        mint_authority: COption::None,
        supply: 0,
        decimals,
        is_initialized: true,
        freeze_authority: COption::None,
    }
    .pack_into_slice(&mut data);
    env.put(address, data, owner);
    address
}

#[test]
fn the_usdc_mint_is_a_classic_six_decimal_mint() {
    let mut env = Env::new();
    let admin = env.admin.insecure_clone();
    let nine = put_mint(&mut env, 9, spl_token::ID);
    let token_2022 = put_mint(&mut env, 6, anchor_spl::token_2022::ID);
    for (mint, refusal) in [
        (nine, anchor_code(ErrorCode::ConstraintMintDecimals)),
        (token_2022, anchor_code(ErrorCode::AccountOwnedByWrongProgram)),
    ] {
        let init = env.init_config_ix_with(
            &admin.pubkey(),
            &admin.pubkey(),
            &program_data_pda(&pepper_dividends::ID),
            &admin.pubkey(),
            &mint,
            &pepper_curve::ID,
            env.config_keys(),
        );
        expect_err(env.send_paid_by(&admin, &[init], &[]), refusal);
    }
    assert_eq!(config_bytes(&env), None);
    // A classic one of 6 decimals other than the suite's is a USDC as far as the program can tell:
    // which mint is Circle's is the deploy script's to check (B13).
    let six = put_mint(&mut env, 6, spl_token::ID);
    let init = env.init_config_ix_with(
        &admin.pubkey(),
        &admin.pubkey(),
        &program_data_pda(&pepper_dividends::ID),
        &admin.pubkey(),
        &six,
        &pepper_curve::ID,
        env.config_keys(),
    );
    env.send_paid_by(&admin, &[init], &[]).unwrap();
    assert_eq!(env.fetch::<DividendsConfig>(&config_pda()).usdc_mint, six);
}

/// An account the Squads program owns, as a multisig account is.
fn put_multisig_account(env: &mut Env) -> Pubkey {
    let address = Pubkey::new_unique();
    env.put(address, vec![7; 120], SQUADS_V4);
    address
}

#[test]
fn a_program_owned_admin_is_refused_at_the_init_and_at_an_update() {
    let mut env = Env::new();
    let admin = env.admin.insecure_clone();
    let multisig = put_multisig_account(&mut env);
    let init = env.init_config_ix(&admin.pubkey(), &admin.pubkey(), &multisig, env.config_keys());
    expect_err(env.send_paid_by(&admin, &[init], &[]), anchor_code(ErrorCode::AccountNotSystemOwned));
    assert_eq!(config_bytes(&env), None);

    env.init_config().unwrap();
    let before = config_bytes(&env).unwrap();
    let update = env.update_config_ix(&admin.pubkey(), Some(multisig), ConfigUpdate::default());
    expect_err(env.send_paid_by(&admin, &[update], &[]), anchor_code(ErrorCode::AccountNotSystemOwned));
    assert_eq!(config_bytes(&env).unwrap(), before);
}

#[test]
fn curve_program_is_an_executable_of_the_upgradeable_loader() {
    let mut env = Env::new();
    let admin = env.admin.insecure_clone();
    // pepper_curve's own bytes loaded elsewhere through LiteSVM's loader, not the upgradeable one.
    let elsewhere = Pubkey::new_unique();
    env.svm.add_program_from_file(elsewhere, curve_so()).unwrap();
    let elsewhere_owner = env.svm.get_account(&elsewhere).unwrap().owner;
    assert_ne!(elsewhere_owner, bpf_loader_upgradeable::ID);
    // Anchor checks the owner before the executable flag.
    for (curve_program, refusal) in [
        // pepper_curve's ProgramData: the upgradeable loader's, but not executable.
        (program_data_pda(&pepper_curve::ID), anchor_code(ErrorCode::ConstraintExecutable)),
        // An executable under another loader.
        (elsewhere, anchor_code(ErrorCode::ConstraintOwner)),
        // The USDC mint: the token program's, and not executable.
        (env.usdc.mint, anchor_code(ErrorCode::ConstraintOwner)),
    ] {
        let init = env.init_config_ix_with(
            &admin.pubkey(),
            &admin.pubkey(),
            &program_data_pda(&pepper_dividends::ID),
            &admin.pubkey(),
            &env.usdc.mint,
            &curve_program,
            env.config_keys(),
        );
        expect_err(env.send_paid_by(&admin, &[init], &[]), refusal);
    }
    assert_eq!(config_bytes(&env), None);
}

#[test]
fn the_approver_is_never_a_funder_at_the_init() {
    let mut env = Env::new();
    let admin = env.admin.insecure_clone();
    for funder in [env.pepper_funder.pubkey(), env.payer.pubkey()] {
        let keys = ConfigKeys { approver: funder, ..env.config_keys() };
        let init = env.init_config_ix(&admin.pubkey(), &admin.pubkey(), &admin.pubkey(), keys);
        expect_err(env.send_paid_by(&admin, &[init], &[]), code(DividendsError::ApproverIsFunder));
    }
    assert_eq!(config_bytes(&env), None);
}

#[test]
fn the_approver_is_never_a_funder_at_an_update_new_or_kept() {
    let mut env = Env::new();
    env.init_config().unwrap();
    let admin = env.admin.insecure_clone();
    let before = config_bytes(&env).unwrap();
    let (approver, fanex, payer) = (env.approver.pubkey(), env.pepper_funder.pubkey(), env.payer.pubkey());
    for update in [
        // A new approver that is a funder.
        ConfigUpdate { approver: Some(fanex), ..ConfigUpdate::default() },
        ConfigUpdate { approver: Some(payer), ..ConfigUpdate::default() },
        // A new funder that is the approver kept.
        ConfigUpdate { pepper_funder: Some(approver), ..ConfigUpdate::default() },
        ConfigUpdate { payer_funder: Some(approver), ..ConfigUpdate::default() },
    ] {
        let ix = env.update_config_ix(&admin.pubkey(), None, update);
        expect_err(env.send_paid_by(&admin, &[ix], &[]), code(DividendsError::ApproverIsFunder));
    }
    assert_eq!(config_bytes(&env).unwrap(), before);
    // The approver and a funder may swap in one update.
    let swap = ConfigUpdate { approver: Some(fanex), pepper_funder: Some(approver), ..ConfigUpdate::default() };
    let ix = env.update_config_ix(&admin.pubkey(), None, swap);
    env.send_paid_by(&admin, &[ix], &[]).unwrap();
    let config: DividendsConfig = env.fetch(&config_pda());
    assert_eq!((config.approver, config.pepper_funder), (fanex, approver));
}

#[test]
fn each_key_updates_alone_and_the_update_logs_all_six() {
    let mut env = Env::new();
    env.init_config().unwrap();
    let admin = env.admin.insecure_clone();
    let fields: [fn(&mut ConfigUpdate, Pubkey); 5] = [
        |u, k| u.pauser = Some(k),
        |u, k| u.approver = Some(k),
        |u, k| u.closer = Some(k),
        |u, k| u.pepper_funder = Some(k),
        |u, k| u.payer_funder = Some(k),
    ];
    for (i, set) in fields.iter().enumerate() {
        let before: DividendsConfig = env.fetch(&config_pda());
        let key = Pubkey::new_unique();
        let mut update = ConfigUpdate::default();
        set(&mut update, key);
        let ix = env.update_config_ix(&admin.pubkey(), None, update);
        let sent = env.send_logged_paid_by(&admin, &[ix], &[]).unwrap();
        let after: DividendsConfig = env.fetch(&config_pda());
        let mut expected = before.clone();
        match i {
            0 => expected.pauser = key,
            1 => expected.approver = key,
            2 => expected.closer = key,
            3 => expected.pepper_funder = key,
            _ => expected.payer_funder = key,
        }
        assert_eq!(account_data(&after, DividendsConfig::INIT_SPACE), account_data(&expected, DividendsConfig::INIT_SPACE), "field {i}");
        assert_eq!(logged(&event::<ConfigUpdated>(&sent.logs)), updated(&after), "field {i}");
    }
    // An update that names nothing changes nothing and still logs the six.
    let before = config_bytes(&env).unwrap();
    let ix = env.update_config_ix(&admin.pubkey(), None, ConfigUpdate::default());
    let sent = env.send_logged_paid_by(&admin, &[ix], &[]).unwrap();
    assert_eq!(config_bytes(&env).unwrap(), before);
    assert_eq!(logged(&event::<ConfigUpdated>(&sent.logs)), updated(&env.fetch(&config_pda())));
}

#[test]
fn the_admin_moves_to_a_system_owned_account_and_the_former_one_is_refused() {
    let mut env = Env::new();
    env.init_config().unwrap();
    let (former, next) = (env.admin.insecure_clone(), env.funded_wallet(SOL));
    let ix = env.update_config_ix(&former.pubkey(), Some(next.pubkey()), ConfigUpdate::default());
    let sent = env.send_logged_paid_by(&former, &[ix], &[]).unwrap();
    let config: DividendsConfig = env.fetch(&config_pda());
    assert_eq!(config.admin, next.pubkey());
    assert_eq!(event::<ConfigUpdated>(&sent.logs).admin, next.pubkey());
    // The set-once fields never move.
    assert_eq!((config.usdc_mint, config.curve_program), (env.usdc.mint, pepper_curve::ID));

    let pauser = Pubkey::new_unique();
    let ix = env.update_config_ix(&former.pubkey(), None, ConfigUpdate { pauser: Some(pauser), ..ConfigUpdate::default() });
    expect_err(env.send_paid_by(&former, &[ix], &[]), code(DividendsError::Unauthorized));
    let ix = env.update_config_ix(&next.pubkey(), None, ConfigUpdate { pauser: Some(pauser), ..ConfigUpdate::default() });
    env.send_paid_by(&next, &[ix], &[]).unwrap();
    assert_eq!(env.fetch::<DividendsConfig>(&config_pda()).pauser, pauser);
}

#[test]
fn every_key_but_the_admin_is_refused_an_update() {
    let mut env = Env::new();
    env.init_config().unwrap();
    let before = config_bytes(&env).unwrap();
    let stranger = env.funded_wallet(SOL);
    for signer in [
        env.pauser.insecure_clone(),
        env.approver.insecure_clone(),
        env.closer.insecure_clone(),
        env.pepper_funder.insecure_clone(),
        env.payer.insecure_clone(),
        stranger,
    ] {
        let ix = env.update_config_ix(&signer.pubkey(), Some(signer.pubkey()), ConfigUpdate::default());
        expect_err(env.send_paid_by(&signer, &[ix], &[]), code(DividendsError::Unauthorized));
    }
    assert_eq!(config_bytes(&env).unwrap(), before);
}
