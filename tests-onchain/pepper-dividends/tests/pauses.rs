//! `set_paused`, `pause_claims` and `resume_claims` (spec §4.3; track B's B5b).
//!
//! The pauser, a hot key, and the admin pause at once; only the admin, the upgrade Squad's vault,
//! ends a pause. A claims pause only starts or extends, at most 7 days from each call, so no value
//! lets an expired month's close skip its 30-day guard; the resume takes no argument, so a vault
//! transaction executed 48 hours after it was written leaves the time it runs, and the stolen
//! pauser's replacement and the resume go through together. Every refusal is read by its code with
//! the config left as it was, and every act's event is decoded.

mod common;

use anchor_lang::error::ErrorCode;
use anchor_lang::Space;
use common::*;
use pepper_dividends::constants::{MAX_CLAIMS_PAUSE_SECONDS, MIN_CLAIMS_PAUSE_SECONDS};
use pepper_dividends::errors::DividendsError;
use pepper_dividends::events::{ClaimsPaused, ConfigUpdated, FundingPauseSet};
use pepper_dividends::instructions::ConfigUpdate;
use pepper_dividends::state::DividendsConfig;
use solana_sdk::{pubkey::Pubkey, signature::Signer};

const HOUR: i64 = 3_600;
/// The upgrade Squad's time lock: its vault executes a transaction 48 hours or more after the
/// transaction was written and approved (MULTISIG.md §5).
const TIME_LOCK: i64 = 48 * HOUR;

fn config(env: &Env) -> DividendsConfig {
    env.fetch(&config_pda())
}

fn config_bytes(env: &Env) -> Vec<u8> {
    env.svm.get_account(&config_pda()).expect("the config").data
}

/// `config` as the program stores it, to compare a whole config at once.
fn bytes_of(config: &DividendsConfig) -> Vec<u8> {
    account_data(config, DividendsConfig::INIT_SPACE)
}

/// The four acts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    PauseFunding,
    UnpauseFunding,
    PauseClaims,
    ResumeClaims,
}

#[test]
fn the_pauser_and_the_admin_pause_and_only_the_admin_ends_a_pause() {
    let mut env = Env::new();
    let stranger = env.funded_wallet(SOL);
    let (admin, pauser) = (env.admin.pubkey(), env.pauser.pubkey());
    let keys = [
        env.admin.insecure_clone(),
        env.pauser.insecure_clone(),
        env.approver.insecure_clone(),
        env.closer.insecure_clone(),
        env.pepper_funder.insecure_clone(),
        env.payer.insecure_clone(),
        env.sponsor.insecure_clone(),
        stranger,
    ];
    let now = env.now();
    for act in [Act::PauseFunding, Act::UnpauseFunding, Act::PauseClaims, Act::ResumeClaims] {
        for key in &keys {
            let by = key.pubkey();
            // Each act from a config where it changes something, and the config it leaves.
            let fresh = env.dividends_config();
            let running = DividendsConfig { claims_paused_until: now + DAY, ..fresh.clone() };
            let (start, end, ix, allowed) = match act {
                Act::PauseFunding => (
                    fresh.clone(),
                    DividendsConfig { paused: true, ..fresh.clone() },
                    env.set_paused_ix(&by, true),
                    by == pauser || by == admin,
                ),
                Act::UnpauseFunding => (
                    DividendsConfig { paused: true, ..fresh.clone() },
                    fresh.clone(),
                    env.set_paused_ix(&by, false),
                    by == admin,
                ),
                Act::PauseClaims => (fresh.clone(), running.clone(), env.pause_claims_ix(&by, now + DAY), by == pauser || by == admin),
                Act::ResumeClaims => (
                    running.clone(),
                    DividendsConfig { claims_paused_until: now, ..fresh.clone() },
                    env.resume_claims_ix(&by),
                    by == admin,
                ),
            };
            env.put_config(&start);
            let result = env.send_logged_paid_by(key, &[ix], &[]);
            if !allowed {
                expect_err(result, code(DividendsError::Unauthorized));
                assert_eq!(config_bytes(&env), bytes_of(&start), "{act:?} by a key that may not");
                continue;
            }
            let sent = result.unwrap_or_else(|e| panic!("{act:?} by {by}: {e:?}"));
            assert_eq!(config_bytes(&env), bytes_of(&end), "{act:?} by {by}");
            match act {
                Act::PauseFunding | Act::UnpauseFunding => {
                    let event = event::<FundingPauseSet>(&sent.logs);
                    assert_eq!((event.paused, event.by), (end.paused, by), "{act:?}");
                }
                Act::PauseClaims | Act::ResumeClaims => {
                    let event = event::<ClaimsPaused>(&sent.logs);
                    assert_eq!((event.until, event.by), (end.claims_paused_until, by), "{act:?}");
                }
            }
        }
    }
}

#[test]
fn the_funding_pause_stops_a_funding_until_the_admin_lifts_it() {
    let mut env = Env::new();
    env.write_config();
    let (pauser, admin, fanex) = (env.pauser.insecure_clone(), env.admin.insecure_clone(), env.pepper_funder.insecure_clone());
    let usdc = env.usdc_account(&fanex.pubkey());
    env.mint_usdc(&usdc, 10 * USDC);
    let distributor = distributor_pda(&env.mint, MONTH);
    let root = build_tree(&dividend_leaves(&distributor, &env.usdc.mint, &holders(2))).root;
    let funding = env.funding(&fanex.pubkey(), root, 10 * USDC, 2);

    let pause = env.set_paused_ix(&pauser.pubkey(), true);
    env.send_paid_by(&pauser, &[pause], &[]).unwrap();
    expect_err(env.fund(&funding, &fanex), code(DividendsError::Paused));
    assert!(!env.exists(&distributor));
    // The pauser cannot lift its own pause; the admin can, and the same funding then opens.
    let lift = env.set_paused_ix(&pauser.pubkey(), false);
    expect_err(env.send_paid_by(&pauser, &[lift], &[]), code(DividendsError::Unauthorized));
    let lift = env.set_paused_ix(&admin.pubkey(), false);
    env.send_paid_by(&admin, &[lift], &[]).unwrap();
    env.fund(&funding, &fanex).unwrap();
    assert_eq!(env.token_amount(&env.usdc_ata(&distributor)), 10 * USDC);
}

#[test]
fn a_claims_pause_lasts_seven_days_at_most_and_is_renewed_at_day_seven() {
    let mut env = Env::new();
    env.write_config();
    assert_eq!(config(&env).claims_paused_until, 0, "a fresh config");
    assert_eq!(MAX_CLAIMS_PAUSE_SECONDS, 7 * DAY);
    let pauser = env.pauser.insecure_clone();
    let t = env.now();
    // From the fresh config's 0: 7 days and a second is too long, 7 days to the second is not.
    let before = config_bytes(&env);
    let too_long = env.pause_claims_ix(&pauser.pubkey(), t + 7 * DAY + 1);
    expect_err(env.send_paid_by(&pauser, &[too_long], &[]), code(DividendsError::PauseTooLong));
    assert_eq!(config_bytes(&env), before);
    let week = env.pause_claims_ix(&pauser.pubkey(), t + 7 * DAY);
    env.send_paid_by(&pauser, &[week], &[]).unwrap();
    assert_eq!(config(&env).claims_paused_until, t + 7 * DAY);
    // At day 7 the pauser renews it for 7 days more, and not a second further.
    env.set_time(t + 7 * DAY);
    let before = config_bytes(&env);
    let too_long = env.pause_claims_ix(&pauser.pubkey(), t + 14 * DAY + 1);
    expect_err(env.send_paid_by(&pauser, &[too_long], &[]), code(DividendsError::PauseTooLong));
    assert_eq!(config_bytes(&env), before);
    let renewal = env.pause_claims_ix(&pauser.pubkey(), t + 14 * DAY);
    let sent = env.send_logged_paid_by(&pauser, &[renewal], &[]).unwrap();
    assert_eq!(config(&env).claims_paused_until, t + 14 * DAY);
    let event = event::<ClaimsPaused>(&sent.logs);
    assert_eq!((event.until, event.by), (t + 14 * DAY, pauser.pubkey()));
}

#[test]
fn an_end_in_the_past_or_before_the_running_pause_is_refused() {
    let mut env = Env::new();
    env.write_config();
    let (pauser, admin) = (env.pauser.insecure_clone(), env.admin.insecure_clone());
    let t = env.now();
    // No pause runs: an end a second in the past is refused.
    let before = config_bytes(&env);
    let past = env.pause_claims_ix(&pauser.pubkey(), t - 1);
    expect_err(env.send_paid_by(&pauser, &[past], &[]), code(DividendsError::PauseEndTooEarly));
    assert_eq!(config_bytes(&env), before);
    // So is one at the current second, or within the hour, by the pauser and the admin alike (the
    // internal audit, 2026-10-29: such a pause refused no claim yet delayed every expired month's
    // close by 30 days); an hour from now, to the second, is a pause.
    assert_eq!(MIN_CLAIMS_PAUSE_SECONDS, 3_600);
    for (signer, until) in [(&pauser, t), (&pauser, t + 60), (&admin, t + 3_599)] {
        let short = env.pause_claims_ix(&signer.pubkey(), until);
        expect_err(env.send_paid_by(signer, &[short], &[]), code(DividendsError::PauseTooShort));
    }
    assert_eq!(config_bytes(&env), before);
    let an_hour = env.pause_claims_ix(&pauser.pubkey(), t + 3_600);
    env.send_paid_by(&pauser, &[an_hour], &[]).unwrap();
    assert_eq!(config(&env).claims_paused_until, t + 3_600);
    // A pause of 5 days: an end a second earlier would shorten it, refused to the pauser and to
    // the admin alike, since only a resume shortens a pause; the same end again is accepted.
    let five = env.pause_claims_ix(&pauser.pubkey(), t + 5 * DAY);
    env.send_paid_by(&pauser, &[five], &[]).unwrap();
    let before = config_bytes(&env);
    for signer in [&pauser, &admin] {
        let shorter = env.pause_claims_ix(&signer.pubkey(), t + 5 * DAY - 1);
        expect_err(env.send_paid_by(signer, &[shorter], &[]), code(DividendsError::PauseEndTooEarly));
    }
    assert_eq!(config_bytes(&env), before);
    let same = env.pause_claims_ix(&admin.pubkey(), t + 5 * DAY);
    env.send_paid_by(&admin, &[same], &[]).unwrap();
    assert_eq!(config(&env).claims_paused_until, t + 5 * DAY);
    // Once that pause has ended, an end between it and now lies in the past.
    env.set_time(t + 6 * DAY);
    let stale = env.pause_claims_ix(&pauser.pubkey(), t + 6 * DAY - 1);
    expect_err(env.send_paid_by(&pauser, &[stale], &[]), code(DividendsError::PauseEndTooEarly));
    assert_eq!(config(&env).claims_paused_until, t + 5 * DAY);
}

#[test]
fn a_resume_written_at_t_and_executed_48_hours_later_leaves_the_time_it_runs() {
    let mut env = Env::new();
    env.write_config();
    let month = env.write_month(MONTH, 2);
    for (holder, _) in &month.holders {
        env.usdc_account(&holder.pubkey());
    }
    let (pauser, admin, sponsor) = (env.pauser.insecure_clone(), env.admin.insecure_clone(), env.sponsor.insecure_clone());
    let t = env.now();
    let pause = env.pause_claims_ix(&pauser.pubkey(), t + 7 * DAY);
    env.send_paid_by(&pauser, &[pause], &[]).unwrap();
    // The vault's transaction is written at T; the resume in it carries no time.
    let resume = env.resume_claims_ix(&admin.pubkey());
    env.set_time(t + HOUR);
    let claim = env.claim_ix(&month, 0);
    expect_err(env.send_paid_by(&sponsor, &[claim.clone()], &[]), code(DividendsError::ClaimsPaused));
    // Executed 48 hours and a second later, it leaves that second, and claims open at it.
    env.set_time(t + TIME_LOCK + 1);
    let sent = env.send_logged_paid_by(&admin, &[resume], &[]).unwrap();
    assert_eq!(config(&env).claims_paused_until, t + TIME_LOCK + 1);
    let event = event::<ClaimsPaused>(&sent.logs);
    assert_eq!((event.until, event.by), (t + TIME_LOCK + 1, admin.pubkey()));
    env.send_paid_by(&sponsor, &[claim], &[]).unwrap();
    assert_eq!(env.token_amount(&env.usdc_ata(&month.holders[0].0.pubkey())), month.holders[0].1);
}

#[test]
fn a_resume_when_no_pause_runs_leaves_the_value_as_it_was_and_fails_nothing() {
    let mut env = Env::new();
    env.write_config();
    let (pauser, admin) = (env.pauser.insecure_clone(), env.admin.insecure_clone());
    let t = env.now();
    // A fresh config's 0 stays 0.
    let resume = env.resume_claims_ix(&admin.pubkey());
    let sent = env.send_logged_paid_by(&admin, &[resume.clone()], &[]).unwrap();
    assert_eq!(config(&env).claims_paused_until, 0);
    assert_eq!(event::<ClaimsPaused>(&sent.logs).until, 0);
    // A pause that ran out keeps its end, the time claims last opened again, which an expired
    // month's close reads: a resume after it moves no close's 30-day guard.
    let day = env.pause_claims_ix(&pauser.pubkey(), t + DAY);
    env.send_paid_by(&pauser, &[day], &[]).unwrap();
    env.set_time(t + 3 * DAY);
    let sent = env.send_logged_paid_by(&admin, &[resume], &[]).unwrap();
    assert_eq!(config(&env).claims_paused_until, t + DAY);
    let event = event::<ClaimsPaused>(&sent.logs);
    assert_eq!((event.until, event.by), (t + DAY, admin.pubkey()));
}

#[test]
fn a_stolen_pauser_is_replaced_and_claims_resumed_in_one_vault_transaction() {
    let mut env = Env::new();
    env.write_config();
    let (thief, admin) = (env.pauser.insecure_clone(), env.admin.insecure_clone());
    let t = env.now();
    // The stolen pauser pauses claims; at T the vault writes the replacement and the resume.
    let pause = env.pause_claims_ix(&thief.pubkey(), t + 7 * DAY);
    env.send_paid_by(&thief, &[pause], &[]).unwrap();
    let next = env.funded_wallet(SOL);
    let replacement = [
        env.update_config_ix(&admin.pubkey(), None, ConfigUpdate { pauser: Some(next.pubkey()), ..ConfigUpdate::default() }),
        env.resume_claims_ix(&admin.pubkey()),
    ];
    // While the proposal waits, the thief renews its pause as far as 7 days allow.
    env.set_time(t + TIME_LOCK - HOUR);
    let renewal = env.pause_claims_ix(&thief.pubkey(), t + TIME_LOCK - HOUR + 7 * DAY);
    env.send_paid_by(&thief, &[renewal], &[]).unwrap();
    // Executed 48 hours and a second after it was written, both go through in one transaction.
    env.set_time(t + TIME_LOCK + 1);
    let sent = env.send_logged_paid_by(&admin, &replacement, &[]).unwrap();
    let after = config(&env);
    assert_eq!((after.pauser, after.claims_paused_until), (next.pubkey(), t + TIME_LOCK + 1));
    assert_eq!(event::<ConfigUpdated>(&sent.logs).pauser, next.pubkey());
    assert_eq!(event::<ClaimsPaused>(&sent.logs).until, t + TIME_LOCK + 1);
    // From then on the former pauser is refused both pauses, and the new one pauses.
    let before = config_bytes(&env);
    let now = env.now();
    for act in [env.pause_claims_ix(&thief.pubkey(), now + DAY), env.set_paused_ix(&thief.pubkey(), true)] {
        expect_err(env.send_paid_by(&thief, &[act], &[]), code(DividendsError::Unauthorized));
    }
    assert_eq!(config_bytes(&env), before);
    let pause = env.pause_claims_ix(&next.pubkey(), now + DAY);
    env.send_paid_by(&next, &[pause], &[]).unwrap();
    assert_eq!(config(&env).claims_paused_until, now + DAY);
}

#[test]
fn a_config_at_any_other_address_is_refused_by_its_seeds() {
    // Only the bank could write one, since the program creates its config at ["config"] alone: a
    // copy naming a stranger as the admin and the pauser takes none of the three acts.
    let mut env = Env::new();
    env.write_config();
    let stranger = env.funded_wallet(SOL);
    let copy = Pubkey::new_unique();
    let forged = DividendsConfig {
        admin: stranger.pubkey(),
        pauser: stranger.pubkey(),
        claims_paused_until: env.now() + DAY,
        ..env.dividends_config()
    };
    env.put(copy, bytes_of(&forged), pepper_dividends::ID);
    let accounts = || pepper_dividends::accounts::Pause { authority: stranger.pubkey(), config: copy };
    let acts = [
        ix(accounts(), pepper_dividends::instruction::SetPaused { paused: true }),
        ix(accounts(), pepper_dividends::instruction::PauseClaims { until: env.now() + 2 * DAY }),
        ix(accounts(), pepper_dividends::instruction::ResumeClaims {}),
    ];
    let before = (config_bytes(&env), env.svm.get_account(&copy).unwrap().data);
    for act in acts {
        expect_err(env.send_paid_by(&stranger, &[act], &[]), anchor_code(ErrorCode::ConstraintSeeds));
    }
    assert_eq!((config_bytes(&env), env.svm.get_account(&copy).unwrap().data), before);
}
