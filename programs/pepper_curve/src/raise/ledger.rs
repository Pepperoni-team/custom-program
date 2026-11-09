//! The monthly ledger and the guarantee (spec §4.6, §4.7; amended spec §4):
//! declarations in order and not before the 15th, settlements in order, the
//! on-time streak, the guarantee's release, when a raise closes and where the
//! pool's USDC goes at close; since the internal audit of 2026-10-29, the
//! window a declaration can be corrected in and when a late payment refills
//! the guarantee.

use anchor_lang::prelude::*;

use crate::raise::calendar::{declare_open_ts, due_ts, SECONDS_PER_DAY};
use crate::raise::checked::sub;
use crate::raise::constants::*;
use crate::errors::FanexError;
use crate::raise::state::RaiseState;

pub fn open_amount(due: u64, paid: u64, covered: u64) -> Result<u64> {
    sub(sub(due, paid)?, covered)
}

pub fn check_declaration(month: u32, last_declared: u32, first_month: u32, term_end: u32, now: i64) -> Result<()> {
    require!(month == last_declared + 1, FanexError::MonthOutOfOrder);
    require!(month >= first_month && month <= term_end, FanexError::MonthOutOfTerm);
    require!(now >= declare_open_ts(month), FanexError::DeclarationTooEarly);
    Ok(())
}

pub fn check_settlement_order(month: u32, last_finalized: u32) -> Result<()> {
    require!(month == last_finalized + 1, FanexError::MonthOutOfOrder);
    Ok(())
}

pub fn next_streak(streak: u8, on_time: bool) -> u8 {
    if on_time {
        streak.saturating_add(1)
    } else {
        0
    }
}

pub fn guarantee_releasable(streak: u8, owed: u64, released: bool) -> bool {
    !released && owed == 0 && streak >= GUARANTEE_RELEASE_STREAK
}

pub fn has_open_months(oldest_open: u32, last_declared: u32) -> bool {
    oldest_open <= last_declared
}

/// M-01: only a closed raise without any quota can extinguish its oldest settled month.
pub fn month_write_off_allowed(state: RaiseState, units: u64, finalized: bool, month: u32, oldest: u32, last_finalized: u32) -> bool {
    state == RaiseState::Closed && units == 0 && finalized && month == oldest && month <= last_finalized
}

/// One persistent bit per term month, taken from Raise's reserved bytes. Terms have at most 60 months.
pub fn month_write_off_bit(month: u32, first: u32, last: u32) -> Result<u64> {
    require!(month >= first && month <= last && month - first < 64, FanexError::MonthNotExtinguishable);
    Ok(1u64 << (month - first))
}

/// A month is overdue from its due date itself (`calendar::due_ts`): from then
/// on a payment pays the arrears first and is late, and anyone settles it.
pub fn is_overdue(now: i64, due_at: i64) -> bool {
    now >= due_at
}

/// Whether a payment into the overdue month `month` may refill the guarantee with what is left
/// once that month is paid (internal audit of 2026-10-29): only while the month after it is not
/// settled yet, so the guarantee never holds money while the pointer stands on a settled month with
/// arrears, which no `settle_month` can cover again and `closable` would then refuse for ever.
pub fn refills_after_arrears(month: u32, last_finalized: u32) -> bool {
    month >= last_finalized
}

/// Can month `month` be declared again at `due` (internal audit of 2026-10-29)? Only before its due
/// date, when `settle_month` opens, so never once settled or covered; only while it is the last
/// declared month, as the calendar makes it until then; and never below what the creator already
/// paid into it, so no money moves back.
#[allow(clippy::too_many_arguments)]
pub fn check_redeclaration(
    now: i64,
    due_at: i64,
    finalized: bool,
    covered: u64,
    month: u32,
    last_declared: u32,
    paid: u64,
    due: u64,
) -> Result<()> {
    require!(
        now < due_at && !finalized && covered == 0 && month == last_declared && due >= paid,
        FanexError::MonthNotRedeclarable
    );
    Ok(())
}

/// The payment pointer once the last declared month `month` owes `open` after a re-declaration:
/// back onto it when it owes again and the pointer had stepped past it (a zero due, or a due paid in
/// full), past it when it owes nothing and the pointer stood on it; anywhere else it stays.
pub fn oldest_open_after_redeclaration(oldest_open: u32, month: u32, open: u64) -> u32 {
    if open > 0 && oldest_open == month + 1 {
        month
    } else if open == 0 && oldest_open == month {
        month + 1
    } else {
        oldest_open
    }
}

/// A `Month` account is finished, and closes (the 2026-10-21 design §3), once it
/// is finalized (`settle_month` ran on it), behind the payment pointer and at
/// or behind the last settled month: nothing reads it again.
pub fn month_closable(finalized: bool, month: u32, oldest_open: u32, last_finalized: u32) -> bool {
    finalized && month < oldest_open && month <= last_finalized
}

/// The `Raise` and its vaults close last, once nothing is owed from them (§3):
/// closed with no month open and no `Position` or `Month` account left, or
/// refunding with none left (a refunding raise never declared a month). Never
/// while raising or active.
pub fn raise_finished(state: RaiseState, oldest_open: u32, last_declared: u32, open_positions: u32, live_months: u16) -> bool {
    let nothing_left = open_positions == 0 && live_months == 0;
    match state {
        RaiseState::Closed => !has_open_months(oldest_open, last_declared) && nothing_left,
        RaiseState::Refunding => nothing_left,
        RaiseState::Raising | RaiseState::Active => false,
    }
}

/// The moment a raise becomes closable even if some months of its term were
/// never declared: CLOSE_FALLBACK_DAYS after the last month's due date, so the
/// pool's USDC and the guarantee never wait on the attester (ruling P1).
pub fn close_fallback_ts(term_end: u32) -> i64 {
    due_ts(term_end) + CLOSE_FALLBACK_DAYS * SECONDS_PER_DAY
}

/// A raise closes once its term is over: the last month declared and due, or,
/// whatever was declared, from `close_fallback_ts`. Either way the guarantee is
/// spent on any arrears first (`settle_month` is permissionless). Nothing else
/// ends it: there is no cap (amended spec D13, §4).
pub fn closable(last_declared: u32, term_end: u32, now: i64, open_months: bool, guarantee_balance: u64) -> bool {
    let term_over = (last_declared == term_end && now >= due_ts(term_end)) || now >= close_fallback_ts(term_end);
    term_over && (!open_months || guarantee_balance == 0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolDestination {
    Holders,
    Creator,
}

/// At close the pool's USDC goes to the quotas still held by fans. When the
/// pool holds every quota there is no holder left: every seller was already
/// paid, so it goes to the creator (amended spec §4; Review Focus 5).
pub fn pool_destination(fan_units: u64) -> PoolDestination {
    if fan_units > 0 {
        PoolDestination::Holders
    } else {
        PoolDestination::Creator
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOVEMBER_2026: u32 = 24_322;
    const TERM_END: u32 = NOVEMBER_2026 + 23;

    #[test]
    fn what_a_month_still_owes() {
        assert_eq!(open_amount(100, 30, 20).unwrap(), 50);
        assert_eq!(open_amount(100, 60, 40).unwrap(), 0);
    }

    #[test]
    fn the_first_month_is_declared_from_the_15th_of_the_next() {
        let open = declare_open_ts(NOVEMBER_2026);
        assert!(check_declaration(NOVEMBER_2026, NOVEMBER_2026 - 1, NOVEMBER_2026, TERM_END, open).is_ok());
        assert_eq!(
            check_declaration(NOVEMBER_2026, NOVEMBER_2026 - 1, NOVEMBER_2026, TERM_END, open - 1).unwrap_err(),
            error!(FanexError::DeclarationTooEarly)
        );
    }

    #[test]
    fn months_are_declared_in_order_and_inside_the_term() {
        let late = declare_open_ts(TERM_END + 1);
        assert_eq!(
            check_declaration(NOVEMBER_2026 + 1, NOVEMBER_2026 - 1, NOVEMBER_2026, TERM_END, late).unwrap_err(),
            error!(FanexError::MonthOutOfOrder)
        );
        assert_eq!(
            check_declaration(TERM_END + 1, TERM_END, NOVEMBER_2026, TERM_END, late).unwrap_err(),
            error!(FanexError::MonthOutOfTerm)
        );
    }

    #[test]
    fn months_are_settled_in_order() {
        assert!(check_settlement_order(NOVEMBER_2026, NOVEMBER_2026 - 1).is_ok());
        assert!(check_settlement_order(NOVEMBER_2026 + 1, NOVEMBER_2026 - 1).is_err());
    }

    #[test]
    fn the_streak_counts_consecutive_on_time_months() {
        assert_eq!(next_streak(11, true), 12);
        assert_eq!(next_streak(12, false), 0);
        assert_eq!(next_streak(u8::MAX, true), u8::MAX);
    }

    #[test]
    fn the_guarantee_is_released_after_twelve_clean_months() {
        assert!(guarantee_releasable(12, 0, false));
        assert!(!guarantee_releasable(11, 0, false));
        assert!(!guarantee_releasable(12, 1, false));
        assert!(!guarantee_releasable(12, 0, true));
    }

    #[test]
    fn open_months_exist_while_the_oldest_open_is_declared() {
        assert!(has_open_months(NOVEMBER_2026, NOVEMBER_2026));
        assert!(!has_open_months(NOVEMBER_2026 + 1, NOVEMBER_2026));
    }

    #[test]
    fn a_month_is_overdue_from_its_due_date_itself() {
        let due = due_ts(NOVEMBER_2026);
        assert!(!is_overdue(due - 1, due));
        assert!(is_overdue(due, due));
    }

    #[test]
    fn a_month_closes_once_settled_and_behind_the_pointer() {
        // November settled, the pointer on December: finished.
        assert!(month_closable(true, NOVEMBER_2026, NOVEMBER_2026 + 1, NOVEMBER_2026));
        // Paid in full and stepped past, but not settled yet: the settlement still reads it.
        assert!(!month_closable(false, NOVEMBER_2026, NOVEMBER_2026 + 1, NOVEMBER_2026 - 1));
        // Settled but still the oldest open month (an arrear): `fund_month` still reads it.
        assert!(!month_closable(true, NOVEMBER_2026, NOVEMBER_2026, NOVEMBER_2026));
        // Behind the pointer but ahead of the last settled month.
        assert!(!month_closable(true, NOVEMBER_2026 + 1, NOVEMBER_2026 + 2, NOVEMBER_2026));
    }

    #[test]
    fn a_raise_finishes_once_nothing_is_owed_from_it() {
        use crate::raise::state::RaiseState::*;
        // Closed, every month paid, every position and month gone.
        assert!(raise_finished(Closed, TERM_END + 1, TERM_END, 0, 0));
        // A position, a month or an open month (an arrear) left: not yet.
        assert!(!raise_finished(Closed, TERM_END + 1, TERM_END, 1, 0));
        assert!(!raise_finished(Closed, TERM_END + 1, TERM_END, 0, 1));
        assert!(!raise_finished(Closed, TERM_END, TERM_END, 0, 0));
        // Refunding: every position refunded or closed (the pool's own included).
        assert!(raise_finished(Refunding, 0, 0, 0, 0));
        assert!(!raise_finished(Refunding, 0, 0, 1, 0));
        // Never while raising or active, whatever the counters say.
        assert!(!raise_finished(Raising, 0, 0, 0, 0));
        assert!(!raise_finished(Active, TERM_END + 1, TERM_END, 0, 0));
    }

    #[test]
    fn a_term_left_undeclared_closes_after_the_fallback() {
        let fallback = close_fallback_ts(TERM_END);
        // 90 days after the last month's due date.
        assert_eq!(fallback - due_ts(TERM_END), 90 * 86_400);
        // Months never declared stop nothing once the fallback has come...
        assert!(!closable(TERM_END - 5, TERM_END, fallback - 1, false, 0));
        assert!(closable(TERM_END - 5, TERM_END, fallback, false, 0));
        assert!(closable(NOVEMBER_2026 - 1, TERM_END, fallback, false, 0));
        // ...but a guarantee left over open months is still spent on them first.
        assert!(!closable(TERM_END - 5, TERM_END, fallback, true, 50));
        assert!(closable(TERM_END - 5, TERM_END, fallback, true, 0));
    }

    #[test]
    fn closable_and_pool_destination() {
        let after = due_ts(TERM_END);
        // Term over, last month declared and due, nothing open.
        assert!(closable(TERM_END, TERM_END, after, false, 50));
        // Open months with a guarantee left: cover them first.
        assert!(!closable(TERM_END, TERM_END, after, true, 50));
        // Open months and an empty guarantee: closable, arrears stay owed.
        assert!(closable(TERM_END, TERM_END, after, true, 0));
        // Before the last due date.
        assert!(!closable(TERM_END, TERM_END, after - 1, false, 0));
        // The last month undeclared at its due date: not yet, and no cap ends a raise early.
        assert!(!closable(TERM_END - 1, TERM_END, after, false, 0));
        // The pool's USDC goes to the fans' quotas, or to the creator when none is left.
        assert_eq!(pool_destination(1), PoolDestination::Holders);
        assert_eq!(pool_destination(0), PoolDestination::Creator);
    }
}

#[cfg(test)]
mod write_off_tests {
    use super::*;
    #[test]
    fn no_holder_write_off_has_strict_guards() {
        assert!(month_write_off_allowed(RaiseState::Closed, 0, true, 10, 10, 10));
        for state in [RaiseState::Active, RaiseState::Raising, RaiseState::Refunding] {
            assert!(!month_write_off_allowed(state, 0, true, 10, 10, 10));
        }
        assert!(!month_write_off_allowed(RaiseState::Closed, 1, true, 10, 10, 10));
        assert!(!month_write_off_allowed(RaiseState::Closed, 0, false, 10, 10, 10));
        assert!(!month_write_off_allowed(RaiseState::Closed, 0, true, 10, 11, 11));
        assert!(!month_write_off_allowed(RaiseState::Closed, 0, true, 10, 10, 9));
        assert_eq!(month_write_off_bit(10, 10, 69).unwrap(), 1);
        assert_eq!(month_write_off_bit(69, 10, 69).unwrap(), 1u64 << 59);
        assert!(month_write_off_bit(9, 10, 69).is_err());
        assert!(month_write_off_bit(70, 10, 69).is_err());
    }
}

/// The internal audit of 2026-10-29: the re-declaration window and the late payment's refill.
#[cfg(test)]
mod audit_tests {
    use super::*;

    const DECEMBER_2026: u32 = 24_323;

    #[test]
    fn an_overdue_payment_refills_only_while_the_next_month_is_unsettled() {
        // December overdue and settled (or not yet settled): January is not settled, so the refill
        // can only stand behind a month the guarantee will still cover.
        assert!(refills_after_arrears(DECEMBER_2026, DECEMBER_2026));
        assert!(refills_after_arrears(DECEMBER_2026, DECEMBER_2026 - 1));
        // January already settled with arrears of its own: the pointer would stand on it.
        assert!(!refills_after_arrears(DECEMBER_2026, DECEMBER_2026 + 1));
    }

    #[test]
    fn a_month_is_redeclared_only_before_its_due_date() {
        let due = due_ts(DECEMBER_2026);
        let ok = |now: i64| check_redeclaration(now, due, false, 0, DECEMBER_2026, DECEMBER_2026, 0, 40);
        assert!(ok(declare_open_ts(DECEMBER_2026)).is_ok());
        assert!(ok(due - 1).is_ok());
        // From the due date `settle_month` opens: the due is final.
        assert_eq!(ok(due).unwrap_err(), error!(FanexError::MonthNotRedeclarable));
    }

    #[test]
    fn a_redeclaration_never_unsettles_never_reaches_back_and_never_returns_money() {
        let (due, now) = (due_ts(DECEMBER_2026), declare_open_ts(DECEMBER_2026));
        let refused = error!(FanexError::MonthNotRedeclarable);
        // Settled, or covered by the guarantee.
        assert_eq!(check_redeclaration(now, due, true, 0, DECEMBER_2026, DECEMBER_2026, 0, 40).unwrap_err(), refused);
        assert_eq!(check_redeclaration(now, due, false, 1, DECEMBER_2026, DECEMBER_2026, 0, 40).unwrap_err(), refused);
        // Not the last declared month.
        assert_eq!(check_redeclaration(now, due, false, 0, DECEMBER_2026 - 1, DECEMBER_2026, 0, 40).unwrap_err(), refused);
        // Below what the creator paid: refused; at it or above: accepted.
        assert_eq!(check_redeclaration(now, due, false, 0, DECEMBER_2026, DECEMBER_2026, 30, 29).unwrap_err(), refused);
        assert!(check_redeclaration(now, due, false, 0, DECEMBER_2026, DECEMBER_2026, 30, 30).is_ok());
        assert!(check_redeclaration(now, due, false, 0, DECEMBER_2026, DECEMBER_2026, 30, 40).is_ok());
    }

    #[test]
    fn the_pointer_follows_what_the_redeclared_month_owes() {
        // A zero due had stepped the pointer past December; owing again, it comes back.
        assert_eq!(oldest_open_after_redeclaration(DECEMBER_2026 + 1, DECEMBER_2026, 40), DECEMBER_2026);
        // The pointer on December and December owing nothing now: past it, as `declare_month` does.
        assert_eq!(oldest_open_after_redeclaration(DECEMBER_2026, DECEMBER_2026, 0), DECEMBER_2026 + 1);
        // Still owing, still the pointer's month.
        assert_eq!(oldest_open_after_redeclaration(DECEMBER_2026, DECEMBER_2026, 40), DECEMBER_2026);
        // Already past and owing nothing: past it.
        assert_eq!(oldest_open_after_redeclaration(DECEMBER_2026 + 1, DECEMBER_2026, 0), DECEMBER_2026 + 1);
        // Older arrears hold the pointer wherever December stands.
        assert_eq!(oldest_open_after_redeclaration(DECEMBER_2026 - 3, DECEMBER_2026, 40), DECEMBER_2026 - 3);
        assert_eq!(oldest_open_after_redeclaration(DECEMBER_2026 - 3, DECEMBER_2026, 0), DECEMBER_2026 - 3);
    }
}
