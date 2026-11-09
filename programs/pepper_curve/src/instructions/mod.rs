// The SOL and USDC entry points share the name `handler`, which `lib.rs` calls by its full path, so
// the globs' clash on it is meant, as in `usdc/mod.rs`.
#![allow(ambiguous_glob_reexports)]
pub mod admin;
pub mod buy;
pub mod claim_vesting;
pub mod create_curve;
pub mod dividends;
pub mod graduate;
pub mod initialize_config;
pub mod launch_guard;
pub mod sell;
pub mod set_vesting_beneficiary;
pub mod sync_curve_creator;
// The USDC curve's instructions, in a folder of their own (USDC curve design §10.3).
pub mod usdc;

pub use admin::*;
pub use buy::*;
pub use claim_vesting::*;
pub use create_curve::*;
pub use dividends::*;
pub use graduate::*;
pub use initialize_config::*;
pub use launch_guard::*;
pub use sell::*;
pub use set_vesting_beneficiary::*;
pub use sync_curve_creator::*;
pub use usdc::*;
