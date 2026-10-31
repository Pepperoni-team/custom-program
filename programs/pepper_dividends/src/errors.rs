use anchor_lang::prelude::*;

/// ⚠️ Append-only from the first deploy: Anchor numbers these from 6000 in order
/// and the TypeScript side matches them by number. Until then (nothing is
/// deployed) the order is spec §6's, with `ExpiryPassed` dropped, since nothing
/// raises it, and `MonthAlreadyFunded`, `MonthHasClaims`, `PauseEndTooEarly` and
/// `ApproverIsFunder` appended (2026-10-23), then `ClaimsHeld` (2026-10-26), then
/// the internal audit's `PauseTooShort`, `UnsupportedVersion` and `NotRehomable`
/// (2026-10-29), devnet's deploy numbering every one before them as it was.
#[error_code]
pub enum DividendsError {
    #[msg("Funding is paused")]
    Paused,
    #[msg("Claims are paused")]
    ClaimsPaused,
    #[msg("A claims pause may end at most 7 days from now")]
    PauseTooLong,
    #[msg("Signer is not authorized for this action")]
    Unauthorized,
    #[msg("Account does not match the expected one")]
    AccountMismatch,
    #[msg("Amount must be greater than zero")]
    ZeroAmount,
    #[msg("Numeric overflow")]
    MathOverflow,
    #[msg("A month needs between 1 and 65,536 leaves")]
    InvalidLeafCount,
    #[msg("Leaf index is outside this month's claim registry")]
    InvalidLeafIndex,
    #[msg("Merkle proof verification failed")]
    InvalidProof,
    #[msg("This dividend has already been claimed")]
    AlreadyClaimed,
    #[msg("Claim would exceed the month's funded total")]
    ExceedsFundedTotal,
    #[msg("A month may expire at most 400 days from its funding")]
    ExpiryTooFar,
    #[msg("The month is neither expired nor fully claimed")]
    NotExpired,
    #[msg("Only Fanex's two funders or the curve's creator may fund a month")]
    NotAFunder,
    #[msg("pepper_curve still holds a live distributor for this token and month")]
    LegacyMonthLive,
    #[msg("The curve is neither a Curve nor a CurveUsdc of pepper_curve")]
    CurveUnreadable,
    #[msg("A month must expire at least 364 days from its funding")]
    ExpiryTooSoon,
    #[msg("Claims were paused too recently for an expired month to close")]
    ClaimsPausedRecently,
    #[msg("This month was already funded here: it is open or closed")]
    MonthAlreadyFunded,
    #[msg("A month with a claim cannot be voided")]
    MonthHasClaims,
    #[msg("A claims pause may only start or extend, never end before now or the current end")]
    PauseEndTooEarly,
    #[msg("The approver must be a key of its own, neither Fanex funder")]
    ApproverIsFunder,
    #[msg("This month's claims are held")]
    ClaimsHeld,
    #[msg("A claims pause or hold must end at least an hour from now")]
    PauseTooShort,
    #[msg("The account's layout version is not one this program reads")]
    UnsupportedVersion,
    #[msg("This month records no Fanex payee to re-home, or the config names no payer")]
    NotRehomable,
}

#[cfg(test)]
mod tests {
    use super::DividendsError;

    /// 6000 + the variant's index, the number a client maps: a variant inserted
    /// anywhere but the end would renumber every one after it.
    #[test]
    fn the_codes_are_pinned_in_order() {
        let cases = [
            (DividendsError::Paused, 6000, "Paused"),
            (DividendsError::ClaimsPaused, 6001, "ClaimsPaused"),
            (DividendsError::PauseTooLong, 6002, "PauseTooLong"),
            (DividendsError::Unauthorized, 6003, "Unauthorized"),
            (DividendsError::AccountMismatch, 6004, "AccountMismatch"),
            (DividendsError::ZeroAmount, 6005, "ZeroAmount"),
            (DividendsError::MathOverflow, 6006, "MathOverflow"),
            (DividendsError::InvalidLeafCount, 6007, "InvalidLeafCount"),
            (DividendsError::InvalidLeafIndex, 6008, "InvalidLeafIndex"),
            (DividendsError::InvalidProof, 6009, "InvalidProof"),
            (DividendsError::AlreadyClaimed, 6010, "AlreadyClaimed"),
            (DividendsError::ExceedsFundedTotal, 6011, "ExceedsFundedTotal"),
            (DividendsError::ExpiryTooFar, 6012, "ExpiryTooFar"),
            (DividendsError::NotExpired, 6013, "NotExpired"),
            (DividendsError::NotAFunder, 6014, "NotAFunder"),
            (DividendsError::LegacyMonthLive, 6015, "LegacyMonthLive"),
            (DividendsError::CurveUnreadable, 6016, "CurveUnreadable"),
            (DividendsError::ExpiryTooSoon, 6017, "ExpiryTooSoon"),
            (DividendsError::ClaimsPausedRecently, 6018, "ClaimsPausedRecently"),
            (DividendsError::MonthAlreadyFunded, 6019, "MonthAlreadyFunded"),
            (DividendsError::MonthHasClaims, 6020, "MonthHasClaims"),
            (DividendsError::PauseEndTooEarly, 6021, "PauseEndTooEarly"),
            (DividendsError::ApproverIsFunder, 6022, "ApproverIsFunder"),
            (DividendsError::ClaimsHeld, 6023, "ClaimsHeld"),
            (DividendsError::PauseTooShort, 6024, "PauseTooShort"),
            (DividendsError::UnsupportedVersion, 6025, "UnsupportedVersion"),
            (DividendsError::NotRehomable, 6026, "NotRehomable"),
        ];
        for (error, code, name) in cases {
            assert_eq!(u32::from(error), code, "{name}");
            assert_eq!(error.name(), name);
        }
    }

    #[test]
    fn an_error_reaches_anchor_with_its_code_and_name() {
        let err: anchor_lang::error::Error = DividendsError::LegacyMonthLive.into();
        match err {
            anchor_lang::error::Error::AnchorError(e) => {
                assert_eq!(e.error_code_number, 6015);
                assert_eq!(e.error_name, "LegacyMonthLive");
            }
            other => panic!("expected an AnchorError, got {other:?}"),
        }
    }
}
