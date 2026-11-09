//! `set_usdc_raydium` (USDC curve design §10.3.10, Rick, 2026-10-22): the five Raydium addresses a
//! graduation sends a curve's reserves to, written by the program's upgrade authority alone, read
//! from its ProgramData. Raydium's ids differ per cluster and the binary is one, so they live in
//! `UsdcConfig`, not in a constant; and a config that repoints the graduation's CPI can empty a
//! curve, so they are not `UsdcConfig.admin`'s: the key that can already replace the program sets
//! them, so the power is no new one. On mainnet that key is the Squad's vault, through a proposal:
//! the same 3 of 5 approve it and the Squad's 48-hour time lock delays it as it delays an upgrade
//! (Rick, 2026-10-23). A Raydium change (a new fee tier, fee account, lock or program) is one such
//! proposal, never a program upgrade.
//!
//! Field by field since 2026-10-24: each address a proposal leaves `None` stays as it is, and the
//! fee tier is read only when the CPMM or the tier itself changes, so a lock or a fee account moves
//! even while Raydium has stopped pool creation on the tier (`PoolCreationDisabled`).
//!
//! Never paused (`USDC_NEVER_PAUSED`): it moves no funds, and a pause must not stop the way out of a
//! Raydium change.
use anchor_lang::prelude::*;
use anchor_lang::solana_program::bpf_loader_upgradeable;

use crate::constants::*;
use crate::errors::FanexError;
use crate::events::UsdcRaydiumSet;
use crate::raydium::check_amm_config;
use crate::state::*;

/// The Raydium addresses to write, each `None` to keep the one the config holds: the first call
/// names all five, a later one only what Raydium moved.
#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct SetUsdcRaydiumParams {
    /// Raydium's CPMM program: mainnet `CPMMoo8L…`, devnet `DRaycpLY…`.
    pub raydium_cpmm_program: Option<Pubkey>,
    /// The CPMM's 1% fee tier: mainnet index 1 `G95xxie3…`, devnet index 3.
    pub raydium_amm_config: Option<Pubkey>,
    /// The account CPMM's `initialize` takes its pool-creation fee into: CPMM binds it itself, so
    /// only clients read it.
    pub raydium_create_pool_fee: Option<Pubkey>,
    /// Raydium's LP lock program: mainnet `LockrWmn…`, devnet `DRay25Us…`.
    pub raydium_lock_program: Option<Pubkey>,
    /// The LP lock's authority PDA.
    pub raydium_lock_authority: Option<Pubkey>,
}

/// The bytes of a ProgramData before the program's ELF (`UpgradeableLoaderState::
/// size_of_programdata_metadata`): the bincode of `UpgradeableLoaderState::ProgramData`, its tag 3
/// as a u32, `slot` as a u64, then `upgrade_authority_address` as an `Option<Pubkey>` (a 0 or 1
/// byte, then the key's 32, left zero when there is none).
pub const PROGRAM_DATA_HEADER_LEN: usize = 45;
const PROGRAM_DATA_TAG: [u8; 4] = [3, 0, 0, 0];
const PROGRAM_DATA_AUTHORITY_TAG_OFFSET: usize = 12;

/// The upgrade authority a ProgramData's header records: `None` for an immutable program, and for
/// bytes that are not a ProgramData's header (too short, another tag of the loader's state, an
/// option byte neither 0 nor 1), so whoever signs is refused.
pub fn program_data_upgrade_authority(data: &[u8]) -> Option<Pubkey> {
    if data.len() < PROGRAM_DATA_HEADER_LEN || data[..4] != PROGRAM_DATA_TAG {
        return None;
    }
    if data[PROGRAM_DATA_AUTHORITY_TAG_OFFSET] != 1 {
        return None;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&data[PROGRAM_DATA_AUTHORITY_TAG_OFFSET + 1..PROGRAM_DATA_HEADER_LEN]);
    Some(Pubkey::new_from_array(key))
}

impl SetUsdcRaydiumParams {
    /// The five, in `UsdcConfig::raydium_addresses`'s order.
    fn given(&self) -> [Option<Pubkey>; 5] {
        [
            self.raydium_cpmm_program,
            self.raydium_amm_config,
            self.raydium_create_pool_fee,
            self.raydium_lock_program,
            self.raydium_lock_authority,
        ]
    }

    /// The five addresses a config holding `current` holds once these are written.
    pub fn applied_to(&self, current: [Pubkey; 5]) -> [Pubkey; 5] {
        let given = self.given();
        std::array::from_fn(|i| given[i].unwrap_or(current[i]))
    }

    /// Whether the fee tier is read: only when the CPMM or the tier changes, the two the tier's
    /// checks bind together.
    pub fn moves_the_tier(&self) -> bool {
        self.raydium_cpmm_program.is_some() || self.raydium_amm_config.is_some()
    }

    /// Whether an address given is all-zero: none is ever written back to unset.
    pub fn unsets_one(&self) -> bool {
        self.given().contains(&Some(Pubkey::default()))
    }
}

#[derive(Accounts)]
pub struct SetUsdcRaydium<'info> {
    /// The program's upgrade authority, as its ProgramData records it. Not `UsdcConfig.admin`.
    pub upgrade_authority: Signer<'info>,

    /// CHECK: bound by its seeds and its owner, its header read by `program_data_upgrade_authority`.
    /// The program's ProgramData, the upgradeable loader's PDA of the program id (`ConstraintSeeds`
    /// anywhere else, so another program's ProgramData, whoever holds it, is refused), owned by the
    /// loader, whose `upgrade_authority_address` must be the signer: `Unauthorized` for any other
    /// key, for an immutable program, which records none, and for a program loaded without the
    /// upgradeable loader, which has no ProgramData. Read by hand, not as Anchor's
    /// `Account<ProgramData>`, whose bincode decoding made the program about 60 KB larger
    /// (2026-10-23).
    #[account(
        seeds = [crate::ID.as_ref()],
        bump,
        seeds::program = bpf_loader_upgradeable::ID,
        owner = bpf_loader_upgradeable::ID @ FanexError::Unauthorized,
        constraint = program_data_upgrade_authority(&program_data.try_borrow_data()?)
            == Some(upgrade_authority.key()) @ FanexError::Unauthorized,
    )]
    pub program_data: UncheckedAccount<'info>,

    #[account(mut, seeds = [USDC_CONFIG_SEED], bump = usdc_config.bump)]
    pub usdc_config: Box<Account<'info, UsdcConfig>>,

    /// CHECK: checked by hand in the handler, through `raydium::check_amm_config`.
    /// The CPMM's 1% fee tier, passed when the CPMM or the tier changes (`AccountMismatch` when it
    /// is missing then) and read only then: at the tier's address once written, owned by the CPMM
    /// once written, an `AmmConfig` at 1% still creating pools. Typed as Raydium's account, it
    /// would need Raydium's crate, which pins Anchor 1.0.2.
    pub raydium_amm_config: Option<UncheckedAccount<'info>>,
}

/// Write the addresses given, after the signer's check above, in this order: none of them all-zero
/// (`RaydiumNotSet`); then, when the CPMM or the tier changes, the `amm_config` the account list
/// carries held to the pair the config will hold (`AccountMismatch` missing, at another address,
/// under another owner, or not an `AmmConfig`; `GraduationFeeTierChanged` off the 1% tier;
/// `PoolCreationDisabled` on a tier that no longer creates pools). The other three are taken as
/// given: CPMM checks its fee account and the lock its authority when the graduation calls them.
/// Emits `UsdcRaydiumSet`, the five as they stand after the write. ⚠️ A curve in PoolCreated keeps
/// its pool's CPMM (`graduate_usdc_lock` binds the pool's own records), so a change of the CPMM or
/// the lock program is proposed only while no curve sits there.
pub fn handler(ctx: Context<SetUsdcRaydium>, params: SetUsdcRaydiumParams) -> Result<()> {
    require!(!params.unsets_one(), FanexError::RaydiumNotSet);
    let [cpmm_program, amm_config, create_pool_fee, lock_program, lock_authority] =
        params.applied_to(ctx.accounts.usdc_config.raydium_addresses());
    if params.moves_the_tier() {
        let account = ctx
            .accounts
            .raydium_amm_config
            .as_ref()
            .ok_or(FanexError::AccountMismatch)?;
        check_amm_config(
            account.key,
            account.owner,
            &account.try_borrow_data()?,
            &amm_config,
            &cpmm_program,
        )?;
    }

    let config = &mut ctx.accounts.usdc_config;
    config.raydium_cpmm_program = cpmm_program;
    config.raydium_amm_config = amm_config;
    config.raydium_create_pool_fee = create_pool_fee;
    config.raydium_lock_program = lock_program;
    config.raydium_lock_authority = lock_authority;
    emit!(UsdcRaydiumSet {
        raydium_cpmm_program: cpmm_program,
        raydium_amm_config: amm_config,
        raydium_create_pool_fee: create_pool_fee,
        raydium_lock_program: lock_program,
        raydium_lock_authority: lock_authority,
        by: ctx.accounts.upgrade_authority.key(),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_upgrade_authority_alone_signs_and_the_config_alone_is_written() {
        let [upgrade_authority, program_data, usdc_config, amm_config] =
            std::array::from_fn(|_| Pubkey::new_unique());
        let metas = crate::accounts::SetUsdcRaydium {
            upgrade_authority,
            program_data,
            usdc_config,
            raydium_amm_config: Some(amm_config),
        }
        .to_account_metas(None);

        let keys: Vec<Pubkey> = metas.iter().map(|m| m.pubkey).collect();
        assert_eq!(
            keys,
            [upgrade_authority, program_data, usdc_config, amm_config]
        );
        let signers: Vec<Pubkey> = metas
            .iter()
            .filter(|m| m.is_signer)
            .map(|m| m.pubkey)
            .collect();
        assert_eq!(signers, [upgrade_authority]);
        let writable: Vec<Pubkey> = metas
            .iter()
            .filter(|m| m.is_writable)
            .map(|m| m.pubkey)
            .collect();
        assert_eq!(writable, [usdc_config]);
    }

    #[test]
    fn the_program_data_is_the_loaders_pda_of_the_program_id() {
        // What `solana program deploy` creates and `solana program show` names "ProgramData Address".
        let (address, _) =
            Pubkey::find_program_address(&[crate::ID.as_ref()], &bpf_loader_upgradeable::ID);
        assert_eq!(
            address,
            bpf_loader_upgradeable::get_program_data_address(&crate::ID)
        );
        assert_ne!(address, crate::ID);
    }

    /// A ProgramData's header as the loader writes it, built by hand, then `elf_len` bytes of ELF.
    fn program_data(slot: u64, authority: Option<Pubkey>, elf_len: usize) -> Vec<u8> {
        let mut data = 3u32.to_le_bytes().to_vec();
        data.extend_from_slice(&slot.to_le_bytes());
        match authority {
            Some(key) => {
                data.push(1);
                data.extend_from_slice(key.as_ref());
            }
            None => data.extend_from_slice(&[0; 33]),
        }
        data.extend(std::iter::repeat(0x7f).take(elf_len));
        data
    }

    #[test]
    fn reads_the_authority_the_loader_s_own_decoding_reads() {
        // Anchor's `ProgramData` decodes with bincode into the loader's own state type: the hand
        // read must agree with it on the recorded authority, and on an immutable program's none.
        assert_eq!(
            UpgradeableLoaderState::size_of_programdata_metadata(),
            PROGRAM_DATA_HEADER_LEN
        );
        let key = Pubkey::new_unique();
        for recorded in [Some(key), None] {
            let data = program_data(457_292_125, recorded, 64);
            let decoded = ProgramData::try_deserialize(&mut data.as_slice()).unwrap();
            assert_eq!(
                (decoded.slot, decoded.upgrade_authority_address),
                (457_292_125, recorded)
            );
            assert_eq!(program_data_upgrade_authority(&data), recorded);
            // The header alone, with no ELF after it, reads the same.
            assert_eq!(
                program_data_upgrade_authority(&data[..PROGRAM_DATA_HEADER_LEN]),
                recorded
            );
        }
        // `set-upgrade-authority --final` (and `deploy --final`, which sends it after the deploy):
        // the loader's `set_state` writes the 13 bytes of the None header over one that recorded a
        // key, so the former key's bytes stay after the option byte. They record no authority.
        let mut made_final = program_data(457_292_125, Some(key), 64);
        made_final[PROGRAM_DATA_AUTHORITY_TAG_OFFSET] = 0;
        assert_eq!(&made_final[13..PROGRAM_DATA_HEADER_LEN], key.as_ref());
        let decoded = ProgramData::try_deserialize(&mut made_final.as_slice()).unwrap();
        assert_eq!(decoded.upgrade_authority_address, None);
        assert_eq!(program_data_upgrade_authority(&made_final), None);
    }

    #[test]
    fn bytes_that_are_no_program_data_header_record_no_authority() {
        let key = Pubkey::new_unique();
        let good = program_data(1, Some(key), 0);
        // One byte short.
        assert_eq!(program_data_upgrade_authority(&good[..44]), None);
        assert_eq!(program_data_upgrade_authority(&[]), None);
        // The loader's other states: Uninitialized 0, Buffer 1, Program 2.
        for tag in [0u8, 1, 2, 4] {
            let mut other = good.clone();
            other[0] = tag;
            assert_eq!(program_data_upgrade_authority(&other), None, "tag {tag}");
        }
        // An option byte bincode would refuse.
        let mut bad_option = good.clone();
        bad_option[12] = 2;
        assert_eq!(program_data_upgrade_authority(&bad_option), None);
        assert!(ProgramData::try_deserialize(&mut bad_option.as_slice()).is_err());
        assert_eq!(program_data_upgrade_authority(&good), Some(key));
    }

    fn params(given: [Option<Pubkey>; 5]) -> SetUsdcRaydiumParams {
        SetUsdcRaydiumParams {
            raydium_cpmm_program: given[0],
            raydium_amm_config: given[1],
            raydium_create_pool_fee: given[2],
            raydium_lock_program: given[3],
            raydium_lock_authority: given[4],
        }
    }

    #[test]
    fn the_params_name_the_five_fields_in_the_config_s_order() {
        let keys: [Pubkey; 5] = std::array::from_fn(|_| Pubkey::new_unique());
        let all = params(keys.map(Some));
        assert_eq!(all.applied_to([Pubkey::default(); 5]), keys);
        // Borsh: an option byte and the key for each given field, one 0 byte for each left out, as
        // a client encodes them.
        let bytes = all.try_to_vec().unwrap();
        assert_eq!(bytes.len(), 5 * 33);
        for (i, key) in keys.iter().enumerate() {
            assert_eq!(bytes[i * 33], 1);
            assert_eq!(&bytes[i * 33 + 1..(i + 1) * 33], key.as_ref());
        }
        assert_eq!(params([None; 5]).try_to_vec().unwrap(), [0u8; 5]);
    }

    #[test]
    fn a_field_left_out_keeps_the_config_s_and_reads_no_tier() {
        let current: [Pubkey; 5] = std::array::from_fn(|_| Pubkey::new_unique());
        let lock = Pubkey::new_unique();
        let only_the_lock = params([None, None, None, Some(lock), None]);
        assert_eq!(
            only_the_lock.applied_to(current),
            [current[0], current[1], current[2], lock, current[4]]
        );
        assert!(!only_the_lock.moves_the_tier());
        assert!(!params([None, None, Some(lock), None, Some(lock)]).moves_the_tier());
        // The CPMM or the tier: read, against the pair the config will hold.
        assert!(params([Some(lock), None, None, None, None]).moves_the_tier());
        assert!(params([None, Some(lock), None, None, None]).moves_the_tier());
        assert_eq!(params([None; 5]).applied_to(current), current);
    }

    #[test]
    fn no_address_is_written_back_to_all_zero() {
        for i in 0..5 {
            let mut given = [None; 5];
            given[i] = Some(Pubkey::default());
            assert!(params(given).unsets_one(), "field {i}");
        }
        assert!(!params([None; 5]).unsets_one());
        assert!(!params([Some(Pubkey::new_unique()); 5]).unsets_one());
    }
}
