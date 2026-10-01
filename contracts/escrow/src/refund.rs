//! Refund validation boundary for `refund_unreleased_milestones`.
//!
//! This module is the **single definition** of which refund requests are
//! accepted, which are rejected, and which error each rejection maps to. Both
//! the mutating entrypoint (`Escrow::refund_unreleased_milestones`) and the
//! read-only simulator (`Escrow::simulate_refund`) run *these* functions, so a
//! client can never observe a simulation that disagrees with the real call.
//!
//! # Why a `Result` API
//!
//! The functions here take no [`soroban_sdk::Env`] and never panic: validation
//! is a pure function of `(request, contract status, milestones, ledger time)`.
//! Each caller translates the outcome to its own contract:
//!
//! | Caller | `Err(error)` becomes |
//! | --- | --- |
//! | `refund_unreleased_milestones` | `env.panic_with_error(error)` — the whole call reverts |
//! | `simulate_refund` | `SimulatedRefund { error_code: Some(error as u32), .. }` |
//!
//! Because the checks are pure they are unit-testable without storage, and the
//! mutating and simulating paths cannot drift apart.
//!
//! # Validation boundaries
//!
//! The checks below are evaluated in this order, by both callers:
//!
//! | # | Boundary | Accepted | Rejected with |
//! | --- | --- | --- | --- |
//! | V1 | Non-empty request | at least one index | `EmptyRefundRequest` |
//! | V1 | Request within protocol maximum | `len <= MAX_REFUND_REQUEST_LEN` | `IndexOutOfBounds` |
//! | V1 | Duplicate-free request | every index distinct | `DuplicateMilestoneInRefund` |
//! | V2 | Lifecycle state | `Created`, `Funded` or `Disputed` | `InvalidState` |
//! | V3 | Index within schedule | `index < milestones.len()` | `IndexOutOfBounds` |
//! | V3 | Milestone not released | `released == false` | `MilestoneAlreadyReleased` |
//! | V3 | Milestone not already refunded | `refunded == false` | `AlreadyRefunded` |
//! | V3 | Deadline satisfied | no deadline, or `now > deadline` | `MilestoneNotOverdue` |
//! | V3 | Request total | `checked_add` of every amount | `PotentialOverflow` |
//! | V4 | Balance arithmetic | `funded - released - refunded` fits `i128` | `PotentialOverflow` |
//! | V5 | Request fits the balance | `available_balance >= total` | `InsufficientFunds` |
//!
//! Order is part of the interface: the first failing boundary decides the error
//! a caller observes. In particular the shape checks in [`validate_request`] run
//! before any storage access, the milestone checks in [`validate_milestones`]
//! report the lowest offending index, and the balance check runs last so a
//! request that is malformed *and* unaffordable reports the malformed part.
//!
//! # Invariants
//!
//! - **I1 — Single source of truth.** The live entrypoint and the simulator call
//!   the same functions; there is no second copy of the boundary to drift.
//! - **I2 — Total.** Every input maps to `Ok` or to one of the typed [`Error`]
//!   codes in the table. No code path can panic with a raw arithmetic fault, so
//!   failures stay diagnosable from the error code alone.
//! - **I3 — Atomic.** Nothing here mutates state. A rejected request leaves
//!   storage byte-identical, which is what makes client retries of the same
//!   request deterministic and safe (`AlreadyRefunded` on replay, exactly like
//!   the first rejection).
//! - **I4 — Deterministic.** The result depends only on the request, the stored
//!   contract/milestone snapshot and the ledger timestamp. Soroban executes one
//!   invocation at a time against a fixed snapshot, so validation and the
//!   mutation it authorises observe the same state.
//! - **I5 — Authorization is separate and mandatory.** Validation answers *is
//!   this request well formed and affordable*; the entrypoint still enforces
//!   `contract.client.require_auth()` before it reads or writes balances. Both
//!   gates must pass, so a failed authorization can never be turned into a
//!   refund.
//! - **I6 — Accounting.** The refunded total is bounded by
//!   `funded_amount - released_amount - refunded_amount`, so an accepted request
//!   preserves `released_amount + refunded_amount <= funded_amount`.

use crate::{Contract, ContractStatus, Error, Milestone};
use soroban_sdk::Vec;

/// Largest number of indices a single refund request may carry.
///
/// A contract can never hold more than `MAX_MAX_MILESTONES` milestones — the
/// absolute ceiling of the configurable max-milestones setting — so a longer
/// request cannot be satisfied by any reachable state. Rejecting it up front
/// keeps the duplicate scan and the per-index checks bounded by the schedule
/// size instead of by attacker-controlled request length.
pub const MAX_REFUND_REQUEST_LEN: u32 = crate::MAX_MAX_MILESTONES;

/// V1 — request shape: non-empty, within the protocol maximum, duplicate-free.
///
/// Returns:
/// * `Err(EmptyRefundRequest)` when no index was supplied,
/// * `Err(IndexOutOfBounds)` when the request is longer than
///   [`MAX_REFUND_REQUEST_LEN`] — the same error the per-index bounds check in
///   [`validate_milestones`] would raise, because such a request necessarily
///   contains an index outside every reachable schedule,
/// * `Err(DuplicateMilestoneInRefund)` when one index is requested twice.
///
/// Duplicate detection is `O(n^2)` over `n = len(milestone_indices)`; the
/// length bound caps it at 4 950 comparisons for the worst accepted request.
pub fn validate_request(milestone_indices: &Vec<u32>) -> Result<(), Error> {
    if milestone_indices.is_empty() {
        return Err(Error::EmptyRefundRequest);
    }

    let len = milestone_indices.len();
    if len > MAX_REFUND_REQUEST_LEN {
        return Err(Error::IndexOutOfBounds);
    }

    for i in 0..len {
        let current = milestone_indices.get(i).unwrap();
        for j in (i + 1)..len {
            if current == milestone_indices.get(j).unwrap() {
                return Err(Error::DuplicateMilestoneInRefund);
            }
        }
    }

    Ok(())
}

/// V2 — lifecycle: only `Created`, `Funded` and `Disputed` contracts may refund.
///
/// `Completed`, `Cancelled` and `Refunded` are terminal for refund purposes and
/// are rejected with `InvalidState`. The entrypoint additionally rejects
/// finalized contracts before this check; see `refund_unreleased_milestones`.
pub fn validate_status(contract: &Contract) -> Result<(), Error> {
    match contract.status {
        ContractStatus::Created | ContractStatus::Funded | ContractStatus::Disputed => Ok(()),
        _ => Err(Error::InvalidState),
    }
}

/// V3 — per-index milestone checks plus the checked request total.
///
/// `now_seconds` is the ledger timestamp; a milestone with a deadline is only
/// refundable when `now_seconds > deadline` (strictly greater, so a request
/// issued at exactly the deadline is still `MilestoneNotOverdue`). A milestone
/// without a deadline never expires and may be refunded at any time.
///
/// Returns the sum to refund, computed with `checked_add` so an overflowing
/// schedule reports `PotentialOverflow` instead of wrapping or aborting with an
/// untranslatable arithmetic fault.
pub fn validate_milestones(
    milestones: &Vec<Milestone>,
    milestone_indices: &Vec<u32>,
    now_seconds: u64,
) -> Result<i128, Error> {
    let milestones_len = milestones.len();
    let mut total: i128 = 0;

    for index in milestone_indices.iter() {
        if index >= milestones_len {
            return Err(Error::IndexOutOfBounds);
        }

        let milestone = milestones.get(index).unwrap();

        if milestone.released {
            return Err(Error::MilestoneAlreadyReleased);
        }

        if milestone.refunded {
            return Err(Error::AlreadyRefunded);
        }

        if let Some(deadline) = milestone.deadline {
            if now_seconds <= deadline {
                return Err(Error::MilestoneNotOverdue);
            }
        }

        total = total
            .checked_add(milestone.amount)
            .ok_or(Error::PotentialOverflow)?;
    }

    Ok(total)
}

/// V4 — the contract's spendable balance: `funded - released - refunded`.
///
/// Uses checked subtraction, so accounting that cannot be expressed as an `i128`
/// difference reports `PotentialOverflow` instead of aborting with an
/// untranslatable arithmetic fault.
///
/// A *negative* result is representable and is returned as-is: it means the
/// stored accounting is over-committed (`released_amount + refunded_amount >
/// funded_amount`). [`ensure_available_balance`] then rejects any positive
/// request against it with `InsufficientFunds`, which is the same verdict the
/// pre-boundary implementation produced — an over-committed contract reports
/// `InsufficientFunds`, and only unrepresentable arithmetic reports
/// `PotentialOverflow`.
pub fn available_balance(contract: &Contract) -> Result<i128, Error> {
    contract
        .funded_amount
        .checked_sub(contract.released_amount)
        .and_then(|available| available.checked_sub(contract.refunded_amount))
        .ok_or(Error::PotentialOverflow)
}

/// V5 — the requested total must fit inside the available balance.
///
/// Equality is accepted: refunding exactly the remaining balance is valid and
/// drives the contract to a terminal status.
pub fn ensure_available_balance(contract: &Contract, refund_amount: i128) -> Result<(), Error> {
    if available_balance(contract)? < refund_amount {
        return Err(Error::InsufficientFunds);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Unit tests for the pure refund validation boundary.
    //!
    //! These pin every accepted/rejected branch and the boundary values
    //! (schedule length, request length, deadline equality, exact balance,
    //! `i128` overflow) without touching storage; end-to-end coverage of the
    //! same boundaries through the entrypoint lives in
    //! `crate::test::refund_validation_boundaries`.

    use super::*;
    use crate::ReleaseAuthorization;
    use soroban_sdk::{testutils::Address as _, vec, Address, Env};

    /// Ledger timestamp used by every deadline case.
    const NOW: u64 = 1_000;

    fn milestone(amount: i128) -> Milestone {
        Milestone {
            amount,
            funded_amount: amount,
            released: false,
            refunded: false,
            work_evidence: None,
            refunded_amount: 0,
            deadline: None,
        }
    }

    fn contract(status: ContractStatus, funded: i128, released: i128, refunded: i128) -> Contract {
        let env = Env::default();
        Contract {
            client: Address::generate(&env),
            freelancer: Address::generate(&env),
            arbiter: None,
            status,
            total_deposited: funded,
            funded_amount: funded,
            released_amount: released,
            refunded_amount: refunded,
            release_authorization: ReleaseAuthorization::ClientOnly,
            reputation_issued: false,
        }
    }

    // ── V1: request shape ────────────────────────────────────────────────────

    #[test]
    fn accepts_distinct_indices() {
        let env = Env::default();
        let indices = vec![&env, 0_u32, 1_u32, 2_u32];
        assert_eq!(validate_request(&indices), Ok(()));
    }

    #[test]
    fn rejects_empty_request() {
        let env = Env::default();
        let indices = vec![&env];
        assert_eq!(validate_request(&indices), Err(Error::EmptyRefundRequest));
    }

    #[test]
    fn rejects_adjacent_duplicates() {
        let env = Env::default();
        let indices = vec![&env, 1_u32, 1_u32];
        assert_eq!(
            validate_request(&indices),
            Err(Error::DuplicateMilestoneInRefund)
        );
    }

    #[test]
    fn rejects_duplicate_spanning_the_request() {
        let env = Env::default();
        let indices = vec![&env, 0_u32, 1_u32, 2_u32, 0_u32];
        assert_eq!(
            validate_request(&indices),
            Err(Error::DuplicateMilestoneInRefund)
        );
    }

    #[test]
    fn accepts_request_at_the_protocol_maximum() {
        let env = Env::default();
        let mut indices = soroban_sdk::Vec::new(&env);
        for index in 0..MAX_REFUND_REQUEST_LEN {
            indices.push_back(index);
        }

        assert_eq!(validate_request(&indices), Ok(()));
    }

    #[test]
    fn rejects_request_beyond_the_protocol_maximum() {
        let env = Env::default();
        let mut indices = soroban_sdk::Vec::new(&env);
        for index in 0..=MAX_REFUND_REQUEST_LEN {
            indices.push_back(index);
        }

        assert_eq!(validate_request(&indices), Err(Error::IndexOutOfBounds));
    }

    #[test]
    fn rejects_oversized_request_before_scanning_for_duplicates() {
        let env = Env::default();
        let mut indices = soroban_sdk::Vec::new(&env);
        for _ in 0..=MAX_REFUND_REQUEST_LEN {
            indices.push_back(7_u32);
        }

        assert_eq!(validate_request(&indices), Err(Error::IndexOutOfBounds));
    }

    // ── V2: lifecycle state ──────────────────────────────────────────────────

    #[test]
    fn accepts_active_statuses() {
        for status in [
            ContractStatus::Created,
            ContractStatus::Funded,
            ContractStatus::Disputed,
        ] {
            assert_eq!(validate_status(&contract(status, 10, 0, 0)), Ok(()));
        }
    }

    #[test]
    fn rejects_every_other_status() {
        for status in [
            ContractStatus::Accepted,
            ContractStatus::Completed,
            ContractStatus::Cancelled,
            ContractStatus::Refunded,
            ContractStatus::PartiallyFunded,
        ] {
            assert_eq!(
                validate_status(&contract(status, 10, 0, 0)),
                Err(Error::InvalidState)
            );
        }
    }

    // ── V3: milestone checks and the request total ───────────────────────────

    fn schedule(env: &Env, amounts: &[i128]) -> soroban_sdk::Vec<Milestone> {
        let mut milestones = soroban_sdk::Vec::new(env);
        for amount in amounts {
            milestones.push_back(milestone(*amount));
        }
        milestones
    }

    fn milestone_with(
        amount: i128,
        released: bool,
        refunded: bool,
        deadline: Option<u64>,
    ) -> Milestone {
        let mut milestone = milestone(amount);
        milestone.released = released;
        milestone.refunded = refunded;
        milestone.deadline = deadline;
        milestone
    }

    fn single(env: &Env, milestone: Milestone) -> soroban_sdk::Vec<Milestone> {
        let mut milestones = soroban_sdk::Vec::new(env);
        milestones.push_back(milestone);
        milestones
    }

    #[test]
    fn accepts_last_index_of_the_schedule() {
        let env = Env::default();
        let milestones = schedule(&env, &[10, 20, 30]);
        let indices = vec![&env, 2_u32];
        assert_eq!(validate_milestones(&milestones, &indices, NOW), Ok(30));
    }

    #[test]
    fn sums_every_requested_amount() {
        let env = Env::default();
        let milestones = schedule(&env, &[10, 20, 30]);
        let indices = vec![&env, 0_u32, 2_u32];
        assert_eq!(validate_milestones(&milestones, &indices, NOW), Ok(40));
    }

    #[test]
    fn rejects_index_equal_to_schedule_length() {
        let env = Env::default();
        let milestones = schedule(&env, &[10, 20, 30]);
        let indices = vec![&env, 3_u32];
        assert_eq!(
            validate_milestones(&milestones, &indices, NOW),
            Err(Error::IndexOutOfBounds)
        );
    }

    #[test]
    fn rejects_index_at_the_u32_ceiling() {
        let env = Env::default();
        let milestones = schedule(&env, &[10, 20, 30]);
        let indices = vec![&env, u32::MAX];
        assert_eq!(
            validate_milestones(&milestones, &indices, NOW),
            Err(Error::IndexOutOfBounds)
        );
    }

    #[test]
    fn rejects_released_milestone() {
        let env = Env::default();
        let milestones = single(&env, milestone_with(10, true, false, None));
        let indices = vec![&env, 0_u32];
        assert_eq!(
            validate_milestones(&milestones, &indices, NOW),
            Err(Error::MilestoneAlreadyReleased)
        );
    }

    #[test]
    fn rejects_already_refunded_milestone() {
        let env = Env::default();
        let milestones = single(&env, milestone_with(10, false, true, None));
        let indices = vec![&env, 0_u32];
        assert_eq!(
            validate_milestones(&milestones, &indices, NOW),
            Err(Error::AlreadyRefunded)
        );
    }

    #[test]
    fn reports_the_first_failing_index_in_request_order() {
        let env = Env::default();
        let mut milestones = soroban_sdk::Vec::new(&env);
        milestones.push_back(milestone(10));
        milestones.push_back(milestone_with(20, true, false, None));
        let indices = vec![&env, 0_u32, 1_u32];
        assert_eq!(
            validate_milestones(&milestones, &indices, NOW),
            Err(Error::MilestoneAlreadyReleased)
        );
    }

    #[test]
    fn rejects_milestone_with_an_unexpired_deadline() {
        let env = Env::default();
        let milestones = single(&env, milestone_with(10, false, false, Some(NOW + 1)));
        let indices = vec![&env, 0_u32];
        assert_eq!(
            validate_milestones(&milestones, &indices, NOW),
            Err(Error::MilestoneNotOverdue)
        );
    }

    #[test]
    fn rejects_milestone_at_exactly_the_deadline() {
        let env = Env::default();
        let milestones = single(&env, milestone_with(10, false, false, Some(NOW)));
        let indices = vec![&env, 0_u32];
        assert_eq!(
            validate_milestones(&milestones, &indices, NOW),
            Err(Error::MilestoneNotOverdue)
        );
    }

    #[test]
    fn accepts_milestone_one_second_past_the_deadline() {
        let env = Env::default();
        let milestones = single(&env, milestone_with(10, false, false, Some(NOW - 1)));
        let indices = vec![&env, 0_u32];
        assert_eq!(validate_milestones(&milestones, &indices, NOW), Ok(10));
    }

    #[test]
    fn accepts_milestone_without_a_deadline() {
        let env = Env::default();
        let milestones = single(&env, milestone_with(10, false, false, None));
        let indices = vec![&env, 0_u32];
        assert_eq!(validate_milestones(&milestones, &indices, u64::MAX), Ok(10));
    }

    #[test]
    fn rejects_request_whose_total_overflows() {
        let env = Env::default();
        let milestones = schedule(&env, &[i128::MAX, 1]);
        let indices = vec![&env, 0_u32, 1_u32];
        assert_eq!(
            validate_milestones(&milestones, &indices, NOW),
            Err(Error::PotentialOverflow)
        );
    }

    // ── V4/V5: available balance ─────────────────────────────────────────────

    #[test]
    fn available_balance_is_funded_minus_released_minus_refunded() {
        let subject = contract(ContractStatus::Funded, 100, 30, 20);
        assert_eq!(available_balance(&subject), Ok(50));
    }

    #[test]
    fn available_balance_can_be_negative_when_accounting_is_broken() {
        let subject = contract(ContractStatus::Funded, 100, 101, 0);
        assert_eq!(available_balance(&subject), Ok(-1));
    }

    #[test]
    fn available_balance_reports_overflow_for_unrepresentable_accounting() {
        let subject = contract(ContractStatus::Funded, i128::MIN, 1, 0);
        assert_eq!(available_balance(&subject), Err(Error::PotentialOverflow));
    }

    #[test]
    fn ensure_available_balance_accepts_the_exact_remaining_balance() {
        let subject = contract(ContractStatus::Funded, 100, 30, 20);
        assert_eq!(ensure_available_balance(&subject, 50), Ok(()));
    }

    #[test]
    fn ensure_available_balance_rejects_one_stroop_beyond_the_balance() {
        let subject = contract(ContractStatus::Funded, 100, 30, 20);
        assert_eq!(
            ensure_available_balance(&subject, 51),
            Err(Error::InsufficientFunds)
        );
    }

    #[test]
    fn ensure_available_balance_rejects_an_over_committed_balance() {
        let subject = contract(ContractStatus::Funded, 100, 101, 0);
        assert_eq!(
            ensure_available_balance(&subject, 1),
            Err(Error::InsufficientFunds)
        );
    }

    #[test]
    fn ensure_available_balance_surfaces_unrepresentable_accounting_first() {
        let subject = contract(ContractStatus::Funded, i128::MIN, 1, 0);
        assert_eq!(
            ensure_available_balance(&subject, 1),
            Err(Error::PotentialOverflow)
        );
    }
}
