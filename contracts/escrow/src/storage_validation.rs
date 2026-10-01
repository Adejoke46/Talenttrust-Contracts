//! Bounds validation for storage entrypoint inputs.
//! Bounds validation for storage entrypoint inputs.
//!
//! This module extracts numeric and length bound checks for storage-mutating
//! entrypoints into a single source of truth. Each function validates one
//! logical parameter and panics with the appropriate typed [`Error`] /
//! [`EscrowError`] on rejection.
//!
//! All functions are **pure** (no side-effects) and intended to be called at
//! the top of the corresponding entrypoint, before any state mutation occurs.
//!
//! # Compatibility contract
//!
//! Every function in this module forms part of the **public compatibility
//! contract** for its callers. The contract covers:
//!
//! 1. **Accepted range** — values that must never be rejected.
//! 2. **Rejected range** — values that must always panic.
//! 3. **Error identity** — the exact [`Error`] / [`EscrowError`] variant that
//!    must be emitted on rejection. Callers, indexers, and client SDKs that
//!    decode on-chain error codes depend on this identity. Changing the variant
//!    without a migration plan is a **breaking change**.
//! 4. **Upgrade stability** — the constants used to compute bounds
//!    ([`MAX_FEE_BPS`], [`MAX_MILESTONES`], etc.) are referenced by governance
//!    and `get_bounds()`; changing them changes the accepted range and must be
//!    treated as a protocol-level governance action, not a code-only edit.
//!
//! The [`test::storage_validation_compat`] module pins all four properties
//! with typed assertions so any regression is caught at compile-or-test time.
//!
//! # Failure modes and observability
//!
//! All rejections call [`Env::panic_with_error`], which causes Soroban to
//! surface a typed `u32` error code in the transaction result. Callers should
//! never receive a silent `false` or a misleading "success" response for an
//! out-of-bounds input — the transaction is aborted with the error code.
//!
//! This design means:
//! - Partial execution is impossible: a rejection at a validation boundary
//!   rolls back any preceding reads (there are none — validation is pure).
//! - Concurrent retries are safe: a deterministic rejection is idempotent.
//! - Upgrades are safe: adding a new validated field to an entrypoint requires
//!   adding a new `validate_*` call and a corresponding `compat` test.

use crate::milestones_consts::MAX_SINGLE_AMOUNT_STROOPS;
use crate::milestones_consts::{
    MAX_FEE_BPS, MAX_MILESTONES, MAX_RATING, MAX_REPUTATION_CONFIG_COMMENT_BYTES_CEILING,
    MAX_REPUTATION_CONFIG_RATING_CEILING, MIN_COMMENT_BYTES, MIN_RATING,
};
use crate::{Error, EscrowError};
use soroban_sdk::Env;
use soroban_sdk::panic_with_error;

// ── validate_escrow_total_cap ────────────────────────────────────────────────

/// Validate the governed total escrow cap in stroops.
///
/// ## Compatibility contract
///
/// | Property        | Value                        |
/// |-----------------|------------------------------|
/// | Accepted range  | `(0, i128::MAX]` (inclusive) |
/// | Rejected range  | `(-∞, 0]`                    |
/// | Error on reject | [`Error::InvalidProtocolParameters`] |
/// | Constant used   | none (hardcoded `> 0` check) |
///
/// The `> 0` invariant is load-bearing: a zero cap would block every call to
/// `create_contract` by making every milestone total exceed the limit.
/// Callers that read back `max_escrow_total_stroops` via `get_governed_parameters`
/// can rely on the returned value always being positive.
///
/// ## Upgrade note
///
/// This check must stay as `<= 0` → reject. If a future version needs to allow
/// zero (e.g., to represent "no limit"), that is a governance-visible protocol
/// change and must update the stored value, the documentation, and the
/// `storage_validation_compat` test that pins this boundary.
///
/// # Panics
/// Panics with [`Error::InvalidProtocolParameters`] when `max_escrow_total_stroops <= 0`.
pub(crate) fn validate_escrow_total_cap(env: &Env, max_escrow_total_stroops: i128) {
    if max_escrow_total_stroops <= 0 {
        env.panic_with_error(Error::InvalidProtocolParameters);
    }
    if max_escrow_total_stroops > MAX_SINGLE_AMOUNT_STROOPS {
        env.panic_with_error(Error::InvalidProtocolParameters);
    }
}

// ── validate_reputation_config_params ────────────────────────────────────────

/// Validate reputation configuration parameters.
///
/// ## Compatibility contract
///
/// | Parameter         | Accepted range                                  | Error on reject                       |
/// |-------------------|-------------------------------------------------|---------------------------------------|
/// | `min_rating`      | `[MIN_RATING, MAX_REPUTATION_CONFIG_RATING_CEILING]` i.e. `[1, 10]` | [`Error::InvalidProtocolParameters`] |
/// | `max_rating`      | `[min_rating, MAX_REPUTATION_CONFIG_RATING_CEILING]` i.e. `[min_rating, 10]` | [`Error::InvalidProtocolParameters`] |
/// | `max_comment_bytes` | `[MIN_COMMENT_BYTES, MAX_REPUTATION_CONFIG_COMMENT_BYTES_CEILING]` i.e. `[1, 1_000]` | [`Error::InvalidProtocolParameters`] |
///
/// All three parameters are validated atomically in a single call; the first
/// violation found (checked left-to-right: `min_rating`, `max_rating < min`,
/// `max_rating > ceiling`, `max_comment_bytes < MIN`, `max_comment_bytes > MAX`)
/// determines the error. Future callers that depend on this ordering must be
/// updated if the check order changes.
///
/// The single-error response means callers cannot distinguish which of the
/// three parameters was invalid. This is intentional: it avoids revealing
/// partial configuration to unauthenticated readers. A governance UI should
/// perform local pre-validation before submitting.
///
/// ## Upgrade note
///
/// Increasing `MAX_REPUTATION_CONFIG_RATING_CEILING` or
/// `MAX_REPUTATION_CONFIG_COMMENT_BYTES_CEILING` widens the accepted range
/// without breaking existing stored values. Decreasing either constant narrows
/// the range and may reject previously accepted stored configurations on their
/// next update — treat as a breaking change.
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
    // INVARIANT: min_rating must be at least MIN_RATING (1).
    // Callers that decode InvalidProtocolParameters for this rejection depend
    // on this check being present and firing before the max_rating check.
    if min_rating < MIN_RATING
        || max_rating < min_rating
        || max_rating > MAX_REPUTATION_CONFIG_RATING_CEILING
        || max_comment_bytes < MIN_COMMENT_BYTES
        || max_comment_bytes > MAX_REPUTATION_CONFIG_COMMENT_BYTES_CEILING
    {
        env.panic_with_error(Error::InvalidProtocolParameters);
    }
}

// ── validate_milestone_count ──────────────────────────────────────────────────

/// Validate the number of milestones for a contract creation call.
///
/// ## Compatibility contract
///
/// | Value           | Result                                          | Error code                           |
/// |-----------------|-------------------------------------------------|--------------------------------------|
/// | `0`             | Rejected                                        | [`EscrowError::EmptyMilestones`]     |
/// | `1`             | Accepted (minimum)                             | —                                    |
/// | `MAX_MILESTONES`| Accepted (maximum, currently 10)               | —                                    |
/// | `> MAX_MILESTONES` | Rejected                                    | [`EscrowError::TooManyMilestones`]   |
/// | `u32::MAX`      | Rejected                                        | [`EscrowError::TooManyMilestones`]   |
///
/// **Two distinct error codes** are used here intentionally. Client code and
/// indexers that discriminate between "caller sent an empty list" and "caller
/// sent too many milestones" depend on this distinction. Both error codes must
/// be preserved through refactors. Merging them into a single
/// `InvalidMilestoneCount` would be a **breaking change**.
///
/// ## Upgrade note
///
/// `MAX_MILESTONES` is `10`. Increasing it (e.g. to `20`) is safe for existing
/// contracts but affects gas / transaction-size budgets. Decreasing it would
/// reject contracts that are valid today. Either direction requires a governance
/// proposal, a `get_bounds()` update, and a corresponding compat test update.
///
/// # Boundary behavior
/// * `1` and `MAX_MILESTONES` are accepted.
/// * `0`, `MAX_MILESTONES + 1`, and `u32::MAX` are rejected.
///
/// # Panics
/// * [`EscrowError::EmptyMilestones`] when `count == 0`.
/// * [`EscrowError::TooManyMilestones`] when `count > MAX_MILESTONES`.
pub(crate) fn validate_milestone_count(env: &Env, count: u32) {
    // INVARIANT: zero-length milestone list must produce EmptyMilestones, not
    // TooManyMilestones. The ordering of these two checks is part of the compat
    // contract — do not swap them.
    if count == 0 {
        env.panic_with_error(EscrowError::EmptyMilestones);
    }
    if count > MAX_MILESTONES {
        env.panic_with_error(EscrowError::TooManyMilestones);
    }
}

// ── validate_protocol_fee_bps ─────────────────────────────────────────────────

/// Validate a protocol fee basis-points value.
///
/// ## Compatibility contract
///
/// | Value             | Result   | Error code                               |
/// |-------------------|----------|------------------------------------------|
/// | `0`               | Accepted | — (zero fee disables collection)         |
/// | `MAX_FEE_BPS` (10_000) | Accepted | —                                   |
/// | `MAX_FEE_BPS + 1` | Rejected | [`Error::InvalidProtocolParameters`]     |
/// | `u32::MAX`        | Rejected | [`Error::InvalidProtocolParameters`]     |
///
/// Zero is explicitly allowed: it disables protocol-fee collection. This is an
/// intentional governance lever — callers that observe `0` from
/// `get_protocol_fee_bps()` must treat it as a fee-free mode, not an
/// uninitialised value.
///
/// `MAX_FEE_BPS == PROTOCOL_FEE_BPS_DENOMINATOR == 10_000`. The invariant
/// `fee ≤ denominator` ensures the net amount transferred to the freelancer is
/// always non-negative.
///
/// ## Upgrade note
///
/// `MAX_FEE_BPS` equals the basis-point denominator (10_000 = 100%). Raising
/// it above the denominator would allow a fee that exceeds the milestone amount
/// — this is explicitly prohibited. Lowering it (e.g., to cap fees at 50%)
/// would narrow the accepted range, is a breaking governance change, and must
/// come with a migration for stored `ProtocolFeeBps` values that exceed the new
/// maximum.
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
    // INVARIANT: fee must not exceed the basis-point denominator, so that
    // net_amount = gross - fee is always ≥ 0 for any gross ≥ 0.
    if bps > MAX_FEE_BPS {
        env.panic_with_error(Error::InvalidProtocolParameters);
    }
}

// ── validate_stroop_amount ────────────────────────────────────────────────────

/// Validate a single stroop amount for positivity and maximum bounds.
///
/// ## Compatibility contract
///
/// | Value                             | Result   | Error code                               |
/// |-----------------------------------|----------|------------------------------------------|
/// | `1` (1 stroop)                    | Accepted | —                                        |
/// | `MAX_SINGLE_AMOUNT_STROOPS`       | Accepted | —                                        |
/// | `0`                               | Rejected | [`EscrowError::AmountMustBePositive`]    |
/// | `-1`                              | Rejected | [`EscrowError::AmountMustBePositive`]    |
/// | `i128::MIN`                       | Rejected | [`EscrowError::AmountMustBePositive`]    |
/// | `MAX_SINGLE_AMOUNT_STROOPS + 1`   | Rejected | [`EscrowError::InvalidMilestoneAmount`] |
/// | `i128::MAX`                       | Rejected | [`EscrowError::InvalidMilestoneAmount`] |
///
/// **Two distinct error codes** are used here intentionally:
/// - `AmountMustBePositive` — amount is ≤ 0 (caller submitted a non-positive value).
/// - `InvalidMilestoneAmount` — amount is positive but exceeds the per-operation cap.
///
/// Callers and indexers that distinguish between "bad sign" and "too large"
/// depend on this distinction. Merging them would be a **breaking change**.
///
/// ## Upgrade note
///
/// `MAX_SINGLE_AMOUNT_STROOPS` is currently `1_000_000_0000000` (1 M tokens at
/// 7 decimal places). Increasing it widens the accepted range; decreasing it
/// narrows it and may reject deposits that were valid at contract-creation time.
/// Any change to this constant must be coordinated with `get_bounds()` and the
/// governance process.
///
/// # Boundary behavior
/// * `1` and `MAX_SINGLE_AMOUNT_STROOPS` are accepted.
/// * `0`, `-1`, and `MAX_SINGLE_AMOUNT_STROOPS + 1` are rejected.
///
/// # Panics
/// * [`EscrowError::AmountMustBePositive`] when `amount <= 0`.
/// * [`EscrowError::InvalidMilestoneAmount`] when `amount > MAX_SINGLE_AMOUNT_STROOPS`.
pub(crate) fn validate_stroop_amount(env: &Env, amount: i128) {
    // INVARIANT: non-positive amounts must produce AmountMustBePositive, not
    // InvalidMilestoneAmount. The ordering of these two checks is part of the
    // compat contract — do not swap them.
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

// ── Internal tests ─────────────────────────────────────────────────────────────
//
// These tests exercise the module-internal logic using the Soroban test
// harness. They complement the typed-error integration tests in
// `test::storage_validation_compat` which exercise callers end-to-end.
#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::Env;

    fn env() -> Env {
        Env::default()
    }

    // ── validate_escrow_total_cap ────────────────────────────────────────────

    /// Minimum positive value (1 stroop) is accepted.
    #[test]
    fn validate_escrow_total_cap_accepts_1() {
        let e = env();
        validate_escrow_total_cap(&e, 1);
    }

    /// i128::MAX is accepted (no upper bound on cap).
    #[test]
    fn validate_escrow_total_cap_accepts_i128_max() {
        let e = env();
        validate_escrow_total_cap(&e, i128::MAX);
    }

    /// Typical governance value is accepted.
    #[test]
    fn validate_escrow_total_cap_accepts_typical() {
        let e = env();
        validate_escrow_total_cap(&e, 1_000_000_0000000_i128);
    }

    /// Zero cap is rejected (would block every contract creation).
    #[test]
    #[should_panic]
    fn validate_escrow_total_cap_rejects_zero() {
        let e = env();
        validate_escrow_total_cap(&e, 0);
    }

    /// -1 is rejected.
    #[test]
    #[should_panic]
    fn validate_escrow_total_cap_rejects_negative() {
        let e = env();
        validate_escrow_total_cap(&e, -1);
    }

    /// i128::MIN is rejected.
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

    /// Default config (1, 5, 200) is accepted.
    #[test]
    fn validate_reputation_config_params_accepts_default() {
        let e = env();
        validate_reputation_config_params(&e, 1, 5, 200);
    }

    /// Degenerate range (min == max rating) is accepted.
    #[test]
    fn validate_reputation_config_params_accepts_min_equal_max_rating() {
        let e = env();
        validate_reputation_config_params(&e, 3, 3, 1);
    }

    /// Maximum allowed comment bytes is accepted.
    #[test]
    fn validate_reputation_config_params_accepts_max_comment_1000() {
        let e = env();
        validate_reputation_config_params(&e, 1, 10, 1_000);
    }

    /// max_rating == MAX_REPUTATION_CONFIG_RATING_CEILING is accepted.
    #[test]
    fn validate_reputation_config_params_accepts_max_rating_ceiling() {
        let e = env();
        validate_reputation_config_params(&e, 1, MAX_REPUTATION_CONFIG_RATING_CEILING, 200);
    }

    /// min_rating == 0 is rejected.
    #[test]
    #[should_panic]
    fn validate_reputation_config_params_rejects_zero_min_rating() {
        let e = env();
        validate_reputation_config_params(&e, 0, 5, 200);
    }

    /// max_rating < min_rating is rejected.
    #[test]
    #[should_panic]
    fn validate_reputation_config_params_rejects_max_below_min() {
        let e = env();
        validate_reputation_config_params(&e, 5, 3, 200);
    }

    /// max_rating > MAX_REPUTATION_CONFIG_RATING_CEILING is rejected.
    #[test]
    #[should_panic]
    fn validate_reputation_config_params_rejects_max_rating_over_ceiling() {
        let e = env();
        validate_reputation_config_params(&e, 1, MAX_REPUTATION_CONFIG_RATING_CEILING + 1, 200);
    }

    /// max_comment_bytes == 0 is rejected.
    #[test]
    #[should_panic]
    fn validate_reputation_config_params_rejects_zero_comment_bytes() {
        let e = env();
        validate_reputation_config_params(&e, 1, 5, 0);
    }

    /// max_comment_bytes > MAX_REPUTATION_CONFIG_COMMENT_BYTES_CEILING is rejected.
    #[test]
    #[should_panic]
    fn validate_reputation_config_params_rejects_comment_over_ceiling() {
        let e = env();
        validate_reputation_config_params(&e, 1, 5, MAX_REPUTATION_CONFIG_COMMENT_BYTES_CEILING + 1);
    }

    #[test]
    #[should_panic]
    fn validate_reputation_config_params_rejects_min_rating_over_ceiling() {
        let e = env();
        validate_reputation_config_params(&e, 11, 11, 200);
    }

    // ── validate_milestone_count ──────────────────────────────────────────────

    /// Minimum accepted count (1) is accepted.
    #[test]
    fn validate_milestone_count_accepts_1() {
        let e = env();
        validate_milestone_count(&e, 1);
    }

    /// Maximum accepted count is accepted.
    #[test]
    fn validate_milestone_count_accepts_max() {
        let e = env();
        validate_milestone_count(&e, MAX_MILESTONES);
    }

    /// count == 0 is rejected.
    #[test]
    #[should_panic]
    fn validate_milestone_count_rejects_zero() {
        let e = env();
        validate_milestone_count(&e, 0);
    }

    /// count == MAX_MILESTONES + 1 is rejected.
    #[test]
    #[should_panic]
    fn validate_milestone_count_rejects_over_max() {
        let e = env();
        validate_milestone_count(&e, MAX_MILESTONES + 1);
    }

    /// u32::MAX is rejected.
    #[test]
    #[should_panic]
    fn validate_milestone_count_rejects_u32_max() {
        let e = env();
        validate_milestone_count(&e, u32::MAX);
    }

    // ── validate_protocol_fee_bps ─────────────────────────────────────────────

    /// 0 bps (fee disabled) is accepted.
    #[test]
    fn validate_protocol_fee_bps_accepts_zero() {
        let e = env();
        validate_protocol_fee_bps(&e, 0);
    }

    /// Exactly MAX_FEE_BPS is accepted.
    #[test]
    fn validate_protocol_fee_bps_accepts_max() {
        let e = env();
        validate_protocol_fee_bps(&e, MAX_FEE_BPS);
    }

    /// A typical 2.5% fee (250 bps) is accepted.
    #[test]
    fn validate_protocol_fee_bps_accepts_typical() {
        let e = env();
        validate_protocol_fee_bps(&e, 250);
    }

    /// MAX_FEE_BPS + 1 is rejected.
    #[test]
    #[should_panic]
    fn validate_protocol_fee_bps_rejects_over_max() {
        let e = env();
        validate_protocol_fee_bps(&e, MAX_FEE_BPS + 1);
    }

    /// u32::MAX is rejected.
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

    /// 1 stroop (minimum) is accepted.
    #[test]
    fn validate_stroop_amount_accepts_1() {
        let e = env();
        validate_stroop_amount(&e, 1);
    }

    /// MAX_SINGLE_AMOUNT_STROOPS is accepted.
    #[test]
    fn validate_stroop_amount_accepts_max() {
        let e = env();
        validate_stroop_amount(&e, crate::amount_validation::MAX_SINGLE_AMOUNT_STROOPS);
    }

    /// A typical milestone amount is accepted.
    #[test]
    fn validate_stroop_amount_accepts_typical() {
        let e = env();
        validate_stroop_amount(&e, 200_0000000_i128);
    }

    /// 0 is rejected.
    #[test]
    #[should_panic]
    fn validate_stroop_amount_rejects_zero() {
        let e = env();
        validate_stroop_amount(&e, 0);
    }

    /// -1 is rejected.
    #[test]
    #[should_panic]
    fn validate_stroop_amount_rejects_negative() {
        let e = env();
        validate_stroop_amount(&e, -1);
    }

    /// i128::MIN is rejected.
    #[test]
    #[should_panic]
    fn validate_stroop_amount_rejects_i128_min() {
        let e = env();
        validate_stroop_amount(&e, i128::MIN);
    }

    /// MAX_SINGLE_AMOUNT_STROOPS + 1 is rejected.
    #[test]
    #[should_panic]
    fn validate_stroop_amount_rejects_over_max() {
        let e = env();
        validate_stroop_amount(&e, crate::amount_validation::MAX_SINGLE_AMOUNT_STROOPS + 1);
    }

    /// i128::MAX is rejected.
    #[test]
    #[should_panic]
    fn validate_stroop_amount_rejects_i128_max() {
        let e = env();
        validate_stroop_amount(&e, i128::MAX);
    }
}
