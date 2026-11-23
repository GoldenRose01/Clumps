//! Clumps for Horizon: merges nearby experience orbs regardless of value and removes XP pickup cooldown.

use mod_api::runtime;
use mod_api::{
    self, ExperienceOrbCanMergeEvent, ExperienceOrbMergeEvent, Hooks, PickupXpEvent, ServerHost,
};

const MAX_ORB_VALUE: u128 = i16::MAX as u128; // 32_767
const MAX_ORB_COUNT: u128 = i32::MAX as u128; // 2_147_483_647

fn orb_xp(value: i32, count: u32) -> u128 {
    (value.max(0) as u128) * (count as u128)
}

fn find_merged_repr(total: u128, into_value: i32, into_count: u32) -> Option<(i32, u32)> {
    if total == 0 || total > MAX_ORB_VALUE * MAX_ORB_COUNT {
        return None;
    }
    let total_u64 = total as u64;
    let v_min = total_u64.div_ceil(MAX_ORB_COUNT as u64);
    let v_max = total_u64.min(MAX_ORB_VALUE as u64);
    for value in (v_min..=v_max).rev() {
        if total_u64 % value == 0 {
            let count = (total_u64 / value) as u32;
            let value = value as i32;
            if (value, count) != (into_value, into_count) {
                return Some((value, count));
            }
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MergeAction {
    AddCounts,
    SetOrb { value: i32, count: u32 },
    Cancel,
}

fn plan_merge(into_value: i32, into_count: u32, from_value: i32, from_count: u32) -> MergeAction {
    let total = orb_xp(into_value, into_count) + orb_xp(from_value, from_count);
    if total == 0 {
        if into_value == 0 {
            return match into_count.checked_add(from_count) {
                Some(c) if c <= MAX_ORB_COUNT as u32 => MergeAction::AddCounts,
                _ => MergeAction::Cancel,
            };
        }
        return MergeAction::SetOrb { value: 0, count: 1 };
    }
    match find_merged_repr(total, into_value, into_count) {
        Some((value, count)) => MergeAction::SetOrb { value, count },
        None => MergeAction::Cancel,
    }
}

fn resolve_pickup(value: i32, count: u32) -> (i32, u32) {
    if count <= 1 {
        return (value, count);
    }
    let total = orb_xp(value, count);
    if total == 0 {
        (0, 1)
    } else if total <= i32::MAX as u128 {
        (total as i32, 1)
    } else {
        (value, count)
    }
}

#[derive(Default)]
struct ClumpsMod;

#[mod_api::horizon_mod]
impl Hooks for ClumpsMod {
    /// Allow any two experience orbs in reach to merge, regardless of value or entity ID.
    fn experience_orb_can_merge(
        &self,
        _host: &mut dyn ServerHost,
        event: &mut ExperienceOrbCanMergeEvent<'_>,
    ) {
        event.can_merge = true;
    }

    /// Combine XP totals exactly when orbs merge, canceling if no exact saved representation exists.
    fn experience_orb_merge(
        &self,
        host: &mut dyn ServerHost,
        event: &mut ExperienceOrbMergeEvent<'_>,
    ) -> runtime::Flow {
        match plan_merge(
            event.into_value,
            event.into_count,
            event.from_value,
            event.from_count,
        ) {
            MergeAction::AddCounts => runtime::Flow::Continue,
            MergeAction::SetOrb { value, count } => {
                if host.set_experience_orb(
                    event.dimension,
                    event.into,
                    Some(value),
                    Some(count),
                    None,
                ) {
                    runtime::Flow::Continue
                } else {
                    runtime::Flow::Cancel
                }
            }
            MergeAction::Cancel => runtime::Flow::Cancel,
        }
    }

    /// Collapse multi-count orbs that fit in `i32` on pickup and remove XP pickup cooldown.
    fn pickup_xp(
        &self,
        host: &mut dyn ServerHost,
        event: &mut PickupXpEvent<'_>,
    ) -> runtime::Flow {
        let (value, count) = resolve_pickup(event.value, event.count);
        event.value = value;
        event.count = count;
        if let Some(player) = host.player(event.player) {
            let _ = player.set_exp_cooldown(host, 0);
        }
        runtime::Flow::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::{MergeAction, orb_xp, plan_merge, resolve_pickup};

    #[test]
    fn small_totals_fit_in_single_orb() {
        assert_eq!(
            plan_merge(3, 2, 5, 1),
            MergeAction::SetOrb { value: 11, count: 1 }
        );
        assert_eq!(
            plan_merge(12, 1, 20, 1),
            MergeAction::SetOrb { value: 32, count: 1 }
        );
        assert_eq!(
            plan_merge(32_766, 1, 1, 1),
            MergeAction::SetOrb {
                value: 32_767,
                count: 1
            }
        );
        assert_eq!(
            plan_merge(5, 2, 5, 1),
            MergeAction::SetOrb { value: 15, count: 1 }
        );
    }

    #[test]
    fn large_exact_factorizations_preserve_xp() {
        assert_eq!(
            plan_merge(32_767, 1, 32_767, 1),
            MergeAction::SetOrb {
                value: 32_767,
                count: 2
            }
        );
        assert_eq!(
            plan_merge(32_767, 2, 1, 1),
            MergeAction::SetOrb {
                value: 21_845,
                count: 3
            }
        );
        // 32_771 is prime and > 32_767; exact representation uses value = 1, count = 32_771.
        assert_eq!(
            plan_merge(32_767, 1, 4, 1),
            MergeAction::SetOrb {
                value: 1,
                count: 32_771
            }
        );
    }

    #[test]
    fn negative_denominations_treated_as_zero() {
        assert_eq!(
            plan_merge(-5, 3, 11, 1),
            MergeAction::SetOrb { value: 11, count: 1 }
        );
        // Must pick (5, 2) instead of (10, 1) so the host does not add `from_count`.
        assert_eq!(
            plan_merge(10, 1, -5, 1),
            MergeAction::SetOrb { value: 5, count: 2 }
        );
        // (1, 1) is the only factorization of 1 and equals `into`, so cancel to keep orbs separate.
        assert_eq!(plan_merge(1, 1, -5, 1), MergeAction::Cancel);
        assert_eq!(plan_merge(1, 1, 0, 1), MergeAction::Cancel);
        // Negative zero-total orbs normalize to (0, 1) with `(0, 1) != (into_value, into_count)`.
        assert_eq!(
            plan_merge(-5, 2, -3, 1),
            MergeAction::SetOrb { value: 0, count: 1 }
        );
        // Zero-valued `into` uses host count addition unless count would exceed `i32::MAX`.
        assert_eq!(plan_merge(0, 1, -3, 1), MergeAction::AddCounts);
        assert_eq!(plan_merge(0, i32::MAX as u32, 0, 1), MergeAction::Cancel);
    }

    #[test]
    fn unrepresentable_positive_totals_cancel_merge() {
        // Exceeds maximum representable XP (32_767 * i32::MAX).
        assert_eq!(
            plan_merge(32_767, i32::MAX as u32, 1, 1),
            MergeAction::Cancel
        );
        // 32_767 * 131_080 + 9 = 4_295_098_369 = 65_537 * 65_537 (prime squared > 32_767).
        // Only divisors are 1, 65_537, and 65_537^2; count for value = 1 exceeds i32::MAX.
        assert_eq!(plan_merge(32_767, 131_080, 9, 1), MergeAction::Cancel);
    }

    #[test]
    fn saved_nbt_range_invariant() {
        let cases = [
            (3, 2, 5, 1),
            (32_767, 1, 4, 1),
            (32_767, 2, 1, 1),
            (10, 1, -5, 1),
            (-5, 3, 11, 1),
            (-5, 2, -3, 1),
            (32_767, 1, 32_767, 1),
            (i32::MIN, 2, 7, 3),
        ];
        for (iv, ic, fv, fc) in cases {
            if let MergeAction::SetOrb { value, count } = plan_merge(iv, ic, fv, fc) {
                assert!(i16::try_from(value).is_ok());
                assert!(count >= 1 && count <= i32::MAX as u32);
                assert_ne!((value, count), (iv, ic));
                assert_eq!(orb_xp(value, count), orb_xp(iv, ic) + orb_xp(fv, fc));
            }
        }
    }

    #[test]
    fn pickup_xp_collapses_multi_count_safely() {
        assert_eq!(resolve_pickup(11, 1), (11, 1));
        assert_eq!(resolve_pickup(-5, 1), (-5, 1));
        assert_eq!(resolve_pickup(1, 32_771), (32_771, 1));
        assert_eq!(resolve_pickup(21_845, 3), (65_535, 1));
        assert_eq!(resolve_pickup(0, 5), (0, 1));
        assert_eq!(resolve_pickup(-5, 4), (0, 1));
        assert_eq!(resolve_pickup(32_767, 100_000), (32_767, 100_000));
    }
}
