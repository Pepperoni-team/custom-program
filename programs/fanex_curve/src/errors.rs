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
    #[msg("Insufficient SOL reserves in the curve vault")]
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
}
