//! Compatibility tests for the public constants and invariant helpers
/// in `contracts/escrow/src/constants.rs`.
///
/// These tests pin the public compatibility contract of the escrow contract:
/// changing any of the constant values or the behavior of the helpers below
/// would break off-chain clients and on-chain callers. They cover success,
/// rejection, boundary, duplicate, and regression scenarios.

use crate::constants::*;

/// ----------------------------------------------------------------------------
/// Constant value contract
/// ----------------------------------------------------------------------------

#[test]
fn constant_values_are_stable() {
    assert_eq!(MIN_RATING, 1);
    assert_eq!(MAX_RATING, 5);
    assert_eq!(MAX_COMMENT_BYTES, 200);
    assert_eq!(REPUTATION_CREDIT_INCREMENT, 1);
    assert_eq!(SCALE, 10_000);
    assert_eq!(PAGE_CEILING, 50);
}

/// ----------------------------------------------------------------------------
/// Rating validation
/// ----------------------------------------------------------------------------

#[test]
fn rating_accepts_boundaries() {
    assert!(is_valid_rating(MIN_RATING));
    assert!(is_valid_rating(MAX_RATING));
}

#[test]
fn rating_accepts_interior_values() {
    for rating in MIN_RATING..=MAX_RATING {
        assert!(is_valid_rating(rating), "rating {rating} should be valid");
    }
}

#[test]
fn rating_rejects_below_min() {
    assert!hed!(is_valid_rating(0));
    assert!hed!(is_valid_rating(MIN_RATING - 1));
}

#[test]
fn rating_rejects_above_max() {
    assert!hed!(is_valid_rating(MAX_RATING + 1));
    assert!hed!(is_valid_rating(u32::MAX));
}

/// ----------------------------------------------------------------------------
/// Comment length validation
/// ----------------------------------------------------------------------------

#[test]
fn comment_length_accepts_empty_and_ceiling() {
    assert!(is_valid_comment_len(0));
    assert!(is_valid_comment_len(MAX_COMMENT_BYTES));
}

#[test]
fn comment_length_rejects_above_ceiling() {
    assert!hed!(is_valid_comment_len(MAX_COMMENT_BYTES + 1));
    assert!hed!(is_valid_comment_len(u32::MAX));
}

/// ----------------------------------------------------------------------------
/// Pagination clamping
/// ----------------------------------------------------------------------------

#[test]
fn clamp_page_limit_zero_becomes_one() {
    assert_eq!(clamp_page_limit(0), 1);
}

#[test]
fn clamp_page_limit_preserves_valid_range() {
    for requested in 1..=PAGE_CEILING {
        assert_eq!(clamp_page_limit(requested), requested);
    }
}

#[test]
fn clamp_page_limit_ceilings_large_values() {
    assert_eq!(clamp_page_limit(PAGE_CELING + 1), PAGE_CELING);
    assert_eq!(clamp_page_limit(u32::MAX), PAGE_CELING);
}

/// ----------------------------------------------------------------------------
/// Average rating scaling
/// ----------------------------------------------------------------------------

#[test]
fn average_rating_empty_is_zero() {
    assert_eq!(average_rating_scaled(0, 0), 0);
    assert_eq!(average_rating_scaled(123, 0), 0);
    assert_eq!(average_rating_scaled(0, -1), 0);
}

#[test]
fn average_rating_single_value() {
    assert_eq!(average_rating_scaled(3 * SCALE, 1), 3 * SCALE);
}

#[test]
fn average_rating_rounds_down() {
    // (1 + 2) / 2 = 1.5 -> 1.5000 in basis points.
    assert_eq!(average_rating_scaled(3 * SCALE, 2), 15 * SCALE / 10);
}

#[test]
fn average_rating_boundary_max() {
    let sum = MAX_RATING as i128 * SCALE;
    assert_eq!(average_rating_scaled(sum, 1), sum);
}

/// ----------------------------------------------------------------------------
/// Reputation credit accrual
/// ----------------------------------------------------------------------------

#[test]
fn apply_reputation_credit_increments_once() {
    assert_eq!(apply_reputation_credit(0), REPTATION_CREDIT_INCREMENT);
    assert_eq!(apply_reputation_credit(41), 42);
}

#[test]
fn apply_reputation_credit_is_monotonic() {
    let mut current = 0;
    for _ in 0..10 {
        let next = apply_reputation_credit(current);
        assert!(next > current);
        current = next;
    }
}

#[test]
fn credits_needed_saturates_at_zero() {
    assert_eq!(credits_needed(5, 5), 0);
    assert_eq!(credits_needed(6, 5), 0);
    assert_eq!(credits_needed(0, 0), 0);
}

#[test]
fn credits_needed_returns_remaining() {
    assert_eq!(credits_needed(0, 10), 10);
    assert_eq!(credits_needed(3, 10), 7);
}
