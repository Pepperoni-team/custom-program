//! The USDC curve's instructions (USDC curve design §10.3), in a folder of their own beside the SOL
//! ones, which do not move. Each module is wired twice, `pub mod` and `pub use`: the `#[program]`
//! macro finds every Accounts struct's generated client module through the globs. Every Accounts
//! struct is named apart from the SOL ones and boxes its accounts.
//!
//! ⚠️ `lib/solana/curve-admin-rules.test.ts` scans this folder for the pause gates: a handler gated by
//! the USDC side's flag checks `require!(!ctx.accounts.usdc_config.paused`, one gated by its curve's
//! `require!(!ctx.accounts.curve.paused`, so the config account is named `usdc_config` and the curve
//! `curve` in every struct here. `usdc_pause.rs`'s two pauses of the SOL side, the USDC pause's
//! other half, write `GlobalConfig.paused` and a SOL `Curve`'s flag and check neither, so they gate
//! nothing.
//!
//! Every module but the config's and the pauses' names its entry point `handler`, which `lib.rs`
//! calls by its full path, so the globs' clash on that one name is meant.
#![allow(ambiguous_glob_reexports)]

pub mod buy_exact_in;
pub mod claim_vesting_usdc;
pub mod create_curve_usdc;
pub mod sell_exact_in;
pub mod sync_curve_creator_usdc;
pub mod usdc_config;
pub mod usdc_pause;
pub mod set_usdc_raydium;
// The graduation's first step (§10.3.9, 2026-10-23).
pub mod graduate_usdc_pool;
// The graduation's second step (§10.3.9, 2026-10-23).
pub mod graduate_usdc_lock;
// A creator raise closed by the admin (the internal audit, 2026-10-29).
pub mod close_creator_raise_usdc;
// A creator's own claim of a fan-led market, co-signed by the approver key (N+3, 2026-11-05).
pub mod claim_fan_led_usdc;
// The vault's undo of a self-claim while nothing has vested (N+3, 2026-11-09).
pub mod revoke_fan_led_usdc;

pub use buy_exact_in::*;
pub use claim_vesting_usdc::*;
pub use create_curve_usdc::*;
pub use sell_exact_in::*;
pub use sync_curve_creator_usdc::*;
pub use usdc_config::*;
pub use usdc_pause::*;
pub use set_usdc_raydium::*;
pub use graduate_usdc_pool::*;
pub use graduate_usdc_lock::*;
pub use close_creator_raise_usdc::*;
pub use claim_fan_led_usdc::*;
pub use revoke_fan_led_usdc::*;
