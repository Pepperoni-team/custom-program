//! The harness itself, before any instruction of `pepper_dividends` exists: both
//! programs load and run under the upgradeable loader, the writers store what the
//! crate reads back, the test USDC behaves as Circle's, `pepper_curve`'s real
//! launches and months leave at the curve's and the legacy address what the
//! program's rules read, and `pepper_curve`'s own layout is the one `constants.rs`
//! pins, so a layout move on its side fails this suite.

mod common;

use anchor_lang::solana_program::bpf_loader_upgradeable;
use anchor_lang::solana_program::program_option::COption;
use anchor_spl::token::spl_token;
use common::*;
use pepper_dividends::constants::{
    CURVE_CREATOR_OFFSET, CURVE_DISCRIMINATOR, CURVE_LEN, CURVE_USDC_CREATOR_OFFSET, CURVE_USDC_DISCRIMINATOR, CURVE_USDC_LEN,
    CURVE_USDC_VERSION, LEGACY_DISTRIBUTOR_DISCRIMINATOR, LEGACY_DISTRIBUTOR_V2_DISCRIMINATOR, LEGACY_EXPIRES_AT_OFFSET,
    MONTH_STATUS_OPEN,
};
use pepper_dividends::state::{DividendsConfig, MonthRecord, UsdcDistributor};
use pepper_dividends::validation::{curve_creator, legacy_month_expiry, legacy_month_live};
use solana_sdk::{
    instruction::Instruction,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};

#[test]
fn anchors_codes_the_suite_pins_are_its_own() {
    use anchor_lang::error::ErrorCode;
    assert_eq!(anchor_code(ErrorCode::InstructionFallbackNotFound), 101);
    assert_eq!(anchor_code(ErrorCode::ConstraintSeeds), 2006);
    assert_eq!(anchor_code(ErrorCode::AccountDiscriminatorMismatch), 3002);
    assert_eq!(anchor_code(ErrorCode::InvalidProgramId), 3008);
    assert_eq!(anchor_code(ErrorCode::AccountNotSigner), 3010);
    assert_eq!(code(pepper_dividends::errors::DividendsError::Paused), 6000);
}

#[test]
fn both_programs_run_under_the_upgradeable_loader() {
    let mut env = Env::new();
    for (program, authority) in [(pepper_dividends::ID, env.admin.pubkey()), (pepper_curve::ID, env.curve_admin.pubkey())] {
        let account = env.svm.get_account(&program).expect("the program account");
        assert!(account.executable);
        assert_eq!(account.owner, bpf_loader_upgradeable::id());
        // The Program account points at its ProgramData, which records the authority.
        assert_eq!(&account.data[4..36], program_data_pda(&program).as_ref());
        let program_data = env.svm.get_account(&program_data_pda(&program)).expect("the ProgramData");
        assert_eq!(program_data.data[12], 1, "an upgrade authority is recorded");
        assert_eq!(&program_data.data[13..45], authority.as_ref());
    }
    // The program answers: a discriminator none of its instructions has is Anchor's
    // InstructionFallbackNotFound, raised by this program's own code.
    let unknown = Instruction { program_id: pepper_dividends::ID, accounts: vec![], data: vec![0; 8] };
    let sponsor = env.sponsor.insecure_clone();
    expect_err(env.send_paid_by(&sponsor, &[unknown], &[]), 101);
}

#[test]
fn the_test_usdc_has_six_decimals_and_a_freeze_authority() {
    let mut env = Env::new();
    let mint = env.usdc_mint_account();
    assert_eq!(mint.decimals, 6);
    assert_eq!(mint.freeze_authority, COption::Some(env.usdc.freeze_authority.pubkey()));
    // A frozen account moves nothing until it is thawed: SPL Token's AccountFrozen (17).
    let holder = Keypair::new();
    let from = env.usdc_account(&holder.pubkey());
    let to = env.usdc_account(&Pubkey::new_unique());
    env.mint_usdc(&from, 5 * USDC);
    env.freeze_usdc(&from);
    let usdc_mint = env.usdc.mint;
    let transfer =
        spl_token::instruction::transfer_checked(&spl_token::ID, &from, &usdc_mint, &to, &holder.pubkey(), &[], USDC, 6).unwrap();
    let sponsor = env.sponsor.insecure_clone();
    expect_err(env.send_paid_by(&sponsor, &[transfer.clone()], &[&holder]), 17);
    env.thaw_usdc(&from);
    env.send_paid_by(&sponsor, &[transfer], &[&holder]).unwrap();
    assert_eq!((env.token_amount(&from), env.token_amount(&to)), (4 * USDC, USDC));
}

#[test]
fn the_writers_store_what_the_crate_reads_back() {
    let mut env = Env::new();
    let config = env.write_config();
    let read: DividendsConfig = env.fetch(&config_pda());
    assert_eq!((read.admin, read.approver, read.usdc_mint, read.curve_program), (config.admin, config.approver, env.usdc.mint, pepper_curve::ID));
    assert_eq!((read.claims_paused_until, read.bump), (0, config.bump));
    assert_eq!(env.data_len(&config_pda()), 8 + 395);

    // A month with a claim recorded: any amounts and bitmap.
    let funder = env.payer.pubkey();
    let mut month = env.usdc_distributor(env.mint, MONTH, funder, [7; 32], 10 * USDC, 14);
    month.claimed_amount = 3 * USDC;
    month.claimed[1] = 0x20;
    let address = env.put_funded_month(&month);
    assert_eq!(address, distributor_pda(&env.mint, MONTH));
    assert_eq!(env.data_len(&address), UsdcDistributor::space(14));
    let read: UsdcDistributor = env.fetch(&address);
    assert_eq!((read.total_amount, read.claimed_amount, read.claimed.clone()), (10 * USDC, 3 * USDC, vec![0, 0x20]));
    assert_eq!((read.funder_usdc_account, read.rent_payer), (env.usdc_ata(&funder), env.payer.pubkey()));
    let record: MonthRecord = env.fetch(&month_record_pda(&env.mint, MONTH));
    assert_eq!((record.status, record.distributor, record.funded_at), (MONTH_STATUS_OPEN, address, env.now()));
    // The vault: the distributor's associated USDC account, what is still unclaimed.
    let vault = env.token_account(&env.usdc_ata(&address));
    assert_eq!((vault.mint, vault.owner, vault.amount), (env.usdc.mint, address, 7 * USDC));
    assert_eq!(env.usdc_mint_account().supply, 7 * USDC);
    // Each at the bank's rent for its size.
    assert_eq!(env.lamports(&address), env.rent(UsdcDistributor::space(14)));
}

#[test]
fn the_creator_reads_from_a_written_curve_and_from_both_curves_the_real_program_launches() {
    let mut env = Env::new();
    // `Env::new` wrote a SOL `Curve` for `mint` through pepper_curve's crate.
    let curve = env.svm.get_account(&curve_pda(&env.mint)).expect("the curve");
    assert_eq!((curve.owner, curve.data.len()), (pepper_curve::ID, CURVE_LEN));
    assert_eq!(curve_creator(&curve.owner, &pepper_curve::ID, &curve.data).unwrap(), env.creator.pubkey());
    // The crate reads it back as a `Curve`.
    let decoded: pepper_curve::state::Curve = env.fetch(&curve_pda(&env.mint));
    assert_eq!((decoded.mint, decoded.creator), (env.mint, env.creator.pubkey()));

    // A `Curve` from `create_curve` and a `CurveUsdc` from `create_curve_usdc`, each read where
    // `constants.rs` says and by the crate.
    let sol_mint = env.launch_curve();
    let usdc_mint_token = env.launch_curve_usdc();
    for (mint, len, discriminator, offset) in [
        (sol_mint, CURVE_LEN, CURVE_DISCRIMINATOR, CURVE_CREATOR_OFFSET),
        (usdc_mint_token, CURVE_USDC_LEN, CURVE_USDC_DISCRIMINATOR, CURVE_USDC_CREATOR_OFFSET),
    ] {
        let curve = env.svm.get_account(&curve_pda(&mint)).expect("the launched curve");
        assert_eq!((curve.owner, curve.data.len()), (pepper_curve::ID, len));
        assert_eq!(&curve.data[..8], &discriminator);
        assert_eq!(&curve.data[offset..offset + 32], env.creator.pubkey().as_ref());
        assert_eq!(curve_creator(&curve.owner, &pepper_curve::ID, &curve.data).unwrap(), env.creator.pubkey());
    }
    let launched: pepper_curve::state::Curve = env.fetch(&curve_pda(&sol_mint));
    assert_eq!((launched.mint, launched.creator), (sol_mint, env.creator.pubkey()));
    let launched: pepper_curve::state::CurveUsdc = env.fetch(&curve_pda(&usdc_mint_token));
    assert_eq!((launched.mint, launched.creator, launched.version), (usdc_mint_token, env.creator.pubkey(), 1));
}

#[test]
fn pepper_curves_own_sizes_offsets_and_discriminators_are_the_ones_constants_rs_pins() {
    use anchor_lang::{Discriminator, Space};
    use pepper_curve::state::{Curve, CurvePhase, CurveUsdc};
    // From pepper_curve's crate: the sizes and the discriminators.
    assert_eq!(8 + Curve::INIT_SPACE, CURVE_LEN);
    assert_eq!(8 + CurveUsdc::INIT_SPACE, CURVE_USDC_LEN);
    assert_eq!(Curve::DISCRIMINATOR, &CURVE_DISCRIMINATOR[..]);
    assert_eq!(CurveUsdc::DISCRIMINATOR, &CURVE_USDC_DISCRIMINATOR[..]);
    // And the creator's offset, from each account as the crate serializes it, every other key apart.
    let (mint, creator, other) = (Pubkey::new_from_array([0x11; 32]), Pubkey::new_from_array([0xC7; 32]), Pubkey::new_from_array([0x22; 32]));
    let curve = Curve {
        mint,
        creator,
        phase: CurvePhase::Bonding,
        creator_target_lamports: 1,
        cpmm_seed_lamports: 2,
        creator_split_bps: 3,
        fanex_split_bps: 4,
        creator_cap_lamports: 5,
        fanex_cap_lamports: 6,
        virtual_sol_reserves: 7,
        virtual_token_reserves: 8,
        real_sol_reserves: 9,
        real_token_reserves: 10,
        migration_token_reserves: 11,
        creator_received_lamports: 12,
        fanex_received_lamports: 13,
        paused: false,
        created_at: 14,
        graduated_at: 15,
        bump: 16,
        sol_vault_bump: 17,
    };
    let curve_usdc = CurveUsdc {
        version: 1,
        bump: 2,
        usdc_vault_bump: 3,
        phase: 0,
        paused: false,
        migrated: false,
        mint,
        creator,
        creator_usdc_account: other,
        creator_split_bps: 4,
        fanex_split_bps: 5,
        creator_target_usdc: 6,
        graduation_threshold_usdc: 7,
        creator_cap_usdc: 8,
        fanex_cap_usdc: 9,
        virtual_usdc_reserves: 10,
        virtual_token_reserves: 11,
        real_usdc_reserves: 12,
        real_token_reserves: 13,
        migration_token_reserves: 14,
        creator_received_usdc: 15,
        fanex_received_usdc: 16,
        created_at: 17,
        graduated_at: 18,
        migrated_at: 19,
        reserved: [0; 128],
    };
    for (data, len, offset) in [
        (account_data(&curve, Curve::INIT_SPACE), CURVE_LEN, CURVE_CREATOR_OFFSET),
        (account_data(&curve_usdc, CurveUsdc::INIT_SPACE), CURVE_USDC_LEN, CURVE_USDC_CREATOR_OFFSET),
    ] {
        assert_eq!(data.len(), len);
        assert_eq!(data.windows(32).position(|w| w == creator.as_ref()), Some(offset));
        assert_eq!(curve_creator(&pepper_curve::ID, &pepper_curve::ID, &data).unwrap(), creator);
    }
    // The layout version the creator read takes is pepper_curve's, its first field, at byte 8.
    assert_eq!(CURVE_USDC_VERSION, pepper_curve::constants::USDC_ACCOUNT_VERSION);
    assert_eq!(account_data(&curve_usdc, CurveUsdc::INIT_SPACE)[8], CURVE_USDC_VERSION);
}

#[test]
fn pepper_curves_own_month_discriminators_and_expiry_offset_are_the_ones_constants_rs_pins() {
    use anchor_lang::{AccountSerialize, Discriminator};
    use pepper_curve::state::{Distributor, DistributorV2};
    assert_eq!(Distributor::DISCRIMINATOR, &LEGACY_DISTRIBUTOR_DISCRIMINATOR[..]);
    assert_eq!(DistributorV2::DISCRIMINATOR, &LEGACY_DISTRIBUTOR_V2_DISCRIMINATOR[..]);
    // The expiry, from each month as the crate serializes it, every other field apart from it.
    let expires_at: i64 = 0x0A0B_0C0D_0E0F_1011;
    let (mint, funder) = (Pubkey::new_from_array([0x11; 32]), Pubkey::new_from_array([0x22; 32]));
    let v1 = Distributor { mint, month: MONTH, merkle_root: [0x33; 32], total_amount: 1, claimed_amount: 2, funder, expires_at, bump: 3 };
    let v2 = DistributorV2 {
        mint,
        month: MONTH,
        merkle_root: [0x33; 32],
        total_amount: 1,
        claimed_amount: 2,
        funder,
        expires_at,
        bump: 3,
        leaf_count: 9,
        claimed: vec![0; 2],
    };
    let mut v1_bytes = Vec::new();
    v1.try_serialize(&mut v1_bytes).unwrap();
    let mut v2_bytes = Vec::new();
    v2.try_serialize(&mut v2_bytes).unwrap();
    assert_eq!(v1_bytes.len(), 137);
    for bytes in [&v1_bytes, &v2_bytes] {
        assert_eq!(bytes.windows(8).position(|w| w == expires_at.to_le_bytes()), Some(LEGACY_EXPIRES_AT_OFFSET));
        assert_eq!(legacy_month_expiry(&pepper_curve::ID, &pepper_curve::ID, bytes), Some(expires_at));
    }
}

/// Whether the legacy address holds a month that blocks this program's, as `init_distributor`
/// reads it: its owner, its bytes and the clock.
fn legacy_live(env: &Env, legacy: &Pubkey) -> bool {
    let account = env.svm.get_account(legacy).unwrap_or_default();
    legacy_month_live(&account.owner, &pepper_curve::ID, &account.data, env.now())
}

#[test]
fn a_v2_month_of_the_real_pepper_curve_is_live_until_it_closes() {
    let mut env = Env::new();
    let mint = env.mint;
    let holders = env.sol_holders(3);
    let tree = env.open_v2(mint, MONTH, &holders).unwrap();
    let legacy = legacy_distributor_pda(&mint, MONTH);
    assert!(legacy_live(&env, &legacy), "an open v2 month holds data");
    for (i, (holder, amount)) in holders.iter().enumerate() {
        let before = env.lamports(&holder.pubkey());
        env.claim_v2(mint, MONTH, holder, i as u32, *amount, tree.proofs[i].clone()).unwrap();
        assert_eq!(env.lamports(&holder.pubkey()), before + amount);
    }
    // Fully claimed, its funder closes it at once.
    env.close_v2(mint, MONTH).unwrap();
    assert!(!env.exists(&legacy));
    assert!(!legacy_live(&env, &legacy));
    // Lamports anyone sends there afterwards are no month.
    env.svm.airdrop(&legacy, SOL).unwrap();
    assert!(env.exists(&legacy));
    assert!(!legacy_live(&env, &legacy));
}

#[test]
fn a_v1_month_of_the_real_pepper_curve_is_live_until_it_closes() {
    let mut env = Env::new();
    let mint = env.mint;
    let holders = env.sol_holders(2);
    let tree = env.open_v1(mint, MONTH, &holders).unwrap();
    let legacy = legacy_distributor_pda(&mint, MONTH);
    assert_eq!(env.data_len(&legacy), 137, "a v1 Distributor");
    assert!(legacy_live(&env, &legacy));
    for (i, (holder, amount)) in holders.iter().enumerate() {
        env.claim_v1(mint, MONTH, holder, *amount, tree.proofs[i].clone()).unwrap();
    }
    env.close_v1(mint, MONTH).unwrap();
    assert!(!legacy_live(&env, &legacy));
    // A month the SOL program closed opens again there: pepper_curve's `init` finds the address free.
    let again = env.sol_holders(1);
    env.open_v1(mint, MONTH, &again).unwrap();
    assert!(legacy_live(&env, &legacy));
}

#[test]
fn an_open_month_of_the_real_pepper_curve_is_live_until_its_expiry_and_no_longer_after() {
    // The legacy guard reads the expiry where pepper_curve's own crate puts it, in the months its
    // real program opens.
    for v2 in [false, true] {
        let mut env = Env::new();
        let mint = env.mint;
        let holders = env.sol_holders(2);
        if v2 {
            env.open_v2(mint, MONTH, &holders).unwrap();
        } else {
            env.open_v1(mint, MONTH, &holders).unwrap();
        }
        let legacy = legacy_distributor_pda(&mint, MONTH);
        let account = env.svm.get_account(&legacy).unwrap();
        let expires_at = if v2 {
            env.fetch::<pepper_curve::state::DistributorV2>(&legacy).expires_at
        } else {
            env.fetch::<pepper_curve::state::Distributor>(&legacy).expires_at
        };
        assert_eq!(expires_at, NOV_1_2026 + 365 * DAY);
        assert_eq!(legacy_month_expiry(&account.owner, &pepper_curve::ID, &account.data), Some(expires_at));
        env.set_time(expires_at - 1);
        assert!(legacy_live(&env, &legacy), "v2 {v2}");
        env.set_time(expires_at);
        assert!(!legacy_live(&env, &legacy), "v2 {v2}");
    }
}

#[test]
fn the_tree_proves_this_programs_leaves_and_the_events_decode() {
    let env = Env::new();
    let distributor = distributor_pda(&env.mint, MONTH);
    for n in [1usize, 2, 3, 5, 8, 13] {
        let holders = holders(n);
        let leaves = dividend_leaves(&distributor, &env.usdc.mint, &holders);
        let tree = build_tree(&leaves);
        for (i, leaf) in leaves.iter().enumerate() {
            assert!(pepper_dividends::math::verify_merkle_proof(&tree.proofs[i], tree.root, *leaf), "{n} leaves, leaf {i}");
        }
    }
    // `event` reads an event back from "Program data:" lines as `emit!` writes them.
    use anchor_lang::Event;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let emitted = pepper_dividends::events::ClaimsPaused { until: env.now() + DAY, by: env.pauser.pubkey() };
    let logs = vec!["Program log: x".to_string(), format!("Program data: {}", STANDARD.encode(emitted.data()))];
    let read: pepper_dividends::events::ClaimsPaused = event(&logs);
    assert_eq!((read.until, read.by), (emitted.until, emitted.by));
    assert!(events_of::<pepper_dividends::events::FundingPauseSet>(&logs).is_empty());
}

#[test]
fn the_clock_moves_where_a_test_sets_it() {
    let mut env = Env::new();
    assert_eq!(env.now(), NOV_1_2026);
    env.set_time(NOV_1_2026 + 48 * 3_600 + 1);
    assert_eq!(env.now(), NOV_1_2026 + 2 * DAY + 1);
}
