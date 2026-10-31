//! `init_distributor` (spec §4.4; track B's B6), on the config the harness writes.
//!
//! A month opens when one of Fanex's two funders or the curve's creator funds it with the
//! approver's signature, the config's dividend payer paying the three rents and no other key; its
//! µUSDC moves from the funder's own USDC ATA into the distributor's vault, its claims held for its
//! review window, and the funding records whether a Fanex key funded it. The curve is read only for
//! a creator, from a `Curve` or a `CurveUsdc` of their exact length (and a `CurveUsdc` of its own
//! layout version), and the legacy address is bound by its seeds under `pepper_curve`: a month the
//! real `pepper_curve.so` still holds blocks the month until that month's expiry, one it closed
//! does not, and lamports there block nothing. The record refuses a month open or closed before the
//! distributor's `init` would, and accepts a voided one. Every refusal is read by its code, with the
//! funder's USDC and the payer's lamports left as they were.

mod common;

use anchor_lang::error::ErrorCode;
use anchor_lang::solana_program::program_option::COption;
use anchor_spl::token::spl_token;
use common::*;
use pepper_dividends::constants::{
    CURVE_DISCRIMINATOR, CURVE_USDC_VERSION, MONTH_STATUS_CLOSED, MONTH_STATUS_OPEN, MONTH_STATUS_VOIDED, REVIEW_WINDOW_SECONDS,
};
use pepper_dividends::errors::DividendsError;
use pepper_dividends::events::UsdcDistributorFunded;
use pepper_dividends::state::{DividendsConfig, MonthRecord, UsdcDistributor};
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};

const TOTAL: u64 = 25 * USDC;

/// An env with the config written and `who` holding `TOTAL` µUSDC in its own USDC ATA.
fn env_funded_by(who: fn(&Env) -> Keypair) -> (Env, Keypair) {
    let mut env = Env::new();
    env.write_config();
    let funder = who(&env);
    let usdc = env.usdc_account(&funder.pubkey());
    env.mint_usdc(&usdc, TOTAL);
    (env, funder)
}

/// A root over `n` leaves of this month (their amounts need not sum to the total here).
fn root(env: &Env, n: usize) -> [u8; 32] {
    let distributor = distributor_pda(&env.mint, MONTH);
    build_tree(&dividend_leaves(&distributor, &env.usdc.mint, &holders(n))).root
}

/// What a refusal must leave as it was: the funder's USDC, the rent payer's lamports, the month's
/// three addresses.
struct Untouched {
    funder_usdc: u64,
    payer_lamports: u64,
    month: [Option<Vec<u8>>; 3],
}

fn month_accounts(env: &Env) -> [Option<Vec<u8>>; 3] {
    let distributor = distributor_pda(&env.mint, MONTH);
    [distributor, month_record_pda(&env.mint, MONTH), env.usdc_ata(&distributor)]
        .map(|address| env.svm.get_account(&address).filter(|a| a.lamports > 0).map(|a| a.data))
}

fn untouched(env: &Env, funder: &Pubkey) -> Untouched {
    Untouched { funder_usdc: env.token_amount(&env.usdc_ata(funder)), payer_lamports: env.lamports(&env.payer.pubkey()), month: month_accounts(env) }
}

fn assert_untouched(env: &Env, funder: &Pubkey, before: &Untouched) {
    assert_eq!(env.token_amount(&env.usdc_ata(funder)), before.funder_usdc, "the funder's USDC moved");
    assert_eq!(env.lamports(&env.payer.pubkey()), before.payer_lamports, "the payer's lamports moved");
    assert_eq!(month_accounts(env), before.month, "the month's accounts moved");
}

/// Sends a funding that must be refused with `code`, and checks it left everything as it was.
fn expect_refused(env: &mut Env, funding: &Funding, funder: &Keypair, code: u32) {
    let before = untouched(env, &funding.funder);
    let ix = env.init_distributor_ix(funding);
    let (payer, approver) = (env.payer.insecure_clone(), env.approver.insecure_clone());
    // A failed transaction still pays its fee: the payer's lamports are compared without it.
    let result = env.send_paid_by(&payer, &[ix], &[funder, &approver]);
    expect_err(result, code);
    let fee = FEE_PER_SIGNATURE * env.signatures(&[&payer.pubkey(), &funder.pubkey(), &approver.pubkey()]);
    let after = Untouched { payer_lamports: before.payer_lamports - fee, ..before };
    assert_untouched(env, &funding.funder, &after);
}

#[test]
fn a_pepper_funder_opens_a_month_paying_no_lamport_and_the_payer_pays_the_rents() {
    let (mut env, funder) = env_funded_by(|env| env.pepper_funder.insecure_clone());
    let funding = env.funding(&funder.pubkey(), root(&env, 5), TOTAL, 5);
    let (funder_before, payer_before) = (env.lamports(&funder.pubkey()), env.lamports(&env.payer.pubkey()));
    let sent = env.fund(&funding, &funder).unwrap();

    let distributor = distributor_pda(&env.mint, MONTH);
    let vault = env.usdc_ata(&distributor);
    assert_eq!(env.token_amount(&env.usdc_ata(&funder.pubkey())), 0);
    assert_eq!(env.token_amount(&vault), TOTAL);
    let d: UsdcDistributor = env.fetch(&distributor);
    assert_eq!((d.version, d.mint, d.month, d.merkle_root), (1, env.mint, MONTH, funding.merkle_root));
    assert_eq!((d.total_amount, d.claimed_amount, d.leaf_count, d.claimed.clone()), (TOTAL, 0, 5, vec![0]));
    assert_eq!((d.funder, d.funder_usdc_account, d.rent_payer), (funder.pubkey(), env.usdc_ata(&funder.pubkey()), env.payer.pubkey()));
    // A fresh month, its claims held for its review window, a day (the internal audit, 2026-10-29),
    // a Fanex key its funder and the dividend payer its rent payer, as its two flags record.
    assert_eq!((d.expires_at, d.claims_held_until, d.reserved), (funding.expires_at, env.now() + REVIEW_WINDOW_SECONDS, [0; 54]));
    assert_eq!((d.funded_by_fanex, d.rents_by_payer), (true, true));
    assert_eq!(d.bump, Pubkey::find_program_address(&[b"distributor", env.mint.as_ref(), &MONTH.to_le_bytes()], &pepper_dividends::ID).1);
    let record: MonthRecord = env.fetch(&month_record_pda(&env.mint, MONTH));
    assert_eq!((record.version, record.status, record.distributor, record.funded_at), (1, MONTH_STATUS_OPEN, distributor, env.now()));
    let event = event::<UsdcDistributorFunded>(&sent.logs);
    assert_eq!((event.mint, event.month, event.total_amount, event.funder), (env.mint, MONTH, TOTAL, funder.pubkey()));

    // The three rents, read from the bank, and the fee of three signatures: the payer's alone.
    let rents = env.rent(UsdcDistributor::space(5)) + env.rent(75) + env.rent(165);
    assert_eq!(payer_before - env.lamports(&env.payer.pubkey()), rents + 3 * FEE_PER_SIGNATURE);
    assert_eq!(env.lamports(&funder.pubkey()), funder_before);
    assert_eq!(env.lamports(&distributor) + env.lamports(&month_record_pda(&env.mint, MONTH)) + env.lamports(&vault), rents);
    println!("init_distributor, a Fanex funder and 5 leaves: {} compute units", sent.compute_units);
}

#[test]
fn the_dividend_payer_funds_a_month_of_its_own() {
    let (mut env, payer) = env_funded_by(|env| env.payer.insecure_clone());
    let funding = env.funding(&payer.pubkey(), root(&env, 3), TOTAL, 3);
    env.fund(&funding, &payer).unwrap();
    assert_eq!(env.token_amount(&env.usdc_ata(&distributor_pda(&env.mint, MONTH))), TOTAL);
    assert_eq!(env.fetch::<UsdcDistributor>(&distributor_pda(&env.mint, MONTH)).funder, payer.pubkey());
}

#[test]
fn a_creator_funds_from_a_curve_and_from_a_curve_usdc_and_pays_no_lamport() {
    // Each curve launched by the real pepper_curve, `create_curve` and `create_curve_usdc`.
    for usdc_curve in [false, true] {
        let (mut env, creator) = env_funded_by(|env| env.creator.insecure_clone());
        env.mint = if usdc_curve { env.launch_curve_usdc() } else { env.launch_curve() };
        let funding = env.funding(&creator.pubkey(), root(&env, 2), TOTAL, 2);
        let lamports = env.lamports(&creator.pubkey());
        env.fund(&funding, &creator).unwrap();
        assert_eq!(env.lamports(&creator.pubkey()), lamports, "a USDC curve: {usdc_curve}");
        assert_eq!(env.token_amount(&env.usdc_ata(&creator.pubkey())), 0);
        let d: UsdcDistributor = env.fetch(&distributor_pda(&env.mint, MONTH));
        assert_eq!(d.funder, creator.pubkey());
        // A creator's month: its rents the payer's, its funder no Fanex key, so no re-home ever
        // moves its leftovers.
        assert_eq!((d.funded_by_fanex, d.rents_by_payer, d.rent_payer), (false, true, env.payer.pubkey()));
    }
}

#[test]
fn the_rent_payer_is_the_configs_dividend_payer_and_no_other_key() {
    // The internal audit (2026-10-29): the rent payer was any signer, and the close pays the rents
    // back to whoever it was. It is the config's payer now, so every month's rents come back to a
    // key a re-home can move, a creator's month included.
    let funders: [fn(&Env) -> Keypair; 2] = [|env| env.pepper_funder.insecure_clone(), |env| env.creator.insecure_clone()];
    for who in funders {
        let (mut env, funder) = env_funded_by(who);
        let stranger = env.funded_wallet(SOL);
        for other in [stranger.insecure_clone(), env.approver.insecure_clone(), env.sponsor.insecure_clone(), funder.insecure_clone()] {
            let funding = Funding { rent_payer: other.pubkey(), ..env.funding(&funder.pubkey(), root(&env, 2), TOTAL, 2) };
            let before = untouched(&env, &funder.pubkey());
            let ix = env.init_distributor_ix(&funding);
            let approver = env.approver.insecure_clone();
            expect_err(env.send_paid_by(&other, &[ix], &[&funder, &approver]), code(DividendsError::Unauthorized));
            assert_untouched(&env, &funder.pubkey(), &before);
        }
        // The config's payer opens it.
        let funding = env.funding(&funder.pubkey(), root(&env, 2), TOTAL, 2);
        env.fund(&funding, &funder).unwrap();
        assert_eq!(env.fetch::<UsdcDistributor>(&distributor_pda(&env.mint, MONTH)).rent_payer, env.payer.pubkey());
    }
}

#[test]
fn a_curve_usdc_of_another_layout_version_is_unreadable_for_its_creator() {
    // The internal audit (2026-10-29): `CurveUsdc.version` was never read, so a later layout of the
    // same 354 bytes with another key at 46 would have been read as the creator.
    let (mut env, creator) = env_funded_by(|env| env.creator.insecure_clone());
    env.mint = env.launch_curve_usdc();
    let curve = curve_pda(&env.mint);
    let launched = env.svm.get_account(&curve).unwrap();
    assert_eq!(launched.data[8], CURVE_USDC_VERSION, "the real launch writes its layout version at 8");
    for version in [0u8, 2] {
        let mut data = launched.data.clone();
        data[8] = version;
        env.put(curve, data, pepper_curve::ID);
        let funding = env.funding(&creator.pubkey(), root(&env, 2), TOTAL, 2);
        expect_refused(&mut env, &funding, &creator, code(DividendsError::CurveUnreadable));
    }
    // Its own layout again: the creator funds.
    env.put(curve, launched.data, pepper_curve::ID);
    let funding = env.funding(&creator.pubkey(), root(&env, 2), TOTAL, 2);
    env.fund(&funding, &creator).unwrap();
}

#[test]
fn a_month_the_real_pepper_curve_left_open_blocks_this_one_until_its_expiry_in_v1_and_in_v2() {
    // The audit's squat (pepper_curve's dividends.rs:224): only a SOL month's funder closes it, so
    // one opened with a root of the opener's own blocked the same token's USDC month for as long as
    // that key chose. It blocks until its own expiry, to the second, closed or not.
    for v2 in [false, true] {
        let (mut env, funder) = env_funded_by(|env| env.pepper_funder.insecure_clone());
        let mint = env.mint;
        let holders = env.sol_holders(2);
        if v2 {
            env.open_v2(mint, MONTH, &holders).unwrap();
        } else {
            env.open_v1(mint, MONTH, &holders).unwrap();
        }
        let legacy = legacy_distributor_pda(&mint, MONTH);
        let expires_at = if v2 {
            env.fetch::<pepper_curve::state::DistributorV2>(&legacy).expires_at
        } else {
            env.fetch::<pepper_curve::state::Distributor>(&legacy).expires_at
        };
        env.set_time(expires_at - 1);
        let funding = env.funding(&funder.pubkey(), root(&env, 2), TOTAL, 2);
        expect_refused(&mut env, &funding, &funder, code(DividendsError::LegacyMonthLive));
        // At its expiry the SOL month is still there, unclosed, and this month opens.
        env.set_time(expires_at);
        assert!(env.data_len(&legacy) > 0, "v2 {v2}: the SOL month is still open");
        let funding = env.funding(&funder.pubkey(), root(&env, 2), TOTAL, 2);
        env.fund(&funding, &funder).unwrap();
        assert_eq!(env.token_amount(&env.usdc_ata(&distributor_pda(&mint, MONTH))), TOTAL, "v2 {v2}");
    }
}

#[test]
fn a_config_or_a_voided_record_of_another_layout_version_is_refused() {
    // The internal audit (2026-10-29): the program reads its own layout alone.
    let (mut env, funder) = env_funded_by(|env| env.pepper_funder.insecure_clone());
    let later = DividendsConfig { version: 2, ..env.dividends_config() };
    env.put_config(&later);
    let funding = env.funding(&funder.pubkey(), root(&env, 2), TOTAL, 2);
    expect_refused(&mut env, &funding, &funder, code(DividendsError::UnsupportedVersion));

    // A voided record of a layout this program does not know is not opened again.
    let (mut env, funder) = env_funded_by(|env| env.pepper_funder.insecure_clone());
    let (mint, distributor, now) = (env.mint, distributor_pda(&env.mint, MONTH), env.now());
    env.put_month_record(mint, MONTH, MONTH_STATUS_VOIDED, distributor, now - 30 * DAY);
    let record = month_record_pda(&mint, MONTH);
    let mut data = env.svm.get_account(&record).unwrap().data;
    data[8] = 2;
    env.put(record, data, pepper_dividends::ID);
    let funding = env.funding(&funder.pubkey(), root(&env, 2), TOTAL, 2);
    expect_refused(&mut env, &funding, &funder, code(DividendsError::UnsupportedVersion));
}

#[test]
fn the_approver_must_sign_and_be_the_configs() {
    let (mut env, funder) = env_funded_by(|env| env.pepper_funder.insecure_clone());
    let funding = env.funding(&funder.pubkey(), root(&env, 2), TOTAL, 2);
    // Named but not signing: Anchor's AccountNotSigner.
    let mut unsigned = env.init_distributor_ix(&funding);
    unsigned.accounts[1].is_signer = false;
    let payer = env.payer.insecure_clone();
    let before = untouched(&env, &funder.pubkey());
    expect_err(env.send_paid_by(&payer, &[unsigned], &[&funder]), anchor_code(ErrorCode::AccountNotSigner));
    let after = Untouched { payer_lamports: before.payer_lamports - 2 * FEE_PER_SIGNATURE, ..before };
    assert_untouched(&env, &funder.pubkey(), &after);
    // Another key signing as approver, the pauser among them.
    for other in [env.pauser.insecure_clone(), Keypair::new()] {
        let mut funding = funding.clone();
        funding.approver = other.pubkey();
        let ix = env.init_distributor_ix(&funding);
        let before = untouched(&env, &funder.pubkey());
        expect_err(env.send_paid_by(&payer, &[ix], &[&funder, &other]), code(DividendsError::Unauthorized));
        let after = Untouched { payer_lamports: before.payer_lamports - 3 * FEE_PER_SIGNATURE, ..before };
        assert_untouched(&env, &funder.pubkey(), &after);
    }
}

#[test]
fn a_stranger_is_not_a_funder() {
    let (mut env, stranger) = env_funded_by(|_| Keypair::new());
    let funding = env.funding(&stranger.pubkey(), root(&env, 2), TOTAL, 2);
    expect_refused(&mut env, &funding, &stranger, code(DividendsError::NotAFunder));
}

#[test]
fn anything_but_a_curve_of_its_exact_length_is_unreadable_for_a_creator() {
    let (mut env, creator) = env_funded_by(|env| env.creator.insecure_clone());
    let curve = curve_pda(&env.mint);
    let real = env.svm.get_account(&curve).unwrap();
    // A byte more, with Curve's discriminator; a byte less; another program's account at the
    // curve's address; and nothing there at all.
    let mut longer = real.data.clone();
    longer.push(0);
    assert_eq!(&longer[..8], &CURVE_DISCRIMINATOR);
    let cases: [(Vec<u8>, Pubkey); 3] =
        [(longer, pepper_curve::ID), (real.data[..real.data.len() - 1].to_vec(), pepper_curve::ID), (real.data.clone(), spl_token::ID)];
    for (data, owner) in cases {
        env.put(curve, data, owner);
        let funding = env.funding(&creator.pubkey(), root(&env, 2), TOTAL, 2);
        expect_refused(&mut env, &funding, &creator, code(DividendsError::CurveUnreadable));
    }
    env.svm.set_account(curve, solana_sdk::account::Account::default()).unwrap();
    let funding = env.funding(&creator.pubkey(), root(&env, 2), TOTAL, 2);
    expect_refused(&mut env, &funding, &creator, code(DividendsError::CurveUnreadable));
}

#[test]
fn another_mints_curve_and_another_months_or_mints_legacy_address_are_refused_by_their_seeds() {
    let (mut env, funder) = env_funded_by(|env| env.pepper_funder.insecure_clone());
    let other_mint = Pubkey::new_unique();
    let (mint, creator) = (other_mint, env.creator.pubkey());
    env.put_curve(mint, creator);
    let base = env.funding(&funder.pubkey(), root(&env, 2), TOTAL, 2);
    for funding in [
        Funding { curve: curve_pda(&other_mint), ..base.clone() },
        Funding { legacy_distributor: legacy_distributor_pda(&env.mint, MONTH + 1), ..base.clone() },
        Funding { legacy_distributor: legacy_distributor_pda(&other_mint, MONTH), ..base.clone() },
        // This program's own distributor address passed as the legacy one.
        Funding { legacy_distributor: distributor_pda(&env.mint, MONTH), ..base.clone() },
    ] {
        expect_refused(&mut env, &funding, &funder, anchor_code(ErrorCode::ConstraintSeeds));
    }
}

#[test]
fn a_month_the_real_pepper_curve_holds_blocks_it_in_v1_and_in_v2() {
    for v2 in [false, true] {
        let (mut env, funder) = env_funded_by(|env| env.pepper_funder.insecure_clone());
        let mint = env.mint;
        let holders = env.sol_holders(2);
        if v2 {
            env.open_v2(mint, MONTH, &holders).unwrap();
        } else {
            env.open_v1(mint, MONTH, &holders).unwrap();
        }
        let funding = env.funding(&funder.pubkey(), root(&env, 2), TOTAL, 2);
        expect_refused(&mut env, &funding, &funder, code(DividendsError::LegacyMonthLive));
        // Another month of the same token opens.
        let next = Funding { month: MONTH + 1, legacy_distributor: legacy_distributor_pda(&mint, MONTH + 1), ..funding };
        env.fund(&next, &funder).unwrap();
    }
}

#[test]
fn a_v2_month_pepper_curve_closed_and_lamports_alone_at_the_legacy_address_block_nothing() {
    let (mut env, funder) = env_funded_by(|env| env.pepper_funder.insecure_clone());
    let mint = env.mint;
    let holders = env.sol_holders(1);
    let tree = env.open_v2(mint, MONTH, &holders).unwrap();
    env.claim_v2(mint, MONTH, &holders[0].0, 0, holders[0].1, tree.proofs[0].clone()).unwrap();
    env.close_v2(mint, MONTH).unwrap();
    // Anyone may send lamports to the closed month's address.
    env.svm.airdrop(&legacy_distributor_pda(&mint, MONTH), SOL).unwrap();
    let funding = env.funding(&funder.pubkey(), root(&env, 2), TOTAL, 2);
    env.fund(&funding, &funder).unwrap();
    assert_eq!(env.token_amount(&env.usdc_ata(&distributor_pda(&mint, MONTH))), TOTAL);
}

#[test]
fn a_fanex_month_opens_for_a_mint_with_no_curve_and_a_creators_does_not() {
    let (mut env, funder) = env_funded_by(|env| env.pepper_funder.insecure_clone());
    env.mint = Pubkey::new_unique();
    assert!(!env.exists(&curve_pda(&env.mint)));
    let funding = env.funding(&funder.pubkey(), root(&env, 2), TOTAL, 2);
    env.fund(&funding, &funder).unwrap();

    let (mut env, creator) = env_funded_by(|env| env.creator.insecure_clone());
    env.mint = Pubkey::new_unique();
    let funding = env.funding(&creator.pubkey(), root(&env, 2), TOTAL, 2);
    expect_refused(&mut env, &funding, &creator, code(DividendsError::CurveUnreadable));
}

#[test]
fn the_expiry_is_held_between_364_and_400_days_to_the_second() {
    for (days, offset, refusal) in [
        (364, -1, Some(DividendsError::ExpiryTooSoon)),
        (364, 0, None),
        (400, 0, None),
        (400, 1, Some(DividendsError::ExpiryTooFar)),
    ] {
        let (mut env, funder) = env_funded_by(|env| env.pepper_funder.insecure_clone());
        let mut funding = env.funding(&funder.pubkey(), root(&env, 2), TOTAL, 2);
        funding.expires_at = env.now() + days * DAY + offset;
        match refusal {
            Some(error) => expect_refused(&mut env, &funding, &funder, code(error)),
            None => {
                env.fund(&funding, &funder).unwrap();
            }
        }
    }
}

#[test]
fn a_zero_total_and_a_leaf_count_out_of_range_are_refused() {
    let (mut env, funder) = env_funded_by(|env| env.pepper_funder.insecure_clone());
    let base = env.funding(&funder.pubkey(), root(&env, 2), TOTAL, 2);
    expect_refused(&mut env, &Funding { total_amount: 0, ..base.clone() }, &funder, code(DividendsError::ZeroAmount));
    for leaf_count in [0, 65_537] {
        expect_refused(&mut env, &Funding { leaf_count, ..base.clone() }, &funder, code(DividendsError::InvalidLeafCount));
    }
}

#[test]
fn another_usdc_mint_and_token_2022s_program_are_refused() {
    let (mut env, funder) = env_funded_by(|env| env.pepper_funder.insecure_clone());
    // A second classic mint of 6 decimals, the funder holding some of it in its own ATA.
    let other = env.second_mint();
    let base = env.funding(&funder.pubkey(), root(&env, 2), TOTAL, 2);
    let other_usdc = env.token_ata_of(&funder.pubkey(), &other);
    expect_refused(&mut env, &Funding { usdc_mint: other, funder_usdc: other_usdc, ..base.clone() }, &funder, code(DividendsError::AccountMismatch));
    expect_refused(&mut env, &Funding { token_program: anchor_spl::token_2022::ID, ..base }, &funder, anchor_code(ErrorCode::InvalidProgramId));
}

#[test]
fn a_funder_usdc_that_is_not_the_funders_ata_is_refused() {
    let (mut env, funder) = env_funded_by(|env| env.pepper_funder.insecure_clone());
    // Another USDC account the funder owns, holding the same amount, and another holder's ATA.
    let other = Pubkey::new_unique();
    let (usdc, owner) = (env.usdc.mint, funder.pubkey());
    env.put_token_account(other, usdc, owner, TOTAL, COption::None);
    let someone = Keypair::new();
    let someones = env.usdc_account(&someone.pubkey());
    env.mint_usdc(&someones, TOTAL);
    let base = env.funding(&funder.pubkey(), root(&env, 2), TOTAL, 2);
    for funder_usdc in [other, someones] {
        expect_refused(&mut env, &Funding { funder_usdc, ..base.clone() }, &funder, code(DividendsError::AccountMismatch));
    }
}

#[test]
fn a_vault_a_stranger_opened_first_is_taken_as_found() {
    // Empty, holding stray USDC, or only lamports at the vault's address: each opens the month, and
    // the stray USDC stays in the vault for the close to sweep.
    for stray in [None, Some(3 * USDC), Some(0)] {
        let (mut env, funder) = env_funded_by(|env| env.pepper_funder.insecure_clone());
        let vault = env.usdc_ata(&distributor_pda(&env.mint, MONTH));
        match stray {
            Some(0) => {
                env.svm.airdrop(&vault, 10_000_000).unwrap();
            }
            Some(amount) => {
                let opened = env.usdc_account(&distributor_pda(&env.mint, MONTH));
                env.mint_usdc(&opened, amount);
            }
            None => {
                env.usdc_account(&distributor_pda(&env.mint, MONTH));
            }
        }
        let funding = env.funding(&funder.pubkey(), root(&env, 2), TOTAL, 2);
        env.fund(&funding, &funder).unwrap();
        let expected = TOTAL + stray.filter(|a| *a > 0).unwrap_or(0);
        assert_eq!(env.token_amount(&vault), expected, "stray {stray:?}");
        assert_eq!(env.token_account(&vault).owner, distributor_pda(&env.mint, MONTH));
    }
}

#[test]
fn lamports_at_the_distributors_and_the_records_addresses_block_nothing() {
    let (mut env, funder) = env_funded_by(|env| env.pepper_funder.insecure_clone());
    for address in [distributor_pda(&env.mint, MONTH), month_record_pda(&env.mint, MONTH)] {
        env.svm.airdrop(&address, 1_000_000).unwrap();
    }
    let funding = env.funding(&funder.pubkey(), root(&env, 2), TOTAL, 2);
    env.fund(&funding, &funder).unwrap();
    assert_eq!(env.fetch::<MonthRecord>(&month_record_pda(&env.mint, MONTH)).status, MONTH_STATUS_OPEN);
    assert_eq!(env.data_len(&distributor_pda(&env.mint, MONTH)), UsdcDistributor::space(2));
}

#[test]
fn funding_paused_is_refused() {
    let (mut env, funder) = env_funded_by(|env| env.pepper_funder.insecure_clone());
    let paused = DividendsConfig { paused: true, ..env.dividends_config() };
    env.put_config(&paused);
    let funding = env.funding(&funder.pubkey(), root(&env, 2), TOTAL, 2);
    expect_refused(&mut env, &funding, &funder, code(DividendsError::Paused));
}

#[test]
fn a_month_open_or_closed_is_never_funded_twice_and_a_voided_one_is() {
    // Open: funded once, then again with the funder's USDC refilled.
    let (mut env, funder) = env_funded_by(|env| env.pepper_funder.insecure_clone());
    let funding = env.funding(&funder.pubkey(), root(&env, 2), TOTAL, 2);
    env.fund(&funding, &funder).unwrap();
    let usdc = env.usdc_ata(&funder.pubkey());
    env.mint_usdc(&usdc, TOTAL);
    expect_refused(&mut env, &funding, &funder, code(DividendsError::MonthAlreadyFunded));

    // Closed or voided, as a close or a void leaves it: the record alone, the distributor gone.
    for (status, opens) in [(MONTH_STATUS_CLOSED, false), (MONTH_STATUS_VOIDED, true)] {
        let (mut env, funder) = env_funded_by(|env| env.pepper_funder.insecure_clone());
        let (mint, distributor, now) = (env.mint, distributor_pda(&env.mint, MONTH), env.now());
        env.put_month_record(mint, MONTH, status, distributor, now - 30 * DAY);
        let funding = env.funding(&funder.pubkey(), root(&env, 2), TOTAL, 2);
        if opens {
            env.fund(&funding, &funder).unwrap();
            let record: MonthRecord = env.fetch(&month_record_pda(&mint, MONTH));
            assert_eq!((record.status, record.funded_at), (MONTH_STATUS_OPEN, env.now()));
        } else {
            expect_refused(&mut env, &funding, &funder, code(DividendsError::MonthAlreadyFunded));
        }
    }
}

#[test]
fn a_full_registry_of_65536_leaves_takes_8466_bytes() {
    let (mut env, funder) = env_funded_by(|env| env.pepper_funder.insecure_clone());
    let funding = env.funding(&funder.pubkey(), [9; 32], TOTAL, 65_536);
    let sent = env.fund(&funding, &funder).unwrap();
    let distributor = distributor_pda(&env.mint, MONTH);
    assert_eq!(env.data_len(&distributor), 8_466);
    assert_eq!(env.lamports(&distributor), env.rent(8_466));
    let d: UsdcDistributor = env.fetch(&distributor);
    assert_eq!((d.leaf_count, d.claimed.len()), (65_536, 8_192));
    assert!(d.claimed.iter().all(|byte| *byte == 0));
    println!("init_distributor, 65,536 leaves: {} compute units", sent.compute_units);
}
