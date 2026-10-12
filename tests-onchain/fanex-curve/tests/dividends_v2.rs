mod common;

use common::*;
use fanex_curve::constants::MAX_DISTRIBUTOR_V2_LEAVES;
use fanex_curve::errors::FanexError;
use fanex_curve::state::DistributorV2;
use solana_sdk::signature::{Keypair, Signer};

#[test]
fn a_v2_month_is_funded_with_an_empty_registry_sized_to_its_leaves() {
    let mut env = Env::new();
    let month = env.v2_month(11);
    env.fund_v2(&month);

    let address = distributor_pda(&env.mint, MONTH);
    let d: DistributorV2 = env.fetch(&address);
    assert_eq!(d.leaf_count, 11);
    assert_eq!(d.claimed, vec![0u8; 2]);
    assert_eq!(d.total_amount, month.total);
    assert_eq!(d.claimed_amount, 0);
    assert_eq!(d.funder, env.admin.pubkey());

    let account = env.svm.get_account(&address).unwrap();
    assert_eq!(account.data.len(), DistributorV2::space(11));
    let rent = env.svm.minimum_balance_for_rent_exemption(account.data.len());
    assert_eq!(account.lamports, rent + month.total);
}

#[test]
fn a_claim_pays_exactly_its_leaf_and_sets_only_its_bit() {
    let mut env = Env::new();
    let month = env.v2_month(11);
    env.fund_v2(&month);
    let (holder, amount) = (&month.holders[9], month.holders[9].1);
    let before = env.lamports(&holder.0.pubkey());

    env.claim_v2(&holder.0, 9, amount, month.proof(9), false).unwrap();

    assert_eq!(env.lamports(&holder.0.pubkey()), before + amount);
    let d: DistributorV2 = env.fetch(&distributor_pda(&env.mint, MONTH));
    assert_eq!(d.claimed_amount, amount);
    assert_eq!(d.claimed, vec![0, 0b0000_0010]);
}

#[test]
fn a_claim_creates_no_account_and_costs_the_sponsor_the_fee_alone() {
    let mut env = Env::new();
    let month = env.v2_month(3);
    env.fund_v2(&month);
    let holder = &month.holders[0];
    let sponsor_before = env.lamports(&env.admin.pubkey());

    env.claim_v2(&holder.0, 0, holder.1, month.proof(0), true).unwrap();

    let distributor = distributor_pda(&env.mint, MONTH);
    assert!(!env.exists(&claim_status_pda(&distributor, &holder.0.pubkey())));
    let spent = sponsor_before - env.lamports(&env.admin.pubkey());
    assert!(spent < 100_000, "the sponsor paid {spent} lamports for one v2 claim");
}

#[test]
fn a_v1_claim_still_costs_the_sponsor_its_deposit() {
    let mut env = Env::new();
    let holders: Vec<(Keypair, u64)> = (0..3)
        .map(|i| {
            let kp = Keypair::new();
            env.svm.airdrop(&kp.pubkey(), SOL).unwrap();
            (kp, 10_000_000 + i * 1_000)
        })
        .collect();
    let tree = env.fund_v1(&holders);
    let sponsor_before = env.lamports(&env.admin.pubkey());

    env.claim_v1(&holders[0].0, holders[0].1, tree.proofs[0].clone()).unwrap();

    let distributor = distributor_pda(&env.mint, MONTH);
    let deposit = env.lamports(&claim_status_pda(&distributor, &holders[0].0.pubkey()));
    assert!(deposit > 0);
    assert!(sponsor_before - env.lamports(&env.admin.pubkey()) >= deposit);
}

#[test]
fn a_claim_through_the_transit_reaches_the_holder_and_leaves_the_transit_empty() {
    let mut env = Env::new();
    let month = env.v2_month(4);
    env.fund_v2(&month);
    let holder = &month.holders[2];
    let before = env.lamports(&holder.0.pubkey());

    env.claim_v2(&holder.0, 2, holder.1, month.proof(2), true).unwrap();

    assert_eq!(env.lamports(&holder.0.pubkey()), before + holder.1);
    assert_eq!(env.lamports(&transit_pda(&distributor_pda(&env.mint, MONTH))), 0);
}

#[test]
fn a_second_claim_of_the_same_leaf_is_refused() {
    let mut env = Env::new();
    let month = env.v2_month(4);
    env.fund_v2(&month);
    let holder = &month.holders[1];
    env.claim_v2(&holder.0, 1, holder.1, month.proof(1), true).unwrap();

    expect_err(env.claim_v2(&holder.0, 1, holder.1, month.proof(1), true), code(FanexError::AlreadyClaimed));
}

#[test]
fn a_holder_cannot_claim_under_another_leafs_index() {
    let mut env = Env::new();
    let month = env.v2_month(4);
    env.fund_v2(&month);
    let holder = &month.holders[1];

    expect_err(env.claim_v2(&holder.0, 2, holder.1, month.proof(1), false), code(FanexError::InvalidProof));
}

#[test]
fn a_leaf_claims_only_for_the_wallet_it_names() {
    let mut env = Env::new();
    let month = env.v2_month(4);
    env.fund_v2(&month);
    let thief = Keypair::new();
    env.svm.airdrop(&thief.pubkey(), SOL).unwrap();

    expect_err(env.claim_v2(&thief, 0, month.holders[0].1, month.proof(0), false), code(FanexError::InvalidProof));
}

#[test]
fn an_index_outside_the_registry_is_refused() {
    let mut env = Env::new();
    let month = env.v2_month(4);
    env.fund_v2(&month);
    let holder = &month.holders[0];

    expect_err(env.claim_v2(&holder.0, 4, holder.1, month.proof(0), false), code(FanexError::InvalidLeafIndex));
}

#[test]
fn the_registry_refuses_an_empty_or_oversized_month() {
    let mut env = Env::new();
    let admin = env.admin.insecure_clone();
    let expires = OCT_5_2026 + 365 * DAY;
    expect_err(env.init_v2(&admin, [1; 32], SOL, expires, 0), code(FanexError::InvalidLeafCount));
    expect_err(
        env.init_v2(&admin, [1; 32], SOL, expires, MAX_DISTRIBUTOR_V2_LEAVES + 1),
        code(FanexError::InvalidLeafCount),
    );
    expect_err(env.init_v2(&admin, [1; 32], SOL, expires, u32::MAX), code(FanexError::InvalidLeafCount));
}

#[test]
fn a_full_registry_fits_an_account_and_its_last_leaf_claims() {
    let mut env = Env::new();
    let n = MAX_DISTRIBUTOR_V2_LEAVES as usize;
    let holder = Keypair::new();
    env.svm.airdrop(&holder.pubkey(), SOL).unwrap();
    let mut leaves: Vec<[u8; 32]> = (0..n - 1)
        .map(|i| fanex_curve::math::dividend_leaf_v2(i as u32, &solana_sdk::pubkey::Pubkey::new_unique(), 1))
        .collect();
    let amount = 5_000_000;
    leaves.push(fanex_curve::math::dividend_leaf_v2((n - 1) as u32, &holder.pubkey(), amount));
    let tree = build_tree(&leaves);
    let admin = env.admin.insecure_clone();
    env.init_v2(&admin, tree.root, (n as u64 - 1) + amount, OCT_5_2026 + 365 * DAY, n as u32).unwrap();

    env.claim_v2(&holder, (n - 1) as u32, amount, tree.proofs[n - 1].clone(), true).unwrap();

    let d: DistributorV2 = env.fetch(&distributor_pda(&env.mint, MONTH));
    assert_eq!(d.claimed.len(), 8_192);
    assert_eq!(d.claimed[8_191], 0b1000_0000);
}

#[test]
fn the_two_versions_never_take_each_others_accounts() {
    let mut env = Env::new();
    let month = env.v2_month(3);
    env.fund_v2(&month);
    let holder = &month.holders[0];
    expect_err(env.claim_v1(&holder.0, holder.1, month.proof(0)), ACCOUNT_DISCRIMINATOR_MISMATCH);

    let mut v1 = Env::new();
    let holders: Vec<(Keypair, u64)> = (0..2)
        .map(|i| {
            let kp = Keypair::new();
            v1.svm.airdrop(&kp.pubkey(), SOL).unwrap();
            (kp, 10_000_000 + i * 1_000)
        })
        .collect();
    let tree = v1.fund_v1(&holders);
    expect_err(
        v1.claim_v2(&holders[0].0, 0, holders[0].1, tree.proofs[0].clone(), false),
        ACCOUNT_DISCRIMINATOR_MISMATCH,
    );
    let admin = v1.admin.insecure_clone();
    assert!(v1.init_v2(&admin, [1; 32], SOL, OCT_5_2026 + 365 * DAY, 2).is_err());
}

#[test]
fn a_fully_claimed_month_closes_early_and_returns_everything_to_its_funder() {
    let mut env = Env::new();
    let month = env.v2_month(5);
    env.fund_v2(&month);
    for (i, (kp, amount)) in month.holders.iter().enumerate() {
        let kp = kp.insecure_clone();
        env.claim_v2(&kp, i as u32, *amount, month.proof(i), true).unwrap();
    }
    let distributor = distributor_pda(&env.mint, MONTH);
    let held = env.lamports(&distributor);
    let funder_before = env.lamports(&env.admin.pubkey());

    let admin = env.admin.insecure_clone();
    env.close_v2(&admin).unwrap();

    assert!(!env.exists(&distributor));
    assert!(env.lamports(&env.admin.pubkey()) + 10_000 >= funder_before + held);
}

#[test]
fn a_partially_claimed_month_waits_for_its_expiry() {
    let mut env = Env::new();
    let month = env.v2_month(3);
    env.fund_v2(&month);
    let holder = &month.holders[0];
    env.claim_v2(&holder.0, 0, holder.1, month.proof(0), true).unwrap();
    let admin = env.admin.insecure_clone();

    expect_err(env.close_v2(&admin), code(FanexError::NotExpired));

    env.set_time(OCT_5_2026 + 365 * DAY);
    env.close_v2(&admin).unwrap();
    assert!(!env.exists(&distributor_pda(&env.mint, MONTH)));
}

#[test]
fn only_the_funder_closes_a_month() {
    let mut env = Env::new();
    let month = env.v2_month(1);
    env.fund_v2(&month);
    let holder = &month.holders[0];
    env.claim_v2(&holder.0, 0, holder.1, month.proof(0), true).unwrap();
    let stranger = Keypair::new();
    env.svm.airdrop(&stranger.pubkey(), SOL).unwrap();

    expect_err(env.close_v2(&stranger), code(FanexError::Unauthorized));
}

#[test]
fn only_fanex_or_the_creator_funds_a_month() {
    let mut env = Env::new();
    let month = env.v2_month(2);
    let stranger = Keypair::new();
    env.svm.airdrop(&stranger.pubkey(), 100 * SOL).unwrap();
    let expires = OCT_5_2026 + 365 * DAY;

    expect_err(env.init_v2(&stranger, month.tree.root, month.total, expires, 2), code(FanexError::Unauthorized));

    let creator = env.creator.insecure_clone();
    env.init_v2(&creator, month.tree.root, month.total, expires, 2).unwrap();
    let d: DistributorV2 = env.fetch(&distributor_pda(&env.mint, MONTH));
    assert_eq!(d.funder, creator.pubkey());
}

#[test]
fn the_global_pause_stops_funding_but_never_a_claim() {
    let mut env = Env::new();
    let month = env.v2_month(2);
    env.fund_v2(&month);
    env.set_paused(true);

    let holder = &month.holders[1];
    env.claim_v2(&holder.0, 1, holder.1, month.proof(1), true).unwrap();

    let mut other = Env::new();
    let next = other.v2_month(2);
    other.set_paused(true);
    let admin = other.admin.insecure_clone();
    expect_err(
        other.init_v2(&admin, next.tree.root, next.total, OCT_5_2026 + 365 * DAY, 2),
        code(FanexError::GloballyPaused),
    );
}
