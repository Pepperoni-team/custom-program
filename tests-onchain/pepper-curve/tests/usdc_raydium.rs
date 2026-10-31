//! `set_usdc_raydium`, run against the built program loaded under the upgradeable loader, as both
//! clusters run it (docs/solana/PEPPER-CURVE-PROGRAM.md §5; usdc-curve design §10.3.10, §10.11).
//!
//! Only the upgrade authority the program's ProgramData records signs it: `UsdcConfig.admin`, a
//! stranger and anyone on an immutable program are refused, and a program with no ProgramData
//! (`add_program_from_file`) has nothing to read. None of the five addresses may be all-zero, and
//! the `amm_config` must be the one named, owned by the CPMM named, an `AmmConfig` at 1% still
//! creating pools: the mainnet tier comes from the T10 fixture, every other one is that account's
//! bytes changed.
//! Every refusal is read by its code, with the config left as it was.

mod common;

use anchor_lang::error::ErrorCode;
use anchor_lang::{AccountDeserialize, ProgramData, Space};
use common::*;
use pepper_curve::errors::FanexError;
use pepper_curve::events::UsdcRaydiumSet;
use pepper_curve::instructions::SetUsdcRaydiumParams;
use pepper_curve::raydium::{
    parse_amm_config, AMM_CONFIG_DISABLE_CREATE_POOL_OFFSET, AMM_CONFIG_DISCRIMINATOR, AMM_CONFIG_INDEX_OFFSET, AMM_CONFIG_TRADE_FEE_RATE_OFFSET,
};
use pepper_curve::state::UsdcConfig;
use solana_sdk::{
    account::Account,
    instruction::Instruction,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::TransactionError,
};

/// The mainnet CPMM's 1% tier, index 1 (fixtures/raydium_cpmm_amm_config.json).
const AMM_CONFIG_FIXTURE: &str = "raydium_cpmm_amm_config.json";
/// The create-pool fee receiver the mainnet CPMM compiles in.
/// The mainnet LP lock's authority PDA.

/// Devnet's Raydium, which the same binary takes from its config there (usdc-curve design §10.12).
const DEVNET_CPMM: Pubkey = Pubkey::from_str_const("DRaycpLY18LhpbydsBWbVJtxpNv9oXPgjRSfpF2bWpYb");
const DEVNET_CREATE_POOL_FEE: Pubkey = Pubkey::from_str_const("3oE58BKVt8KuYkGxx8zBojugnymWmBiyafWgMrnb6eYy");
const DEVNET_LOCK: Pubkey = Pubkey::from_str_const("DRay25Usp3YJAi7beckgpGUC7mGJ2cR1AVPxhYfwVCUX");
const DEVNET_LOCK_AUTHORITY: Pubkey = Pubkey::from_str_const("7qWVV8UY2bRJfDLP4s37YzBPKUkVB46DStYJBpYbQzu3");

fn anchor(code: ErrorCode) -> u32 {
    code.into()
}

fn mainnet_params() -> SetUsdcRaydiumParams {
    SetUsdcRaydiumParams {
        raydium_cpmm_program: Some(RAYDIUM_CPMM_ID),
        raydium_amm_config: Some(MAINNET_AMM_CONFIG),
        raydium_create_pool_fee: Some(MAINNET_CREATE_POOL_FEE),
        raydium_lock_program: Some(RAYDIUM_LP_LOCK_ID),
        raydium_lock_authority: Some(MAINNET_LOCK_AUTHORITY),
    }
}

/// No field given: each stays the config's.
fn none_given() -> SetUsdcRaydiumParams {
    SetUsdcRaydiumParams {
        raydium_cpmm_program: None,
        raydium_amm_config: None,
        raydium_create_pool_fee: None,
        raydium_lock_program: None,
        raydium_lock_authority: None,
    }
}

/// `set_usdc_raydium(params)` signed by `signer`, the program's own ProgramData passed, and
/// `amm_config` as the tier's account: the client decides, so a test can pass another one.
fn set_ix(signer: &Pubkey, amm_config: &Pubkey, params: &SetUsdcRaydiumParams) -> Instruction {
    set_ix_with(signer, &program_data_pda(&pepper_curve::ID), amm_config, params)
}

fn set_ix_with(signer: &Pubkey, program_data: &Pubkey, amm_config: &Pubkey, params: &SetUsdcRaydiumParams) -> Instruction {
    set_ix_tier(signer, program_data, Some(*amm_config), params)
}

/// As `set_ix_with`, the tier's account left out when `amm_config` is `None`.
fn set_ix_tier(signer: &Pubkey, program_data: &Pubkey, amm_config: Option<Pubkey>, params: &SetUsdcRaydiumParams) -> Instruction {
    ix(
        pepper_curve::accounts::SetUsdcRaydium {
            upgrade_authority: *signer,
            program_data: *program_data,
            usdc_config: usdc_config_pda(),
            raydium_amm_config: amm_config,
        },
        pepper_curve::instruction::SetUsdcRaydium { params: params.clone() },
    )
}

/// Sends `ix` paid and signed by `signer` alone.
fn send_by(env: &mut Env, signer: &Keypair, ix: Instruction) -> Result<Vec<String>, TransactionError> {
    let signer = signer.insecure_clone();
    env.send_logged_paid_by(&signer, &[ix], &[])
}

fn unit(result: Result<Vec<String>, TransactionError>) -> Result<(), TransactionError> {
    result.map(|_| ())
}

fn raw_config(env: &Env) -> Vec<u8> {
    env.svm.get_account(&usdc_config_pda()).unwrap().data
}

/// The program loaded upgradeable, `authority` its upgrade authority (funded, so it pays its own
/// transactions), the harness's `UsdcConfig` written (its admin `env.admin`, the five Raydium
/// fields all-zero), and the mainnet tier at its address.
struct Suite {
    env: Env,
    authority: Keypair,
}

fn suite() -> Suite {
    let authority = Keypair::new();
    let mut env = Env::upgradeable(Some(authority.pubkey()));
    env.svm.airdrop(&authority.pubkey(), SOL).unwrap();
    env.write_usdc_config();
    assert_eq!(env.load_fixture_account(AMM_CONFIG_FIXTURE), MAINNET_AMM_CONFIG);
    Suite { env, authority }
}

/// The mainnet tier's account with its trade fee rate set to `trade_fee_rate`, owned by `owner`:
/// what Raydium's admin would leave after changing that tier's rate, or a tier of another cluster.
fn tier_account(trade_fee_rate: u64, owner: Pubkey) -> Account {
    let (_, mut account) = fixture_account(AMM_CONFIG_FIXTURE);
    let at = AMM_CONFIG_TRADE_FEE_RATE_OFFSET;
    account.data[at..at + 8].copy_from_slice(&trade_fee_rate.to_le_bytes());
    account.owner = owner;
    account
}

// ── The harness ──────────────────────────────────────────────────────────────

#[test]
fn the_program_runs_from_a_program_data_that_records_its_authority() {
    let authority = Pubkey::new_unique();
    for recorded in [Some(authority), None] {
        let env = Env::upgradeable(recorded);
        let program = env.svm.get_account(&pepper_curve::ID).unwrap();
        assert!(program.executable);
        assert_eq!(program.owner, bpf_loader_upgradeable_id());
        assert_eq!(program.data.len(), 36);
        assert_eq!(&program.data[4..], program_data_pda(&pepper_curve::ID).as_ref());

        let program_data = env.svm.get_account(&program_data_pda(&pepper_curve::ID)).unwrap();
        assert_eq!((program_data.owner, program_data.executable), (bpf_loader_upgradeable_id(), false));
        // Anchor's `ProgramData`, which the instruction takes, reads the hand-written header back
        // as the loader's bincode: the authority recorded, or none.
        let decoded = ProgramData::try_deserialize(&mut program_data.data.as_slice()).unwrap();
        assert_eq!(decoded.upgrade_authority_address, recorded);
        let elf = std::fs::read(program_so()).unwrap();
        assert_eq!(&program_data.data[PROGRAM_DATA_HEADER..], &elf[..]);
    }
    // Without it, the program `Env::new` loads has no ProgramData at all.
    let env = Env::new();
    assert!(!env.exists(&program_data_pda(&pepper_curve::ID)));
}

#[test]
fn the_parser_reads_the_mainnet_one_percent_tier() {
    let (address, account) = fixture_account(AMM_CONFIG_FIXTURE);
    assert_eq!((address, account.owner, account.data.len()), (MAINNET_AMM_CONFIG, RAYDIUM_CPMM_ID, 236));
    assert_eq!(account.data[..8], AMM_CONFIG_DISCRIMINATOR);
    let tier = parse_amm_config(&account.data).unwrap();
    assert_eq!((tier.index, tier.trade_fee_rate, tier.create_pool_fee, tier.disable_create_pool), (1, 10_000, 150_000_000, false));
    assert!(tier.charges_one_percent());
}

// ── Who signs ────────────────────────────────────────────────────────────────

#[test]
fn the_upgrade_authority_writes_the_five_addresses_and_nothing_else() {
    let Suite { mut env, authority } = suite();
    let params = mainnet_params();

    let logs = send_by(&mut env, &authority, set_ix(&authority.pubkey(), &MAINNET_AMM_CONFIG, &params)).unwrap();

    let expected = env.usdc_config_with_raydium();
    assert_eq!(raw_config(&env), account_data(&expected, UsdcConfig::INIT_SPACE));

    let events = events_of::<UsdcRaydiumSet>(&logs);
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!(
        [e.raydium_cpmm_program, e.raydium_amm_config, e.raydium_create_pool_fee, e.raydium_lock_program, e.raydium_lock_authority, e.by],
        [RAYDIUM_CPMM_ID, MAINNET_AMM_CONFIG, MAINNET_CREATE_POOL_FEE, RAYDIUM_LP_LOCK_ID, MAINNET_LOCK_AUTHORITY, authority.pubkey()],
    );
}

#[test]
fn a_new_set_replaces_the_old_one_whole() {
    // The way out of a Raydium change, and the one binary on both clusters: devnet's addresses
    // over mainnet's, its 1% tier an account of devnet's CPMM.
    let Suite { mut env, authority } = suite();
    send_by(&mut env, &authority, set_ix(&authority.pubkey(), &MAINNET_AMM_CONFIG, &mainnet_params())).unwrap();

    let devnet_tier = Pubkey::new_unique();
    let mut account = tier_account(10_000, DEVNET_CPMM);
    account.data[AMM_CONFIG_INDEX_OFFSET..AMM_CONFIG_INDEX_OFFSET + 2].copy_from_slice(&3u16.to_le_bytes());
    env.svm.set_account(devnet_tier, account).unwrap();
    let devnet = SetUsdcRaydiumParams {
        raydium_cpmm_program: Some(DEVNET_CPMM),
        raydium_amm_config: Some(devnet_tier),
        raydium_create_pool_fee: Some(DEVNET_CREATE_POOL_FEE),
        raydium_lock_program: Some(DEVNET_LOCK),
        raydium_lock_authority: Some(DEVNET_LOCK_AUTHORITY),
    };
    let logs = send_by(&mut env, &authority, set_ix(&authority.pubkey(), &devnet_tier, &devnet)).unwrap();

    let c: UsdcConfig = env.fetch(&usdc_config_pda());
    assert_eq!(
        [c.raydium_cpmm_program, c.raydium_amm_config, c.raydium_create_pool_fee, c.raydium_lock_program, c.raydium_lock_authority],
        [DEVNET_CPMM, devnet_tier, DEVNET_CREATE_POOL_FEE, DEVNET_LOCK, DEVNET_LOCK_AUTHORITY],
    );
    assert_eq!(event::<UsdcRaydiumSet>(&logs).raydium_amm_config, devnet_tier);
}

#[test]
fn refuses_the_usdc_admin_the_pauser_and_a_stranger() {
    let Suite { mut env, authority } = suite();
    let before = raw_config(&env);
    // `env.admin` is `UsdcConfig.admin` and `GlobalConfig.admin` both: neither is the upgrade authority.
    let admin = env.admin.insecure_clone();
    let pauser = env.pauser.insecure_clone();
    env.svm.airdrop(&pauser.pubkey(), SOL).unwrap();
    let stranger = env.funded_wallet(SOL);
    assert_eq!(env.fetch::<UsdcConfig>(&usdc_config_pda()).admin, admin.pubkey());

    for signer in [&admin, &pauser, &stranger] {
        let set = set_ix(&signer.pubkey(), &MAINNET_AMM_CONFIG, &mainnet_params());
        expect_err(unit(send_by(&mut env, signer, set)), code(FanexError::Unauthorized));
    }
    assert_eq!(raw_config(&env), before);
    // The authority, named as the signer, must sign: Anchor's `AccountNotSigner`.
    let mut unsigned = set_ix(&authority.pubkey(), &MAINNET_AMM_CONFIG, &mainnet_params());
    unsigned.accounts[0].is_signer = false;
    expect_err(unit(send_by(&mut env, &stranger, unsigned)), ACCOUNT_NOT_SIGNER);
    assert_eq!(raw_config(&env), before);
}

#[test]
fn refuses_every_key_on_an_immutable_program() {
    // Two immutable programs: a header that records none, padded with zeros (`Env::upgradeable(None)`),
    // and one the loader itself made immutable, as `set-upgrade-authority --final` (or `deploy
    // --final`, which sends it after the deploy) leaves one on a cluster. Its `SetAuthority` writes
    // the 13 bytes of the None header over the 45, so the former authority's key stays after the
    // option byte: the former authority is refused with the rest.
    let former = Keypair::new();
    let mut made_final = Env::upgradeable(Some(former.pubkey()));
    made_final.svm.airdrop(&former.pubkey(), SOL).unwrap();
    let to_none = anchor_lang::solana_program::bpf_loader_upgradeable::set_upgrade_authority(&pepper_curve::ID, &former.pubkey(), None);
    send_by(&mut made_final, &former, to_none).unwrap();
    let header = made_final.svm.get_account(&program_data_pda(&pepper_curve::ID)).unwrap().data[..PROGRAM_DATA_HEADER].to_vec();
    assert_eq!(header[12], 0, "the loader recorded no authority");
    assert_eq!(&header[13..], former.pubkey().as_ref(), "the former key's bytes stay after the option byte");

    for (mut env, former) in [(Env::upgradeable(None), None), (made_final, Some(former))] {
        env.write_usdc_config();
        env.load_fixture_account(AMM_CONFIG_FIXTURE);
        let before = raw_config(&env);
        let mut signers = vec![env.admin.insecure_clone(), env.funded_wallet(SOL)];
        signers.extend(former);
        for signer in &signers {
            let set = set_ix(&signer.pubkey(), &MAINNET_AMM_CONFIG, &mainnet_params());
            expect_err(unit(send_by(&mut env, signer, set)), code(FanexError::Unauthorized));
        }
        assert_eq!(raw_config(&env), before);
    }
}

#[test]
fn refuses_a_program_loaded_without_the_upgradeable_loader() {
    // `add_program_from_file` gives the program no ProgramData: nothing the loader owns is at its
    // address, so no key is its upgrade authority.
    let mut env = Env::new();
    env.write_usdc_config();
    env.load_fixture_account(AMM_CONFIG_FIXTURE);
    let before = raw_config(&env);
    let admin = env.admin.insecure_clone();
    let set = set_ix(&admin.pubkey(), &MAINNET_AMM_CONFIG, &mainnet_params());
    expect_err(unit(send_by(&mut env, &admin, set)), code(FanexError::Unauthorized));
    assert_eq!(raw_config(&env), before);
}

#[test]
fn refuses_bytes_at_the_program_data_address_the_loader_does_not_own() {
    // The header of a ProgramData naming the stranger, written at the right address under another
    // owner: only the loader writes there on a cluster, and the program reads the owner first.
    let Suite { mut env, .. } = suite();
    let before = raw_config(&env);
    let stranger = env.funded_wallet(SOL);
    let address = program_data_pda(&pepper_curve::ID);
    let mut forged = env.svm.get_account(&address).unwrap();
    forged.data[..PROGRAM_DATA_HEADER].copy_from_slice(&program_data_bytes(0, Some(stranger.pubkey()), &[]));
    forged.owner = Pubkey::new_unique();
    env.svm.set_account(address, forged).unwrap();
    let set = set_ix(&stranger.pubkey(), &MAINNET_AMM_CONFIG, &mainnet_params());
    expect_err(unit(send_by(&mut env, &stranger, set)), code(FanexError::Unauthorized));
    assert_eq!(raw_config(&env), before);
}

#[test]
fn refuses_the_program_data_of_another_program() {
    // A program of the stranger's own, the stranger its upgrade authority: its ProgramData records
    // the signer, but sits at another program's address (`ConstraintSeeds`).
    let Suite { mut env, .. } = suite();
    let before = raw_config(&env);
    let stranger = env.funded_wallet(SOL);
    let theirs = program_data_pda(&Pubkey::new_unique());
    let data = program_data_bytes(0, Some(stranger.pubkey()), &[0; 64]);
    let lamports = env.svm.minimum_balance_for_rent_exemption(data.len());
    env.svm.set_account(theirs, Account { lamports, data, owner: bpf_loader_upgradeable_id(), executable: false, rent_epoch: 0 }).unwrap();

    let set = set_ix_with(&stranger.pubkey(), &theirs, &MAINNET_AMM_CONFIG, &mainnet_params());
    expect_err(unit(send_by(&mut env, &stranger, set)), anchor(ErrorCode::ConstraintSeeds));
    assert_eq!(raw_config(&env), before);
}

// ── What it writes ───────────────────────────────────────────────────────────

#[test]
fn refuses_any_address_left_all_zero() {
    let Suite { mut env, authority } = suite();
    let before = raw_config(&env);
    type Unset = fn(&mut SetUsdcRaydiumParams);
    let cases: [(&str, Unset); 5] = [
        ("raydium_cpmm_program", |p| p.raydium_cpmm_program = Some(Pubkey::default())),
        ("raydium_amm_config", |p| p.raydium_amm_config = Some(Pubkey::default())),
        ("raydium_create_pool_fee", |p| p.raydium_create_pool_fee = Some(Pubkey::default())),
        ("raydium_lock_program", |p| p.raydium_lock_program = Some(Pubkey::default())),
        ("raydium_lock_authority", |p| p.raydium_lock_authority = Some(Pubkey::default())),
    ];
    for (name, unset) in cases {
        let mut params = mainnet_params();
        unset(&mut params);
        // Refused before the tier is read: with the tier's own account, and with the account the
        // all-zero param names (the system program's).
        for amm_config in [MAINNET_AMM_CONFIG, Pubkey::default()] {
            let set = set_ix(&authority.pubkey(), &amm_config, &params);
            let result = unit(send_by(&mut env, &authority, set));
            assert!(result.is_err(), "{name}");
            expect_err(result, code(FanexError::RaydiumNotSet));
        }
    }
    let unset_all = SetUsdcRaydiumParams {
        raydium_cpmm_program: Some(Pubkey::default()),
        raydium_amm_config: Some(Pubkey::default()),
        raydium_create_pool_fee: Some(Pubkey::default()),
        raydium_lock_program: Some(Pubkey::default()),
        raydium_lock_authority: Some(Pubkey::default()),
    };
    expect_err(unit(send_by(&mut env, &authority, set_ix(&authority.pubkey(), &Pubkey::default(), &unset_all))), code(FanexError::RaydiumNotSet));
    // The signer is checked first: a stranger learns nothing from the fields.
    let stranger = env.funded_wallet(SOL);
    expect_err(unit(send_by(&mut env, &stranger, set_ix(&stranger.pubkey(), &Pubkey::default(), &unset_all))), code(FanexError::Unauthorized));
    assert_eq!(raw_config(&env), before);
}

#[test]
fn refuses_an_amm_config_at_another_address_than_the_param() {
    let Suite { mut env, authority } = suite();
    let before = raw_config(&env);
    // A good 1% tier of the same CPMM at another address, both ways round.
    let other_tier = Pubkey::new_unique();
    env.svm.set_account(other_tier, tier_account(10_000, RAYDIUM_CPMM_ID)).unwrap();
    let mismatch = code(FanexError::AccountMismatch);

    let set = set_ix(&authority.pubkey(), &other_tier, &mainnet_params());
    expect_err(unit(send_by(&mut env, &authority, set)), mismatch);
    let mut naming_other = mainnet_params();
    naming_other.raydium_amm_config = Some(other_tier);
    let set = set_ix(&authority.pubkey(), &MAINNET_AMM_CONFIG, &naming_other);
    expect_err(unit(send_by(&mut env, &authority, set)), mismatch);
    assert_eq!(raw_config(&env), before);

    // Named and passed alike, the other tier is taken.
    send_by(&mut env, &authority, set_ix(&authority.pubkey(), &other_tier, &naming_other)).unwrap();
    assert_eq!(env.fetch::<UsdcConfig>(&usdc_config_pda()).raydium_amm_config, other_tier);
}

#[test]
fn refuses_an_amm_config_owned_by_any_program_but_the_cpmm_named() {
    let Suite { mut env, authority } = suite();
    let before = raw_config(&env);
    let mismatch = code(FanexError::AccountMismatch);

    // Mainnet's tier under devnet's CPMM id: the tier belongs to another program than the one named.
    let mut devnet_cpmm = mainnet_params();
    devnet_cpmm.raydium_cpmm_program = Some(DEVNET_CPMM);
    expect_err(unit(send_by(&mut env, &authority, set_ix(&authority.pubkey(), &MAINNET_AMM_CONFIG, &devnet_cpmm))), mismatch);

    // The tier's very bytes in an account a stranger's program owns.
    let forged = Pubkey::new_unique();
    env.svm.set_account(forged, tier_account(10_000, Pubkey::new_unique())).unwrap();
    let mut naming_forged = mainnet_params();
    naming_forged.raydium_amm_config = Some(forged);
    expect_err(unit(send_by(&mut env, &authority, set_ix(&authority.pubkey(), &forged, &naming_forged))), mismatch);
    assert_eq!(raw_config(&env), before);
}

#[test]
fn refuses_a_cpmm_account_that_is_not_an_amm_config() {
    let Suite { mut env, authority } = suite();
    let before = raw_config(&env);
    let mismatch = code(FanexError::AccountMismatch);
    let mut pool_state = tier_account(10_000, RAYDIUM_CPMM_ID);
    pool_state.data[..8].copy_from_slice(&[247, 237, 227, 245, 215, 195, 222, 70]);
    let mut short = tier_account(10_000, RAYDIUM_CPMM_ID);
    short.data.truncate(100);
    for (name, account) in [("a PoolState", pool_state), ("100 bytes", short)] {
        let address = Pubkey::new_unique();
        env.svm.set_account(address, account).unwrap();
        let mut params = mainnet_params();
        params.raydium_amm_config = Some(address);
        let result = unit(send_by(&mut env, &authority, set_ix(&authority.pubkey(), &address, &params)));
        assert!(result.is_err(), "{name}");
        expect_err(result, mismatch);
    }
    assert_eq!(raw_config(&env), before);
}

#[test]
fn refuses_a_tier_that_no_longer_charges_one_percent() {
    // Raydium's admin changing the rate of the tier at the pinned address: the address alone
    // cannot see it, the rate read from the account does.
    let Suite { mut env, authority } = suite();
    let before = raw_config(&env);
    for rate in [2_500, 9_999, 10_001, 40_000] {
        env.svm.set_account(MAINNET_AMM_CONFIG, tier_account(rate, RAYDIUM_CPMM_ID)).unwrap();
        let set = set_ix(&authority.pubkey(), &MAINNET_AMM_CONFIG, &mainnet_params());
        let result = unit(send_by(&mut env, &authority, set));
        assert!(result.is_err(), "{rate}");
        expect_err(result, code(FanexError::GraduationFeeTierChanged));
    }
    assert_eq!(raw_config(&env), before);
    env.svm.set_account(MAINNET_AMM_CONFIG, tier_account(10_000, RAYDIUM_CPMM_ID)).unwrap();
    send_by(&mut env, &authority, set_ix(&authority.pubkey(), &MAINNET_AMM_CONFIG, &mainnet_params())).unwrap();
}

#[test]
fn refuses_a_tier_that_stops_pool_creation() {
    // Raydium's admin setting `disable_create_pool` on a 1% tier: CPMM's own `initialize` refuses
    // every pool there (its `NotApproved`, `usdc_graduation.rs`), so the program never names it
    // (2026-10-23, the default for Rick to confirm). A tier that also changed its rate is a changed
    // tier first.
    let Suite { mut env, authority } = suite();
    let before = raw_config(&env);
    let mut disabled = tier_account(10_000, RAYDIUM_CPMM_ID);
    disabled.data[AMM_CONFIG_DISABLE_CREATE_POOL_OFFSET] = 1;
    env.svm.set_account(MAINNET_AMM_CONFIG, disabled.clone()).unwrap();
    let set = set_ix(&authority.pubkey(), &MAINNET_AMM_CONFIG, &mainnet_params());
    expect_err(unit(send_by(&mut env, &authority, set)), code(FanexError::PoolCreationDisabled));
    let at = AMM_CONFIG_TRADE_FEE_RATE_OFFSET;
    disabled.data[at..at + 8].copy_from_slice(&2_500u64.to_le_bytes());
    env.svm.set_account(MAINNET_AMM_CONFIG, disabled).unwrap();
    let set = set_ix(&authority.pubkey(), &MAINNET_AMM_CONFIG, &mainnet_params());
    expect_err(unit(send_by(&mut env, &authority, set)), code(FanexError::GraduationFeeTierChanged));
    assert_eq!(raw_config(&env), before);
    // Creating pools again, the tier is taken.
    env.svm.set_account(MAINNET_AMM_CONFIG, tier_account(10_000, RAYDIUM_CPMM_ID)).unwrap();
    send_by(&mut env, &authority, set_ix(&authority.pubkey(), &MAINNET_AMM_CONFIG, &mainnet_params())).unwrap();
}

#[test]
fn a_paused_config_still_takes_its_raydium_addresses() {
    // `USDC_NEVER_PAUSED`: a pause must never stop the way out of a Raydium change.
    let Suite { mut env, authority } = suite();
    let mut config = env.usdc_config();
    config.paused = true;
    env.put_usdc_config(&config);
    send_by(&mut env, &authority, set_ix(&authority.pubkey(), &MAINNET_AMM_CONFIG, &mainnet_params())).unwrap();
    let c: UsdcConfig = env.fetch(&usdc_config_pda());
    assert!(c.paused);
    assert_eq!(c.raydium_amm_config, MAINNET_AMM_CONFIG);
}

// ── Field by field (2026-10-24) ─────────────────────────────────────────────

/// The setter's suite with mainnet's five already written, as after the first proposal.
fn suite_set() -> Suite {
    let Suite { mut env, authority } = suite();
    send_by(&mut env, &authority, set_ix(&authority.pubkey(), &MAINNET_AMM_CONFIG, &mainnet_params())).unwrap();
    Suite { env, authority }
}

#[test]
fn a_lock_moves_alone_while_the_tier_stops_pool_creation_and_no_tier_account_is_passed() {
    // Raydium stops pool creation on the pinned tier and moves its lock: the lock is written
    // without the tier, which is read only when the CPMM or the tier itself changes.
    let Suite { mut env, authority } = suite_set();
    let mut disabled = tier_account(10_000, RAYDIUM_CPMM_ID);
    disabled.data[AMM_CONFIG_DISABLE_CREATE_POOL_OFFSET] = 1;
    env.svm.set_account(MAINNET_AMM_CONFIG, disabled).unwrap();
    let (lock, authority_of_lock) = (Pubkey::new_unique(), Pubkey::new_unique());
    let mut only_the_lock = none_given();
    only_the_lock.raydium_lock_program = Some(lock);
    only_the_lock.raydium_lock_authority = Some(authority_of_lock);

    let set = set_ix_tier(&authority.pubkey(), &program_data_pda(&pepper_curve::ID), None, &only_the_lock);
    let logs = send_by(&mut env, &authority, set).unwrap();

    let mut expected = env.usdc_config_with_raydium();
    expected.raydium_lock_program = lock;
    expected.raydium_lock_authority = authority_of_lock;
    assert_eq!(raw_config(&env), account_data(&expected, UsdcConfig::INIT_SPACE));
    // The event says the five as they stand after the write.
    let e = &events_of::<UsdcRaydiumSet>(&logs)[0];
    assert_eq!(
        [e.raydium_cpmm_program, e.raydium_amm_config, e.raydium_create_pool_fee, e.raydium_lock_program, e.raydium_lock_authority],
        [RAYDIUM_CPMM_ID, MAINNET_AMM_CONFIG, MAINNET_CREATE_POOL_FEE, lock, authority_of_lock],
    );
    // The fee account alone, the disabled tier's account passed: not read either.
    let fee = Pubkey::new_unique();
    let mut only_the_fee = none_given();
    only_the_fee.raydium_create_pool_fee = Some(fee);
    send_by(&mut env, &authority, set_ix(&authority.pubkey(), &MAINNET_AMM_CONFIG, &only_the_fee)).unwrap();
    assert_eq!(env.fetch::<UsdcConfig>(&usdc_config_pda()).raydium_create_pool_fee, fee);
}

#[test]
fn a_tier_or_cpmm_change_reads_the_tier_against_the_pair_the_config_will_hold() {
    let Suite { mut env, authority } = suite_set();
    let before = raw_config(&env);
    let mismatch = code(FanexError::AccountMismatch);
    let program_data = program_data_pda(&pepper_curve::ID);

    // The tier changes, its account left out: refused.
    let other_tier = Pubkey::new_unique();
    env.svm.set_account(other_tier, tier_account(10_000, RAYDIUM_CPMM_ID)).unwrap();
    let mut the_tier = none_given();
    the_tier.raydium_amm_config = Some(other_tier);
    expect_err(unit(send_by(&mut env, &authority, set_ix_tier(&authority.pubkey(), &program_data, None, &the_tier))), mismatch);
    // The CPMM alone: the tier the config keeps is mainnet's, owned by another program than devnet's CPMM.
    let mut the_cpmm = none_given();
    the_cpmm.raydium_cpmm_program = Some(DEVNET_CPMM);
    expect_err(unit(send_by(&mut env, &authority, set_ix(&authority.pubkey(), &MAINNET_AMM_CONFIG, &the_cpmm))), mismatch);
    // A tier changed to one that stops pool creation.
    let mut disabled = tier_account(10_000, RAYDIUM_CPMM_ID);
    disabled.data[AMM_CONFIG_DISABLE_CREATE_POOL_OFFSET] = 1;
    env.svm.set_account(other_tier, disabled).unwrap();
    expect_err(unit(send_by(&mut env, &authority, set_ix(&authority.pubkey(), &other_tier, &the_tier))), code(FanexError::PoolCreationDisabled));
    assert_eq!(raw_config(&env), before);

    // Passed and usable, the tier is taken alone, the other four kept.
    env.svm.set_account(other_tier, tier_account(10_000, RAYDIUM_CPMM_ID)).unwrap();
    send_by(&mut env, &authority, set_ix(&authority.pubkey(), &other_tier, &the_tier)).unwrap();
    let mut expected = env.usdc_config_with_raydium();
    expected.raydium_amm_config = other_tier;
    assert_eq!(raw_config(&env), account_data(&expected, UsdcConfig::INIT_SPACE));
}

#[test]
fn nothing_given_leaves_the_config_as_it_is() {
    let Suite { mut env, authority } = suite_set();
    let before = raw_config(&env);
    let set = set_ix_tier(&authority.pubkey(), &program_data_pda(&pepper_curve::ID), None, &none_given());
    let logs = send_by(&mut env, &authority, set).unwrap();
    assert_eq!(raw_config(&env), before);
    let e = &events_of::<UsdcRaydiumSet>(&logs)[0];
    assert_eq!(e.raydium_amm_config, MAINNET_AMM_CONFIG);
    // Still the upgrade authority's alone.
    let stranger = env.funded_wallet(SOL);
    let set = set_ix_tier(&stranger.pubkey(), &program_data_pda(&pepper_curve::ID), None, &none_given());
    expect_err(unit(send_by(&mut env, &stranger, set)), code(FanexError::Unauthorized));
}

fn bpf_loader_upgradeable_id() -> Pubkey {
    anchor_lang::solana_program::bpf_loader_upgradeable::id()
}
