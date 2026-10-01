//! Bounds validation for storage entrypoint inputs.
//! Bounds validation for storage entrypoint inputs.
//!
//! This module extracts numeric and length bound checks for storage-mutating
//! entrypoints into a single source of truth. Each function validates one
//! logical parameter and panics with the appropriate typed [`EscrowError`]
//! on rejection.
//!
//! # State invariants
//!
//! These validators are the *only* sanctioned gate for the storage-mutating
//! entrypoints they guard. Callers MUST invoke the matching validator before
//! any state mutation so the following invariants hold:
//!
//! * `max_escrow_total_stroops > 0` — a non-positive cap would permanently
//!   block contract creation.
//! * `MIN_RATING <= min_rating <= max_rating <= MAX_REPUTATION_CONFIG_RATING_CEILING`
//!   and `MIN_COMMENT_BYTES <= max_comment_bytes <= MAX_REPUTATION_CONFIG_COMMENT_BYTES_CEILING`.
//! * `1 <= milestone_count <= MAX_MILESTONES`.
//! * `0 <= fee_bps <= MAX_FEE_BPS`.
//! * `0 < amount <= MAX_SINGLE_AMOUNT_STROOPS`.
//!
//! All checks are pure and deterministic: identical inputs always yield the
//! same accept/reject decision, and rejection happens before any storage
//! write, so partial failure cannot leave inconsistent state.
//!
//! All functions are pure (no side-effects) and intended to be called at the
//! top of the corresponding entrypoint, before any state mutation occurs.
//!
//! # Invariants
//!
//! * Every validator is total: it either returns `()` or panics with a typed
//!   error. It never mutates state, never allocates unbounded memory, and
//!   never performs I/O.
//! * Validators are idempotent and side-effect free, so they may be safely
//!   re-run on retries or after a partial failure without changing the
//!   outcome.
//! * Boundary values are inclusive on the accepted side and rejected on the
//!   first out-of-range value (e.g. `MAX_MILESTONES` is accepted,
//!   `MAX_MILESTONES + 1` is rejected).
//! * Duplicate submissions are handled by the caller's state machine; these
//!   validators only assert that the *shape* of the input is well-formed, so
//!   a duplicate that reaches a validator is validated identically to a
//!   first-time submission.
//! * Rejections never leak sensitive data: only the typed error code is
//!   surfaced to the caller.

use crate::milestones_consts::MAX_SINGLE_AMOUNT_STROOPS;
use crate::milestones_consts::{
    MAX_FEE_BPS, MAX_MILESTONES, MAX_RATING, MAX_REPUTATION_CONFIG_COMMENT_BYTES_CEILING,
    MAX_REPUTATION_CONFIG_RATING_CEILING, MIN_COMMENT_BYTES, MIN_RATING,
};
use crate::{Error, EscrowError};
use soroban_sdk::Env;
use soroban_sdk::panic_with_error;

/// Validate the governed total escrow cap in stroops.
///
/// # Accepted values
/// * Any `i128` in `(0, i128::MAX]`.
///
/// # Rejected values
/// * `0` — a zero cap would block every contract creation.
/// * Negative values — amounts must be positive.
/// * Values above [`MAX_SINGLE_AMOUNT_STROOPS`] — the cap must not exceed
///   the per-milestone amount ceiling, otherwise a single milestone could
///   never satisfy the cap and the invariant would be unenforceable.
///
/// # Boundary behavior
/// * `1` is accepted (smallest positive cap).
/// * `i128::MAX` is accepted (largest representable cap).
/// * `0`, `-1`, and `i128::MIN` are rejected.
///
/// # Panics
/// Panics with [`Error::InvalidProtocolParameters`] when the cap is out
/// of range.
///
/// # Invariants
/// * The stored cap is strictly positive, so `total_escrowed <= cap` remains
///   satisfiable for any non-negative escrow total.
pub(crate) fn validate_escrow_total_cap(env: &Env, max_escrow_total_stroops: i128) {
    if max_escrow_total_stroops <= 0 {
        env.panic_with_error(Error::InvalidProtocolParameters);
    }
    if max_escrow_total_stroops > MAX_SINGLE_AMOUNT_STROOPS {
        env.panic_with_error(Error::InvalidProtocolParameters);
    }
}

/// Validate reputation configuration parameters.
///
/// # Accepted values
/// * `min_rating` in `[1, 10]`
/// * `max_rating` in `[min_rating, 10]`
/// * `max_comment_bytes` in `[1, 1_000]`
///
/// # Boundary behavior
/// * `min_rating == max_rating` is accepted (single-value range).
/// * `max_comment_bytes == 1` and `max_comment_bytes == 1_000` are accepted.
/// * `min_rating == 0`, `max_rating < min_rating`, `max_rating > 10`,
///   `max_comment_bytes == 0`, and `max_comment_bytes > 1_000` are rejected.
///
/// # Panics
/// Panics with [`Error::InvalidProtocolParameters`] when any bound is violated.
///
/// # Invariants
/// * `MIN_RATING <= min_rating <= max_rating <= MAX_REPUTATION_CONFIG_RATING_CEILING`,
///   so the accepted rating window is never empty and never exceeds the
///   protocol ceiling.
/// * `MIN_COMMENT_BYTES <= max_comment_bytes <= MAX_REPUTATION_CONFIG_COMMENT_BYTES_CEILING`.
pub(crate) fn validate_reputation_config_params(
    env: &Env,
    min_rating: u32,
    max_rating: u32,
    max_comment_bytes: u32,
) {
    if min_rating > MAX_REPUTATION_CONFIG_RATING_CEILING {
        env.panic_with_error(Error::InvalidProtocolParameters);
    }
    if min_rating < MIN_RATING
        || max_rating < min_rating
        || max_rating > MAX_REPUTATION_CONFIG_RATING_CEILING
        || max_comment_bytes < MIN_COMMENT_BYTES
        || max_comment_bytes > MAX_REPUTATION_CONFIG_COMMENT_BYTES_CEILING
    {
        env.panic_with_error(Error::InvalidProtocolParameters);
    }
}

/// Validate the number of milestones for a contract creation call.
///
/// # Accepted values
/// * `count` in `[1, MAX_MILESTONES]`
///
/// # Rejected values
/// * `0` — at least one milestone is required.
/// * Values > `MAX_MILESTONES` (10).
///
/// # Boundary behavior
/// * `1` and `MAX_MILESTONES` are accepted.
/// * `0`, `MAX_MILESTONES + 1`, and `u32::MAX` are rejected.
///
/// # Panics
/// Panics with [`EscrowError::EmptyMilestones`] when `count == 0` or
/// [`EscrowError::TooManyMilestones`] when `count > MAX_MILESTONES`.
///
/// # Invariants
/// * `1 <= count <= MAX_MILESTONES`, so downstream milestone indexing is
///   always in-bounds and the empty-milestones state is unreachable.
pub(crate) fn validate_milestone_count(env: &Env, count: u32) {
    if count == 0 {
        env.panic_with_error(EscrowError::EmptyMilestones);
    }
    if count > MAX_MILESTONES {
        env.panic_with_error(EscrowError::TooManyMilestones);
    }
}

/// Validate a protocol fee basis-points value.
///
/// # Accepted values
/// * `bps` in `[0, MAX_FEE_BPS]` (0–10 000).
///
/// # Boundary behavior
/// * `0` and `MAX_FEE_BPS` are accepted.
/// * `MAX_FEE_BPS + 1` and `u32::MAX` are rejected.
///
/// # Panics
/// Panics with [`Error::InvalidProtocolParameters`] when `bps > MAX_FEE_BPS`.
///
/// # Invariants
/// * `bps <= MAX_FEE_BPS`, so fee arithmetic cannot exceed the total amount
///   and the payout invariant `net + fee == gross` holds.
pub(crate) fn validate_protocol_fee_bps(env: &Env, bps: u32) {
    if bps > MAX_FEE_BPS {
        env.panic_with_error(Error::InvalidProtocolParameters);
    }
}

/// Validate a single stroop amount for positivity and maximum bounds.
///
/// # Accepted values
/// * `amount` in `(0, MAX_SINGLE_AMOUNT_STROOPS]`.
///
/// # Boundary behavior
/// * `1` and `MAX_SINGLE_AMOUNT_STROOPS` are accepted.
/// * `0`, `-1`, and `MAX_SINGLE_AMOUNT_STROOPS + 1` are rejected.
///
/// # Panics
/// Panics with [`EscrowError::AmountMustBePositive`] when `amount <= 0` or
/// [`EscrowError::InvalidMilestoneAmount`] when the amount exceeds the cap.
///
/// # Invariants
/// * `0 < amount <= MAX_SINGLE_AMOUNT_STROOPS`, so no zero-value or
///   overflow-prone amount can be persisted.
pub(crate) fn validate_stroop_amount(env: &Env, amount: i128) {
    if amount <= 0 {
        env.panic_with_error(crate::EscrowError::AmountMustBePositive);
    }
    if amount > crate::amount_validation::MAX_SINGLE_AMOUNT_STROOPS {
        env.panic_with_error(crate::EscrowError::InvalidMilestoneAmount);
    }
    if amount > crate::amount_validation::MAX_SINGLE_AMOUNT_STROOPS {
        env.panic_with_error(crate::EscrowError::InvalidMilestoneAmount);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::Env;

    fn env() -> Env {
        Env::default()
    }

    // ── validate_escrow_total_cap ────────────────────────────────────────────

    #[test]
    fn validate_escrow_total_cap_accepts_1() {
        let e = env();
        validate_escrow_total_cap(&e, 1);
    }

    #[test]
    fn validate_escrow_total_cap_accepts_i128_max() {
        let e = env();
        validate_escrow_total_cap(&e, i128::MAX);
    }

    #[test]
    #[should_panic]
    fn validate_escrow_total_cap_rejects_zero() {
        let e = env();
        validate_escrow_total_cap(&e, 0);
    }

    #[test]
    #[should_panic]
    fn validate_escrow_total_cap_rejects_negative() {
        let e = env();
        validate_escrow_total_cap(&e, -1);
    }

    #[test]
    #[should_panic]
    fn validate_escrow_total_cap_rejects_i128_min() {
        let e = env();
        validate_escrow_total_cap(&e, i128::MIN);
    }

    #[test]
    #[should_panic]
    fn validate_escrow_total_cap_rejects_over_single_amount_max() {
        let e = env();
        validate_escrow_total_cap(&e, MAX_SINGLE_AMOUNT_STROOPS + 1);
    }

    // ── validate_reputation_config_params ─────────────────────────────────────

    #[test]
    fn validate_reputation_config_params_accepts_default() {
        let e = env();
        validate_reputation_config_params(&e, 1, 5, 200);
    }

    #[test]
    fn validate_reputation_config_params_accepts_min_equal_max_rating() {
        let e = env();
        validate_reputation_config_params(&e, 3, 3, 1);
    }

    #[test]
    fn validate_reputation_config_params_accepts_max_comment_1000() {
        let e = env();
        validate_reputation_config_params(&e, 1, 10, 1_000);
    }

    #[test]
    #[should_panic]
    fn validate_reputation_config_params_rejects_zero_min_rating() {
        let e = env();
        validate_reputation_config_params(&e, 0, 5, 200);
    }

    #[test]
    #[should_panic]
    fn validate_reputation_config_params_rejects_max_below_min() {
        let e = env();
        validate_reputation_config_params(&e, 5, 3, 200);
    }

    #[test]
    #[should_panic]
    fn validate_reputation_config_params_rejects_max_rating_over_10() {
        let e = env();
        validate_reputation_config_params(&e, 1, 11, 200);
    }

    #[test]
    #[should_panic]
    fn validate_reputation_config_params_rejects_zero_comment_bytes() {
        let e = env();
        validate_reputation_config_params(&e, 1, 5, 0);
    }

    #[test]
    #[should_panic]
    fn validate_reputation_config_params_rejects_comment_over_1000() {
        let e = env();
        validate_reputation_config_params(&e, 1, 5, 1_001);
    }

    #[test]
    #[should_panic]
    fn validate_reputation_config_params_rejects_min_rating_over_ceiling() {
        let e = env();
        validate_reputation_config_params(&e, 11, 11, 200);
    }

    // ── validate_milestone_count ──────────────────────────────────────────────

    #[test]
    fn validate_milestone_count_accepts_1() {
        let e = env();
        validate_milestone_count(&e, 1);
    }

    #[test]
    fn validate_milestone_count_accepts_max() {
        let e = env();
        validate_milestone_count(&e, MAX_MILESTONES);
    }

    #[test]
    #[should_panic]
    fn validate_milestone_count_rejects_zero() {
        let e = env();
        validate_milestone_count(&e, 0);
    }

    #[test]
    #[should_panic]
    fn validate_milestone_count_rejects_over_max() {
        let e = env();
        validate_milestone_count(&e, MAX_MILESTONES + 1);
    }

    #[test]
    #[should_panic]
    fn validate_milestone_count_rejects_u32_max() {
        let e = env();
        validate_milestone_count(&e, u32::MAX);
    }

    // ── validate_protocol_fee_bps ─────────────────────────────────────────────

    #[test]
    fn validate_protocol_fee_bps_accepts_zero() {
        let e = env();
        validate_protocol_fee_bps(&e, 0);
    }

    #[test]
    fn validate_protocol_fee_bps_accepts_max() {
        let e = env();
        validate_protocol_fee_bps(&e, MAX_FEE_BPS);
    }

    #[test]
    #[should_panic]
    fn validate_protocol_fee_bps_rejects_over_max() {
        let e = env();
        validate_protocol_fee_bps(&e, MAX_FEE_BPS + 1);
    }

    #[test]
    #[should_panic]
    fn validate_protocol_fee_bps_rejects_u32_max() {
        let e = env();
        validate_protocol_fee_bps(&e, u32::MAX);
    }

    #[test]
    #[should_panic]
    fn validate_protocol_fee_bps_rejects_max_plus_two() {
        let e = env();
        validate_protocol_fee_bps(&e, MAX_FEE_BPS + 2);
    }

    // ── validate_stroop_amount ────────────────────────────────────────────────

    #[test]
    fn validate_stroop_amount_accepts_1() {
        let e = env();
        validate_stroop_amount(&e, 1);
    }

    #[test]
    fn validate_stroop_amount_accepts_max() {
        let e = env();
        validate_stroop_amount(&e, crate::amount_validation::MAX_SINGLE_AMOUNT_STROOPS);
    }

    #[test]
    #[should_panic]
    fn validate_stroop_amount_rejects_zero() {
        let e = env();
        validate_stroop_amount(&e, 0);
    }

    #[test]
    #[should_panic]
    fn validate_stroop_amount_rejects_negative() {
        let e = env();
        validate_stroop_amount(&e, -1);
    }

    #[test]
    #[should_panic]
    fn validate_stroop_amount_rejects_over_max() {
        let e = env();
        validate_stroop_amount(&e, crate::amount_validation::MAX_SINGLE_AMOUNT_STROOPS + 1);
    }

    #[test]
    #[should_panic]
    fn validate_stroop_amount_rejects_i128_max() {
        let e = env();
        validate_stroop_amount(&e, i128::MAX);
    }
}
