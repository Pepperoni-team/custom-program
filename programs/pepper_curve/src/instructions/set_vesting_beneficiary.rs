use anchor_lang::prelude::*;
use anchor_spl::token::Mint;

use crate::constants::*;
use crate::errors::FanexError;
use crate::events::VestingBeneficiarySet;
use crate::state::*;
use crate::validation::validate_beneficiary_reassignable;

/// The keyless placeholder beneficiary stamped on a fan-led launch.
///
/// Derived, never stored: `Vesting` and `GlobalConfig` already exist on-chain at
/// their current sizes and both derive `InitSpace`, so neither struct can gain a
/// field without making the program deserialise every existing account against a
/// longer layout — which would break every already-launched token.
///
/// A PDA is off-curve by construction, so no private key exists for it. That is
/// the whole point: an unclaimed fan-led allocation can be reassigned by this
/// instruction, but it can never be *spent* by anyone in the meantime.
pub fn fan_led_placeholder() -> Pubkey {
    Pubkey::find_program_address(&[FAN_LED_PLACEHOLDER_SEED], &crate::ID).0
}

#[derive(Accounts)]
pub struct SetVestingBeneficiary<'info> {
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ FanexError::Unauthorized
    )]
    pub config: Account<'info, GlobalConfig>,

    pub admin: Signer<'info>,

    pub mint: Account<'info, Mint>,

    #[account(
        mut,
        seeds = [VESTING_SEED, mint.key().as_ref()],
        bump = vesting.bump,
        has_one = mint @ FanexError::AccountMismatch,
    )]
    pub vesting: Account<'info, Vesting>,
}

/// Point an unclaimed fan-led vesting allocation at the creator who has just
/// proven they own the channel, and start its clock (see `hand_over`).
///
/// The `require!` inside `validate_beneficiary_reassignable` matters more than
/// the instruction itself: it makes it *structurally impossible* for this code
/// path to touch a vesting account that belongs to a real creator, so adding it
/// does not widen the blast radius of a compromised admin key.
pub fn handler(ctx: Context<SetVestingBeneficiary>, new_beneficiary: Pubkey) -> Result<()> {
    let vesting = &mut ctx.accounts.vesting;
    let previous = vesting.beneficiary;

    validate_beneficiary_reassignable(previous, fan_led_placeholder())?;

    // An all-zero beneficiary would be unclaimable forever — the opposite of
    // what this instruction exists to fix.
    require_keys_neq!(
        new_beneficiary,
        Pubkey::default(),
        FanexError::AccountMismatch
    );

    hand_over(vesting, new_beneficiary, Clock::get()?.unix_timestamp);

    emit!(VestingBeneficiarySet {
        mint: ctx.accounts.mint.key(),
        previous,
        new_beneficiary,
    });
    Ok(())
}

/// The state change of a hand-over, kept free of the runtime so it can be unit-tested.
///
/// The clock restarts here: the cliff and the ramp count from the day the allocation
/// gets an owner, not from the launch. Kept from launch, a token claimed months later
/// would hand its creator a matured share on claim day, sellable at once, with none of
/// the lock the product promises holders. Nothing can have been released before this
/// point (the placeholder holds no key), so `released_amount` is still 0 and the terms
/// (total, cliff, duration) stay the ones stamped at `create_curve`.
pub fn hand_over(vesting: &mut Vesting, new_beneficiary: Pubkey, now: i64) {
    vesting.beneficiary = new_beneficiary;
    vesting.start_time = now;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::vested_amount;

    const DAY: i64 = 86_400;
    const LAUNCH: i64 = 1_789_000_000;
    const TOTAL: u64 = 10_000_000_000_000;

    /// An unclaimed equity allocation as `create_curve` leaves it: 90-day cliff, 270-day ramp.
    fn unclaimed() -> Vesting {
        Vesting {
            mint: Pubkey::new_unique(),
            beneficiary: fan_led_placeholder(),
            total_amount: TOTAL,
            released_amount: 0,
            start_time: LAUNCH,
            cliff_seconds: 90 * DAY,
            duration_seconds: 270 * DAY,
            bump: 255,
        }
    }

    fn vested_at(v: &Vesting, now: i64) -> u64 {
        vested_amount(v.total_amount, v.start_time, v.cliff_seconds, v.duration_seconds, now)
    }

    #[test]
    fn a_creator_who_claims_after_the_full_schedule_gets_nothing_on_claim_day() {
        let mut v = unclaimed();
        let claim = LAUNCH + 400 * DAY;
        assert_eq!(vested_at(&v, claim), TOTAL, "precondition: the launch clock has run out");

        hand_over(&mut v, Pubkey::new_unique(), claim);

        assert_eq!(vested_at(&v, claim), 0);
    }

    #[test]
    fn the_cliff_and_the_ramp_count_from_the_hand_over() {
        let mut v = unclaimed();
        let claim = LAUNCH + 120 * DAY;
        hand_over(&mut v, Pubkey::new_unique(), claim);

        assert_eq!(v.start_time, claim);
        assert_eq!(vested_at(&v, claim + 90 * DAY - 1), 0);
        assert_eq!(vested_at(&v, claim + 90 * DAY + 135 * DAY), TOTAL / 2);
        assert_eq!(vested_at(&v, claim + 360 * DAY), TOTAL);
    }

    #[test]
    fn the_hand_over_keeps_the_terms_stamped_at_launch() {
        let mut v = unclaimed();
        let creator = Pubkey::new_unique();
        hand_over(&mut v, creator, LAUNCH + 30 * DAY);

        assert_eq!(v.beneficiary, creator);
        assert_eq!(v.total_amount, TOTAL);
        assert_eq!(v.released_amount, 0);
        assert_eq!(v.cliff_seconds, 90 * DAY);
        assert_eq!(v.duration_seconds, 270 * DAY);
    }
}
