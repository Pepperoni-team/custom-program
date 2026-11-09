//! # The revenue raise (upgrade N+3)
//!
//! The fixed-price USDC raise for a Fanex video revenue asset: fans fund an
//! advance to the creator, who pays them a share of the videos' revenue every
//! month for the whole term. Holds the payment guarantee and the exit pool.
//! Spec: docs/superpowers/specs/2026-10-01-revenue-raise-design.md as amended by
//! docs/superpowers/specs/2026-10-02-revenue-raise-term-and-pool-design.md;
//! canonical doc: docs/solana/PEPPER-RAISE-PROGRAM.md.
//!
//! The separate `pepper_raise` program until N+3, moved here whole before it reached mainnet:
//! its modules keep their names under `crate::raise`, its refusals are `FanexError`'s from 7000,
//! and its instructions are the last of `#[program]` in lib.rs.

pub mod accrual;
pub mod calendar;
pub mod checked;
pub mod constants;
pub mod events;
pub mod instructions;
pub mod ledger;
pub mod money;
pub mod pool_math;
pub mod pricing;
pub mod state;
pub mod transfer;
pub mod validation;

pub use validation::{RaiseParams, TierInput};

#[cfg(test)]
mod tests {
    /// The names of the structs under `attribute` in `source`, as the file writes them.
    fn names(source: &str, attribute: &str) -> Vec<String> {
        let lines: Vec<&str> = source.lines().map(str::trim).collect();
        let mut out = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            if !line.starts_with(attribute) {
                continue;
            }
            if let Some(def) = lines[i + 1..].iter().find(|l| l.starts_with("pub struct ")) {
                let name: String = def["pub struct ".len()..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                out.push(name);
            }
        }
        out
    }

    /// Since N+3 the raise's events and accounts carry pepper_curve's program id beside the curves',
    /// and Anchor's discriminator is sha256 of the kind and the name alone: a name both sides held
    /// would let one decode as the other. Held apart here, as the dividends program holds its own
    /// events off both (programs/pepper_dividends/src/events.rs).
    #[test]
    fn no_raise_event_or_account_shares_a_name_with_the_curves() {
        let curve_events = names(include_str!("../events.rs"), "#[event]");
        let raise_events = names(include_str!("events.rs"), "#[event]");
        let curve_accounts = names(include_str!("../state.rs"), "#[account");
        let raise_accounts = names(include_str!("state.rs"), "#[account");
        // The reader is proven on names each file is known to hold.
        assert!(curve_events.iter().any(|n| n == "DividendClaimed") && raise_events.iter().any(|n| n == "QuotasBought"));
        assert!(curve_accounts.iter().any(|n| n == "CurveUsdc") && curve_accounts.iter().any(|n| n == "DistributorV2"));
        assert_eq!(raise_accounts, ["RaiseConfig", "Raise", "Position", "Month", "PendingAdmin"]);
        for name in &raise_events {
            assert!(!curve_events.contains(name), "event {name}");
        }
        for name in &raise_accounts {
            assert!(!curve_accounts.contains(name), "account {name}");
        }
    }

    /// Since N+3 the raise's PDAs and the curves' derive under one program id. An address is the
    /// hash of the seeds' bytes run together (the bump last), so two families can meet only at the
    /// same length with their literal seeds agreeing wherever both have one: `pool` is where
    /// `pool_creator` starts, `payout` where `payouts` does. Each family is its literal seed and the
    /// bytes after it, the lengths the `seeds = [..]` constraints and `payout.rs` write.
    #[test]
    fn no_raise_pda_can_be_a_curve_pda() {
        use super::constants::*;
        use crate::constants::*;
        const KEY: usize = 32;
        let raise: [(&[u8], usize); 9] = [
            (RAISE_CONFIG_SEED, 0),
            (RAISE_SEED, KEY + 8), // creator, raise_id u64
            (POSITION_SEED, 2 * KEY), // raise, owner (the pool's: raise, raise)
            (MONTH_SEED, KEY + 4), // raise, month u32
            (ESCROW_SEED, KEY),
            (GUARANTEE_SEED, KEY),
            (POOL_SEED, KEY),
            (PAYOUTS_SEED, KEY),
            (PENDING_ADMIN_SEED, 0),
        ];
        let curve: [(&[u8], usize); 16] = [
            (CONFIG_SEED, 0),
            (CURVE_SEED, KEY),
            (SOL_VAULT_SEED, KEY),
            (VESTING_VAULT_SEED, KEY),
            (VESTING_SEED, KEY),
            (DISTRIBUTOR_SEED, KEY + 8), // mint, month i64
            (CLAIM_SEED, 2 * KEY),
            (LAUNCH_GUARD_SEED, 0),
            (LAUNCH_GUARD_WAIVER_SEED, KEY),
            (PAYOUT_TRANSIT_SEED, KEY),
            (FAN_LED_PLACEHOLDER_SEED, 0),
            (USDC_CONFIG_SEED, 0),
            (USDC_VAULT_SEED, KEY),
            (POOL_CREATOR_SEED, KEY),
            (CPMM_POOL_SEED, KEY),
            (FEE_NFT_SEED, KEY),
        ];
        let mut same_length = 0;
        for (r, r_rest) in raise {
            for (c, c_rest) in curve {
                assert_ne!(r, c, "one literal seed on both sides");
                if r.len() + r_rest != c.len() + c_rest {
                    continue;
                }
                same_length += 1;
                let common = r.len().min(c.len());
                assert_ne!(
                    &r[..common],
                    &c[..common],
                    "{} and {} can name one address",
                    String::from_utf8_lossy(r),
                    String::from_utf8_lossy(c)
                );
            }
        }
        // The check is not empty: guarantee, payouts and the singletons meet curve families of their length.
        assert!(same_length >= 5, "{same_length}");
    }
}
