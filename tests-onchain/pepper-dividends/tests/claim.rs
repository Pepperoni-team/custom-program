//! `claim` (spec §4.6; track B's B7), on months the harness writes into the bank.
//!
//! The claim stays as today: the holder's «Riscatta», the sponsor paying the fee. The program takes
//! it from any sender, the µUSDC reaching only the claimant's own USDC ATA, which a holder without
//! one creates in the same transaction, handing its close authority to the sponsor. Each refusal of
//! §4.6 is read by its code, the vault, the holder and the registry left as they were; the worst
//! case, the last leaf of a full registry with the account created, fits one legacy transaction.

mod common;

use anchor_lang::error::ErrorCode;
use anchor_lang::solana_program::program_option::COption;
use common::*;
use pepper_dividends::constants::REVIEW_WINDOW_SECONDS;
use pepper_dividends::errors::DividendsError;
use pepper_dividends::events::UsdcDividendClaimed;
use pepper_dividends::state::{DividendsConfig, UsdcDistributor};
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    instruction::Instruction,
    packet::PACKET_DATA_SIZE,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

/// An env with the config written and a month of `n` holders, each with a USDC account.
fn month_with_accounts(n: usize) -> (Env, Month) {
    let mut env = Env::new();
    env.write_config();
    let month = env.write_month(MONTH, n);
    for (holder, _) in &month.holders {
        env.usdc_account(&holder.pubkey());
    }
    (env, month)
}

/// What a refused claim must leave as it was.
fn snapshot(env: &Env, month: &Month) -> (u64, Vec<u8>) {
    (env.token_amount(&month.vault), env.svm.get_account(&month.distributor).unwrap().data)
}

fn send_by(env: &mut Env, payer: &Keypair, ixs: &[Instruction], extra: &[&Keypair]) -> Result<Sent, solana_sdk::transaction::TransactionError> {
    env.send_logged_paid_by(payer, ixs, extra)
}

#[test]
fn the_sponsor_alone_sends_a_claim_and_the_holder_is_paid_exactly() {
    let (mut env, month) = month_with_accounts(5);
    let sponsor = env.sponsor.insecure_clone();
    let (holder, amount, _) = month.leaf(3);
    let holder_key = holder.pubkey();
    let (holder_lamports, sponsor_lamports) = (env.lamports(&holder_key), env.lamports(&sponsor.pubkey()));
    let claim = env.claim_ix(&month, 3);
    let sent = send_by(&mut env, &sponsor, &[claim], &[]).unwrap();

    assert_eq!(env.token_amount(&env.usdc_ata(&holder_key)), amount);
    assert_eq!(env.token_amount(&month.vault), month.total - amount);
    let d: UsdcDistributor = env.fetch(&month.distributor);
    assert_eq!((d.claimed_amount, d.claimed.clone()), (amount, vec![0b0000_1000]));
    let event = event::<UsdcDividendClaimed>(&sent.logs);
    assert_eq!((event.mint, event.month, event.claimant, event.index, event.amount), (env.mint, MONTH, holder_key, 3, amount));
    // The holder pays nothing; the sponsor pays its one signature's fee and nothing else.
    assert_eq!(env.lamports(&holder_key), holder_lamports);
    assert_eq!(sponsor_lamports - env.lamports(&sponsor.pubkey()), FEE_PER_SIGNATURE);
}

#[test]
fn a_stranger_may_send_it_and_the_money_still_reaches_only_the_claimant() {
    let (mut env, month) = month_with_accounts(3);
    let stranger = env.funded_wallet(SOL);
    let stranger_usdc = env.usdc_account(&stranger.pubkey());
    let (holder, amount, _) = month.leaf(0);
    let holder_key = holder.pubkey();
    let claim = env.claim_ix(&month, 0);
    send_by(&mut env, &stranger, &[claim], &[]).unwrap();
    assert_eq!(env.token_amount(&env.usdc_ata(&holder_key)), amount);
    assert_eq!(env.token_amount(&stranger_usdc), 0);
}

#[test]
fn a_holder_without_a_usdc_account_creates_it_in_the_claims_own_transaction() {
    let mut env = Env::new();
    env.write_config();
    let month = env.write_month(MONTH, 4);
    let sponsor = env.sponsor.insecure_clone();
    let (holder, amount, _) = month.leaf(2);
    let holder = holder.insecure_clone();
    env.svm.airdrop(&holder.pubkey(), SOL).unwrap();
    let holder_lamports = env.lamports(&holder.pubkey());
    let [create, to_sponsor] = env.open_for_claim_ixs(&holder.pubkey());
    let claim = env.claim_ix(&month, 2);
    send_by(&mut env, &sponsor, &[create.clone(), to_sponsor.clone(), claim.clone()], &[&holder]).unwrap();

    let account = env.token_account(&env.usdc_ata(&holder.pubkey()));
    assert_eq!((account.owner, account.amount), (holder.pubkey(), amount));
    assert_eq!(account.close_authority, COption::Some(sponsor.pubkey()));
    // The sponsor fronted the account's rent: the holder paid no lamport.
    assert_eq!(env.lamports(&holder.pubkey()), holder_lamports);
    // Strict: the same three again, for another leaf of the same holder's month, fail on the
    // creation of an account that already exists, and nothing is claimed twice.
    let again = send_by(&mut env, &sponsor, &[create, to_sponsor, claim], &[&holder]);
    assert!(again.is_err());
    assert_eq!(env.token_amount(&env.usdc_ata(&holder.pubkey())), amount);
}

#[test]
fn the_last_leaf_of_a_full_registry_fits_one_legacy_transaction_with_its_account_created() {
    // 65,536 leaves: a proof of 16 hashes. The worst case the sponsor sends: the compute budget's two
    // instructions, the holder's strict account creation and its close authority, the claim; two
    // signers, the sponsor and the holder.
    let mut env = Env::new();
    env.write_config();
    let month = env.write_month(MONTH, 65_536);
    let sponsor = env.sponsor.insecure_clone();
    let (holder, amount, proof) = month.leaf(65_535);
    assert_eq!(proof.len(), 16);
    let holder = holder.insecure_clone();
    let [create, to_sponsor] = env.open_for_claim_ixs(&holder.pubkey());
    let ixs = [
        ComputeBudgetInstruction::set_compute_unit_limit(250_000),
        ComputeBudgetInstruction::set_compute_unit_price(50_000),
        create,
        to_sponsor,
        env.claim_ix(&month, 65_535),
    ];
    let tx = Transaction::new_signed_with_payer(&ixs, Some(&sponsor.pubkey()), &[&sponsor, &holder], env.svm.latest_blockhash());
    // The wire size: the signatures' count (one byte below 128), the signatures, the message.
    let size = 1 + 64 * tx.signatures.len() + tx.message.serialize().len();
    println!("the worst-case claim transaction: {size} bytes of {PACKET_DATA_SIZE}");
    assert!(size <= PACKET_DATA_SIZE, "{size} bytes");
    let sent = send_by(&mut env, &sponsor, &ixs, &[&holder]).unwrap();
    println!("claim, the last leaf of 65,536, its account created: {} compute units", sent.compute_units);
    assert_eq!(env.token_amount(&env.usdc_ata(&holder.pubkey())), amount);
    let d: UsdcDistributor = env.fetch(&month.distributor);
    assert_eq!((d.claimed[8_191], d.claimed.iter().filter(|b| **b != 0).count()), (0x80, 1));
}

#[test]
fn each_refusal_of_a_claim_is_read_by_its_code() {
    let (mut env, month) = month_with_accounts(4);
    let sponsor = env.sponsor.insecure_clone();
    let (holder, amount, proof) = month.leaf(1);
    let accounts = env.claim_accounts(&month, &holder.pubkey());
    let cases: [(u32, u64, Vec<[u8; 32]>, DividendsError); 4] = [
        (1, 0, proof.clone(), DividendsError::ZeroAmount),
        (4, amount, proof.clone(), DividendsError::InvalidLeafIndex),
        (1, amount + 1, proof.clone(), DividendsError::InvalidProof),
        // Leaf 1's proof under leaf 0's index.
        (0, amount, proof.clone(), DividendsError::InvalidProof),
    ];
    for (index, claimed, proof, refusal) in cases {
        let before = snapshot(&env, &month);
        let ix = env.claim_ix_with(&accounts, index, claimed, proof);
        expect_err(send_by(&mut env, &sponsor, &[ix], &[]), code(refusal));
        assert_eq!(snapshot(&env, &month), before, "{refusal:?}");
    }
    // Claimed once, then again: AlreadyClaimed, the holder paid once.
    let claim = env.claim_ix(&month, 1);
    send_by(&mut env, &sponsor, &[claim.clone()], &[]).unwrap();
    let before = snapshot(&env, &month);
    expect_err(send_by(&mut env, &sponsor, &[claim], &[]), code(DividendsError::AlreadyClaimed));
    assert_eq!(snapshot(&env, &month), before);
    assert_eq!(env.token_amount(&env.usdc_ata(&holder.pubkey())), amount);
}

#[test]
fn a_root_promising_more_than_the_month_holds_stops_at_its_total() {
    // A funding wrong by its total: the harness writes a month whose total is less than its leaves.
    let mut env = Env::new();
    env.write_config();
    let holders = holders(3);
    let month = env.write_month_of(MONTH, holders);
    let mut short: UsdcDistributor = env.fetch(&month.distributor);
    short.total_amount = month.holders[0].1 + month.holders[1].1 / 2;
    env.put_distributor(&short);
    for (holder, _) in &month.holders {
        env.usdc_account(&holder.pubkey());
    }
    let sponsor = env.sponsor.insecure_clone();
    let first = env.claim_ix(&month, 0);
    send_by(&mut env, &sponsor, &[first], &[]).unwrap();
    let second = env.claim_ix(&month, 1);
    expect_err(send_by(&mut env, &sponsor, &[second], &[]), code(DividendsError::ExceedsFundedTotal));
}

#[test]
fn another_owners_or_mints_account_and_another_months_vault_are_refused() {
    let (mut env, month) = month_with_accounts(3);
    let sponsor = env.sponsor.insecure_clone();
    let (holder, amount, proof) = month.leaf(0);
    let holder_key = holder.pubkey();
    let base = env.claim_accounts(&month, &holder_key);
    // Another holder's USDC account, the holder's own account of another mint, a USDC account of
    // the holder's that is not its ATA, another month's vault, another mint.
    let other_holder = env.usdc_ata(&month.holders[1].0.pubkey());
    let other_mint = env.second_mint();
    let holders_other = env.token_ata_of(&holder_key, &other_mint);
    let not_ata = Pubkey::new_unique();
    let (usdc, owner) = (env.usdc.mint, holder_key);
    env.put_token_account(not_ata, usdc, owner, 0, COption::None);
    let other = env.write_month(MONTH + 1, 2);
    // Another mint all through: the distributor's account of it, which anyone may open and fill,
    // and the holder's. Only the config's USDC moves: the mint's own check refuses it.
    let distributors_other = env.token_ata_of(&month.distributor, &other_mint);
    let cases: [(ClaimAccounts, u32); 6] = [
        (ClaimAccounts { claimant_usdc: other_holder, ..base.clone() }, anchor_code(ErrorCode::ConstraintTokenOwner)),
        (ClaimAccounts { claimant_usdc: holders_other, ..base.clone() }, anchor_code(ErrorCode::ConstraintAssociated)),
        (ClaimAccounts { claimant_usdc: not_ata, ..base.clone() }, anchor_code(ErrorCode::ConstraintAssociated)),
        (ClaimAccounts { vault: other.vault, ..base.clone() }, anchor_code(ErrorCode::ConstraintTokenOwner)),
        // Another mint with the USDC vault: the vault's own check catches it first.
        (ClaimAccounts { usdc_mint: other_mint, ..base.clone() }, anchor_code(ErrorCode::ConstraintAssociated)),
        (
            ClaimAccounts { usdc_mint: other_mint, vault: distributors_other, claimant_usdc: holders_other, ..base.clone() },
            code(DividendsError::AccountMismatch),
        ),
    ];
    for (accounts, refusal) in cases {
        let before = snapshot(&env, &month);
        let ix = env.claim_ix_with(&accounts, 0, amount, proof.clone());
        expect_err(send_by(&mut env, &sponsor, &[ix], &[]), refusal);
        assert_eq!(snapshot(&env, &month), before);
    }
    // A month's proof on another month of the same token proves nothing.
    let ix = env.claim_ix_with(&ClaimAccounts { distributor: other.distributor, vault: other.vault, ..base }, 0, amount, proof);
    expect_err(send_by(&mut env, &sponsor, &[ix], &[]), code(DividendsError::InvalidProof));
}

#[test]
fn claims_open_the_second_the_pause_ends_and_stay_open_past_the_expiry() {
    let (mut env, month) = month_with_accounts(3);
    let sponsor = env.sponsor.insecure_clone();
    let until = env.now() + 100;
    let paused = DividendsConfig { claims_paused_until: until, ..env.dividends_config() };
    env.put_config(&paused);
    env.set_time(until - 1);
    let claim = env.claim_ix(&month, 0);
    expect_err(send_by(&mut env, &sponsor, &[claim.clone()], &[]), code(DividendsError::ClaimsPaused));
    env.set_time(until);
    send_by(&mut env, &sponsor, &[claim], &[]).unwrap();
    // Past the month's expiry a claim still pays, until the month closes.
    let expires_at = env.fetch::<UsdcDistributor>(&month.distributor).expires_at;
    env.set_time(expires_at + 30 * DAY);
    let late = env.claim_ix(&month, 1);
    send_by(&mut env, &sponsor, &[late], &[]).unwrap();
    assert_eq!(env.token_amount(&env.usdc_ata(&month.holders[1].0.pubkey())), month.holders[1].1);
}

#[test]
fn a_month_funded_through_the_instruction_pays_its_first_claim_a_day_later_to_the_second() {
    // The internal audit (2026-10-29): claims opened at the funding's own slot, so the void, the
    // only fix for a wrong root or total, was a race. They open REVIEW_WINDOW_SECONDS later.
    let mut env = Env::new();
    env.init_config().unwrap();
    let (payer, sponsor) = (env.payer.insecure_clone(), env.sponsor.insecure_clone());
    let distributor = distributor_pda(&env.mint, MONTH);
    let holders = holders(2);
    let tree = build_tree(&dividend_leaves(&distributor, &env.usdc.mint, &holders));
    let total: u64 = holders.iter().map(|(_, amount)| amount).sum();
    let payer_usdc = env.usdc_account(&payer.pubkey());
    env.mint_usdc(&payer_usdc, total);
    let funded_at = env.now();
    let funding = env.funding(&payer.pubkey(), tree.root, total, 2);
    env.fund(&funding, &payer).unwrap();
    let month = Month { mint: env.mint, month: MONTH, distributor, vault: env.usdc_ata(&distributor), holders, tree, total };
    for (holder, _) in &month.holders {
        env.usdc_account(&holder.pubkey());
    }
    assert_eq!(REVIEW_WINDOW_SECONDS, DAY);
    let claim = env.claim_ix(&month, 0);
    for at in [funded_at, funded_at + REVIEW_WINDOW_SECONDS - 1] {
        env.set_time(at);
        let before = snapshot(&env, &month);
        expect_err(send_by(&mut env, &sponsor, &[claim.clone()], &[]), code(DividendsError::ClaimsHeld));
        assert_eq!(snapshot(&env, &month), before);
    }
    env.set_time(funded_at + REVIEW_WINDOW_SECONDS);
    send_by(&mut env, &sponsor, &[claim], &[]).unwrap();
    assert_eq!(env.token_amount(&env.usdc_ata(&month.holders[0].0.pubkey())), month.holders[0].1);
}

#[test]
fn a_config_or_a_month_of_another_layout_version_pays_no_claim() {
    // The internal audit (2026-10-29): version bytes were written and never read.
    for later_config in [true, false] {
        let (mut env, month) = month_with_accounts(2);
        let sponsor = env.sponsor.insecure_clone();
        if later_config {
            let config = DividendsConfig { version: 2, ..env.dividends_config() };
            env.put_config(&config);
        } else {
            let mut written: UsdcDistributor = env.fetch(&month.distributor);
            written.version = 2;
            env.put_distributor(&written);
        }
        let before = snapshot(&env, &month);
        let claim = env.claim_ix(&month, 0);
        expect_err(send_by(&mut env, &sponsor, &[claim], &[]), code(DividendsError::UnsupportedVersion));
        assert_eq!(snapshot(&env, &month), before, "a later config: {later_config}");
    }
}

#[test]
fn a_frozen_holder_account_or_vault_stops_the_claim_until_thawed() {
    let (mut env, month) = month_with_accounts(2);
    let sponsor = env.sponsor.insecure_clone();
    let holder_usdc = env.usdc_ata(&month.holders[0].0.pubkey());
    for frozen in [holder_usdc, month.vault] {
        env.freeze_usdc(&frozen);
        let before = snapshot(&env, &month);
        let claim = env.claim_ix(&month, 0);
        expect_err(send_by(&mut env, &sponsor, &[claim], &[]), SPL_TOKEN_ACCOUNT_FROZEN);
        assert_eq!(snapshot(&env, &month), before);
        env.thaw_usdc(&frozen);
    }
    let claim = env.claim_ix(&month, 0);
    send_by(&mut env, &sponsor, &[claim], &[]).unwrap();
}

#[test]
fn a_whole_month_claimed_leaf_by_leaf_and_the_compute_units_over_64_claimants() {
    let (mut env, month) = month_with_accounts(64);
    let sponsor = env.sponsor.insecure_clone();
    let mut most = 0;
    for index in 0..64 {
        let claim = env.claim_ix(&month, index);
        let sent = send_by(&mut env, &sponsor, &[claim], &[]).unwrap();
        most = most.max(sent.compute_units);
        let (holder, amount, _) = month.leaf(index);
        assert_eq!(env.token_amount(&env.usdc_ata(&holder.pubkey())), amount);
    }
    let d: UsdcDistributor = env.fetch(&month.distributor);
    assert_eq!((d.claimed_amount, d.claimed.clone()), (month.total, vec![0xff; 8]));
    assert_eq!(env.token_amount(&month.vault), 0);
    println!("claim, the most over 64 claimants: {most} compute units");
    assert!(most < 200_000);
}
