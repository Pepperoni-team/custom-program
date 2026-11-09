//! The USDC pause of the SOL side, run against the built program
//! (docs/solana/PEPPER-CURVE-PROGRAM.md §5, §5.1; usdc-curve design §10.3.3;
//! Rick, 2026-10-23: «la pausa deve essere immediata», «si chiamerà pausa USDC»).
//!
//! `usdc_pause_sol` writes `GlobalConfig.paused = true` and
//! `usdc_pause_sol_curve` one SOL `Curve.paused = true`, each signed by
//! `UsdcConfig.pauser` alone, so once `GlobalConfig.admin` is the Squad's vault
//! a pause still lands at once. Pause only: unpausing stays `set_paused(false)`
//! and `set_curve_paused(false)`, `GlobalConfig.admin`'s. The config is made by
//! `initialize_usdc_config`, as on a cluster; every refusal is read by its code.

mod common;

use anchor_lang::error::ErrorCode;
use anchor_lang::Space;
use anchor_spl::associated_token::get_associated_token_address as ata;
use anchor_spl::token::spl_token;
use common::*;
use pepper_curve::constants::SOL_VAULT_SEED;
use pepper_curve::errors::FanexError;
use pepper_curve::events::{UsdcPauseSet, UsdcPauseSolSet};
use pepper_curve::instructions::InitializeUsdcConfigParams;
use pepper_curve::state::{Curve, CurveUsdc, GlobalConfig, UsdcConfig};
use solana_sdk::{
    instruction::Instruction,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_program,
    transaction::TransactionError,
};

fn anchor(code: ErrorCode) -> u32 {
    code.into()
}

// ── Instructions ─────────────────────────────────────────────────────────────

fn usdc_pause_sol_ix(signer: &Pubkey) -> Instruction {
    usdc_pause_sol_ix_with(signer, &usdc_config_pda(), &config_pda())
}

fn usdc_pause_sol_ix_with(signer: &Pubkey, usdc_config: &Pubkey, global_config: &Pubkey) -> Instruction {
    ix(
        pepper_curve::accounts::UsdcPauseSol { pauser: *signer, usdc_config: *usdc_config, global_config: *global_config },
        pepper_curve::instruction::UsdcPauseSol {},
    )
}

fn usdc_pause_sol_curve_ix(signer: &Pubkey, curve: &Pubkey) -> Instruction {
    ix(
        pepper_curve::accounts::UsdcPauseSolCurve { pauser: *signer, usdc_config: usdc_config_pda(), curve: *curve },
        pepper_curve::instruction::UsdcPauseSolCurve {},
    )
}

/// The SOL `set_paused`, `GlobalConfig.admin`'s.
fn set_paused_ix(signer: &Pubkey, paused: bool) -> Instruction {
    ix(
        pepper_curve::accounts::UpdateConfig { config: config_pda(), admin: *signer },
        pepper_curve::instruction::SetPaused { paused },
    )
}

/// The SOL `set_curve_paused`, `GlobalConfig.admin`'s.
fn set_curve_paused_ix(signer: &Pubkey, curve: &Pubkey, paused: bool) -> Instruction {
    ix(
        pepper_curve::accounts::SetCurvePaused { config: config_pda(), admin: *signer, curve: *curve },
        pepper_curve::instruction::SetCurvePaused { paused },
    )
}

fn set_usdc_paused_ix(signer: &Pubkey, paused: bool) -> Instruction {
    ix(
        pepper_curve::accounts::SetUsdcPaused { authority: *signer, usdc_config: usdc_config_pda() },
        pepper_curve::instruction::SetUsdcPaused { paused },
    )
}

fn update_usdc_config_ix(admin: &Pubkey, new_admin: Option<Pubkey>, new_pauser: Option<Pubkey>) -> Instruction {
    ix(
        pepper_curve::accounts::UpdateUsdcConfig {
            admin: *admin,
            usdc_config: usdc_config_pda(),
            global_config: config_pda(),
            new_fee_usdc_account: None,
        },
        pepper_curve::instruction::UpdateUsdcConfig {
            new_admin,
            new_pauser,
            new_migration_authority: None,
            new_lp_fee_owner: None,
            new_launcher: None,
        },
    )
}

/// `sell(token_in, 0)` on `env.mint`, from `seller`'s associated account.
fn sell_ix(env: &Env, seller: &Pubkey, token_in: u64) -> Instruction {
    let curve = curve_pda(&env.mint);
    ix(
        pepper_curve::accounts::Sell {
            seller: *seller,
            config: config_pda(),
            curve,
            mint: env.mint,
            sol_vault: pda(&[SOL_VAULT_SEED, env.mint.as_ref()]),
            token_vault: ata(&curve, &env.mint),
            seller_token_account: ata(seller, &env.mint),
            fanex: env.fee_wallet.pubkey(),
            token_program: spl_token::ID,
            system_program: system_program::id(),
        },
        pepper_curve::instruction::Sell { token_in, min_sol_out: 0 },
    )
}

// ── Setup ────────────────────────────────────────────────────────────────────

/// Sends `ixs` paid and signed by `signer` alone: the admin signs nothing.
fn send_by(env: &mut Env, signer: &Keypair, ixs: &[Instruction]) -> Result<Vec<String>, TransactionError> {
    let signer = signer.insecure_clone();
    env.send_logged_paid_by(&signer, ixs, &[])
}

fn unit(result: Result<Vec<String>, TransactionError>) -> Result<(), TransactionError> {
    result.map(|_| ())
}

/// The USDC config created through `initialize_usdc_config` by the admin as the program's upgrade
/// authority (the internal audit, 2026-10-29), naming itself the config's admin, its pauser
/// `env.pauser` (holding SOL, so it pays its own transactions), and a SOL curve at `env.mint`
/// launched a day before the clock, so the launch cap is over, with a buyer who already holds some
/// of its tokens.
fn initialized() -> (Env, Keypair) {
    let mut env = Env::upgradeable_by_admin();
    let admin = env.admin.pubkey();
    // The fees' USDC account is the fee wallet's hardened ATA, a key apart from the admin since
    // the fee-wallet split (2026-10-24).
    let fee = env.hardened_fee_account();
    let params = InitializeUsdcConfigParams {
        pauser: env.pauser.pubkey(),
        migration_authority: Pubkey::default(),
        lp_fee_owner: admin,
        launcher: admin,
    };
    let init = init_usdc_config_ix(&admin, &admin, &admin, &env.usdc.mint, &fee, &params);
    env.send(&[init], &[]).unwrap();
    let pauser = env.pauser.pubkey();
    env.svm.airdrop(&pauser, SOL).unwrap();
    assert_eq!(env.fetch::<UsdcConfig>(&usdc_config_pda()).pauser, pauser);

    env.write_tradeable_curve(OCT_5_2026 - DAY);
    let buyer = env.funded_wallet(10 * SOL);
    env.buy(&buyer, SOL / 100).unwrap();
    assert!(env.token_balance(&buyer.pubkey()) > 0);
    (env, buyer)
}

fn raw(env: &Env, address: &Pubkey) -> Vec<u8> {
    env.svm.get_account(address).unwrap().data
}

fn global(env: &Env) -> GlobalConfig {
    env.fetch(&config_pda())
}

fn sol_curve(env: &Env, mint: &Pubkey) -> Curve {
    env.fetch(&curve_pda(mint))
}

fn usdc_paused(env: &Env) -> bool {
    env.fetch::<UsdcConfig>(&usdc_config_pda()).paused
}

/// A buy and a sale of a tenth of what `buyer` holds, each sent alone.
fn trade(env: &mut Env, buyer: &Keypair) -> (Result<(), TransactionError>, Result<(), TransactionError>) {
    let bought = env.buy(buyer, SOL / 100);
    let held = env.token_balance(&buyer.pubkey());
    let sale = sell_ix(env, &buyer.pubkey(), held / 10);
    let sold = unit(send_by(env, buyer, &[sale]));
    (bought, sold)
}

fn assert_paused_event(logs: &[String], curve: Pubkey, by: Pubkey) {
    let events = events_of::<UsdcPauseSolSet>(logs);
    assert_eq!(events.len(), 1);
    assert_eq!((events[0].curve, events[0].by), (curve, by));
}

/// A second tradeable SOL curve, `env.mint` left on the first.
fn second_curve(env: &mut Env) -> Pubkey {
    let first = env.mint;
    env.mint = Pubkey::new_unique();
    env.write_tradeable_curve(OCT_5_2026 - DAY);
    let second = env.mint;
    env.mint = first;
    second
}

// ── usdc_pause_sol ───────────────────────────────────────────────────────────

#[test]
fn the_pauser_pauses_the_sol_side_and_a_buy_and_a_sale_are_refused() {
    let (mut env, buyer) = initialized();
    let (bought, sold) = trade(&mut env, &buyer);
    bought.unwrap();
    sold.unwrap();
    let pauser = env.pauser.insecure_clone();
    let mut expected = global(&env);
    assert!(!expected.paused);

    let logs = send_by(&mut env, &pauser, &[usdc_pause_sol_ix(&pauser.pubkey())]).unwrap();
    assert_paused_event(&logs, Pubkey::default(), pauser.pubkey());
    expected.paused = true;
    assert_eq!(raw(&env, &config_pda()), account_data(&expected, GlobalConfig::INIT_SPACE), "the flag and nothing else");
    // The USDC side's own flag is the pauser's other instruction's.
    assert!(!usdc_paused(&env));

    let held = env.token_balance(&buyer.pubkey());
    let (bought, sold) = trade(&mut env, &buyer);
    expect_err(bought, code(FanexError::GloballyPaused));
    expect_err(sold, code(FanexError::GloballyPaused));
    assert_eq!(env.token_balance(&buyer.pubkey()), held);

    // Only the admin unpauses, through the SOL `set_paused`.
    let admin = env.admin.insecure_clone();
    send_by(&mut env, &admin, &[set_paused_ix(&admin.pubkey(), false)]).unwrap();
    assert!(!global(&env).paused);
    let (bought, sold) = trade(&mut env, &buyer);
    bought.unwrap();
    sold.unwrap();
}

// ── usdc_pause_sol_curve ─────────────────────────────────────────────────────

#[test]
fn the_pauser_pauses_one_sol_curve_and_no_other() {
    let (mut env, buyer) = initialized();
    let other = second_curve(&mut env);
    let address = curve_pda(&env.mint);
    let pauser = env.pauser.insecure_clone();
    let mut expected = sol_curve(&env, &env.mint);

    let logs = send_by(&mut env, &pauser, &[usdc_pause_sol_curve_ix(&pauser.pubkey(), &address)]).unwrap();
    assert_paused_event(&logs, address, pauser.pubkey());
    expected.paused = true;
    assert_eq!(raw(&env, &address), account_data(&expected, Curve::INIT_SPACE), "the flag and nothing else");
    assert!(!sol_curve(&env, &other).paused);
    assert!(!global(&env).paused);

    let (bought, sold) = trade(&mut env, &buyer);
    expect_err(bought, code(FanexError::CurvePaused));
    expect_err(sold, code(FanexError::CurvePaused));

    // The other curve still trades.
    let first = env.mint;
    env.mint = other;
    env.buy(&buyer, SOL / 100).unwrap();
    env.mint = first;

    // Only the admin unpauses it, through the SOL `set_curve_paused`.
    let admin = env.admin.insecure_clone();
    send_by(&mut env, &admin, &[set_curve_paused_ix(&admin.pubkey(), &address, false)]).unwrap();
    assert!(!sol_curve(&env, &env.mint).paused);
    let (bought, sold) = trade(&mut env, &buyer);
    bought.unwrap();
    sold.unwrap();
}

// ── Pause only ───────────────────────────────────────────────────────────────

#[test]
fn the_pauser_cannot_unpause_and_the_admin_still_must() {
    let (mut env, _) = initialized();
    let address = curve_pda(&env.mint);
    let pauser = env.pauser.insecure_clone();
    send_by(&mut env, &pauser, &[usdc_pause_sol_ix(&pauser.pubkey()), usdc_pause_sol_curve_ix(&pauser.pubkey(), &address)]).unwrap();
    assert!(global(&env).paused && sol_curve(&env, &env.mint).paused);

    // The two instructions take no value; the SOL ones that do are the admin's.
    let refusals = [set_paused_ix(&pauser.pubkey(), false), set_curve_paused_ix(&pauser.pubkey(), &address, false)];
    for refused in refusals {
        expect_err(unit(send_by(&mut env, &pauser, &[refused])), code(FanexError::Unauthorized));
    }
    assert!(global(&env).paused && sol_curve(&env, &env.mint).paused);

    let admin = env.admin.insecure_clone();
    send_by(&mut env, &admin, &[set_paused_ix(&admin.pubkey(), false), set_curve_paused_ix(&admin.pubkey(), &address, false)])
        .unwrap();
    assert!(!global(&env).paused && !sol_curve(&env, &env.mint).paused);

    // A `false` appended to the data is no argument: Anchor reads none, and the flag is written `true`.
    let mut side = usdc_pause_sol_ix(&pauser.pubkey());
    side.data.push(0);
    let mut one = usdc_pause_sol_curve_ix(&pauser.pubkey(), &address);
    one.data.push(0);
    send_by(&mut env, &pauser, &[side, one]).unwrap();
    assert!(global(&env).paused && sol_curve(&env, &env.mint).paused);

    send_by(&mut env, &admin, &[set_paused_ix(&admin.pubkey(), false), set_curve_paused_ix(&admin.pubkey(), &address, false)])
        .unwrap();
    assert!(!global(&env).paused && !sol_curve(&env, &env.mint).paused);
}

#[test]
fn a_stranger_and_both_admins_are_refused() {
    // Only the pauser signs them: `GlobalConfig.admin` already pauses through
    // `set_paused` and `set_curve_paused`, and `UsdcConfig.admin` holds no SOL power.
    let (mut env, _) = initialized();
    let address = curve_pda(&env.mint);
    let admin = env.admin.insecure_clone();
    let usdc_admin = env.funded_wallet(SOL);
    send_by(&mut env, &admin, &[update_usdc_config_ix(&admin.pubkey(), Some(usdc_admin.pubkey()), None)]).unwrap();
    assert_eq!(env.fetch::<UsdcConfig>(&usdc_config_pda()).admin, usdc_admin.pubkey());
    assert_eq!(global(&env).admin, admin.pubkey());
    let stranger = env.funded_wallet(SOL);
    let config_before = raw(&env, &config_pda());
    let curve_before = raw(&env, &address);

    for signer in [&stranger, &admin, &usdc_admin] {
        expect_err(unit(send_by(&mut env, signer, &[usdc_pause_sol_ix(&signer.pubkey())])), code(FanexError::Unauthorized));
        expect_err(
            unit(send_by(&mut env, signer, &[usdc_pause_sol_curve_ix(&signer.pubkey(), &address)])),
            code(FanexError::Unauthorized),
        );
    }
    assert_eq!(raw(&env, &config_pda()), config_before);
    assert_eq!(raw(&env, &address), curve_before);
}

#[test]
fn the_pauser_s_key_unsigned_is_refused() {
    // Its address alone, signed by a stranger who pays: the pauser's signature is what counts.
    let (mut env, _) = initialized();
    let stranger = env.funded_wallet(SOL);
    let mut side = usdc_pause_sol_ix(&env.pauser.pubkey());
    side.accounts[0].is_signer = false;
    expect_err(unit(send_by(&mut env, &stranger, &[side])), ACCOUNT_NOT_SIGNER);
    let mut one = usdc_pause_sol_curve_ix(&env.pauser.pubkey(), &curve_pda(&env.mint));
    one.accounts[0].is_signer = false;
    expect_err(unit(send_by(&mut env, &stranger, &[one])), ACCOUNT_NOT_SIGNER);
    assert!(!global(&env).paused && !sol_curve(&env, &env.mint).paused);
}

#[test]
fn the_pauser_of_now_signs_and_the_one_replaced_is_refused() {
    let (mut env, _) = initialized();
    let old = env.pauser.insecure_clone();
    let new = env.funded_wallet(SOL);
    let admin = env.admin.insecure_clone();
    send_by(&mut env, &admin, &[update_usdc_config_ix(&admin.pubkey(), None, Some(new.pubkey()))]).unwrap();

    expect_err(unit(send_by(&mut env, &old, &[usdc_pause_sol_ix(&old.pubkey())])), code(FanexError::Unauthorized));
    assert!(!global(&env).paused);
    let logs = send_by(&mut env, &new, &[usdc_pause_sol_ix(&new.pubkey())]).unwrap();
    assert_paused_event(&logs, Pubkey::default(), new.pubkey());
    assert!(global(&env).paused);
}

#[test]
fn nobody_pauses_through_them_before_the_usdc_config_exists() {
    // The SOL pause by the pauser exists only once `initialize_usdc_config` has
    // named one, so `GlobalConfig.admin` goes to the vault only after that.
    let mut env = Env::new();
    let pauser = env.pauser.insecure_clone();
    env.svm.airdrop(&pauser.pubkey(), SOL).unwrap();
    assert!(!env.exists(&usdc_config_pda()));
    let address = curve_pda(&env.mint);
    expect_err(unit(send_by(&mut env, &pauser, &[usdc_pause_sol_ix(&pauser.pubkey())])), ACCOUNT_NOT_INITIALIZED);
    expect_err(unit(send_by(&mut env, &pauser, &[usdc_pause_sol_curve_ix(&pauser.pubkey(), &address)])), ACCOUNT_NOT_INITIALIZED);
    assert!(!global(&env).paused);
}

// ── The accounts ─────────────────────────────────────────────────────────────

#[test]
fn refuses_a_forged_usdc_config_or_global_config() {
    let (mut env, _) = initialized();
    let pauser = env.pauser.insecure_clone();
    let stranger = env.funded_wallet(SOL);

    // A `UsdcConfig` naming the stranger as its pauser, at an address of no seeds.
    let mut forged_usdc = env.fetch::<UsdcConfig>(&usdc_config_pda());
    forged_usdc.pauser = stranger.pubkey();
    let forged_usdc_address = Pubkey::new_unique();
    env.put(forged_usdc_address, account_data(&forged_usdc, UsdcConfig::INIT_SPACE), pepper_curve::ID);
    let side = usdc_pause_sol_ix_with(&stranger.pubkey(), &forged_usdc_address, &config_pda());
    expect_err(unit(send_by(&mut env, &stranger, &[side])), anchor(ErrorCode::ConstraintSeeds));

    // `GlobalConfig`'s bytes at an address of no seeds: the pauser writes no copy.
    let forged_global = Pubkey::new_unique();
    let bytes = raw(&env, &config_pda());
    env.put(forged_global, bytes.clone(), pepper_curve::ID);
    let side = usdc_pause_sol_ix_with(&pauser.pubkey(), &usdc_config_pda(), &forged_global);
    expect_err(unit(send_by(&mut env, &pauser, &[side])), anchor(ErrorCode::ConstraintSeeds));
    assert_eq!(raw(&env, &forged_global), bytes);
    assert!(!global(&env).paused);
}

#[test]
fn refuses_a_sol_curve_anywhere_but_its_own_seeds() {
    let (mut env, _) = initialized();
    let pauser = env.pauser.insecure_clone();
    let bytes = raw(&env, &curve_pda(&env.mint));

    // The same bytes at an address of no seeds, and at another mint's curve address.
    for address in [Pubkey::new_unique(), curve_pda(&Pubkey::new_unique())] {
        env.put(address, bytes.clone(), pepper_curve::ID);
        expect_err(
            unit(send_by(&mut env, &pauser, &[usdc_pause_sol_curve_ix(&pauser.pubkey(), &address)])),
            anchor(ErrorCode::ConstraintSeeds),
        );
        assert_eq!(raw(&env, &address), bytes);
    }
    assert!(!sol_curve(&env, &env.mint).paused);
}

#[test]
fn refuses_a_usdc_curve_by_its_discriminator() {
    // A USDC curve is paused by `set_curve_usdc_paused` alone.
    let (mut env, _) = initialized();
    let pauser = env.pauser.insecure_clone();
    let usdc_curve = env.write_curve_usdc(&vectors()[0].state);
    let address = curve_pda(&usdc_curve.mint);
    let before = raw(&env, &address);

    expect_err(unit(send_by(&mut env, &pauser, &[usdc_pause_sol_curve_ix(&pauser.pubkey(), &address)])), ACCOUNT_DISCRIMINATOR_MISMATCH);
    assert_eq!(raw(&env, &address), before);
    assert!(!env.fetch::<CurveUsdc>(&address).paused);
}

// ── Idempotent, and one key for both sides ───────────────────────────────────

#[test]
fn pausing_an_already_paused_flag_is_no_error() {
    let (mut env, _) = initialized();
    let address = curve_pda(&env.mint);
    let pauser = env.pauser.insecure_clone();
    let admin = env.admin.insecure_clone();

    // Paused by the admin first, then by the pauser twice: each write lands and emits.
    send_by(&mut env, &admin, &[set_paused_ix(&admin.pubkey(), true), set_curve_paused_ix(&admin.pubkey(), &address, true)])
        .unwrap();
    let config_paused = raw(&env, &config_pda());
    let curve_paused = raw(&env, &address);
    // Read after every write, so a toggle that lands back on `true` every other time shows.
    for round in 0..2 {
        let logs = send_by(&mut env, &pauser, &[usdc_pause_sol_ix(&pauser.pubkey())]).unwrap();
        assert_paused_event(&logs, Pubkey::default(), pauser.pubkey());
        assert_eq!(raw(&env, &config_pda()), config_paused, "round {round}");
        let logs = send_by(&mut env, &pauser, &[usdc_pause_sol_curve_ix(&pauser.pubkey(), &address)]).unwrap();
        assert_paused_event(&logs, address, pauser.pubkey());
        assert_eq!(raw(&env, &address), curve_paused, "round {round}");
    }
}

#[test]
fn one_key_pauses_both_sides_in_one_transaction_and_a_paused_side_never_stops_it() {
    // The console's kill switch: the USDC pause and the SOL one, the pauser's one signature.
    let (mut env, buyer) = initialized();
    let pauser = env.pauser.insecure_clone();
    let both = [set_usdc_paused_ix(&pauser.pubkey(), true), usdc_pause_sol_ix(&pauser.pubkey())];
    let logs = send_by(&mut env, &pauser, &both).unwrap();
    assert!(usdc_paused(&env) && global(&env).paused);
    assert_eq!(events_of::<UsdcPauseSet>(&logs).len(), 1);
    assert_paused_event(&logs, Pubkey::default(), pauser.pubkey());
    let (bought, _) = trade(&mut env, &buyer);
    expect_err(bought, code(FanexError::GloballyPaused));

    // Both sides paused: a curve's pause still lands, read no flag.
    let address = curve_pda(&env.mint);
    send_by(&mut env, &pauser, &[usdc_pause_sol_curve_ix(&pauser.pubkey(), &address)]).unwrap();
    assert!(sol_curve(&env, &env.mint).paused);
}
