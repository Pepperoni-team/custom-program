//! The fee-wallet split (2026-10-24), run against the built program
//! (docs/ops/KEY-ROTATION.md §1e, docs/solana/PEPPER-CURVE-PROGRAM.md §13).
//!
//! Until then `config.fanex_fee_wallet` was also the hot wallet that signed:
//! the v1 dividend funder, the v2 approver, the Metaplex update authority of
//! every SOL token. Now the fee wallet is receive-only and `buy` and `sell` are
//! its only readers; the dividend roles are `config.graduation_authority`'s, the
//! metadata authority `config.admin`'s. And `set_distributor_funder` hands the v1
//! months the hot wallet funded to the dividend payer, so its key can retire.
//!
//! ⚠️ Not here: a new fee wallet named before it holds its rent minimum. The
//! runtime refuses a trade whose fee would leave it under that minimum, but
//! LiteSVM 0.6.1 checks the rent state of accounts holding data alone
//! (`check_accounts_rent`), and a wallet holds none, so a suite run here would
//! land that trade. The cut-over funds the wallet first (KEY-ROTATION.md §1e).

mod common;

use common::*;
use pepper_curve::constants::{
    CURVE_SEED, DIVIDEND_PAYER, SOL_VAULT_SEED, TOKEN_TOTAL_SUPPLY, VESTING_SEED, VESTING_VAULT_SEED,
};
use pepper_curve::errors::FanexError;
use pepper_curve::events::DistributorFunderSet;
use pepper_curve::state::{Curve, Distributor, DistributorV2, GlobalConfig};
use solana_sdk::compute_budget::ComputeBudgetInstruction;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signer};
use solana_sdk::{system_program, sysvar};

/// Anchor's code for a program run at an address other than its own: what the
/// stand-in Metaplex (this program's own `.so`, loaded at Metaplex's address)
/// answers the metadata CPI with, so a launch that reaches it passed every
/// account check.
const DECLARED_PROGRAM_ID_MISMATCH: u32 = 4100;

const METAPLEX: &str = "metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s";

/// 0.03 SOL, about 0.6% of a fresh curve's supply: under the launch cap.
const BUY: u64 = 3 * SOL / 100;

/// A tradeable curve ten seconds after its launch, and a funded buyer.
fn launched() -> (Env, Keypair) {
    let mut env = Env::new();
    env.write_tradeable_curve(OCT_5_2026);
    env.set_time(OCT_5_2026 + 10);
    let buyer = env.funded_wallet(10 * SOL);
    (env, buyer)
}

/// The SOL a buy of `sol_in` pays Fanex on `env`'s curve as it stands: its share
/// of the raise and the 1% platform fee, the one transfer `buy` makes.
fn fanex_take_of_a_buy(env: &Env, sol_in: u64) -> u64 {
    let curve: Curve = env.fetch(&curve_pda(&env.mint));
    let fee = pepper_curve::math::platform_swap_fee(sol_in).unwrap();
    let split = pepper_curve::math::split_with_cap(
        sol_in - fee,
        curve.creator_split_bps,
        curve.fanex_split_bps,
        curve.creator_cap_lamports,
        curve.fanex_cap_lamports,
        curve.creator_received_lamports,
        curve.fanex_received_lamports,
    )
    .unwrap();
    split.fanex + fee
}

// ── the cut-over's write ─────────────────────────────────────────────────────

#[test]
fn update_config_moves_the_fee_wallet_alone() {
    // The runbook's one write (KEY-ROTATION.md §1e): the other two keys stay.
    let mut env = Env::one_key();
    let admin = env.admin.insecure_clone();
    let new_fee_wallet = Pubkey::new_unique();

    env.update_config_by(&admin, None, Some(new_fee_wallet), None).unwrap();

    let config: GlobalConfig = env.fetch(&config_pda());
    assert_eq!(config.fanex_fee_wallet, new_fee_wallet);
    assert_eq!(config.admin, admin.pubkey());
    assert_eq!(config.graduation_authority, admin.pubkey());
}

#[test]
fn update_config_refuses_to_hand_the_admin_to_nobody_and_takes_a_vault() {
    // The internal audit (2026-10-29): only the admin unpauses the SOL side, and no key signs for
    // all-zero. A Squads vault, an off-curve key, is taken.
    let mut env = Env::one_key();
    let admin = env.admin.insecure_clone();
    expect_err(env.update_config_by(&admin, Some(Pubkey::default()), None, None), code(FanexError::AdminUnset));
    let config: GlobalConfig = env.fetch(&config_pda());
    assert_eq!(config.admin, admin.pubkey());

    let vault = Pubkey::find_program_address(&[b"multisig", b"vault"], &Pubkey::new_unique()).0;
    env.update_config_by(&admin, Some(vault), None, None).unwrap();
    let config: GlobalConfig = env.fetch(&config_pda());
    assert_eq!(config.admin, vault);
}

// ── buy and sell pay the fee wallet, and nothing else ────────────────────────

#[test]
fn a_buy_pays_the_platform_fee_to_the_fee_wallet_alone() {
    let (mut env, buyer) = launched();
    let (fee_wallet, authority, admin) =
        (env.fee_wallet.pubkey(), env.graduation_authority.pubkey(), env.admin.pubkey());
    let before = [env.lamports(&fee_wallet), env.lamports(&authority), env.lamports(&admin)];

    env.buy(&buyer, BUY).unwrap();

    // An equity curve: no raise share, so the 1% is all Fanex takes.
    let fee = pepper_curve::math::platform_swap_fee(BUY).unwrap();
    assert!(fee > 0);
    assert_eq!(env.lamports(&fee_wallet), before[0] + fee);
    assert_eq!(env.lamports(&authority), before[1]);
    assert_eq!(env.lamports(&admin), before[2]);
}

#[test]
fn a_raise_buy_pays_the_fee_wallet_its_share_and_the_fee_in_one() {
    let mut env = Env::new();
    env.write_tradeable_curve_raising(OCT_5_2026, Some(Raise::revenue()));
    env.set_time(OCT_5_2026 + 10);
    let buyer = env.funded_wallet(10 * SOL);
    let (fee_wallet, admin) = (env.fee_wallet.pubkey(), env.admin.pubkey());
    let before = [env.lamports(&fee_wallet), env.lamports(&admin)];
    let take = fanex_take_of_a_buy(&env, BUY);

    let ix = env.buy_ix(&buyer.pubkey(), BUY, &admin);
    expect_err(env.send_paid_by(&buyer, &[ix], &[]), code(FanexError::AccountMismatch));
    env.buy(&buyer, BUY).unwrap();

    assert!(take > pepper_curve::math::platform_swap_fee(BUY).unwrap(), "a raise share was taken");
    assert_eq!(env.lamports(&fee_wallet), before[0] + take);
    assert_eq!(env.lamports(&admin), before[1]);
}

#[test]
fn a_buy_naming_another_config_key_as_fanex_is_refused() {
    let (mut env, buyer) = launched();
    for other in [env.admin.pubkey(), env.graduation_authority.pubkey()] {
        let ix = env.buy_ix(&buyer.pubkey(), BUY, &other);
        expect_err(env.send_paid_by(&buyer, &[ix], &[]), code(FanexError::AccountMismatch));
    }
    assert_eq!(env.token_balance(&buyer.pubkey()), 0);
}

#[test]
fn a_sell_pays_the_platform_fee_to_the_fee_wallet_alone() {
    let (mut env, seller) = launched();
    env.buy(&seller, BUY).unwrap();
    let tokens = env.token_balance(&seller.pubkey());
    let curve: Curve = env.fetch(&curve_pda(&env.mint));
    let sol_out =
        pepper_curve::math::sell_sol_out(curve.virtual_sol_reserves, curve.virtual_token_reserves, tokens).unwrap();
    let fee = pepper_curve::math::platform_swap_fee(sol_out).unwrap();
    let (fee_wallet, authority) = (env.fee_wallet.pubkey(), env.graduation_authority.pubkey());
    let before = [env.lamports(&fee_wallet), env.lamports(&authority)];

    for other in [env.admin.pubkey(), authority] {
        let ix = env.sell_ix(&seller.pubkey(), tokens, &other);
        expect_err(env.send_paid_by(&seller, &[ix], &[]), code(FanexError::AccountMismatch));
    }
    let ix = env.sell_ix(&seller.pubkey(), tokens, &fee_wallet);
    env.send_paid_by(&seller, &[ix], &[]).unwrap();

    assert!(fee > 0);
    assert_eq!(env.lamports(&fee_wallet), before[0] + fee);
    assert_eq!(env.lamports(&authority), before[1]);
    assert_eq!(env.token_balance(&seller.pubkey()), 0);
}

// ── the fee wallet signs nothing a month needs ───────────────────────────────

#[test]
fn the_fee_wallet_neither_funds_nor_approves_a_month() {
    let mut env = Env::new();
    env.svm.airdrop(&env.fee_wallet.pubkey(), 10 * SOL).unwrap();
    let fee_wallet = env.fee_wallet.insecure_clone();
    let creator = env.creator.insecure_clone();
    let month = env.v2_month(2);
    let expires = OCT_5_2026 + 365 * DAY;

    expect_err(env.init_v1(&fee_wallet, [7u8; 32], SOL), code(FanexError::Unauthorized));
    expect_err(env.init_v2(&fee_wallet, month.tree.root, month.total, expires, 2), code(FanexError::Unauthorized));
    let ix = env.init_v2_ix(&creator, &fee_wallet.pubkey(), month.tree.root, month.total, expires, 2);
    expect_err(env.send(&[ix], &[&creator, &fee_wallet]), code(FanexError::Unauthorized));
    assert!(!env.exists(&distributor_pda(&env.mint, MONTH)));
}

#[test]
fn the_graduation_authority_approves_a_creators_month_but_cannot_fund_one_of_its_own() {
    let mut env = Env::new();
    let authority = env.graduation_authority.insecure_clone();
    let creator = env.creator.insecure_clone();
    let month = env.v2_month(2);
    let expires = OCT_5_2026 + 365 * DAY;

    expect_err(env.init_v2(&authority, month.tree.root, month.total, expires, 2), code(FanexError::Unauthorized));
    env.init_v2(&creator, month.tree.root, month.total, expires, 2).unwrap();
}

// ── a token's metadata answers to the admin ──────────────────────────────────

/// `create_curve` for a fresh mint, `update_authority` named as the metadata's
/// update authority, the equity shape `launch_cap.rs` trades.
fn launch(env: &mut Env, update_authority: &Pubkey) -> Result<(), solana_sdk::transaction::TransactionError> {
    let metaplex: Pubkey = METAPLEX.parse().unwrap();
    let mint = Keypair::new();
    let m = mint.pubkey();
    let curve = pda(&[CURVE_SEED, m.as_ref()]);
    let create = ix(
        pepper_curve::accounts::CreateCurve {
            launcher: env.admin.pubkey(),
            creator: env.creator.pubkey(),
            config: config_pda(),
            mint: m,
            curve,
            sol_vault: pda(&[SOL_VAULT_SEED, m.as_ref()]),
            vesting: pda(&[VESTING_SEED, m.as_ref()]),
            token_vault: anchor_spl::associated_token::get_associated_token_address(&curve, &m),
            vesting_vault: pda(&[VESTING_VAULT_SEED, m.as_ref()]),
            metadata: Pubkey::find_program_address(&[b"metadata", metaplex.as_ref(), m.as_ref()], &metaplex).0,
            metadata_update_authority: *update_authority,
            token_program: anchor_spl::token::spl_token::ID,
            token_metadata_program: metaplex,
            associated_token_program: anchor_spl::associated_token::ID,
            system_program: system_program::id(),
            rent: sysvar::rent::id(),
        },
        pepper_curve::instruction::CreateCurve {
            params: pepper_curve::instructions::create_curve::CreateCurveParams {
                creator_target_lamports: 0,
                cpmm_seed_lamports: 24 * SOL,
                creator_split_bps: 0,
                fanex_split_bps: 0,
                creator_cap_lamports: 0,
                fanex_cap_lamports: 0,
                virtual_sol_reserves: 5 * SOL,
                virtual_token_reserves: TOKEN_TOTAL_SUPPLY,
                curve_token_supply: 80_000_000 * 1_000_000,
                migration_token_supply: 14_000_000 * 1_000_000,
                vesting_token_supply: 6_000_000 * 1_000_000,
                vesting_cliff_seconds: 0,
                vesting_duration_seconds: 0,
                name: "Split".to_string(),
                symbol: "SPLT".to_string(),
                uri: "https://pepper.fan/split.json".to_string(),
            },
        },
    );
    let budget = ComputeBudgetInstruction::set_compute_unit_limit(400_000);
    env.send(&[budget, create], &[&mint])
}

/// An `Env` with this program's own `.so` at Metaplex's address: executable, so
/// the account check passes, and refusing the CPI, so nothing is created.
fn with_stand_in_metaplex() -> Env {
    let mut env = Env::new();
    env.svm.add_program_from_file(METAPLEX.parse::<Pubkey>().unwrap(), program_so()).unwrap();
    env
}

#[test]
fn a_launch_names_the_admin_as_its_metadata_authority() {
    // The update authority passes and the launch runs to the metadata CPI.
    let mut env = with_stand_in_metaplex();
    let admin = env.admin.pubkey();
    expect_err(launch(&mut env, &admin), DECLARED_PROGRAM_ID_MISMATCH);
}

#[test]
fn a_launch_naming_the_fee_wallet_or_the_graduation_authority_as_metadata_authority_is_refused() {
    let mut env = with_stand_in_metaplex();
    for other in [env.fee_wallet.pubkey(), env.graduation_authority.pubkey()] {
        expect_err(launch(&mut env, &other), code(FanexError::AccountMismatch));
    }
}

// ── set_distributor_funder: a v1 month's close to the dividend payer ─────────

#[test]
fn a_v1_months_close_is_handed_to_the_dividend_payer_and_nothing_else_moves() {
    let mut env = Env::unverified();
    let holders = env.v1_holders(3);
    let tree = env.fund_v1(&holders);
    let address = distributor_pda(&env.mint, MONTH);
    env.claim_v1(&holders[0].0, holders[0].1, tree.proofs[0].clone()).unwrap();
    let before: Distributor = env.fetch(&address);
    let held = env.lamports(&address);
    let authority = env.graduation_authority.insecure_clone();

    let logs = env.send_logged(&[env.set_funder_ix(&authority.pubkey(), &DIVIDEND_PAYER)], &[&authority]).unwrap();

    let after: Distributor = env.fetch(&address);
    assert_eq!(after.funder, DIVIDEND_PAYER);
    assert_eq!(
        (after.merkle_root, after.total_amount, after.claimed_amount, after.expires_at, after.month, after.mint),
        (before.merkle_root, before.total_amount, before.claimed_amount, before.expires_at, before.month, before.mint),
    );
    assert_eq!(env.lamports(&address), held);
    let emitted = events::<DistributorFunderSet>(&logs);
    assert_eq!(emitted.len(), 1);
    let event = &emitted[0];
    assert_eq!((event.mint, event.month, event.previous, event.funder), (env.mint, MONTH, authority.pubkey(), DIVIDEND_PAYER));

    // The month still pays its holders.
    env.claim_v1(&holders[1].0, holders[1].1, tree.proofs[1].clone()).unwrap();
}

#[test]
fn a_handed_over_month_closes_to_the_dividend_payer_and_its_deposits_to_their_payer() {
    let mut env = Env::unverified();
    env.svm.airdrop(&DIVIDEND_PAYER, SOL).unwrap();
    let holders = env.v1_holders(2);
    let tree = env.fund_v1(&holders);
    env.claim_v1(&holders[0].0, holders[0].1, tree.proofs[0].clone()).unwrap();
    let authority = env.graduation_authority.insecure_clone();
    env.send(&[env.set_funder_ix(&authority.pubkey(), &DIVIDEND_PAYER)], &[&authority]).unwrap();
    env.set_time(OCT_5_2026 + 365 * DAY);
    let address = distributor_pda(&env.mint, MONTH);
    let held = env.lamports(&address);
    let payer_before = env.lamports(&DIVIDEND_PAYER);

    // The old funder no longer closes it.
    expect_err(env.send(&[env.close_v1_ix_by(&authority.pubkey())], &[&authority]), code(FanexError::Unauthorized));
    env.send_unverified(&[env.close_v1_ix_by(&DIVIDEND_PAYER)], &[]).unwrap();

    assert!(!env.exists(&address));
    assert_eq!(env.lamports(&DIVIDEND_PAYER), payer_before + held);

    // A v1 claim's deposit returns to whoever paid it, the hand-over notwithstanding.
    let admin = env.admin.pubkey();
    let deposit = env.lamports(&claim_status_pda(&address, &holders[0].0.pubkey()));
    assert!(deposit > 0);
    let admin_before = env.lamports(&admin);
    env.send(&[env.close_claim_status_ix(&holders[0].0.pubkey(), &admin)], &[]).unwrap();
    assert_eq!(env.lamports(&admin), admin_before + deposit - 5_000);
}

#[test]
fn a_month_is_handed_to_the_dividend_payer_alone() {
    let mut env = Env::new();
    let holders = env.v1_holders(2);
    env.fund_v1(&holders);
    let authority = env.graduation_authority.insecure_clone();
    let others = [
        env.admin.pubkey(),
        env.fee_wallet.pubkey(),
        env.creator.pubkey(),
        authority.pubkey(),
        Pubkey::default(),
        Pubkey::new_unique(),
    ];

    for other in others {
        let ix = env.set_funder_ix(&authority.pubkey(), &other);
        expect_err(env.send(&[ix], &[&authority]), code(FanexError::Unauthorized));
    }
    let d: Distributor = env.fetch(&distributor_pda(&env.mint, MONTH));
    assert_eq!(d.funder, authority.pubkey());
}

#[test]
fn only_the_recorded_funder_hands_a_month_over() {
    let mut env = Env::new();
    let holders = env.v1_holders(2);
    env.fund_v1(&holders);
    let authority = env.graduation_authority.pubkey();

    for signer in [env.admin.insecure_clone(), env.fee_wallet.insecure_clone(), env.creator.insecure_clone()] {
        let ix = env.set_funder_ix(&signer.pubkey(), &DIVIDEND_PAYER);
        expect_err(env.send(&[ix], &[&signer]), code(FanexError::Unauthorized));
    }
    // The funder named but not signing.
    let mut ix = env.set_funder_ix(&authority, &DIVIDEND_PAYER);
    ix.accounts[0].is_signer = false;
    expect_err(env.send(&[ix], &[]), ACCOUNT_NOT_SIGNER);

    let d: Distributor = env.fetch(&distributor_pda(&env.mint, MONTH));
    assert_eq!(d.funder, authority);
}

#[test]
fn a_v2_month_is_not_handed_over() {
    // v2's months are the payer's or the creator's already: the instruction reads v1 alone.
    let mut env = Env::new();
    let month = env.v2_month(2);
    env.fund_v2(&month);
    let creator = env.creator.insecure_clone();

    let ix = env.set_funder_ix(&creator.pubkey(), &DIVIDEND_PAYER);
    expect_err(env.send(&[ix], &[&creator]), ACCOUNT_DISCRIMINATOR_MISMATCH);
}

#[test]
fn the_global_pause_does_not_stop_a_hand_over() {
    // It moves no funds, and the pause is when a key is likeliest to be retiring.
    let mut env = Env::new();
    let holders = env.v1_holders(2);
    env.fund_v1(&holders);
    env.set_paused(true);
    let authority = env.graduation_authority.insecure_clone();

    env.send(&[env.set_funder_ix(&authority.pubkey(), &DIVIDEND_PAYER)], &[&authority]).unwrap();

    let d: Distributor = env.fetch(&distributor_pda(&env.mint, MONTH));
    assert_eq!(d.funder, DIVIDEND_PAYER);
}

// ── one key, as mainnet holds it until the cut-over ──────────────────────────

#[test]
fn with_one_key_every_role_is_that_key() {
    // Rick's «behaves as today», in one place: the upgraded program on a config
    // whose three keys are one, with the one narrowing asked for (no config key
    // funds a v2 month).
    let mut env = Env::one_key();
    env.write_tradeable_curve(OCT_5_2026);
    env.set_time(OCT_5_2026 + 10);
    let key = env.admin.insecure_clone();
    let trader = env.funded_wallet(10 * SOL);
    let before = env.lamports(&key.pubkey());

    env.buy(&trader, BUY).unwrap();
    let tokens = env.token_balance(&trader.pubkey());
    let ix = env.sell_ix(&trader.pubkey(), tokens, &key.pubkey());
    env.send_paid_by(&trader, &[ix], &[]).unwrap();
    assert!(env.lamports(&key.pubkey()) > before, "the one key was paid both fees");

    let creator = env.creator.insecure_clone();
    let month = env.v2_month(2);
    let expires = OCT_5_2026 + 365 * DAY;
    expect_err(env.init_v2(&key, month.tree.root, month.total, expires, 2), code(FanexError::Unauthorized));
    env.init_v2(&creator, month.tree.root, month.total, expires, 2).unwrap();
    let d: DistributorV2 = env.fetch(&distributor_pda(&env.mint, MONTH));
    assert_eq!(d.funder, creator.pubkey());

    let mut v1 = Env::one_key();
    let holders = v1.v1_holders(2);
    v1.fund_v1(&holders);
    let d: Distributor = v1.fetch(&distributor_pda(&v1.mint, MONTH));
    assert_eq!(d.funder, v1.admin.pubkey());

    let mut launches = Env::one_key();
    launches.svm.add_program_from_file(METAPLEX.parse::<Pubkey>().unwrap(), program_so()).unwrap();
    let admin = launches.admin.pubkey();
    expect_err(launch(&mut launches, &admin), DECLARED_PROGRAM_ID_MISMATCH);
}

#[test]
fn with_one_key_the_dividend_payer_funds_a_v2_month_it_approves() {
    let mut env = Env::unverified();
    let key = env.admin.insecure_clone();
    env.update_config_by(&key, None, Some(key.pubkey()), Some(key.pubkey())).unwrap();
    env.svm.airdrop(&DIVIDEND_PAYER, 10 * SOL).unwrap();
    let month = env.v2_month(2);

    let ix = env.init_v2_ix_by(&DIVIDEND_PAYER, &key.pubkey(), month.tree.root, month.total, OCT_5_2026 + 365 * DAY, 2);
    env.send_unverified(&[ix], &[]).unwrap();

    let d: DistributorV2 = env.fetch(&distributor_pda(&env.mint, MONTH));
    assert_eq!(d.funder, DIVIDEND_PAYER);
}

#[test]
fn a_month_funded_before_the_split_closes_with_its_funder_and_is_handed_over_by_it() {
    // Mainnet's v1 months: funded by the one key, which keeps closing them after
    // the fee wallet and the graduation authority move, and hands each to the payer.
    let mut env = Env::one_key();
    let holders = env.v1_holders(2);
    env.fund_v1(&holders);
    let old = env.admin.insecure_clone();
    let (fee_wallet, authority) = (env.funded_wallet(SOL), env.funded_wallet(SOL));
    env.update_config_by(&old, None, Some(fee_wallet.pubkey()), Some(authority.pubkey())).unwrap();
    env.set_time(OCT_5_2026 + 365 * DAY);

    for other in [&fee_wallet, &authority] {
        expect_err(env.send(&[env.close_v1_ix_by(&other.pubkey())], &[other]), code(FanexError::Unauthorized));
        let ix = env.set_funder_ix(&other.pubkey(), &DIVIDEND_PAYER);
        expect_err(env.send(&[ix], &[other]), code(FanexError::Unauthorized));
    }
    env.send(&[env.set_funder_ix(&old.pubkey(), &DIVIDEND_PAYER)], &[]).unwrap();
    let d: Distributor = env.fetch(&distributor_pda(&env.mint, MONTH));
    assert_eq!(d.funder, DIVIDEND_PAYER);
}

#[test]
fn a_month_funded_before_the_split_still_closes_with_its_funder() {
    let mut env = Env::one_key();
    let holders = env.v1_holders(2);
    env.fund_v1(&holders);
    let old = env.admin.insecure_clone();
    let fee_wallet = env.funded_wallet(SOL);
    env.update_config_by(&old, None, Some(fee_wallet.pubkey()), None).unwrap();
    env.set_time(OCT_5_2026 + 365 * DAY);
    let address = distributor_pda(&env.mint, MONTH);
    let held = env.lamports(&address);
    let before = env.lamports(&old.pubkey());

    env.send(&[env.close_v1_ix_by(&old.pubkey())], &[]).unwrap();

    assert!(!env.exists(&address));
    assert_eq!(env.lamports(&old.pubkey()), before + held - 5_000);
}

// ── The internal audit's fix round (2026-10-29) ─────────────────────────────

fn set_v2_funder_ix(env: &Env, funder: &Pubkey, new_funder: &Pubkey) -> solana_sdk::instruction::Instruction {
    ix(
        pepper_curve::accounts::SetDistributorV2Funder { funder: *funder, distributor: distributor_pda(&env.mint, MONTH) },
        pepper_curve::instruction::SetDistributorV2Funder { new_funder: *new_funder },
    )
}

fn close_expired_ix(env: &Env, funder: &Pubkey) -> solana_sdk::instruction::Instruction {
    ix(
        pepper_curve::accounts::CloseExpiredDistributor { distributor: distributor_pda(&env.mint, MONTH), funder: *funder },
        pepper_curve::instruction::CloseExpiredDistributor {},
    )
}

fn close_expired_v2_ix(env: &Env, funder: &Pubkey) -> solana_sdk::instruction::Instruction {
    ix(
        pepper_curve::accounts::CloseExpiredDistributorV2 { distributor: distributor_pda(&env.mint, MONTH), funder: *funder },
        pepper_curve::instruction::CloseExpiredDistributorV2 {},
    )
}

#[test]
fn a_v2_months_close_is_handed_to_the_dividend_payer_by_its_funder_alone() {
    // Before the fee-wallet split the hot wallet funded v2 months too, and only a month's funder
    // closes it: `set_distributor_v2_funder` hands such a month to the dividend payer, as
    // `set_distributor_funder` does a v1 one, so the hot key can retire with none left on it.
    let mut env = Env::new();
    let month = env.v2_month(3);
    env.fund_v2(&month);
    let funder = env.creator.insecure_clone();
    let address = distributor_pda(&env.mint, MONTH);
    let before: DistributorV2 = env.fetch(&address);
    let held = env.lamports(&address);

    // A stranger, and any key but the dividend payer as the new funder: refused.
    let stranger = env.funded_wallet(SOL);
    let by_stranger = set_v2_funder_ix(&env, &stranger.pubkey(), &DIVIDEND_PAYER);
    expect_err(env.send(&[by_stranger], &[&stranger]), code(FanexError::Unauthorized));
    let elsewhere = set_v2_funder_ix(&env, &funder.pubkey(), &stranger.pubkey());
    expect_err(env.send(&[elsewhere], &[&funder]), code(FanexError::Unauthorized));

    let logs = env.send_logged(&[set_v2_funder_ix(&env, &funder.pubkey(), &DIVIDEND_PAYER)], &[&funder]).unwrap();
    let after: DistributorV2 = env.fetch(&address);
    assert_eq!(after.funder, DIVIDEND_PAYER);
    assert_eq!(
        (after.merkle_root, after.total_amount, after.claimed_amount, after.expires_at, after.leaf_count, after.claimed.clone()),
        (before.merkle_root, before.total_amount, before.claimed_amount, before.expires_at, before.leaf_count, before.claimed.clone()),
    );
    assert_eq!(env.lamports(&address), held);
    let event: DistributorFunderSet = event(&logs);
    assert_eq!((event.mint, event.month, event.previous, event.funder), (env.mint, MONTH, funder.pubkey(), DIVIDEND_PAYER));
    // The old funder no longer closes it.
    env.set_time(before.expires_at);
    expect_err(env.close_v2(&funder), code(FanexError::Unauthorized));
}

#[test]
fn anyone_closes_an_expired_v1_month_to_its_funder_and_nobody_before() {
    // The internal audit (2026-10-29): only its funder closed a month, so a month its funder never
    // closed held its address for good, whatever its expiry; a key that may fund one (the graduation
    // authority on v1) could squat a token's future months.
    let mut env = Env::new();
    let holders = env.v1_holders(2);
    env.fund_v1(&holders);
    let authority = env.graduation_authority.pubkey();
    let address = distributor_pda(&env.mint, MONTH);
    let expires_at = env.fetch::<Distributor>(&address).expires_at;
    let stranger = env.funded_wallet(SOL);

    env.set_time(expires_at - 1);
    let early = close_expired_ix(&env, &authority);
    expect_err(env.send_paid_by(&stranger, &[early], &[]), code(FanexError::NotExpired));
    // Only to the recorded funder.
    env.set_time(expires_at);
    let elsewhere = close_expired_ix(&env, &stranger.pubkey());
    expect_err(env.send_paid_by(&stranger, &[elsewhere], &[]), code(FanexError::Unauthorized));

    let held = env.lamports(&address);
    let funder_before = env.lamports(&authority);
    env.send_paid_by(&stranger, &[close_expired_ix(&env, &authority)], &[]).unwrap();
    assert!(!env.exists(&address));
    assert_eq!(env.lamports(&authority), funder_before + held);
    // The month's address is free again.
    env.fund_v1(&holders);
}

#[test]
fn anyone_closes_an_expired_v2_month_to_its_funder_and_nobody_before() {
    let mut env = Env::new();
    let month = env.v2_month(2);
    env.fund_v2(&month);
    let funder = env.creator.pubkey();
    let address = distributor_pda(&env.mint, MONTH);
    let expires_at = env.fetch::<DistributorV2>(&address).expires_at;
    let stranger = env.funded_wallet(SOL);

    env.set_time(expires_at - 1);
    let early = close_expired_v2_ix(&env, &funder);
    expect_err(env.send_paid_by(&stranger, &[early], &[]), code(FanexError::NotExpired));
    env.set_time(expires_at);
    // A v1 instruction on a v2 month is refused by its type.
    let wrong_type = close_expired_ix(&env, &funder);
    expect_err(env.send_paid_by(&stranger, &[wrong_type], &[]), ACCOUNT_DISCRIMINATOR_MISMATCH);

    let held = env.lamports(&address);
    let funder_before = env.lamports(&funder);
    env.send_paid_by(&stranger, &[close_expired_v2_ix(&env, &funder)], &[]).unwrap();
    assert!(!env.exists(&address));
    assert_eq!(env.lamports(&funder), funder_before + held);
}
