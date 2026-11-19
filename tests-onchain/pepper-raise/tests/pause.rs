mod common;

use common::*;
use pepper_curve::raise::calendar::{declare_open_ts, due_ts};
use pepper_curve::errors::FanexError;
use pepper_curve::raise::ledger::close_fallback_ts;
use pepper_curve::raise::state::{Month, Raise, RaiseConfig, RaiseState};
use solana_sdk::signature::{Keypair, Signer};

fn paused(env: &Env) -> bool {
    env.fetch::<RaiseConfig>(&config_pda()).paused
}

/// "Never trap funds": the pause stops what opens new exposure (`create_raise`,
/// `buy`, `declare_month` and the two pool trades) and nothing that moves money
/// already in the program. This raise lives its whole life paused, resumed only
/// for Fanex to declare its one month. (`cancel_raise` and `refund` while paused:
/// closing.rs; `fund_direct` and `sweep_pool_earnings`: pool.rs.)
#[test]
fn the_pause_never_traps_funds() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise) = env.launch(1);
    let buyers = env.sell_out(&raise);
    env.set_paused(true).unwrap();

    // Nothing new opens...
    let (newcomer, newcomer_usdc) = env.user(10 * USD);
    let now = env.now();
    expect_err(
        env.create_raise(&newcomer, &newcomer_usdc, small_params(2, now)).map(|_| ()),
        code(FanexError::RaiseGloballyPaused),
    );
    // ...and the sold-out raise still closes and splits.
    env.finalize(&raise, &creator_usdc).unwrap();
    assert_eq!(env.fetch::<Raise>(&raise).state, RaiseState::Active);

    env.set_time(declare_open_ts(NOVEMBER_2026));
    expect_err(env.declare(&raise, NOVEMBER_2026, 100 * USD), code(FanexError::RaiseGloballyPaused));
    // Fanex resumes long enough to declare November, then pauses again.
    env.set_paused(false).unwrap();
    env.declare(&raise, NOVEMBER_2026, 100 * USD).unwrap();
    env.set_paused(true).unwrap();

    // November goes unpaid: from its due date the guarantee covers what it can...
    env.set_time(due_ts(NOVEMBER_2026));
    env.settle(&raise, NOVEMBER_2026).unwrap();
    assert_eq!(env.fetch::<Month>(&month_pda(&raise, NOVEMBER_2026)).covered, 45_454_545);
    // ...the creator pays the rest late, and the same payment rebuilds the guarantee (the arrears
    // first, then the guarantee: the internal audit of 2026-10-29).
    env.mint_to(&creator_usdc, 100 * USD);
    env.fund_month(&creator, &creator_usdc, &raise, NOVEMBER_2026, 100 * USD).unwrap();
    assert_eq!(env.fetch::<Month>(&month_pda(&raise, NOVEMBER_2026)).paid, 54_545_455);
    assert_eq!(env.fetch::<Raise>(&raise).guarantee_usdc, 45_454_545);
    expect_err(env.replenish(&creator, &creator_usdc, &raise, 45_454_545), code(FanexError::NothingOwed));
    // Its release waits for twelve on-time months, never for the pause (the
    // release itself while paused: the next test).
    expect_err(env.release(&raise, &creator_usdc), code(FanexError::GuaranteeNotReleasable));

    // A holder claims a tenth of the guarantee's 44,090,909 and of the late 52,909,092.
    let (holder, holder_usdc) = &buyers[0];
    env.claim(&raise, &holder.pubkey(), holder_usdc).unwrap();
    assert_eq!(env.balance(holder_usdc), 9_700_000);

    // Fanex never declares again: the raise closes after the fallback, and the
    // rebuilt guarantee goes back to the creator...
    let term_end = env.fetch::<Raise>(&raise).term_end_month;
    env.set_time(close_fallback_ts(term_end));
    let before = env.balance(&creator_usdc);
    env.close_raise(&raise, &creator_usdc).unwrap();
    assert_eq!(env.balance(&creator_usdc) - before, 45_454_545);
    // ...and a holder's position closes with a tenth of both payments and of the pool.
    let (other, other_usdc) = &buyers[1];
    env.close_position(&raise, &other.pubkey(), other_usdc).unwrap();
    assert_eq!(env.balance(other_usdc), 18_790_909);
    assert!(paused(&env));
}

/// The kill switch's pause (2026-11-05): `UsdcConfig.pauser`, the curves' hot key, pauses the raise
/// at once, with no proposal of the raise admin; it stops a purchase and a launch like the admin's
/// pause, and only the admin's `set_raise_paused(false)` resumes it.
#[test]
fn the_curves_pauser_pauses_the_raise_and_only_the_admin_resumes_it() {
    let mut env = Env::new();
    let pauser = Keypair::new();
    env.put_usdc_config(&pauser.pubkey());
    let (_, _, raise) = env.launch(1);
    let (fan, fan_usdc) = env.user(10 * USD);

    env.usdc_pause_raise(&pauser).unwrap();
    assert!(paused(&env));
    expect_err(env.buy(&raise, &fan, &fan_usdc, 10 * USD), code(FanexError::RaiseGloballyPaused));
    let (newcomer, newcomer_usdc) = env.user(10 * USD);
    let now = env.now();
    expect_err(
        env.create_raise(&newcomer, &newcomer_usdc, small_params(2, now)).map(|_| ()),
        code(FanexError::RaiseGloballyPaused),
    );
    // Pressed twice: no error, still paused.
    env.usdc_pause_raise(&pauser).unwrap();
    assert!(paused(&env));

    env.set_paused(false).unwrap();
    assert!(!paused(&env));
    env.buy(&raise, &fan, &fan_usdc, 10 * USD).unwrap();
}

/// Only `UsdcConfig.pauser` signs it: a stranger and the raise admin are refused (`Unauthorized`,
/// the curves' error, as on `usdc_pause_sol`), the admin pausing through its own instruction.
#[test]
fn only_the_curves_pauser_signs_the_raise_s_usdc_pause() {
    let mut env = Env::new();
    let pauser = Keypair::new();
    env.put_usdc_config(&pauser.pubkey());

    let stranger = Keypair::new();
    expect_err(env.usdc_pause_raise(&stranger), code(FanexError::Unauthorized));
    let admin = env.admin.insecure_clone();
    expect_err(env.usdc_pause_raise(&admin), code(FanexError::Unauthorized));
    assert!(!paused(&env));
}

/// The release needs twelve on-time months, so it gets a lifecycle of its own:
/// declared and paid while the program runs, settled and released while paused.
#[test]
fn the_pause_never_holds_back_the_guarantees_release() {
    let mut env = Env::new();
    let (creator, creator_usdc, raise, _) = env.active_raise(1);
    env.mint_to(&creator_usdc, 120 * USD);
    for i in 0..12u32 {
        let month = NOVEMBER_2026 + i;
        env.set_time(declare_open_ts(month));
        env.declare(&raise, month, 10 * USD).unwrap();
        env.fund_month(&creator, &creator_usdc, &raise, month, 10 * USD).unwrap();
    }
    env.set_paused(true).unwrap();
    env.set_time(due_ts(NOVEMBER_2026 + 11));
    for i in 0..12u32 {
        env.settle(&raise, NOVEMBER_2026 + i).unwrap();
    }
    assert_eq!(env.fetch::<Raise>(&raise).on_time_streak, 12);
    let before = env.balance(&creator_usdc);
    env.release(&raise, &creator_usdc).unwrap();
    assert_eq!(env.balance(&creator_usdc) - before, 45_454_545);
    assert!(paused(&env));
}
