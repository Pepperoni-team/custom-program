//! The harness the USDC curve's tests stand on (docs/solana/PEPPER-CURVE-PROGRAM.md
//! §13; usdc-curve design §10.11, 2026-10-22). The suites write the USDC
//! accounts into the bank rather than through their instructions: these hold
//! what the harness writes and reads to what the program takes, so a later
//! refusal is the program's, never a harness that wrote the wrong account
//! (`usdc_config.rs` holds `write_usdc_config` to what `initialize_usdc_config`
//! writes).

mod common;

use anchor_lang::solana_program::program_option::COption;
use anchor_lang::solana_program::program_pack::Pack;
use anchor_lang::{AccountDeserialize, AccountSerialize};
use anchor_spl::associated_token::get_associated_token_address as ata;
use anchor_spl::token::spl_token::{self, error::TokenError};
use common::*;
use pepper_curve::constants::{CURVE_SEED, PHASE_BONDING, USDC_CONFIG_SEED, USDC_VAULT_SEED};
use pepper_curve::events::{GraduationReady, Trade};
use pepper_curve::state::{Curve, CurveUsdc, UsdcConfig};
use pepper_curve::validation::{creator_usdc_account_required, creator_usdc_account_valid};
use pepper_curve_math::CurveState;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signer;

const REVENUE_FRESH: &str = "revenue, fresh: a buy inside both caps";
const EQUITY_AFTER_A_BUY: &str = "equity, after a 100 USDC buy: a sale of 1,000,000 tokens";

fn bytes<T: AccountSerialize>(account: &T) -> Vec<u8> {
    let mut out = Vec::new();
    account.try_serialize(&mut out).unwrap();
    out
}

/// Anchor's error number for the account at `address` read as a `T`.
fn read_error<T: AccountDeserialize>(env: &Env, address: &Pubkey) -> u32 {
    let account = env.svm.get_account(address).expect("account exists");
    match T::try_deserialize(&mut account.data.as_slice()) {
        Err(anchor_lang::error::Error::AnchorError(e)) => e.error_code_number,
        Err(other) => panic!("not an Anchor error: {other}"),
        Ok(_) => panic!("{address} read as the other kind of curve"),
    }
}

/// The counters a `CurveUsdc` holds, in the math crate's shape: the writer's
/// mapping read backwards, so a field written to the wrong twin shows.
fn state_of(curve: &CurveUsdc) -> CurveState {
    CurveState {
        virtual_reserve: curve.virtual_usdc_reserves,
        virtual_token_reserves: curve.virtual_token_reserves,
        real_reserve: curve.real_usdc_reserves,
        real_token_reserves: curve.real_token_reserves,
        creator_split_bps: curve.creator_split_bps.into(),
        fanex_split_bps: curve.fanex_split_bps.into(),
        creator_cap: curve.creator_cap_usdc,
        fanex_cap: curve.fanex_cap_usdc,
        creator_received: curve.creator_received_usdc,
        fanex_received: curve.fanex_received_usdc,
        graduation_threshold: curve.graduation_threshold_usdc,
        created_at: curve.created_at,
    }
}

#[test]
fn a_written_usdc_config_fetches_back_through_its_own_type() {
    let mut env = Env::new();
    let written = env.write_usdc_config();

    let account = env.svm.get_account(&usdc_config_pda()).unwrap();
    assert_eq!((account.owner, account.data.len()), (pepper_curve::ID, 491));
    let config: UsdcConfig = env.fetch(&usdc_config_pda());
    assert_eq!(bytes(&config), bytes(&written));

    let admin = env.admin.pubkey();
    assert_eq!(config.version, 1);
    assert_eq!(config.bump, Pubkey::find_program_address(&[USDC_CONFIG_SEED], &pepper_curve::ID).1);
    assert!(!config.paused);
    assert_eq!((config.admin, config.launcher, config.lp_fee_owner), (admin, admin, admin));
    assert_eq!(config.pauser, env.pauser.pubkey());
    assert_eq!(config.migration_authority, Pubkey::default());
    assert_eq!(config.usdc_mint, env.usdc.mint);
    // Every fee lands in the fee wallet's USDC account, opened with the config:
    // `GlobalConfig.fanex_fee_wallet`, a key apart from the admin since the
    // fee-wallet split (2026-10-24).
    let fee_wallet = env.fee_wallet.pubkey();
    assert_ne!(fee_wallet, admin);
    assert_eq!(config.fee_usdc_account, ata(&fee_wallet, &env.usdc.mint));
    let fee = env.token_account(&config.fee_usdc_account);
    assert_eq!((fee.mint, fee.owner, fee.amount), (env.usdc.mint, fee_wallet, 0));
    // The five Raydium fields stay all-zero until `set_usdc_raydium`.
    for field in [
        config.raydium_cpmm_program,
        config.raydium_amm_config,
        config.raydium_create_pool_fee,
        config.raydium_lock_program,
        config.raydium_lock_authority,
    ] {
        assert_eq!(field, Pubkey::default());
    }
}

#[test]
fn a_written_usdc_curve_fetches_back_through_its_own_type_beside_a_sol_curve() {
    let mut env = Env::new();
    env.write_usdc_config();
    let revenue = vector(REVENUE_FRESH).state;
    let written = env.write_curve_usdc(&revenue);
    assert_ne!(written.mint, env.mint, "a fresh mint: the SOL curve keeps its own");

    let address = curve_pda(&written.mint);
    let account = env.svm.get_account(&address).unwrap();
    assert_eq!((account.owner, account.data.len()), (pepper_curve::ID, 354));
    let curve: CurveUsdc = env.fetch(&address);
    assert_eq!(bytes(&curve), bytes(&written));
    assert_eq!(state_of(&curve), revenue);
    assert_eq!((curve.version, curve.phase, curve.paused, curve.migrated), (1, PHASE_BONDING, false, false));
    assert_eq!((curve.mint, curve.creator), (written.mint, env.creator.pubkey()));
    assert_eq!(curve.creator_target_usdc, revenue.creator_cap + revenue.fanex_cap);
    assert_eq!((curve.graduated_at, curve.migrated_at), (0, 0));

    // Each kind refuses the other's bytes with Anchor's 3002, and the SOL curve
    // `Env::new` wrote is still there, read as a `Curve`.
    assert_eq!(read_error::<Curve>(&env, &address), ACCOUNT_DISCRIMINATOR_MISMATCH);
    assert_eq!(read_error::<CurveUsdc>(&env, &curve_pda(&env.mint)), ACCOUNT_DISCRIMINATOR_MISMATCH);
    let sol: Curve = env.fetch(&curve_pda(&env.mint));
    assert_eq!(sol.mint, env.mint);
}

#[test]
fn the_test_usdc_is_a_six_decimal_mint_with_a_freeze_authority() {
    let env = Env::new();
    let account = env.svm.get_account(&env.usdc.mint).unwrap();
    assert_eq!(account.owner, spl_token::ID);
    let mint = spl_token::state::Mint::unpack(&account.data).unwrap();
    assert_eq!(mint.decimals, 6);
    assert_eq!(mint.freeze_authority, COption::Some(env.usdc.freeze_authority.pubkey()));
    assert_eq!(mint.mint_authority, COption::Some(env.usdc.mint_authority.pubkey()));
    assert_eq!(mint.supply, 0);
}

#[test]
fn a_frozen_usdc_account_neither_sends_nor_receives_until_thawed() {
    let mut env = Env::new();
    let holder = env.funded_wallet(SOL);
    let from = env.usdc_account(&holder.pubkey());
    let creator = env.creator.pubkey();
    let to = env.usdc_account(&creator);
    env.mint_usdc(&from, 5 * USDC);
    let transfer = spl_token::instruction::transfer_checked(
        &spl_token::ID,
        &from,
        &env.usdc.mint,
        &to,
        &holder.pubkey(),
        &[],
        USDC,
        USDC_DECIMALS,
    )
    .unwrap();

    // Only the freeze authority freezes: the mint authority cannot.
    let mint_authority = env.usdc.mint_authority.insecure_clone();
    expect_err(env.freeze_usdc_by(&mint_authority, &from), TokenError::OwnerMismatch as u32);

    env.freeze_usdc(&from);
    assert!(env.token_account(&from).is_frozen());
    expect_err(env.send_paid_by(&holder, &[transfer.clone()], &[]), TokenError::AccountFrozen as u32);
    env.thaw_usdc(&from);

    // A frozen destination, as a frozen fee account would be.
    env.freeze_usdc(&to);
    expect_err(env.send_paid_by(&holder, &[transfer.clone()], &[]), TokenError::AccountFrozen as u32);
    assert_eq!((env.token_amount(&from), env.token_amount(&to)), (5 * USDC, 0));
    env.thaw_usdc(&to);

    env.send_paid_by(&holder, &[transfer], &[]).unwrap();
    assert_eq!((env.token_amount(&from), env.token_amount(&to)), (4 * USDC, USDC));
}

#[test]
fn a_usdc_curve_s_two_vaults_answer_to_the_curve_pda_alone() {
    let mut env = Env::new();
    env.write_usdc_config();
    let curve = env.write_curve_usdc(&vector(EQUITY_AFTER_A_BUY).state);
    assert!(curve.real_usdc_reserves > 0, "a curve that holds USDC");
    let address = curve_pda(&curve.mint);
    let usdc_vault = usdc_vault_pda(&curve.mint);

    let reserve = env.token_account(&usdc_vault);
    assert_eq!((reserve.mint, reserve.owner, reserve.amount), (env.usdc.mint, address, curve.real_usdc_reserves));
    assert_eq!((reserve.delegate, reserve.close_authority), (COption::None, COption::None));
    assert!(!reserve.is_frozen());
    // A PDA of its own, not the curve's associated USDC account (§10.2).
    assert_ne!(usdc_vault, ata(&address, &env.usdc.mint));
    // Minted, so the USDC's supply counts what the vault holds.
    let usdc = spl_token::state::Mint::unpack(&env.svm.get_account(&env.usdc.mint).unwrap().data).unwrap();
    assert_eq!(usdc.supply, curve.real_usdc_reserves);

    let tokens = env.token_account(&ata(&address, &curve.mint));
    assert_eq!(
        (tokens.mint, tokens.owner, tokens.amount),
        (curve.mint, address, curve.real_token_reserves + curve.migration_token_reserves)
    );
    assert_eq!((tokens.delegate, tokens.close_authority), (COption::None, COption::None));

    // The bumps the curve records give back its address and its vault's: the
    // seeds the program signs every move out of either vault with.
    let mint = curve.mint;
    assert_eq!(Pubkey::create_program_address(&[CURVE_SEED, mint.as_ref(), &[curve.bump]], &pepper_curve::ID), Ok(address));
    assert_eq!(
        Pubkey::create_program_address(&[USDC_VAULT_SEED, mint.as_ref(), &[curve.usdc_vault_bump]], &pepper_curve::ID),
        Ok(usdc_vault)
    );

    // No key but the curve PDA moves the reserve: the admin's signature is refused.
    let admin = env.admin.pubkey();
    let fee = env.usdc_account(&admin);
    let drain = spl_token::instruction::transfer_checked(&spl_token::ID, &usdc_vault, &env.usdc.mint, &fee, &admin, &[], 1, USDC_DECIMALS)
        .unwrap();
    expect_err(env.send(&[drain], &[]), TokenError::OwnerMismatch as u32);
    assert_eq!(env.token_amount(&usdc_vault), curve.real_usdc_reserves);
}

#[test]
fn a_revenue_curve_names_a_creator_usdc_account_the_program_accepts() {
    let mut env = Env::new();
    let config = env.write_usdc_config();
    let curve = env.write_curve_usdc(&vector(REVENUE_FRESH).state);

    assert_eq!(curve.creator_usdc_account, ata(&curve.creator, &env.usdc.mint));
    let account = env.token_account(&curve.creator_usdc_account);
    assert!(creator_usdc_account_valid(
        &curve.creator_usdc_account,
        &account,
        &curve.creator,
        &config.usdc_mint,
        &config.launcher,
    ));
    // A fresh revenue curve's buy carries it, account 8: the creator's cap is open.
    assert!(creator_usdc_account_required(curve.creator_split_bps, curve.creator_received_usdc, curve.creator_cap_usdc));

    // An equity curve takes no split and names none.
    let equity = env.write_curve_usdc(&vectors()[0].state);
    assert_eq!(equity.creator_usdc_account, Pubkey::default());
}

#[test]
fn the_golden_vectors_parse_to_their_21_cases() {
    // 21 since the internal audit (2026-10-29): a partial fill keeping the raise's shares and one
    // whose fee on the net is clamped to the fee on the input.
    let vectors = vectors();
    assert_eq!(vectors.len(), 21);
    let buys = vectors.iter().filter(|v| matches!(v.op, VectorOp::Buy { .. })).count();
    let refused = vectors.iter().filter(|v| matches!(v.outcome, VectorOutcome::Refused(_))).count();
    assert_eq!((buys, vectors.len() - buys, refused), (16, 5, 6));
    assert_eq!(vectors[0].name, "equity, fresh: the smallest buy");
    assert_eq!(vectors[0].op, VectorOp::Buy { amount: 1, now: 1_800_000_600 });
    assert!(matches!(vectors[0].outcome, VectorOutcome::Bought(q) if q.tokens_out == 38_214));

    // Each case, read back, is what the crate returns for it today: a field
    // read from the wrong key would differ.
    for v in &vectors {
        assert_eq!(v.quote(), v.outcome, "{}", v.name);
    }
}

#[test]
fn send_logged_hands_back_the_events_a_buy_emits() {
    let mut env = Env::new();
    env.write_tradeable_curve(OCT_5_2026 - DAY);
    let buyer = env.funded_wallet(SOL);
    let sol_in = SOL / 20;

    let buy = env.buy_ix(&buyer.pubkey(), sol_in, &env.fee_wallet.pubkey());
    let logs = env.send_logged_paid_by(&buyer, &[buy], &[]).unwrap();

    let trade: Trade = event(&logs);
    assert_eq!((trade.mint, trade.trader, trade.is_buy), (env.mint, buyer.pubkey(), true));
    assert_eq!((trade.sol_amount, trade.platform_fee), (sol_in, sol_in / 100));
    assert!(trade.token_amount > 0);
    assert_eq!(trade.token_amount, env.token_balance(&buyer.pubkey()));
    assert_eq!(trade.real_sol_reserves, sol_in - trade.platform_fee);
    assert_eq!(trade.real_token_reserves, 80_000_000 * 1_000_000 - trade.token_amount);
    assert_eq!(events_of::<Trade>(&logs).len(), 1);
    assert!(events_of::<GraduationReady>(&logs).is_empty());
}
