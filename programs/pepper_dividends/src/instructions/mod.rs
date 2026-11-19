//! The instructions, one module each (the canonical doc, §3).

pub mod claim;
pub mod close_distributor;
pub mod config;
pub mod deposit_reserve;
pub mod fund_from_reserve;
pub mod hold;
pub mod init_distributor;
pub mod pauses;
pub mod rehome;
pub mod reserve_withdrawal;
pub mod top_up;
pub mod void_distributor;

pub use claim::*;
pub use close_distributor::*;
pub use config::*;
pub use deposit_reserve::*;
pub use fund_from_reserve::*;
pub use hold::*;
pub use init_distributor::*;
pub use pauses::*;
pub use rehome::*;
pub use reserve_withdrawal::*;
pub use top_up::*;
pub use void_distributor::*;
