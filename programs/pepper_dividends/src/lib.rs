//! # pepper_dividends
//!
//! A token's monthly dividends paid in USDC: the funder moves the month's µUSDC
//! into the distributor's vault against a merkle root, each holder claims their
//! leaf into their own USDC account, and the close returns to the funder what
//! nobody claimed. A month funded in SOL stays in `pepper_curve` until it closes.
//! Spec: docs/superpowers/specs/2026-10-21-usdc-dividends-program-design.md;
//! canonical doc: docs/solana/PEPPER-DIVIDENDS-PROGRAM.md.
//!
//! The program id is the address of the program's keypair, approved by Rick on
//! 2026-10-26: one id on every cluster, as `Anchor.toml` and the TypeScript
//! mirror name it (`lib/solana/pepper-dividends/rust-parity.test.ts`). The keypair
//! is kept outside the repository, secret until the mainnet deploy; the
//! `pepper_dividends-keypair.json` that `cargo-build-sbf` writes beside the `.so`
//! is a throwaway, never this program's (the canonical doc, §1).

use anchor_lang::prelude::*;

pub mod constants;
pub mod errors;
pub mod events;
pub mod instructions;
pub mod math;
pub mod settle;
pub mod state;
pub mod transfer;
pub mod validation;

use instructions::*;

declare_id!("GdWxRsHF5wneNgLQmdoKS9Zcu6enMVzoVTe9EFaxJxcU");

/// The instructions, each with its LiteSVM suite (the canonical doc, §3).
#[program]
pub mod pepper_dividends {
    use super::*;

    // ── The config (spec §4.1, §4.2) ─────────────────────────────────────────

    /// Signed by this program's upgrade authority, read from its ProgramData.
    pub fn initialize_config(ctx: Context<InitializeConfig>, keys: ConfigKeys) -> Result<()> {
        instructions::config::initialize_config(ctx, keys)
    }

    /// Signed by the admin; each key an `Option`, the admin through `new_admin`.
    pub fn update_config(ctx: Context<UpdateConfig>, update: ConfigUpdate) -> Result<()> {
        instructions::config::update_config(ctx, update)
    }

    // ── The pauses (spec §4.3) ───────────────────────────────────────────────

    /// The funding paused by the pauser or the admin, unpaused by the admin alone.
    pub fn set_paused(ctx: Context<Pause>, paused: bool) -> Result<()> {
        instructions::pauses::set_paused(ctx, paused)
    }

    /// Claims paused until `until`, by the pauser or the admin: only started or extended, an hour
    /// at least and 7 days at most from when it runs.
    pub fn pause_claims(ctx: Context<Pause>, until: i64) -> Result<()> {
        instructions::pauses::pause_claims(ctx, until)
    }

    /// Claims open again, by the admin alone, at the time it runs.
    pub fn resume_claims(ctx: Context<Pause>) -> Result<()> {
        instructions::pauses::resume_claims(ctx)
    }

    // ── A month (spec §4.4 to §4.8) ──────────────────────────────────────────

    /// A month funded: the funder's µUSDC into the vault, with the approver's signature, the
    /// dividend payer paying the rents; its claims held for its review window, a day.
    pub fn init_distributor(
        ctx: Context<InitDistributor>,
        month: i64,
        merkle_root: [u8; 32],
        total_amount: u64,
        expires_at: i64,
        leaf_count: u32,
    ) -> Result<()> {
        instructions::init_distributor::init_distributor(ctx, month, merkle_root, total_amount, expires_at, leaf_count)
    }

    /// Leaf `index` paid into the claimant's own USDC account, from any sender.
    pub fn claim(ctx: Context<Claim>, index: u32, amount: u64, proof: Vec<[u8; 32]>) -> Result<()> {
        instructions::claim::claim(ctx, index, amount, proof)
    }

    /// A month closed by its funder or the closer: what is left back to the funder, the rents to
    /// the rent payer, the record kept closed.
    pub fn close_distributor(ctx: Context<CloseDistributor>) -> Result<()> {
        instructions::close_distributor::close_distributor(ctx)
    }

    /// A month funded wrong, voided by the admin while nobody has claimed from it: settled as at a
    /// close, the record voided, so the month may be funded again.
    pub fn void_distributor(ctx: Context<VoidDistributor>) -> Result<()> {
        instructions::void_distributor::void_distributor(ctx)
    }

    // ── A month's claims hold (Rick, 2026-10-26) ─────────────────────────────

    /// One month's claims held until `until`, by the pauser or the admin: only started or
    /// extended, an hour at least and 7 days at most; every other month keeps claiming.
    pub fn hold_claims(ctx: Context<Hold>, until: i64) -> Result<()> {
        instructions::hold::hold_claims(ctx, until)
    }

    /// That month's claims open again, by the admin alone, at the time it runs.
    pub fn release_claims(ctx: Context<Hold>) -> Result<()> {
        instructions::hold::release_claims(ctx)
    }

    // ── The internal audit's fixes (2026-10-29) ──────────────────────────────

    /// An open month's funder, its USDC account and its rent payer moved by the admin to the
    /// config's dividend payer as it stands, each only where the funding recorded a Fanex payee.
    pub fn rehome_distributor(ctx: Context<RehomeDistributor>) -> Result<()> {
        instructions::rehome::rehome_distributor(ctx)
    }

    /// More µUSDC into an open month from its funder's USDC, with the approver's signature: its
    /// total grows by as much, its root stays.
    pub fn top_up_distributor(ctx: Context<TopUpDistributor>, amount: u64) -> Result<()> {
        instructions::top_up::top_up_distributor(ctx, amount)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_program_id_is_the_keypair_rick_approved() {
        // 2026-10-26: a keypair's address, so on the curve, unlike the placeholder it replaced, and
        // the one the first deploy on each cluster signs for.
        assert_eq!(crate::ID.to_string(), "GdWxRsHF5wneNgLQmdoKS9Zcu6enMVzoVTe9EFaxJxcU");
        assert!(crate::ID.is_on_curve());
    }
}
