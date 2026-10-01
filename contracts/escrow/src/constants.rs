//! Deterministic policy constants and checked arithmetic for the escrow contract.
//!
//! # Why this module exists
//!
//! A boundary value that decides whether an operation succeeds, fails with a
//! typed error, or is rejected must be defined exactly once. When a limit or an
//! increment is written as a literal at each call site, two paths that are
//! supposed to behave identically can diverge — one wraps on overflow while the
//! other rejects, one accepts a value while the other refuses it. Failure
//! recovery then depends on *which* path happened to run, which is the
//! non-determinism this module removes: every limit and every increment used by
//! the recovery paths below is owned here and shared by every caller.
//!
//! # The pending reputation-credit recovery ledger
//!
//! `DataKey::PendingReputationCredits(freelancer)` is a durable counter of
//! "this freelancer completed a contract and is therefore owed one reputation
//! issuance":
//!
//! * **Accrual** — exactly one credit is added whenever a contract reaches
//!   [`crate::ContractStatus::Completed`], on every terminal path (final
//!   milestone release, batch settlement, partial-refund completion, and
//!   arbiter dispute resolution). A fully `Refunded` contract never accrues.
//! * **Consumption** — exactly one credit is removed by `issue_reputation`, the
//!   only way a completion becomes a stored reputation record.
//!
//! # Invariants
//!
//! | # | Invariant |
//! |---|-----------|
//! | I1 | A stored ledger value is always within `0..=MAX_PENDING_REPUTATION_CREDITS`. |
//! | I2 | An accrual changes a ledger by exactly `REPUTATION_CREDIT_INCREMENT`; a consumption removes exactly the same amount. |
//! | I3 | Accrual past the ceiling and consumption from an empty ledger are *rejected* — never wrapped, saturated, or silently clamped. |
//! | I4 | Rejection is reported as `None` from the pure helpers, so the caller raises a typed contract error (`Error::PotentialOverflow` / `Error::NotCompleted`) and the stored ledger is left unchanged. A retry therefore observes exactly the same state and fails the same way. |
//!
//! The helpers below are the only supported arithmetic for that ledger, and the
//! `const` assertions at the end of this file turn an inconsistent policy into a
//! compile-time failure instead of a runtime surprise.

/// Minimum valid reputation rating (inclusive).
///
/// # Invariant
/// `MIN_RATING >= 1` — zero is not a valid rating because absence of rating
/// is represented by `None`, not by a zero value.  Every reputation-issuing
/// path (`issue_reputation`) must reject `rating < MIN_RATING`.
pub const MIN_RATING: u32 = 1;

/// Maximum valid reputation rating (inclusive).
///
/// # Invariant
/// `MAX_RATING >= MIN_RATING` — the valid rating interval must be non-empty.
/// The current 1–5 scale matches common freelance platforms and is small
/// enough to avoid precision disputes.  `issue_reputation` must reject
/// `rating > MAX_RATING`.
pub const MAX_RATING: u32 = 5;

/// Max byte length of a reputation feedback comment.
///
/// # Invariant
/// `MAX_COMMENT_BYTES >= 1` — a zero-length comment is rejected as empty; use
/// a minimum of 1 byte so the interval `[1, MAX_COMMENT_BYTES]` is non-empty.
/// `issue_reputation` must reject comments whose `len() > MAX_COMMENT_BYTES`.
pub const MAX_COMMENT_BYTES: u32 = 200;

/// Unit increment for pending reputation credits.
///
/// Every accrual adds exactly this amount and every consumption removes exactly
/// this amount, so the ledger is a faithful count of completed contracts that
/// have not yet been rated. It is deliberately `1`: one completed contract
/// yields exactly one issuable reputation.
pub const REPUTATION_CREDIT_INCREMENT: i128 = 1;

/// Deterministic upper bound on a single freelancer's pending-credit ledger.
///
/// The ledger counts completed-but-unrated contracts, so sitting at this value
/// indicates a bug or an attempted accounting attack rather than a legitimate
/// workload. Accruing past it is rejected with a typed error instead of
/// wrapping, which keeps failure recovery deterministic (see
/// [`accrue_pending_credit`]).
///
/// The bound is many orders of magnitude below `i128::MAX`, so checked
/// arithmetic can never be the first thing to fail, and it is far above any
/// realistic number of completed contracts for a single freelancer, so it never
/// acts as a business limit.
pub const MAX_PENDING_REPUTATION_CREDITS: i128 = 1_000_000;

/// Basis-point scaling factor for `get_average_rating` (×10_000 preserves four decimal places).
///
/// # Invariant
/// `SCALE > 0` — the scaling factor must be strictly positive so that the
/// fixed-point arithmetic used in `get_average_rating` never divides by zero
/// and always yields a non-negative result for valid ratings.
pub const SCALE: i128 = 10_000;

/// Upper bound on the `limit` parameter of paginated read views.
///
/// Keeps per-call storage reads bounded and prevents callers from requesting
/// unbounded scans in a single invocation.
///
/// # Invariant
/// `PAGE_CEILING >= 1` — at least one record per page must be returnable;
/// a ceiling of zero would make every paginated read vacuous.
pub const PAGE_CEILING: u32 = 50;

// ── Pending reputation-credit ledger arithmetic ───────────────────────────────
//
// These helpers are pure so that the outcome of an accrual or a consumption is
// a function of the stored value alone. They are the single implementation used
// by every completion path and by `issue_reputation`.

/// Returns `true` when `value` is a legal stored ledger value (invariant I1).
pub fn is_valid_pending_credit_ledger(value: i128) -> bool {
    (0..=MAX_PENDING_REPUTATION_CREDITS).contains(&value)
}

/// Accrues exactly one pending reputation credit onto `current`.
///
/// Returns the new ledger value, or `None` when the accrual must be rejected:
///
/// * `current` is outside the legal ledger range (corrupted state, invariant
///   I1), or
/// * adding [`REPUTATION_CREDIT_INCREMENT`] would exceed
///   [`MAX_PENDING_REPUTATION_CREDITS`] (invariant I3), or
/// * the addition is not representable (defensive; unreachable while the
///   ceiling holds).
///
/// Returning `None` instead of wrapping or saturating is what makes recovery
/// deterministic: the caller maps it onto a single typed error and leaves the
/// stored ledger untouched, so a retry re-observes the same state.
pub fn accrue_pending_credit(current: i128) -> Option<i128> {
    if !is_valid_pending_credit_ledger(current) {
        return None;
    }
    let next = current.checked_add(REPUTATION_CREDIT_INCREMENT)?;
    is_valid_pending_credit_ledger(next).then_some(next)
}

/// Consumes exactly one pending reputation credit from `current`.
///
/// Returns the new ledger value, or `None` when there is nothing to consume
/// (`current < REPUTATION_CREDIT_INCREMENT`, i.e. the freelancer has no
/// completed contract awaiting a rating) or when the result would no longer be
/// a legal ledger value (invariant I1).
pub fn consume_pending_credit(current: i128) -> Option<i128> {
    if current < REPUTATION_CREDIT_INCREMENT {
        return None;
    }
    let next = current.checked_sub(REPUTATION_CREDIT_INCREMENT)?;
    is_valid_pending_credit_ledger(next).then_some(next)
}

// ── Compile-time policy guards ────────────────────────────────────────────────
//
// An inconsistent policy must fail the build, not shrink the recovery window or
// make the ceiling unreachable in production.

const _: () = assert!(REPUTATION_CREDIT_INCREMENT > 0);
const _: () = assert!(MAX_PENDING_REPUTATION_CREDITS >= REPUTATION_CREDIT_INCREMENT);
const _: () = assert!(SCALE > 0);
const _: () = assert!(MIN_RATING <= MAX_RATING);
