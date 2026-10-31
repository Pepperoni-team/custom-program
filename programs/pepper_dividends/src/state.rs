//! On-chain account state (spec §3). ⚠️ A stored layout is a contract with every
//! account already on chain: a new field takes its bytes from `reserved`, never
//! reorders. Each account opens with `version` (`ACCOUNT_VERSION`) and its bump,
//! and its size and offsets are pinned by the tests below.

use anchor_lang::prelude::*;

use crate::constants::{DISTRIBUTOR_SEED, MAX_DISTRIBUTOR_LEAVES};
use crate::validation::claim_registry_bytes;

/// The program's one config (PDA: [CONFIG_SEED]), 403 bytes on chain (8 + 395).
#[account]
#[derive(InitSpace)]
pub struct DividendsConfig {
    pub version: u8,
    pub bump: u8,
    /// Stops `init_distributor`: the pauser or the admin sets it, the admin
    /// alone clears it.
    pub paused: bool,
    /// Unix seconds: a claim before it is refused (`ClaimsPaused`). Kept once it
    /// has passed, as the time claims last opened again, which an expired month's
    /// close reads (`CLAIMS_OPEN_BEFORE_CLOSE_SECONDS`); 0 on a fresh config.
    pub claims_paused_until: i64,
    /// The upgrade Squad's vault, never the multisig account: every config
    /// change, the funding unpause, `resume_claims` and `void_distributor`.
    pub admin: Pubkey,
    /// A hot key: pauses funding and claims, never resumes either.
    pub pauser: Pubkey,
    /// Co-signs every month's funding: a key of its own, neither funder
    /// (`ApproverIsFunder`, Rick, 2026-10-21).
    pub approver: Pubkey,
    /// Closes an expired month, beside its funder.
    pub closer: Pubkey,
    /// Set once: the classic SPL Token USDC, 6 decimals.
    pub usdc_mint: Pubkey,
    /// Fanex's funder of a month, beside the dividend payer. Which key is open:
    /// the receive-only fee wallet signs nothing (all-zero or the dividend payer
    /// proposed), and never the production hot key.
    pub pepper_funder: Pubkey,
    /// The dividend payer, the funder of Fanex's months.
    pub payer_funder: Pubkey,
    /// Set once: `pepper_curve`, whose curves and v1 and v2 distributors
    /// `init_distributor` reads by their seeds.
    pub curve_program: Pubkey,
    /// Room to grow without a layout break.
    pub reserved: [u8; 128],
}

/// One token's month (PDA: [DISTRIBUTOR_SEED, mint, month as i64 LE]): the root
/// its claims prove against and the claims made. Its µUSDC sits in its vault, the
/// associated USDC account of this PDA. A header of `HEADER_SPACE` bytes, then
/// one bit per leaf.
#[account]
pub struct UsdcDistributor {
    pub version: u8,
    pub bump: u8,
    pub mint: Pubkey,
    /// The month's key (YYYYMM), as in `pepper_curve`: not validated, as there.
    pub month: i64,
    /// Root over this program's leaves (`math::dividend_leaf`).
    pub merkle_root: [u8; 32],
    /// µUSDC the funding moved into the vault.
    pub total_amount: u64,
    /// µUSDC claimed so far, never above `total_amount`.
    pub claimed_amount: u64,
    pub funder: Pubkey,
    /// The funder's USDC ATA, where the close and the void send the vault's
    /// whole balance.
    pub funder_usdc_account: Pubkey,
    /// Who paid the rents, and gets the vault's and the distributor's back.
    pub rent_payer: Pubkey,
    /// Unix seconds: from then the funder or the closer may close the month.
    pub expires_at: i64,
    /// Leaves the root commits to; `claimed` holds one bit for each.
    pub leaf_count: u32,
    /// Unix seconds: this month's claims are refused before it (`ClaimsHeld`),
    /// the hold the pauser or the admin sets on this month alone (Rick,
    /// 2026-10-26). The funding writes the end of its review window
    /// (`REVIEW_WINDOW_SECONDS`, 2026-10-29); kept once it has passed, as when this
    /// month's claims last opened again, which its close reads. Taken from the
    /// reserved bytes on 2026-10-26.
    pub claims_held_until: i64,
    /// Whether the funder was one of Fanex's two funders at the funding
    /// (`is_pepper_funder`): only then does `rehome_distributor` move the funder and
    /// its USDC account, so a creator's leftovers never move. False in every month
    /// funded before 2026-10-29. Taken from the reserved bytes on 2026-10-29.
    pub funded_by_fanex: bool,
    /// Whether the config's dividend payer paid the rents (every funding since
    /// 2026-10-29, which refuses any other rent payer): only then does
    /// `rehome_distributor` move the rent payer. Taken from the reserved bytes then.
    pub rents_by_payer: bool,
    /// Room to grow without a layout break, before the bitmap, whose length varies.
    pub reserved: [u8; 54],
    /// Bit `index % 8` of byte `index / 8`, least significant first, set once
    /// leaf `index` has claimed: exactly ⌈leaf_count / 8⌉ bytes.
    pub claimed: Vec<u8>,
}

impl UsdcDistributor {
    /// The discriminator, every field before `claimed`, and the bitmap's 4-byte length.
    pub const HEADER_SPACE: usize = 8 + 1 + 1 + 32 + 8 + 32 + 8 + 8 + 32 + 32 + 32 + 8 + 4 + 8 + 1 + 1 + 54 + 4;

    /// The account's size for `leaf_count` leaves, discriminator included. Clamped
    /// to `MAX_DISTRIBUTOR_LEAVES`, so an absurd count reaches the handler's
    /// `InvalidLeafCount` instead of failing the allocation first.
    pub fn space(leaf_count: u32) -> usize {
        Self::HEADER_SPACE + claim_registry_bytes(leaf_count.min(MAX_DISTRIBUTOR_LEAVES))
    }
}

/// Owned copies of a distributor's seeds, so a handler can sign for its vault
/// while it still mutates the distributor (as `pepper_raise`'s `RaiseSeeds`).
pub struct DistributorSeeds {
    mint: Pubkey,
    month: [u8; 8],
    bump: [u8; 1],
}

impl DistributorSeeds {
    pub fn of(distributor: &UsdcDistributor) -> Self {
        Self { mint: distributor.mint, month: distributor.month.to_le_bytes(), bump: [distributor.bump] }
    }

    pub fn seeds(&self) -> [&[u8]; 4] {
        [DISTRIBUTOR_SEED, self.mint.as_ref(), &self.month, &self.bump]
    }
}

/// A month's record (PDA: [MONTH_SEED, mint, month as i64 LE]), 75 bytes on chain
/// (8 + 67). Created with the month and never closed: a month closed here never
/// opens again, a voided one may.
#[account]
#[derive(InitSpace)]
pub struct MonthRecord {
    pub version: u8,
    pub bump: u8,
    /// `MONTH_STATUS_OPEN`, `MONTH_STATUS_CLOSED` or `MONTH_STATUS_VOIDED`; 0 only
    /// inside the funding that creates the record.
    pub status: u8,
    /// The distributor the month's last funding opened.
    pub distributor: Pubkey,
    /// Unix seconds of that funding.
    pub funded_at: i64,
    /// Room to grow without a layout break.
    pub reserved: [u8; 24],
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::solana_program::hash::hash;

    fn key(byte: u8) -> Pubkey {
        Pubkey::new_from_array([byte; 32])
    }

    /// What the program writes for `account`: its discriminator, then its fields.
    fn account_bytes<T: AccountSerialize>(account: &T) -> Vec<u8> {
        let mut bytes = Vec::new();
        account.try_serialize(&mut bytes).unwrap();
        bytes
    }

    fn assert_fields_at(bytes: &[u8], fields: &[(&str, usize, Vec<u8>)]) {
        for (name, offset, expected) in fields {
            assert_eq!(&bytes[*offset..*offset + expected.len()], expected.as_slice(), "{name} at {offset}");
        }
    }

    fn sample_config() -> DividendsConfig {
        DividendsConfig {
            version: 1,
            bump: 254,
            paused: true,
            claims_paused_until: -0x0102_0304_0506_0708,
            admin: key(1),
            pauser: key(2),
            approver: key(3),
            closer: key(4),
            usdc_mint: key(5),
            pepper_funder: key(6),
            payer_funder: key(7),
            curve_program: key(8),
            reserved: [0xAB; 128],
        }
    }

    fn sample_distributor(leaf_count: u32) -> UsdcDistributor {
        UsdcDistributor {
            version: 1,
            bump: 253,
            mint: key(21),
            month: 202_611,
            merkle_root: [0x5A; 32],
            total_amount: 0x1111_1111_1111_1101,
            claimed_amount: 0x1111_1111_1111_1102,
            funder: key(22),
            funder_usdc_account: key(23),
            rent_payer: key(24),
            expires_at: 0x2222_2222_2222_2203,
            leaf_count,
            claims_held_until: 0x4444_4444_4444_4405,
            funded_by_fanex: true,
            rents_by_payer: false,
            reserved: [0xCD; 54],
            claimed: vec![0xEE; claim_registry_bytes(leaf_count)],
        }
    }

    fn sample_record() -> MonthRecord {
        MonthRecord {
            version: 1,
            bump: 252,
            status: crate::constants::MONTH_STATUS_VOIDED,
            distributor: key(31),
            funded_at: 0x3333_3333_3333_3304,
            reserved: [0xEF; 24],
        }
    }

    /// A change here is a layout break for every account already on chain: take
    /// the bytes from `reserved` instead. Counted by hand, field by field (Pubkey
    /// 32, u64/i64 8, u32 4, u8/bool 1, [u8; n] n): DividendsConfig 3 x 1 + 8 +
    /// 8 x 32 + 128 = 395; the distributor's header 8 + 2 x 1 + 32 + 8 + 32 + 2 x
    /// 8 + 3 x 32 + 8 + 4 + 8 + 2 x 1 + 54 + 4 = 274, the hold's 8 bytes taken from
    /// the 64 reserved ones (2026-10-26), the two funding flags' 2 bytes from the 56
    /// left (2026-10-29); MonthRecord 3 x 1 + 32 + 8 + 24 = 67.
    #[test]
    fn layouts_are_pinned() {
        assert_eq!(DividendsConfig::INIT_SPACE, 395);
        assert_eq!(3 + 8 + 8 * 32 + 128, 395);
        assert_eq!(UsdcDistributor::HEADER_SPACE, 274);
        assert_eq!(8 + 2 + 32 + 8 + 32 + 2 * 8 + 3 * 32 + 8 + 4 + 8 + 2 + 54 + 4, 274);
        assert_eq!(MonthRecord::INIT_SPACE, 67);
        assert_eq!(3 + 32 + 8 + 24, 67);
    }

    #[test]
    fn the_discriminators_are_pinned() {
        // The first 8 bytes of sha256("account:<Name>"): a rename moves them, and
        // every account of that type already on chain stops decoding.
        assert_eq!(DividendsConfig::DISCRIMINATOR, &[20, 198, 109, 122, 226, 121, 191, 253]);
        assert_eq!(UsdcDistributor::DISCRIMINATOR, &[42, 186, 165, 125, 5, 62, 139, 212]);
        assert_eq!(MonthRecord::DISCRIMINATOR, &[253, 225, 197, 68, 4, 50, 181, 176]);
        for (name, discriminator) in [
            ("DividendsConfig", DividendsConfig::DISCRIMINATOR),
            ("UsdcDistributor", UsdcDistributor::DISCRIMINATOR),
            ("MonthRecord", MonthRecord::DISCRIMINATOR),
        ] {
            assert_eq!(&hash(format!("account:{name}").as_bytes()).to_bytes()[..8], discriminator, "{name}");
        }
        // None reads as one of `pepper_curve`'s curves, whose bytes the creator read checks.
        for discriminator in [DividendsConfig::DISCRIMINATOR, UsdcDistributor::DISCRIMINATOR, MonthRecord::DISCRIMINATOR] {
            assert_ne!(discriminator, crate::constants::CURVE_DISCRIMINATOR);
            assert_ne!(discriminator, crate::constants::CURVE_USDC_DISCRIMINATOR);
        }
    }

    #[test]
    fn every_config_field_sits_at_its_offset() {
        let bytes = account_bytes(&sample_config());
        assert_eq!(bytes.len(), 403);
        assert_eq!(&bytes[..8], DividendsConfig::DISCRIMINATOR);
        let k = |byte: u8| key(byte).to_bytes().to_vec();
        assert_fields_at(
            &bytes,
            &[
                ("version", 8, vec![1]),
                ("bump", 9, vec![254]),
                ("paused", 10, vec![1]),
                ("claims_paused_until", 11, (-0x0102_0304_0506_0708i64).to_le_bytes().to_vec()),
                ("admin", 19, k(1)),
                ("pauser", 51, k(2)),
                ("approver", 83, k(3)),
                ("closer", 115, k(4)),
                ("usdc_mint", 147, k(5)),
                ("pepper_funder", 179, k(6)),
                ("payer_funder", 211, k(7)),
                ("curve_program", 243, k(8)),
                ("reserved", 275, vec![0xAB; 128]),
            ],
        );
    }

    #[test]
    fn every_distributor_field_sits_at_its_offset() {
        let bytes = account_bytes(&sample_distributor(14));
        assert_eq!(bytes.len(), UsdcDistributor::space(14));
        assert_eq!(&bytes[..8], UsdcDistributor::DISCRIMINATOR);
        let k = |byte: u8| key(byte).to_bytes().to_vec();
        let u = |v: u64| v.to_le_bytes().to_vec();
        let i = |v: i64| v.to_le_bytes().to_vec();
        assert_fields_at(
            &bytes,
            &[
                ("version", 8, vec![1]),
                ("bump", 9, vec![253]),
                ("mint", 10, k(21)),
                ("month", 42, i(202_611)),
                ("merkle_root", 50, vec![0x5A; 32]),
                ("total_amount", 82, u(0x1111_1111_1111_1101)),
                ("claimed_amount", 90, u(0x1111_1111_1111_1102)),
                ("funder", 98, k(22)),
                ("funder_usdc_account", 130, k(23)),
                ("rent_payer", 162, k(24)),
                ("expires_at", 194, i(0x2222_2222_2222_2203)),
                ("leaf_count", 202, 14u32.to_le_bytes().to_vec()),
                ("claims_held_until", 206, i(0x4444_4444_4444_4405)),
                ("funded_by_fanex", 214, vec![1]),
                ("rents_by_payer", 215, vec![0]),
                ("reserved", 216, vec![0xCD; 54]),
                // The bitmap: its u32 length, then one bit per leaf.
                ("claimed's length", 270, 2u32.to_le_bytes().to_vec()),
                ("claimed", 274, vec![0xEE; 2]),
            ],
        );
    }

    #[test]
    fn every_record_field_sits_at_its_offset() {
        let bytes = account_bytes(&sample_record());
        assert_eq!(bytes.len(), 75);
        assert_eq!(&bytes[..8], MonthRecord::DISCRIMINATOR);
        assert_fields_at(
            &bytes,
            &[
                ("version", 8, vec![1]),
                ("bump", 9, vec![252]),
                ("status", 10, vec![3]),
                ("distributor", 11, key(31).to_bytes().to_vec()),
                ("funded_at", 43, 0x3333_3333_3333_3304i64.to_le_bytes().to_vec()),
                ("reserved", 51, vec![0xEF; 24]),
            ],
        );
    }

    #[test]
    fn a_distributor_takes_exactly_its_space_for_any_leaf_count() {
        for leaf_count in [1u32, 7, 8, 9, 14, 1_000, MAX_DISTRIBUTOR_LEAVES - 1, MAX_DISTRIBUTOR_LEAVES] {
            let bytes = account_bytes(&sample_distributor(leaf_count));
            assert_eq!(bytes.len(), UsdcDistributor::space(leaf_count), "{leaf_count} leaves");
        }
        assert_eq!(UsdcDistributor::space(1), 275);
        // A full registry: 8 KiB of bitmap, inside the 10 KiB an account created by a
        // program may take.
        assert_eq!(UsdcDistributor::space(MAX_DISTRIBUTOR_LEAVES), 8_466);
        assert!(UsdcDistributor::space(MAX_DISTRIBUTOR_LEAVES) <= 10_240);
    }

    #[test]
    fn the_space_clamps_an_absurd_leaf_count_so_the_handler_can_refuse_it() {
        let full = UsdcDistributor::space(MAX_DISTRIBUTOR_LEAVES);
        assert_eq!(UsdcDistributor::space(MAX_DISTRIBUTOR_LEAVES + 1), full);
        assert_eq!(UsdcDistributor::space(u32::MAX), full);
        // Zero leaves allocate the header alone; the handler refuses the count.
        assert_eq!(UsdcDistributor::space(0), UsdcDistributor::HEADER_SPACE);
    }

    #[test]
    fn an_all_zero_account_decodes_with_every_reserved_byte() {
        // As `init` finds a fixed-size account: zeroed, `reserved` included.
        let config = DividendsConfig::try_from_slice(&[0u8; DividendsConfig::INIT_SPACE]).unwrap();
        assert_eq!((config.version, config.claims_paused_until, config.reserved.len()), (0, 0, 128));
        let record = MonthRecord::try_from_slice(&[0u8; MonthRecord::INIT_SPACE]).unwrap();
        assert_eq!((record.status, record.reserved.len()), (0, 24));
        // A distributor's zeroed header reads as no leaf, no hold and an empty bitmap.
        let distributor = UsdcDistributor::try_from_slice(&[0u8; UsdcDistributor::HEADER_SPACE - 8]).unwrap();
        assert_eq!(
            (distributor.leaf_count, distributor.claims_held_until, distributor.claimed.len(), distributor.reserved.len()),
            (0, 0, 0, 54)
        );
        // And, as every month funded before 2026-10-29 reads, with neither funding flag: a month
        // no re-home may touch.
        assert_eq!((distributor.funded_by_fanex, distributor.rents_by_payer), (false, false));
    }

    #[test]
    fn a_funding_flag_is_one_byte_of_zero_or_one_and_nothing_else_decodes() {
        // The flags took two bytes that were reserved and zero in every month already on chain,
        // so those decode as false; a byte other than 0 or 1 is no bool, and no month at all.
        let mut bytes = account_bytes(&sample_distributor(3));
        bytes[215] = 1;
        let read = UsdcDistributor::try_deserialize(&mut bytes.as_slice()).unwrap();
        assert_eq!((read.funded_by_fanex, read.rents_by_payer), (true, true));
        bytes[214] = 2;
        assert!(UsdcDistributor::try_deserialize(&mut bytes.as_slice()).is_err());
    }

    #[test]
    fn the_signer_seeds_derive_the_distributors_own_address() {
        let mint = key(41);
        let month: i64 = 202_611;
        let (address, bump) =
            Pubkey::find_program_address(&[DISTRIBUTOR_SEED, mint.as_ref(), &month.to_le_bytes()], &crate::ID);
        let distributor = UsdcDistributor { mint, month, bump, ..sample_distributor(1) };
        let seeds = DistributorSeeds::of(&distributor);
        assert_eq!(Pubkey::create_program_address(&seeds.seeds(), &crate::ID).unwrap(), address);
        // Another month of the same token signs for another address.
        let other = DistributorSeeds::of(&UsdcDistributor { month: month + 1, ..distributor });
        assert_ne!(Pubkey::create_program_address(&other.seeds(), &crate::ID).ok(), Some(address));
    }

    /// What `Account::<T>::try_from` answers for an account holding `bytes`
    /// owned by `owner`: Ok, or the Anchor error's number.
    fn load_as<T>(mut bytes: Vec<u8>, owner: Pubkey) -> std::result::Result<(), u32>
    where
        T: AccountSerialize + AccountDeserialize + Owner + Clone,
    {
        let key = Pubkey::new_unique();
        let mut lamports = 1_000_000u64;
        let info = AccountInfo::new(&key, false, true, &mut lamports, &mut bytes, &owner, false, 0);
        match Account::<T>::try_from(&info) {
            Ok(_) => Ok(()),
            Err(anchor_lang::error::Error::AnchorError(e)) => Err(e.error_code_number),
            Err(other) => panic!("expected an AnchorError, got {other:?}"),
        }
    }

    #[test]
    fn each_account_loads_as_itself_and_never_as_another() {
        let mismatch = u32::from(anchor_lang::error::ErrorCode::AccountDiscriminatorMismatch);
        assert_eq!(mismatch, 3002);
        let (config, distributor, record) =
            (account_bytes(&sample_config()), account_bytes(&sample_distributor(14)), account_bytes(&sample_record()));
        assert_eq!(load_as::<DividendsConfig>(config.clone(), crate::ID), Ok(()));
        assert_eq!(load_as::<UsdcDistributor>(distributor.clone(), crate::ID), Ok(()));
        assert_eq!(load_as::<MonthRecord>(record.clone(), crate::ID), Ok(()));
        assert_eq!(load_as::<UsdcDistributor>(record.clone(), crate::ID), Err(mismatch));
        assert_eq!(load_as::<MonthRecord>(distributor.clone(), crate::ID), Err(mismatch));
        assert_eq!(load_as::<DividendsConfig>(distributor.clone(), crate::ID), Err(mismatch));
        // The same bytes under another program's ownership load as nothing of ours.
        let wrong_owner = u32::from(anchor_lang::error::ErrorCode::AccountOwnedByWrongProgram);
        assert_eq!(wrong_owner, 3007);
        assert_eq!(load_as::<UsdcDistributor>(distributor, Pubkey::new_unique()), Err(wrong_owner));
    }
}
