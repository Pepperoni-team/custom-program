//! A USDC curve's vesting and its fan-led hand-over, run against the built program
//! (docs/solana/PEPPER-CURVE-PROGRAM.md §5.4, §11; usdc-curve design §10.3.7,
//! §10.3.8).
//!
//! `claim_vesting_usdc` keeps today's schedule and today's lock, a curve created
//! from `VESTING_LOCKED_UNTIL_GRADUATION_FROM` released once its pool exists
//! (PoolCreated or Graduated, Rick, 2026-10-22), into a token account that must
//! already exist: the sponsor opens it in the claim's own transaction, so the
//! beneficiary only signs. `sync_curve_creator_usdc` is today's sync on a
//! `CurveUsdc`, signed by `GlobalConfig.admin`. Each kind of curve refuses the
//! other's instruction by its discriminator (3002). The curves, their `Vesting`
//! and its vault are written into the bank, as `vesting_lock.rs` writes them, so
//! a phase or a clock is set by hand; `usdc_create.rs` pins the `Vesting` a
//! `create_curve_usdc` launch writes.

mod common;

use anchor_lang::solana_program::program_option::COption;
use anchor_lang::Space;
use anchor_spl::associated_token::get_associated_token_address as ata;
use anchor_spl::associated_token::spl_associated_token_account;
use anchor_spl::token::spl_token;
use common::*;
use pepper_curve::constants::*;
use pepper_curve::errors::FanexError;
use pepper_curve::events::{CurveCreatorSynced, VestingBeneficiarySet, VestingReleased};
use pepper_curve::instructions::set_vesting_beneficiary::fan_led_placeholder;
use pepper_curve::state::{Curve, CurveUsdc, Vesting};
use pepper_curve_math::CurveState;
use solana_sdk::{
    instruction::Instruction,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_program,
    transaction::TransactionError,
};

/// The curve's tokens for sale and for its pool, as `write_tradeable_curve` keeps them.
const FOR_SALE: u64 = 80_000_000 * 1_000_000;
/// The creator's vesting: what the supply leaves after the curve's and the pool's tokens.
const TOTAL: u64 = TOKEN_TOTAL_SUPPLY - FOR_SALE - USDC_CURVE_MIGRATION_TOKENS;
/// The first curve the lock holds: created at 2026-11-01 00:00:00 UTC.
const LOCK: i64 = VESTING_LOCKED_UNTIL_GRADUATION_FROM;

fn vesting_pda(mint: &Pubkey) -> Pubkey {
    pda(&[VESTING_SEED, mint.as_ref()])
}

fn vesting_vault_pda(mint: &Pubkey) -> Pubkey {
    pda(&[VESTING_VAULT_SEED, mint.as_ref()])
}

/// A fresh USDC curve's counters, launched at `created_at`, with no raise split: no creator USDC
/// account, the shape a fan-led launch takes.
fn fresh_state(created_at: i64) -> CurveState {
    CurveState {
        virtual_reserve: 5_000 * USDC,
        virtual_token_reserves: TOKEN_TOTAL_SUPPLY,
        real_reserve: 0,
        real_token_reserves: FOR_SALE,
        creator_split_bps: 0,
        fanex_split_bps: 0,
        creator_cap: 0,
        fanex_cap: 0,
        creator_received: 0,
        fanex_received: 0,
        graduation_threshold: MIN_GRADUATION_THRESHOLD_USDC,
        created_at,
    }
}

/// A `Vesting` of `TOTAL` for `beneficiary` at `mint`, counting from `start`, and its vault holding
/// it, the curve PDA its authority, as `create_curve` leaves them.
fn put_vesting(env: &mut Env, mint: Pubkey, beneficiary: Pubkey, start: i64, cliff: i64, duration: i64) {
    let (vesting, bump) = Pubkey::find_program_address(&[VESTING_SEED, mint.as_ref()], &pepper_curve::ID);
    let data = account_data(
        &Vesting {
            mint,
            beneficiary,
            total_amount: TOTAL,
            released_amount: 0,
            start_time: start,
            cliff_seconds: cliff,
            duration_seconds: duration,
            bump,
        },
        Vesting::INIT_SPACE,
    );
    env.put(vesting, data, pepper_curve::ID);
    env.put_token_account(vesting_vault_pda(&mint), mint, curve_pda(&mint), TOTAL, COption::None);
}

/// A USDC curve created at `created_at`, in `phase`, its creator `creator`, with a `Vesting` for
/// `beneficiary` (a one-day cliff, released whole), the clock set two days after the launch. Its
/// mint is fresh: `Env::new`'s SOL curve stays at `env.mint`.
fn usdc_curve(env: &mut Env, created_at: i64, phase: u8, creator: Pubkey, beneficiary: Pubkey) -> Pubkey {
    let mut curve = env.curve_usdc(Pubkey::new_unique(), &fresh_state(created_at));
    curve.phase = phase;
    curve.creator = creator;
    env.put_curve_usdc(&curve);
    put_vesting(env, curve.mint, beneficiary, created_at, DAY, 0);
    env.set_time(created_at + 2 * DAY);
    curve.mint
}

/// A USDC curve whose creator, `env.creator`, is its vesting's beneficiary.
fn creators_usdc_curve(env: &mut Env, created_at: i64, phase: u8) -> Pubkey {
    let creator = env.creator.pubkey();
    usdc_curve(env, created_at, phase, creator, creator)
}

/// A fan-led USDC curve, created before the lock: the keyless placeholder is its creator and its
/// vesting's beneficiary until the claim.
fn fan_led_usdc_curve(env: &mut Env) -> Pubkey {
    usdc_curve(env, LOCK - DAY, PHASE_BONDING, fan_led_placeholder(), fan_led_placeholder())
}

/// The SOL curve `write_tradeable_curve` writes at `env.mint`, created at `created_at`, its creator
/// `creator`, with a `Vesting` for `beneficiary` as `usdc_curve` writes one.
fn sol_curve(env: &mut Env, created_at: i64, creator: Pubkey, beneficiary: Pubkey) -> Pubkey {
    env.write_tradeable_curve(created_at);
    let mint = env.mint;
    let mut curve: Curve = env.fetch(&curve_pda(&mint));
    curve.creator = creator;
    env.put(curve_pda(&mint), account_data(&curve, Curve::INIT_SPACE), pepper_curve::ID);
    put_vesting(env, mint, beneficiary, created_at, DAY, 0);
    env.set_time(created_at + 2 * DAY);
    mint
}

fn claim_usdc_ix(mint: &Pubkey, beneficiary: &Pubkey, token_account: Pubkey) -> Instruction {
    ix(
        pepper_curve::accounts::ClaimVestingUsdc {
            beneficiary: *beneficiary,
            curve: curve_pda(mint),
            mint: *mint,
            vesting: vesting_pda(mint),
            vesting_vault: vesting_vault_pda(mint),
            beneficiary_token_account: token_account,
            token_program: spl_token::ID,
        },
        pepper_curve::instruction::ClaimVestingUsdc {},
    )
}

/// The sponsor (the admin) opening `owner`'s associated account of `mint` when it is missing.
fn open_ata_ix(env: &Env, owner: &Pubkey, mint: &Pubkey) -> Instruction {
    spl_associated_token_account::instruction::create_associated_token_account_idempotent(
        &env.admin.pubkey(),
        owner,
        mint,
        &spl_token::ID,
    )
}

/// `claim_vesting_usdc` signed by `beneficiary`, the sponsor paying the fee and opening the
/// beneficiary's associated account in the same transaction, as the app sends it.
fn claim_usdc(env: &mut Env, mint: &Pubkey, beneficiary: &Keypair) -> Result<Vec<String>, TransactionError> {
    let open = open_ata_ix(env, &beneficiary.pubkey(), mint);
    let claim = claim_usdc_ix(mint, &beneficiary.pubkey(), ata(&beneficiary.pubkey(), mint));
    env.send_logged(&[open, claim], &[beneficiary])
}

/// Today's `claim_vesting`, signed and paid by `beneficiary`, which opens its account if missing.
fn claim_sol(env: &mut Env, mint: &Pubkey, beneficiary: &Keypair) -> Result<(), TransactionError> {
    let beneficiary_key = beneficiary.pubkey();
    let claim = ix(
        pepper_curve::accounts::ClaimVesting {
            beneficiary: beneficiary_key,
            curve: curve_pda(mint),
            mint: *mint,
            vesting: vesting_pda(mint),
            vesting_vault: vesting_vault_pda(mint),
            beneficiary_token_account: ata(&beneficiary_key, mint),
            token_program: spl_token::ID,
            associated_token_program: anchor_spl::associated_token::ID,
            system_program: system_program::id(),
        },
        pepper_curve::instruction::ClaimVesting {},
    );
    env.send_paid_by(beneficiary, &[claim], &[])
}

/// `set_vesting_beneficiary(new_beneficiary)` on `mint`'s vesting, signed by `GlobalConfig.admin`.
fn set_beneficiary(env: &mut Env, mint: &Pubkey, new_beneficiary: Pubkey) -> Result<Vec<String>, TransactionError> {
    let set = ix(
        pepper_curve::accounts::SetVestingBeneficiary {
            config: config_pda(),
            admin: env.admin.pubkey(),
            mint: *mint,
            vesting: vesting_pda(mint),
        },
        pepper_curve::instruction::SetVestingBeneficiary { new_beneficiary },
    );
    env.send_logged(&[set], &[])
}

fn sync_usdc_ix(signer: &Pubkey, mint: &Pubkey) -> Instruction {
    ix(
        pepper_curve::accounts::SyncCurveCreatorUsdc {
            global_config: config_pda(),
            admin: *signer,
            mint: *mint,
            vesting: vesting_pda(mint),
            curve: curve_pda(mint),
        },
        pepper_curve::instruction::SyncCurveCreatorUsdc {},
    )
}

/// `sync_curve_creator_usdc` signed by `signer`, the admin paying the fee.
fn sync_usdc_by(env: &mut Env, signer: &Keypair, mint: &Pubkey) -> Result<Vec<String>, TransactionError> {
    let sync = sync_usdc_ix(&signer.pubkey(), mint);
    env.send_logged(&[sync], &[signer])
}

/// `sync_curve_creator_usdc` signed by `GlobalConfig.admin`.
fn sync_usdc(env: &mut Env, mint: &Pubkey) -> Result<Vec<String>, TransactionError> {
    let admin = env.admin.insecure_clone();
    sync_usdc_by(env, &admin, mint)
}

/// Today's `sync_curve_creator`, signed by `GlobalConfig.admin`.
fn sync_sol(env: &mut Env, mint: &Pubkey) -> Result<(), TransactionError> {
    let sync = ix(
        pepper_curve::accounts::SyncCurveCreator {
            config: config_pda(),
            admin: env.admin.pubkey(),
            mint: *mint,
            vesting: vesting_pda(mint),
            curve: curve_pda(mint),
        },
        pepper_curve::instruction::SyncCurveCreator {},
    );
    env.send(&[sync], &[])
}

fn released(env: &Env, mint: &Pubkey) -> u64 {
    env.fetch::<Vesting>(&vesting_pda(mint)).released_amount
}

fn discarded<T>(result: Result<T, TransactionError>) -> Result<(), TransactionError> {
    result.map(|_| ())
}

fn anchor_code(e: anchor_lang::error::ErrorCode) -> u32 {
    e.into()
}

#[test]
fn a_new_usdc_curve_keeps_its_vesting_until_its_pool_exists() {
    // Phase 4 is no phase the program writes: the rule names the phases that unlock.
    for phase in [PHASE_BONDING, PHASE_GRADUATING, 4] {
        let mut env = Env::new();
        let mint = creators_usdc_curve(&mut env, LOCK, phase);
        let creator = env.creator.insecure_clone();

        expect_err(discarded(claim_usdc(&mut env, &mint, &creator)), code(FanexError::VestingLockedUntilGraduation));
        assert_eq!(code(FanexError::VestingLockedUntilGraduation), 6035);
        assert_eq!(released(&env, &mint), 0, "phase {phase}");
        assert_eq!(env.token_amount(&vesting_vault_pda(&mint)), TOTAL, "phase {phase}");
    }
}

#[test]
fn a_new_usdc_curve_releases_its_vesting_once_its_pool_is_created_or_it_graduates() {
    for phase in [PHASE_POOL_CREATED, PHASE_GRADUATED] {
        let mut env = Env::new();
        let mint = creators_usdc_curve(&mut env, LOCK, phase);
        let creator = env.creator.insecure_clone();
        let lamports = env.lamports(&creator.pubkey());

        let logs = claim_usdc(&mut env, &mint, &creator).unwrap();

        assert_eq!(released(&env, &mint), TOTAL, "phase {phase}");
        assert_eq!(env.token_amount(&ata(&creator.pubkey(), &mint)), TOTAL);
        assert_eq!(env.token_amount(&vesting_vault_pda(&mint)), 0);
        let paid: VestingReleased = event(&logs);
        assert_eq!((paid.mint, paid.beneficiary, paid.amount), (mint, creator.pubkey(), TOTAL));
        // By `transfer_checked` (§10.3.7), where today's claim calls a plain `transfer`.
        assert!(logs.iter().any(|l| l == "Program log: Instruction: TransferChecked"), "phase {phase}");
        assert!(!logs.iter().any(|l| l == "Program log: Instruction: Transfer"), "phase {phase}");
        // The sponsor paid the fee and the account's rent: the creator only signed.
        assert_eq!(env.lamports(&creator.pubkey()), lamports);
    }
}

#[test]
fn an_older_usdc_curve_releases_on_its_schedule_while_bonding() {
    let mut env = Env::new();
    let created_at = LOCK - DAY;
    let mint = creators_usdc_curve(&mut env, created_at, PHASE_BONDING);
    // A 30-day cliff, then a 100-day ramp, from the launch.
    let creator = env.creator.pubkey();
    put_vesting(&mut env, mint, creator, created_at, 30 * DAY, 100 * DAY);
    let creator = env.creator.insecure_clone();

    env.set_time(created_at + 30 * DAY - 1);
    expect_err(discarded(claim_usdc(&mut env, &mint, &creator)), code(FanexError::NothingToRelease));

    env.set_time(created_at + 30 * DAY + 25 * DAY);
    claim_usdc(&mut env, &mint, &creator).unwrap();
    assert_eq!(released(&env, &mint), TOTAL / 4);
    expect_err(discarded(claim_usdc(&mut env, &mint, &creator)), code(FanexError::NothingToRelease));

    env.set_time(created_at + 130 * DAY);
    let logs = claim_usdc(&mut env, &mint, &creator).unwrap();
    assert_eq!(event::<VestingReleased>(&logs).amount, TOTAL - TOTAL / 4);
    assert_eq!(released(&env, &mint), TOTAL);
    assert_eq!(env.token_amount(&ata(&creator.pubkey(), &mint)), TOTAL);
    assert_eq!(env.token_amount(&vesting_vault_pda(&mint)), 0);
}

#[test]
fn a_migrated_curve_keeps_its_vesting_until_its_pool_exists_whenever_it_launched() {
    // The internal audit (2026-10-29): a migrated curve keeps its SOL `created_at`, before the lock,
    // and its restatement pays back only the supply the curve sold: a vesting sold into it in
    // Bonding would be paid from the holders' reserve, so it waits for the pool like a new curve's.
    for phase in [PHASE_BONDING, PHASE_GRADUATING] {
        let mut env = Env::new();
        let mint = creators_usdc_curve(&mut env, LOCK - 30 * DAY, phase);
        let mut curve: CurveUsdc = env.fetch(&curve_pda(&mint));
        curve.migrated = true;
        env.put(curve_pda(&mint), account_data(&curve, CurveUsdc::INIT_SPACE), pepper_curve::ID);
        let creator = env.creator.insecure_clone();
        expect_err(discarded(claim_usdc(&mut env, &mint, &creator)), code(FanexError::VestingLockedUntilGraduation));
        assert_eq!(released(&env, &mint), 0, "phase {phase}");
    }
    // Once its pool exists, it releases on its schedule.
    let mut env = Env::new();
    let mint = creators_usdc_curve(&mut env, LOCK - 30 * DAY, PHASE_POOL_CREATED);
    let mut curve: CurveUsdc = env.fetch(&curve_pda(&mint));
    curve.migrated = true;
    env.put(curve_pda(&mint), account_data(&curve, CurveUsdc::INIT_SPACE), pepper_curve::ID);
    let creator = env.creator.insecure_clone();
    claim_usdc(&mut env, &mint, &creator).unwrap();
    assert_eq!(released(&env, &mint), TOTAL);
}

#[test]
fn refuses_a_signer_who_is_not_the_beneficiary() {
    let mut env = Env::new();
    let mint = creators_usdc_curve(&mut env, LOCK - DAY, PHASE_BONDING);
    let stranger = Keypair::new();

    expect_err(discarded(claim_usdc(&mut env, &mint, &stranger)), code(FanexError::Unauthorized));
    assert_eq!(released(&env, &mint), 0);
    assert_eq!(env.token_amount(&vesting_vault_pda(&mint)), TOTAL);
}

#[test]
fn refuses_a_beneficiary_whose_token_account_is_missing_or_not_their_own_ata() {
    let mut env = Env::new();
    let mint = creators_usdc_curve(&mut env, LOCK - DAY, PHASE_BONDING);
    let creator = env.creator.insecure_clone();
    let own_ata = ata(&creator.pubkey(), &mint);

    // Missing: refused, never opened at the beneficiary's expense.
    let claim = claim_usdc_ix(&mint, &creator.pubkey(), own_ata);
    expect_err(env.send(&[claim], &[&creator]), ACCOUNT_NOT_INITIALIZED);
    assert!(!env.exists(&own_ata));

    // The beneficiary's token account of the mint, at another address.
    let elsewhere = Pubkey::new_unique();
    env.put_token_account(elsewhere, mint, creator.pubkey(), 0, COption::None);
    let claim = claim_usdc_ix(&mint, &creator.pubkey(), elsewhere);
    expect_err(env.send(&[claim], &[&creator]), anchor_code(anchor_lang::error::ErrorCode::ConstraintAssociated));

    // The beneficiary's ATA handed to another owner (classic SPL Token allows it).
    env.put_token_account(own_ata, mint, Pubkey::new_unique(), 0, COption::None);
    let claim = claim_usdc_ix(&mint, &creator.pubkey(), own_ata);
    expect_err(env.send(&[claim], &[&creator]), anchor_code(anchor_lang::error::ErrorCode::ConstraintTokenOwner));

    assert_eq!(released(&env, &mint), 0);
    assert_eq!(env.token_amount(&vesting_vault_pda(&mint)), TOTAL);
}

#[test]
fn the_sol_claim_vesting_refuses_a_usdc_curve_and_the_usdc_claim_takes_it() {
    let mut env = Env::new();
    let mint = creators_usdc_curve(&mut env, LOCK - DAY, PHASE_BONDING);
    let creator = env.creator.insecure_clone();

    expect_err(claim_sol(&mut env, &mint, &creator), ACCOUNT_DISCRIMINATOR_MISMATCH);
    assert_eq!(released(&env, &mint), 0);

    claim_usdc(&mut env, &mint, &creator).unwrap();
    assert_eq!(released(&env, &mint), TOTAL);
}

#[test]
fn claim_vesting_usdc_refuses_a_sol_curve_and_the_sol_claim_takes_it() {
    let mut env = Env::new();
    let creator = env.creator.insecure_clone();
    let mint = sol_curve(&mut env, LOCK - DAY, creator.pubkey(), creator.pubkey());

    expect_err(discarded(claim_usdc(&mut env, &mint, &creator)), ACCOUNT_DISCRIMINATOR_MISMATCH);
    assert_eq!(released(&env, &mint), 0);

    claim_sol(&mut env, &mint, &creator).unwrap();
    assert_eq!(released(&env, &mint), TOTAL);
}

#[test]
fn set_vesting_beneficiary_hands_a_usdc_curves_vesting_to_its_creator() {
    let mut env = Env::new();
    let mint = fan_led_usdc_curve(&mut env);
    let claimed_by = env.creator.insecure_clone();
    let claimed_at = LOCK + 10 * DAY;
    env.set_time(claimed_at);

    let logs = set_beneficiary(&mut env, &mint, claimed_by.pubkey()).unwrap();

    let vesting: Vesting = env.fetch(&vesting_pda(&mint));
    assert_eq!((vesting.beneficiary, vesting.start_time), (claimed_by.pubkey(), claimed_at));
    assert_eq!((vesting.total_amount, vesting.released_amount), (TOTAL, 0));
    let set: VestingBeneficiarySet = event(&logs);
    assert_eq!((set.mint, set.previous, set.new_beneficiary), (mint, fan_led_placeholder(), claimed_by.pubkey()));
    // Once: the vesting has left the placeholder.
    expect_err(discarded(set_beneficiary(&mut env, &mint, Pubkey::new_unique())), code(FanexError::VestingNotReassignable));

    // The cliff counts from the hand-over, then the creator claims through the USDC claim.
    env.set_time(claimed_at + DAY - 1);
    expect_err(discarded(claim_usdc(&mut env, &mint, &claimed_by)), code(FanexError::NothingToRelease));
    env.set_time(claimed_at + DAY);
    claim_usdc(&mut env, &mint, &claimed_by).unwrap();
    assert_eq!(env.token_amount(&ata(&claimed_by.pubkey(), &mint)), TOTAL);
}

#[test]
fn syncs_a_fan_led_usdc_curves_creator_once() {
    let mut env = Env::new();
    let mint = fan_led_usdc_curve(&mut env);
    let creator = env.creator.pubkey();

    // Before the claim the vesting still names the placeholder: nothing to copy.
    expect_err(discarded(sync_usdc(&mut env, &mint)), code(FanexError::CurveCreatorNotSyncable));
    set_beneficiary(&mut env, &mint, creator).unwrap();

    let logs = sync_usdc(&mut env, &mint).unwrap();

    let curve: CurveUsdc = env.fetch(&curve_pda(&mint));
    assert_eq!(curve.creator, creator);
    let synced: CurveCreatorSynced = event(&logs);
    assert_eq!((synced.mint, synced.previous, synced.new_creator), (mint, fan_led_placeholder(), creator));
    // Once: the creator has left the placeholder.
    expect_err(discarded(sync_usdc(&mut env, &mint)), code(FanexError::CurveCreatorNotSyncable));
    assert_eq!(code(FanexError::CurveCreatorNotSyncable), 6028);
    assert_eq!(env.fetch::<CurveUsdc>(&curve_pda(&mint)).creator, creator);
}

#[test]
fn refuses_the_usdc_sync_of_a_curve_a_real_creator_launched() {
    let mut env = Env::new();
    // A real creator's curve, its vesting their own: the sync has no placeholder to move.
    let mint = creators_usdc_curve(&mut env, LOCK - DAY, PHASE_BONDING);

    expect_err(discarded(sync_usdc(&mut env, &mint)), code(FanexError::CurveCreatorNotSyncable));
    assert_eq!(env.fetch::<CurveUsdc>(&curve_pda(&mint)).creator, env.creator.pubkey());
}

#[test]
fn only_the_global_admin_signs_the_usdc_sync_never_the_usdc_configs_admin() {
    let mut env = Env::new();
    // The USDC side's admin is the Squad's vault after the hand-over, another key than GlobalConfig's.
    let multisig = Keypair::new();
    let mut config = env.usdc_config();
    config.admin = multisig.pubkey();
    env.put_usdc_config(&config);
    let mint = fan_led_usdc_curve(&mut env);
    let creator = env.creator.pubkey();
    set_beneficiary(&mut env, &mint, creator).unwrap();

    for signer in [multisig, Keypair::new()] {
        expect_err(discarded(sync_usdc_by(&mut env, &signer, &mint)), code(FanexError::Unauthorized));
        assert_eq!(env.fetch::<CurveUsdc>(&curve_pda(&mint)).creator, fan_led_placeholder());
    }

    sync_usdc(&mut env, &mint).unwrap();
    assert_eq!(env.fetch::<CurveUsdc>(&curve_pda(&mint)).creator, creator);
}

#[test]
fn the_sol_sync_refuses_a_usdc_curve_and_the_usdc_sync_takes_it() {
    let mut env = Env::new();
    let mint = fan_led_usdc_curve(&mut env);
    let creator = env.creator.pubkey();
    set_beneficiary(&mut env, &mint, creator).unwrap();

    expect_err(sync_sol(&mut env, &mint), ACCOUNT_DISCRIMINATOR_MISMATCH);
    assert_eq!(env.fetch::<CurveUsdc>(&curve_pda(&mint)).creator, fan_led_placeholder());

    sync_usdc(&mut env, &mint).unwrap();
    assert_eq!(env.fetch::<CurveUsdc>(&curve_pda(&mint)).creator, creator);
}

#[test]
fn sync_curve_creator_usdc_refuses_a_sol_curve_and_the_sol_sync_takes_it() {
    let mut env = Env::new();
    let creator = env.creator.pubkey();
    // A fan-led SOL curve whose vesting a claim already handed to the creator.
    let mint = sol_curve(&mut env, LOCK - DAY, fan_led_placeholder(), creator);

    expect_err(discarded(sync_usdc(&mut env, &mint)), ACCOUNT_DISCRIMINATOR_MISMATCH);
    assert_eq!(env.fetch::<Curve>(&curve_pda(&mint)).creator, fan_led_placeholder());

    sync_sol(&mut env, &mint).unwrap();
    assert_eq!(env.fetch::<Curve>(&curve_pda(&mint)).creator, creator);
}
