use anchor_lang::prelude::*;

#[error_code]
pub enum FanexError {
    #[msg("Trading is globally halted by the Fanex kill-switch")]
    GloballyPaused,
    #[msg("This curve is paused")]
    CurvePaused,
    #[msg("Curve is not in the Bonding phase")]
    NotBonding,
    #[msg("Curve has not reached its graduation threshold yet")]
    NotGraduatable,
    #[msg("Curve is not awaiting graduation")]
    NotGraduating,
    #[msg("Numeric overflow")]
    MathOverflow,
    #[msg("Amount must be greater than zero")]
    ZeroAmount,
    #[msg("Slippage exceeded: output below the requested minimum")]
    SlippageExceeded,
    #[msg("Curve token reserves are exhausted")]
    CurveComplete,
    #[msg("Insufficient reserves in the curve vault")]
    InsufficientReserves,
    #[msg("Creator target below the minimum allowed")]
    CreatorTargetTooLow,
    #[msg("CPMM seed outside the allowed range")]
    CpmmSeedOutOfRange,
    #[msg("Fee split basis points are invalid (must leave a positive curve share)")]
    InvalidSplitBps,
    #[msg("Creator + Fanex caps must equal the creator target")]
    InvalidCaps,
    #[msg("Token supply allocation does not sum to the fixed total supply")]
    InvalidSupplyAllocation,
    #[msg("Vesting allocation does not match the 2% rule")]
    InvalidVestingAllocation,
    #[msg("A provided account did not match the value bound in curve/config state")]
    AccountMismatch,
    #[msg("Unauthorized: signer is not the required authority")]
    Unauthorized,
    #[msg("String field exceeds its maximum length")]
    StringTooLong,
    #[msg("Nothing is currently releasable")]
    NothingToRelease,
    #[msg("Merkle proof verification failed")]
    InvalidProof,
    #[msg("This dividend allocation has already been claimed")]
    AlreadyClaimed,
    #[msg("Claim would exceed the distributor's funded total")]
    ExceedsFundedTotal,
    #[msg("Distributor is not yet expired")]
    NotExpired,
    #[msg("This vesting account is not reassignable: its beneficiary is not the fan-led placeholder")]
    VestingNotReassignable,
    #[msg("Not a ClaimStatus account this program recognises")]
    ClaimStatusFormat,
    #[msg("The distributor is still open; its ClaimStatus is the double-claim guard")]
    DistributorStillOpen,
    #[msg("Rent must return to the recorded payer")]
    WrongRentRecipient,
    // ⚠️ Appended, never inserted: Anchor numbers these from 6000 in order, so a
    // variant added above would renumber every code the app already maps.
    #[msg("Curve creator can only be synced from the fan-led placeholder to an already-claimed vesting beneficiary")]
    CurveCreatorNotSyncable,
    #[msg("A curve whose creator is the keyless fan-led placeholder cannot take a creator fundraise split")]
    PlaceholderCannotRaise,
    #[msg("A claim registry needs between 1 and the maximum number of leaves")]
    InvalidLeafCount,
    #[msg("Leaf index is outside this distributor's claim registry")]
    InvalidLeafIndex,
    #[msg("A distributor may expire at most 400 days from now")]
    ExpiryTooFar,
    #[msg("In the first five minutes after launch a wallet may hold at most 1% of the supply")]
    LaunchWalletCapExceeded,
    #[msg("A curve may be launched without the launch cap only before it exists")]
    LaunchGuardAfterLaunch,
    #[msg("A curve launched from 2026-11-01 releases its vesting only once it has graduated")]
    VestingLockedUntilGraduation,
    // The USDC curve's own refusals (USDC curve design §10.7); everything else it
    // refuses with the codes above.
    #[msg("In the first five minutes after launch a buy delivers only to the signer's associated token account")]
    LaunchDestinationNotOwnAta,
    #[msg("The creator's USDC account must be the creator's own unfrozen USDC associated token account, closable only by the launch's payer")]
    CreatorUsdcAccountInvalid,
    #[msg("Only the pauser or the admin may pause, and only the admin may unpause")]
    NotPauser,
    #[msg("The pinned Raydium fee tier no longer charges 1%")]
    GraduationFeeTierChanged,
    #[msg("The Raydium pool has not been created yet")]
    PoolNotCreated,
    #[msg("The Raydium addresses of the USDC config are not set")]
    RaydiumNotSet,
    // The graduation's (2026-10-23, USDC curve design §10.3.9, §10.3.10): Raydium's switch that
    // stops pool creation on a tier, refused before any move.
    #[msg("The pinned Raydium fee tier no longer lets a pool be created")]
    PoolCreationDisabled,
    // The graduation that never fails (2026-10-24, USDC curve design §10.3.1, §10.3.9): a config
    // that would hand every Fee Key to nobody, and a caller's extra funding past its bound.
    #[msg("The Fee Key's owner of the USDC config may not be all-zero")]
    LpFeeOwnerUnset,
    #[msg("A graduation's extra funding is past its bound")]
    GraduationExtraFundingTooLarge,
    // The internal audit's fix round (2026-10-29, docs/security/INTERNAL-AUDIT-2026-10-29.md): a fee
    // account its owner could close or that is not the fee wallet's ATA, an admin handed to nobody,
    // a launch off the raise's relation, and a Raydium pool-creation fee past the program's bound.
    #[msg("The fee account must be the fee wallet's unfrozen USDC associated token account, closable only by the USDC config")]
    FeeUsdcAccountInvalid,
    #[msg("The admin may not be all-zero")]
    AdminUnset,
    #[msg("A USDC launch's raise terms are off the 94/6 relation to its target and threshold, or its target is past the ceiling")]
    LaunchTermsOutOfBounds,
    #[msg("Raydium's pool-creation fee is past the program's bound")]
    CreatePoolFeeTooHigh,
}

/// The math crate refuses with the program's own codes: a USDC handler maps a `quote_buy` or
/// `quote_sell` refusal with `.map_err(FanexError::from)?`, and the client reads the same number a
/// SOL refusal of that kind carries (USDC curve design §10.4, §10.7). The match has no wildcard, so
/// a variant added to the crate fails to compile here until it is mapped.
impl From<pepper_curve_math::MathError> for FanexError {
    fn from(e: pepper_curve_math::MathError) -> Self {
        use pepper_curve_math::MathError;
        match e {
            MathError::MathOverflow => FanexError::MathOverflow,
            MathError::ZeroAmount => FanexError::ZeroAmount,
            MathError::CurveComplete => FanexError::CurveComplete,
            MathError::InsufficientReserves => FanexError::InsufficientReserves,
        }
    }
}

#[cfg(test)]
mod math_error_tests {
    use super::FanexError;
    use pepper_curve_math::MathError;

    #[test]
    fn each_math_error_maps_to_the_program_code_of_its_name() {
        let cases = [
            (MathError::MathOverflow, 6005),
            (MathError::ZeroAmount, 6006),
            (MathError::CurveComplete, 6008),
            (MathError::InsufficientReserves, 6009),
        ];
        for (math, code) in cases {
            let mapped = FanexError::from(math);
            assert_eq!(mapped as u32 + 6000, code, "{}", math.name());
            assert_eq!(u32::from(mapped), code, "{}", math.name());
            assert_eq!(mapped.name(), math.name());
        }
    }

    #[test]
    fn a_math_error_reaches_anchor_with_its_code() {
        let err: anchor_lang::error::Error = FanexError::from(MathError::CurveComplete).into();
        match err {
            anchor_lang::error::Error::AnchorError(e) => {
                assert_eq!(e.error_code_number, 6008);
                assert_eq!(e.error_name, "CurveComplete");
            }
            other => panic!("expected an AnchorError, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod usdc_error_tests {
    use super::FanexError;

    /// Appended after 6035 in the spec's order (USDC curve design §10.7): a client
    /// maps these numbers, so a variant inserted above them would renumber every one.
    #[test]
    fn the_usdc_errors_follow_6035_in_order() {
        assert_eq!(u32::from(FanexError::VestingLockedUntilGraduation), 6035);
        let cases = [
            (
                FanexError::LaunchDestinationNotOwnAta,
                6036,
                "LaunchDestinationNotOwnAta",
            ),
            (
                FanexError::CreatorUsdcAccountInvalid,
                6037,
                "CreatorUsdcAccountInvalid",
            ),
            (FanexError::NotPauser, 6038, "NotPauser"),
            (
                FanexError::GraduationFeeTierChanged,
                6039,
                "GraduationFeeTierChanged",
            ),
            (FanexError::PoolNotCreated, 6040, "PoolNotCreated"),
            (FanexError::RaydiumNotSet, 6041, "RaydiumNotSet"),
        ];
        for (error, code, name) in cases {
            assert_eq!(u32::from(error), code, "{name}");
            assert_eq!(error.name(), name);
        }
    }

    #[test]
    fn insufficient_reserves_names_no_currency() {
        // Raised by both curves since the USDC one: its message says neither SOL nor USDC.
        let msg = FanexError::InsufficientReserves.to_string();
        assert_eq!(msg, "Insufficient reserves in the curve vault");
        assert!(!msg.contains("SOL"));
        assert_eq!(u32::from(FanexError::InsufficientReserves), 6009);
    }
}

#[cfg(test)]
mod graduation_error_tests {
    use super::FanexError;

    /// The graduation's own refusal, appended after 6041 (2026-10-23): a tier Raydium stopped
    /// creating pools on, refused by `set_usdc_raydium` and by `graduate_usdc_pool`.
    #[test]
    fn pool_creation_disabled_follows_raydium_not_set() {
        assert_eq!(u32::from(FanexError::RaydiumNotSet), 6041);
        assert_eq!(u32::from(FanexError::PoolCreationDisabled), 6042);
        assert_eq!(
            FanexError::PoolCreationDisabled.name(),
            "PoolCreationDisabled"
        );
    }

    /// Appended after 6042 (2026-10-24): `initialize_usdc_config` and `update_usdc_config`
    /// refusing an all-zero `lp_fee_owner`, and either graduation step an extra past its bound.
    #[test]
    fn the_never_failing_graduation_s_errors_follow_6042() {
        assert_eq!(u32::from(FanexError::LpFeeOwnerUnset), 6043);
        assert_eq!(FanexError::LpFeeOwnerUnset.name(), "LpFeeOwnerUnset");
        assert_eq!(u32::from(FanexError::GraduationExtraFundingTooLarge), 6044);
        assert_eq!(
            FanexError::GraduationExtraFundingTooLarge.name(),
            "GraduationExtraFundingTooLarge"
        );
    }

    /// Appended after 6044 by the internal audit's fix round (2026-10-29).
    #[test]
    fn the_audit_fix_round_s_errors_follow_6044() {
        let cases = [
            (FanexError::FeeUsdcAccountInvalid, 6045, "FeeUsdcAccountInvalid"),
            (FanexError::AdminUnset, 6046, "AdminUnset"),
            (FanexError::LaunchTermsOutOfBounds, 6047, "LaunchTermsOutOfBounds"),
            (FanexError::CreatePoolFeeTooHigh, 6048, "CreatePoolFeeTooHigh"),
        ];
        for (error, code, name) in cases {
            assert_eq!(u32::from(error), code, "{name}");
            assert_eq!(error.name(), name);
        }
    }
}
