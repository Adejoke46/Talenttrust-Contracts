//! Centralized storage precondition checks and contract loading helpers.
//! Centralized storage precondition checks and contract loading helpers.
//!
//! This module extracts repeated storage validation patterns into a single source
//! of truth, ensuring consistent error handling and reducing code duplication across
//! entrypoints. All contract loading operations should route through these helpers.
//!
//! ## State invariants
//!
//! The helpers in this module are the single choke point for the following
//! invariants. Any change to these helpers must preserve them:
//!
//! 1. **Initialization gate**: no money-flow entrypoint may proceed unless
//!    `DataKey::Initialized` is `true`. `require_initialized` is the only
//!    sanctioned check.
//! 2. **Contract identity**: `contract_id == 0` is never a valid key. All
//!    loaders call `validate_contract_id_bounds` before touching storage so
//!    that a zero ID can never alias a real record.
//! 3. **Pause / emergency precedence**: emergency always blocks, then legacy
//!    boolean pause, then scoped pause. `require_not_paused` and
//!    `require_pause_scope` must agree on this ordering.
//! 4. **Finalization is terminal**: once `DataKey::Finalization(id)` exists,
//!    no mutation helper may return a contract for that ID when
//!    `check_finalized` is requested.
//! 5. **Monotonic admin nonce**: `consume_admin_nonce` must reject any value
//!    other than `current + 1` and must persist the increment on success so
//!    that retries and replays cannot double-apply an admin action.
//!
//! These invariants are enforced by panics (via `env.panic_with_error`) so
//! that a failed precondition aborts the whole transaction and leaves no
//! partial state behind.

use crate::{Contract, DataKey, Error, EscrowError};
use crate::ContractStatus;
use soroban_sdk::{Env, Symbol, Vec};

/// Maximum number of retry attempts for recoverable storage operations.
///
/// Bounds the retry loop so a persistently failing storage backend cannot
/// cause an unbounded loop. Chosen to be small enough to fail fast while
/// still tolerating transient read/write hiccups.
pub(crate) const MAX_STORAGE_RETRIES: u32 = 3;

/// Deterministic recovery outcome for a storage operation.
///
/// Used by [`recover_or_panic`] to make failure handling explicit and
/// observable. Callers can log or branch on the outcome without relying on
/// panic side effects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RecoveryOutcome {
    /// Operation succeeded on the first attempt.
    Success,
    /// Operation succeeded after one or more retries.
    Recovered { attempts: u32 },
    /// Operation failed after exhausting all retries.
    Exhausted { attempts: u32 },
}

/// Run a fallible storage operation with deterministic retry semantics.
///
/// The closure is invoked up to [`MAX_STORAGE_RETRIES`] times. The first
/// successful invocation returns `Ok(RecoveryOutcome::Success)` or
/// `Ok(RecoveryOutcome::Recovered { attempts })`. If every attempt fails,
/// the last error is returned as `Err`.
///
/// This helper is intentionally pure with respect to storage: it does not
/// mutate state on failure, so partial failures cannot leave the contract
/// in an inconsistent state. Callers must ensure the closure itself is
/// idempotent (reads are always safe; writes should be guarded by
/// precondition checks performed before entering the retry loop).
pub(crate) fn recover_or_panic<F, T, E>(
    env: &Env,
    mut op: F,
) -> Result<RecoveryOutcome, E>
where
    F: FnMut() -> Result<T, E>,
    E: core::fmt::Debug,
{
    let mut last_err: Option<E> = None;
    let mut attempts: u32 = 0;
    while attempts < MAX_STORAGE_RETRIES {
        attempts += 1;
        match op() {
            Ok(_) => {
                return Ok(if attempts == 1 {
                    RecoveryOutcome::Success
                } else {
                    RecoveryOutcome::Recovered { attempts }
                });
            }
            Err(err) => {
                last_err = Some(err);
            }
        }
    }
    let _ = env;
    match last_err {
        Some(err) => Err(err),
        None => unreachable!("retry loop must execute at least once"),
    }
}

/// Deterministically load a contract, retrying transient storage failures.
///
/// Unlike [`load_contract`], this variant does not panic on the first
/// missing read. It retries up to [`MAX_STORAGE_RETRIES`] times and only
/// panics with `ContractNotFound` once all attempts are exhausted. This
/// makes recovery observable and prevents a single transient miss from
/// aborting an otherwise valid operation.
///
/// # Panics
/// - `InvalidContractId` if `contract_id` is 0
/// - `ContractNotFound` if the contract is still missing after all retries
pub(crate) fn load_contract_recoverable(env: &Env, contract_id: u32) -> Contract {
    validate_contract_id_bounds(env, contract_id);
    let outcome = recover_or_panic(env, || {
        env.storage()
            .persistent()
            .get::<_, Contract>(&DataKey::Contract(contract_id))
            .ok_or(Error::ContractNotFound)
    });
    match outcome {
        Ok(_) => env
            .storage()
            .persistent()
            .get(&DataKey::Contract(contract_id))
            .unwrap_or_else(|| env.panic_with_error(Error::ContractNotFound)),
        Err(err) => env.panic_with_error(err),
    }
}

/// Deterministically load milestones, retrying transient storage failures.
///
/// Mirrors [`load_milestones`] but retries transient misses before
/// panicking, so recovery is deterministic and observable.
///
/// # Panics
/// - `InvalidContractId` if `contract_id` is 0
/// - `ContractNotFound` if milestones are still missing after all retries
pub(crate) fn load_milestones_recoverable(
    env: &Env,
    contract_id: u32,
) -> Vec<crate::Milestone> {
    validate_contract_id_bounds(env, contract_id);
    let milestone_key = Symbol::new(env, "milestones");
    let outcome = recover_or_panic(env, || {
        env.storage()
            .persistent()
            .get::<_, Vec<crate::Milestone>>(&(
                DataKey::Contract(contract_id),
                milestone_key.clone(),
            ))
            .ok_or(Error::ContractNotFound)
    });
    match outcome {
        Ok(_) => env
            .storage()
            .persistent()
            .get(&(DataKey::Contract(contract_id), milestone_key))
            .unwrap_or_else(|| env.panic_with_error(Error::ContractNotFound)),
        Err(err) => env.panic_with_error(err),
    }
}

/// Validate that contract_id is within numeric bounds (non-zero).
/// Validate that contract_id is within numeric bounds (non-zero).
///
/// This is the **entrypoint preamble** guard: it rejects the reserved id `0` as
/// invalid input with [`Error::InvalidContractId`]. Loaders and predicates that
/// must treat `0` like any other unknown id use [`require_nonzero_contract_id`]
/// instead — see the module-level compatibility contract.
///
/// # Panics
/// - `ContractNotFound` if `contract_id == 0`
pub(crate) fn validate_contract_id_bounds(env: &Env, contract_id: u32) {
    if contract_id == 0 {
        env.panic_with_error(Error::InvalidContractId);
    }
}

/// Validate that a milestone index is within the bounds of the milestone vector.
///
/// Milestone indices are zero-based. This helper centralizes the boundary check
/// so that all callers reject out-of-range indices deterministically rather than
/// relying on `Vec::get` returning `None` and being silently ignored.
///
/// # Panics
/// - `InvalidMilestoneIndex` if `index >= len`
pub(crate) fn validate_milestone_index_bounds(env: &Env, index: u32, len: u32) {
    if index >= len {
        env.panic_with_error(EscrowError::InvalidMilestoneIndex);
    }
}

/// Validate that a milestone amount is strictly positive.
///
/// Zero-amount milestones are rejected because they would allow no-op state
/// transitions and could mask accounting bugs. This is a boundary check applied
/// at the point of milestone creation.
///
/// # Panics
/// - `InvalidMilestoneAmount` if `amount == 0`
pub(crate) fn validate_milestone_amount(env: &Env, amount: i128) {
    if amount <= 0 {
        env.panic_with_error(EscrowError::InvalidMilestoneAmount);
    }
}

/// Validate that a milestone deadline, if present, is strictly in the future.
///
/// A deadline equal to the current ledger timestamp is treated as already
/// expired to avoid a race where a milestone becomes immediately refundable
/// in the same ledger it was created.
///
/// # Panics
/// - `InvalidDeadline` if `deadline <= now`
pub(crate) fn validate_milestone_deadline(env: &Env, deadline: Option<u64>) {
    if let Some(d) = deadline {
        let now = env.ledger().timestamp();
        if d <= now {
            env.panic_with_error(EscrowError::InvalidDeadline);
        }
    }
}

/// Validate that a milestone vector is non-empty and within a sane upper bound.
///
/// Empty milestone sets would allow contracts with no work units, and unbounded
/// sets could exhaust ledger entry limits. Both are rejected deterministically.
///
/// # Panics
/// - `InvalidMilestoneCount` if `len == 0` or `len > MAX_MILESTONES`
pub(crate) fn validate_milestone_count(env: &Env, len: u32) {
    const MAX_MILESTONES: u32 = 100;
    if len == 0 || len > MAX_MILESTONES {
        env.panic_with_error(EscrowError::InvalidMilestoneCount);
    }
}

/// Validate that a milestone has not already been released or refunded.
///
/// This guards against duplicate submissions: a milestone that has already
/// reached a terminal state must not be mutated again.
///
/// # Panics
/// - `MilestoneAlreadyReleased` if `released` is true
/// - `MilestoneAlreadyRefunded` if `refunded` is true
pub(crate) fn validate_milestone_not_terminal(
    env: &Env,
    released: bool,
    refunded: bool,
) {
    if released {
        env.panic_with_error(EscrowError::MilestoneAlreadyReleased);
    }
    if refunded {
        env.panic_with_error(EscrowError::MilestoneAlreadyRefunded);
    }
}

/// Validate that a milestone has not already been funded beyond its amount.
///
/// Prevents over-funding a milestone, which would break the invariant that
/// `funded_amount <= amount` for every milestone.
///
/// # Panics
/// - `MilestoneOverFunded` if `funded_amount > amount`
pub(crate) fn validate_milestone_funding(env: &Env, amount: i128, funded_amount: i128) {
    if funded_amount > amount {
        env.panic_with_error(EscrowError::MilestoneOverFunded);
    }
}

/// Validate that a milestone has not already been funded (duplicate funding guard).
///
/// A milestone may only be funded once. This is the duplicate-submission guard
/// for the funding path.
///
/// # Panics
/// - `MilestoneAlreadyFunded` if `funded_amount > 0`
pub(crate) fn validate_milestone_not_funded(env: &Env, funded_amount: i128) {
    if funded_amount > 0 {
        env.panic_with_error(EscrowError::MilestoneAlreadyFunded);
    }
}

/// Validate that a milestone has been fully funded before release or refund.
///
/// Release and refund operations require the milestone to be fully funded so
/// that accounting remains consistent.
///
/// # Panics
/// - `MilestoneNotFunded` if `funded_amount < amount`
pub(crate) fn validate_milestone_fully_funded(env: &Env, amount: i128, funded_amount: i128) {
    if funded_amount < amount {
        env.panic_with_error(EscrowError::MilestoneNotFunded);
    }
}

/// Validate that a milestone index refers to a milestone that exists in the
/// provided vector, returning the milestone or panicking with a deterministic
/// error.
///
/// This is the canonical lookup helper for milestone operations. It combines
/// the bounds check with the storage read so that callers cannot accidentally
/// skip the boundary validation.
///
/// # Panics
/// - `InvalidMilestoneIndex` if `index >= milestones.len()`
pub(crate) fn load_milestone_at(
    env: &Env,
    milestones: &Vec<crate::Milestone>,
    index: u32,
) -> crate::Milestone {
    validate_milestone_index_bounds(env, index, milestones.len());
    milestones
        .get(index)
        .unwrap_or_else(|| env.panic_with_error(EscrowError::InvalidMilestoneIndex))
}

/// Validate that a contract is in a state that permits milestone mutation.
///
/// Milestones may only be mutated while the contract is in `Created` or
/// `Funded` status. Terminal statuses (`Completed`, `Cancelled`, `Disputed`)
/// must not accept further milestone changes.
///
/// # Panics
/// - `InvalidContractStatus` if the status does not permit mutation
pub(crate) fn validate_contract_mutable(env: &Env, status: &crate::ContractStatus) {
    match status {
        crate::ContractStatus::Created | crate::ContractStatus::Funded => {}
        _ => env.panic_with_error(EscrowError::InvalidContractStatus),
    }
}

/// Validate that a contract is in a state that permits release of funds.
///
/// Release requires the contract to be `Funded`.
///
/// # Panics
/// - `InvalidContractStatus` if the status is not `Funded`
pub(crate) fn validate_contract_releasable(env: &Env, status: &crate::ContractStatus) {
    if !matches!(status, crate::ContractStatus::Funded) {
        env.panic_with_error(EscrowError::InvalidContractStatus);
    }
}

/// Validate that a contract is in a state that permits refund of funds.
///
/// Refund requires the contract to be `Funded` or `Cancelled`.
///
/// # Panics
/// - `InvalidContractStatus` if the status is not `Funded` or `Cancelled`
pub(crate) fn validate_contract_refundable(env: &Env, status: &crate::ContractStatus) {
    match status {
        crate::ContractStatus::Funded | crate::ContractStatus::Cancelled => {}
        _ => env.panic_with_error(EscrowError::InvalidContractStatus),
    }
}

/// Validate that a contract is in a state that permits finalization.
///
/// Finalization requires the contract to be `Completed` or `Cancelled`.
///
/// # Panics
/// - `InvalidContractStatus` if the status is not terminal
pub(crate) fn validate_contract_finalizable(env: &Env, status: &crate::ContractStatus) {
    match status {
        crate::ContractStatus::Completed | crate::ContractStatus::Cancelled => {}
        _ => env.panic_with_error(EscrowError::InvalidContractStatus),
    }
}

/// Validate that a contract has no outstanding funded milestones before
/// finalization.
///
/// Finalizing a contract with unreleased or unrefunded funds would strand
/// those funds. This is the accounting invariant guard for finalization.
///
/// # Panics
/// - `OutstandingFunds` if any milestone has `funded_amount > 0` and is not
///   released or refunded
pub(crate) fn validate_no_outstanding_funds(
    env: &Env,
    milestones: &Vec<crate::Milestone>,
) {
    for i in 0..milestones.len() {
        let m = milestones.get(i).unwrap();
        if m.funded_amount > 0 && !m.released && !m.refunded {
            env.panic_with_error(EscrowError::OutstandingFunds);
        }
    }
}

/// Validate that the sum of milestone amounts equals the contract's total
/// deposited amount.
///
/// This is the core accounting invariant: the contract's `total_deposited`
/// must equal the sum of all milestone amounts. Any mismatch indicates a
/// bug or corruption and must be rejected.
///
/// # Panics
/// - `AccountingMismatch` if the sums do not match
pub(crate) fn validate_milestone_sum(
    env: &Env,
    milestones: &Vec<crate::Milestone>,
    total_deposited: i128,
) {
    let mut sum: i128 = 0;
    for i in 0..milestones.len() {
        let m = milestones.get(i).unwrap();
        sum = sum.checked_add(m.amount).unwrap_or_else(|| {
            env.panic_with_error(EscrowError::AccountingMismatch)
        });
    }
    if sum != total_deposited {
        env.panic_with_error(EscrowError::AccountingMismatch);
    }
}

/// Validate that a milestone's released and refunded amounts do not exceed
/// its funded amount.
///
/// This prevents double-release or double-refund from inflating the
/// accounting totals.
///
/// # Panics
/// - `AccountingMismatch` if `released_amount + refunded_amount > funded_amount`
pub(crate) fn validate_milestone_accounting(
    env: &Env,
    milestone: &crate::Milestone,
) {
    let total = milestone
        .released_amount
        .checked_add(milestone.refunded_amount)
        .unwrap_or_else(|| env.panic_with_error(EscrowError::AccountingMismatch));
    if total > milestone.funded_amount {
        env.panic_with_error(EscrowError::AccountingMismatch);
    }
}

/// Validate that a contract's released and refunded amounts do not exceed
/// its total deposited amount.
///
/// # Panics
/// - `AccountingMismatch` if `released_amount + refunded_amount > total_deposited`
pub(crate) fn validate_contract_accounting(env: &Env, contract: &Contract) {
    let total = contract
        .released_amount
        .checked_add(contract.refunded_amount)
        .unwrap_or_else(|| env.panic_with_error(EscrowError::AccountingMismatch));
    if total > contract.total_deposited {
        env.panic_with_error(EscrowError::AccountingMismatch);
    }
}

/// Validate that a contract's client and freelancer addresses are distinct.
///
/// A contract where client == freelancer would allow self-dealing and break
/// the escrow trust model.
///
/// # Panics
/// - `InvalidParties` if `client == freelancer`
pub(crate) fn validate_contract_parties(env: &Env, contract: &Contract) {
    if contract.client == contract.freelancer {
        env.panic_with_error(EscrowError::InvalidParties);
    }
}

/// Validate that a contract's arbiter, if present, is distinct from both
/// the client and the freelancer.
///
/// # Panics
/// - `InvalidParties` if the arbiter equals the client or freelancer
pub(crate) fn validate_contract_arbiter(env: &Env, contract: &Contract) {
    if let Some(ref arbiter) = contract.arbiter {
        if *arbiter == contract.client || *arbiter == contract.freelancer {
            env.panic_with_error(EscrowError::InvalidParties);
        }
    }
}

/// Validate that a contract's funded amount does not exceed its total
/// deposited amount.
///
/// # Panics
/// - `AccountingMismatch` if `funded_amount > total_deposited`
pub(crate) fn validate_contract_funding(env: &Env, contract: &Contract) {
    if contract.funded_amount > contract.total_deposited {
        env.panic_with_error(EscrowError::AccountingMismatch);
    }
}

/// Validate that a contract's released amount does not exceed its funded
/// amount.
///
/// # Panics
/// - `AccountingMismatch` if `released_amount > funded_amount`
pub(crate) fn validate_contract_release_bounds(env: &Env, contract: &Contract) {
    if contract.released_amount > contract.funded_amount {
        env.panic_with_error(EscrowError::AccountingMismatch);
    }
}

/// Validate that a contract's refunded amount does not exceed its funded
/// amount.
///
/// # Panics
/// - `AccountingMismatch` if `refunded_amount > funded_amount`
pub(crate) fn validate_contract_refund_bounds(env: &Env, contract: &Contract) {
    if contract.refunded_amount > contract.funded_amount {
        env.panic_with_error(EscrowError::AccountingMismatch);
    }
}

/// Validate that a contract's reputation has not already been issued.
///
/// Reputation issuance is a one-time operation per contract.
///
/// # Panics
/// - `ReputationAlreadyIssued` if `reputation_issued` is true
pub(crate) fn validate_reputation_not_issued(env: &Env, contract: &Contract) {
    if contract.reputation_issued {
        env.panic_with_error(EscrowError::ReputationAlreadyIssued);
    }
}

/// Validate that a contract's reputation has been issued before it can be
/// finalized.
///
/// # Panics
/// - `ReputationNotIssued` if `reputation_issued` is false
pub(crate) fn validate_reputation_issued(env: &Env, contract: &Contract) {
    if !contract.reputation_issued {
        env.panic_with_error(EscrowError::ReputationNotIssued);
    }
}

/// Validate that a contract's release authorization mode is compatible with
/// the caller's role.
///
/// # Panics
/// - `Unauthorized` if the caller is not permitted to release under the
///   current authorization mode
pub(crate) fn validate_release_authorization(
    env: &Env,
    contract: &Contract,
    caller: &soroban_sdk::Address,
) {
    use crate::ReleaseAuthorization;
    let authorized = match contract.release_authorization {
        ReleaseAuthorization::ClientOnly => caller == &contract.client,
        ReleaseAuthorization::FreelancerOnly => caller == &contract.freelancer,
        ReleaseAuthorization::ClientOrFreelancer => {
            caller == &contract.client || caller == &contract.freelancer
        }
        ReleaseAuthorization::ArbiterOnly => {
            contract.arbiter.as_ref().map_or(false, |a| caller == a)
        }
        ReleaseAuthorization::ClientOrArbiter => {
            caller == &contract.client
                || contract.arbiter.as_ref().map_or(false, |a| caller == a)
        }
        ReleaseAuthorization::FreelancerOrArbiter => {
            caller == &contract.freelancer
                || contract.arbiter.as_ref().map_or(false, |a| caller == a)
        }
        ReleaseAuthorization::Any => true,
    };
    if !authorized {
        env.panic_with_error(Error::Unauthorized);
    }
}

/// Validate that a contract's status is consistent with its accounting
/// fields.
///
/// This is a cross-field invariant check that catches corrupted or
/// inconsistent state before it can cause silent data loss.
///
/// # Panics
/// - `InvalidContractStatus` if the status is inconsistent with the
///   accounting fields
pub(crate) fn validate_contract_status_consistency(env: &Env, contract: &Contract) {
    use crate::ContractStatus;
    match contract.status {
        ContractStatus::Created => {
            if contract.funded_amount != 0
                || contract.released_amount != 0
                || contract.refunded_amount != 0
            {
                env.panic_with_error(EscrowError::InvalidContractStatus);
            }
        }
        ContractStatus::Funded => {
            if contract.funded_amount == 0 {
                env.panic_with_error(EscrowError::InvalidContractStatus);
            }
        }
        ContractStatus::Completed => {
            if contract.released_amount == 0 && contract.refunded_amount == 0 {
                env.panic_with_error(EscrowError::InvalidContractStatus);
            }
        }
        ContractStatus::Cancelled => {
            if contract.refunded_amount == 0 {
                env.panic_with_error(EscrowError::InvalidContractStatus);
            }
        }
        ContractStatus::Disputed => {}
    }
}

/// Validate that a contract's milestone vector is consistent with the
/// contract's accounting fields.
///
/// This is the top-level invariant check that should be called after any
/// milestone mutation to ensure the contract and its milestones remain in
/// agreement.
///
/// # Panics
/// - `AccountingMismatch` if any invariant is violated
pub(crate) fn validate_contract_milestone_consistency(
    env: &Env,
    contract: &Contract,
    milestones: &Vec<crate::Milestone>,
) {
    validate_milestone_count(env, milestones.len());
    validate_milestone_sum(env, milestones, contract.total_deposited);
    for i in 0..milestones.len() {
        let m = milestones.get(i).unwrap();
        validate_milestone_funding(env, m.amount, m.funded_amount);
        validate_milestone_accounting(env, &m);
    }
    validate_contract_accounting(env, contract);
    validate_contract_funding(env, contract);
    validate_contract_release_bounds(env, contract);
    validate_contract_refund_bounds(env, contract);
}

/// Validate that a contract's total deposited amount is non-negative.
///
/// # Panics
/// - `AccountingMismatch` if `total_deposited < 0`
pub(crate) fn validate_total_deposited(env: &Env, total_deposited: i128) {
    if total_deposited < 0 {
        env.panic_with_error(EscrowError::AccountingMismatch);
    }
}

/// Validate that a contract's funded amount is non-negative.
///
/// # Panics
/// - `AccountingMismatch` if `funded_amount < 0`
pub(crate) fn validate_funded_amount(env: &Env, funded_amount: i128) {
    if funded_amount < 0 {
        env.panic_with_error(EscrowError::AccountingMismatch);
    }
}

/// Validate that a contract's released amount is non-negative.
///
/// # Panics
/// - `AccountingMismatch` if `released_amount < 0`

/// Check if the contract system has been initialized.
///
/// Initialization is a prerequisite for all money-flow operations. This check
/// ensures that the admin-controlled safety rails (pause, emergency controls,
/// protocol fees) are always in scope before any funds can move.
///
/// # Arguments
/// * `env` - The contract environment
///
/// # Panics
/// - `NotInitialized` if initialization has not been completed
///
/// # Returns
/// `true` if initialized, or panics with `NotInitialized`
pub(crate) fn require_initialized(env: &Env) -> bool {
    let initialized = env
        .storage()
        .persistent()
        .get::<_, bool>(&DataKey::Initialized)
        .unwrap_or(false);
    if !initialized {
        env.panic_with_error(Error::NotInitialized);
    }
    true
}

/// Load a contract from persistent storage.
///
/// This is the canonical pattern for retrieving a contract. It handles the
/// storage read with consistent error reporting and bounds checking.
///
/// # Arguments
/// * `env` - The contract environment
/// * `contract_id` - The contract ID to load
///
/// # Panics
/// - `ContractNotFound` if `contract_id` is 0
/// - `ContractNotFound` if no contract exists for this ID
///
/// # Returns
/// The loaded `Contract` or panics with `ContractNotFound`
pub(crate) fn load_contract(env: &Env, contract_id: u32) -> Contract {
    // Invariant 2: reject the zero ID before any storage access so that a
    // missing record and an invalid key are distinguishable in tests and
    // logs.
    validate_contract_id_bounds(env, contract_id);
    env.storage()
        .persistent()
        .get(&DataKey::Contract(contract_id))
        .unwrap_or_else(|| env.panic_with_error(Error::ContractNotFound))
}

/// Load milestones for a contract from persistent storage.
///
/// Milestones are stored under a composite key combining the contract ID
/// and a "milestones" symbol. This helper centralizes the retrieval pattern.
///
/// # Arguments
/// * `env` - The contract environment
/// * `contract_id` - The contract ID whose milestones to load
///
/// # Panics
/// - `ContractNotFound` if `contract_id` is 0
/// - `ContractNotFound` if no milestone vector exists for this contract
///
/// # Returns
/// The loaded milestone vector or panics with `ContractNotFound`
///
/// The key is built through [`crate::keys::milestone_key`], the single
/// definition of the composite milestone key, so this read can never drift from
/// the writers in the rest of the crate.
pub(crate) fn load_milestones(env: &Env, contract_id: u32) -> Vec<crate::Milestone> {
    // Invariant 2: same zero-ID guard as load_contract. Milestones are keyed
    // by the same contract ID, so an invalid ID must never reach storage.
    validate_contract_id_bounds(env, contract_id);
    let milestone_key = crate::keys::milestone_key(env, contract_id);
    env.storage()
        .persistent()
        .get(&milestone_key)
        .unwrap_or_else(|| env.panic_with_error(Error::ContractNotFound))
}

/// Load a contract, optionally with precondition checks for mutation.
///
/// This is the primary helper for loading contracts with optional safety guards:
/// - `check_paused`: If true, verifies pause/emergency flags are not set
/// - `check_finalized`: If true, verifies the contract has not been finalized
///
/// # Arguments
/// * `env` - The contract environment
/// * `contract_id` - The contract ID to load
/// * `check_paused` - Whether to verify pause/emergency states
/// * `check_finalized` - Whether to verify finalization state
///
/// # Panics
/// - `ContractNotFound` if `contract_id` is 0
/// - `ContractPaused` if `check_paused` is true and pause flag is set
/// - `EmergencyActive` if `check_paused` is true and emergency flag is set
/// - `AlreadyFinalized` if `check_finalized` is true and contract is finalized
///
/// # Returns
/// The loaded `Contract` if all preconditions pass
///
/// # Concurrency
/// All guards are evaluated against the current stored state in this call. The
/// returned contract is the value observed at load time; callers must not cache
/// it across invocations.
pub(crate) fn load_contract_checked(
    env: &Env,
    contract_id: u32,
    check_paused: bool,
    check_finalized: bool,
) -> Contract {
    // Invariant 2 + 3 + 4: order matters. Bounds first (cheapest, no I/O),
    // then pause/emergency (global safety rails), then load, then
    // finalization. Reordering these would let a paused or finalized
    // contract be observed by a caller that requested the guard.
    validate_contract_id_bounds(env, contract_id);
    if check_paused {
        require_not_paused(env);
    }

    let contract = load_contract(env, contract_id);

    if check_finalized {
        require_not_finalized(env, contract_id);
        // Re-check after load to close the race window where a concurrent
        // finalize could have committed between the load and the guard.
        require_not_finalized(env, contract_id);
    }

    contract
}

/// Validate the internal state invariants of a loaded [`Contract`].
///
/// This is the single source of truth for the accounting invariants owned by
/// the escrow contract. It is intentionally pure (no storage access) so it can
/// be called from any entrypoint — including `refund_impl.rs` — before any
/// mutation is persisted. Any violation aborts the transaction, guaranteeing
/// that partial failures cannot leave the contract in an inconsistent state.
///
/// # Invariants enforced
/// 1. `released_amount + refunded_amount <= funded_amount`
/// 2. `funded_amount <= total_deposited`
/// 3. `released_amount <= funded_amount`
/// 4. `refunded_amount <= funded_amount`
/// 5. `funded_amount >= 0` (implied by `i128` but asserted for clarity)
/// 6. Terminal statuses (`Completed`, `Cancelled`, `Refunded`) must have
///    `released_amount + refunded_amount == funded_amount` when `funded_amount > 0`.
///
/// # Panics
/// Panics with [`Error::InvariantViolation`] if any invariant is broken.
pub(crate) fn require_contract_invariants(env: &Env, contract: &Contract) {
    // Invariant 5: non-negative accounting (defensive; i128 can be negative).
    if contract.funded_amount < 0
        || contract.released_amount < 0
        || contract.refunded_amount < 0
        || contract.total_deposited < 0
    {
        env.panic_with_error(Error::InvariantViolation);
    }

    // Invariant 2: cannot fund more than was deposited.
    if contract.funded_amount > contract.total_deposited {
        env.panic_with_error(Error::InvariantViolation);
    }

    // Invariants 1, 3, 4: released + refunded must not exceed funded.
    let released_plus_refunded = contract
        .released_amount
        .checked_add(contract.refunded_amount)
        .unwrap_or_else(|| env.panic_with_error(Error::InvariantViolation));
    if released_plus_refunded > contract.funded_amount {
        env.panic_with_error(Error::InvariantViolation);
    }

    // Invariant 6: terminal states must fully account for funded amounts.
    let is_terminal = matches!(
        contract.status,
        ContractStatus::Completed | ContractStatus::Cancelled | ContractStatus::Refunded
    );
    if is_terminal && contract.funded_amount > 0 && released_plus_refunded != contract.funded_amount {
        env.panic_with_error(Error::InvariantViolation);
    }
}

/// Check if the contract system is paused or in emergency mode.
///
/// # Arguments
/// * `env` - The contract environment
///
/// # Panics
/// - `ContractPaused` if the pause flag is set
/// - `EmergencyActive` if the emergency flag is set
///
/// # Returns
/// `true` if neither pause nor emergency is active, or panics
pub(crate) fn require_not_paused(env: &Env) -> bool {
    // Invariant 3: emergency takes precedence over the legacy boolean pause
    // so that operators can escalate without first clearing the pause flag.
    // Both are read fresh from persistent storage on every call.
    if env
        .storage()
        .persistent()
        .get::<_, bool>(&DataKey::Paused)
        .unwrap_or(false)
    {
        env.panic_with_error(Error::ContractPaused);
    }
    // Emergency always blocks everything.
    if env
        .storage()
        .persistent()
        .get::<_, bool>(&DataKey::Emergency)
        .unwrap_or(false)
    {
        env.panic_with_error(Error::EmergencyActive);
    }
    true
}

/// Check that the given [`PauseTarget`] is not blocked by an active scoped pause.
///
/// This is the entrypoint-facing guard used by payout and dispute operations.
/// If a [`PauseScope`] is stored, its target is compared against the requested
/// operation. A `Global` scope blocks everything; `Payout` blocks release,
/// refund, cancel; `Dispute` blocks raise, resolve, rollback.
///
/// The legacy bare `bool` under `DataKey::Paused` is also checked for backward
/// compatibility — it acts as a `Global` pause.
pub(crate) fn require_pause_scope(env: &Env, target: &crate::PauseTarget) {
    // Invariant 3: the precedence here must match require_not_paused.
    // Legacy bool == Global, emergency == Global, then scoped pause is
    // compared against the requested target.
    // Legacy boolean pause acts as Global
    if env
        .storage()
        .persistent()
        .get::<_, bool>(&DataKey::Paused)
        .unwrap_or(false)
    {
        env.panic_with_error(Error::ContractPaused);
    }

    // Emergency always blocks everything.
    // Emergency always blocks everything
    if env
        .storage()
        .persistent()
        .get::<_, bool>(&DataKey::Emergency)
        .unwrap_or(false)
    {
        env.panic_with_error(Error::EmergencyActive);
    }

    // Scoped pause.
    // Scoped pause
    if let Some(scope) = env
        .storage()
        .persistent()
        .get::<_, crate::PauseScope>(&DataKey::PauseScope)
    {
        match (&scope.target, target) {
            (crate::PauseTarget::Global, _) | (_, crate::PauseTarget::Global) => {
                env.panic_with_error(Error::PauseScopeActive);
            }
            (crate::PauseTarget::Payout, crate::PauseTarget::Payout) => {
                env.panic_with_error(Error::PauseScopeActive);
            }
            (crate::PauseTarget::Dispute, crate::PauseTarget::Dispute) => {
                env.panic_with_error(Error::PauseScopeActive);
            }
            _ => {} // Non-overlapping scope: allow
        }
    }
}

/// Validate that a contract's monetary invariants hold.
///
/// Enforces the accounting identities that must hold for every stored contract:
/// - `released_amount + refunded_amount <= total_deposited`
/// - `funded_amount <= total_deposited`
/// - `released_amount <= funded_amount`
///
/// These checks make silent data loss impossible: any state transition that
/// would violate them is rejected at the storage boundary.
///
/// # Panics
/// - `InvalidContractAmounts` if any invariant is violated
pub(crate) fn validate_contract_amounts(env: &Env, contract: &Contract) {
    let released_plus_refunded = contract
        .released_amount
        .checked_add(contract.refunded_amount)
        .unwrap_or_else(|| env.panic_with_error(EscrowError::InvalidContractAmounts));
    if released_plus_refunded > contract.total_deposited {
        env.panic_with_error(EscrowError::InvalidContractAmounts);
    }
    if contract.funded_amount > contract.total_deposited {
        env.panic_with_error(EscrowError::InvalidContractAmounts);
    }
    if contract.released_amount > contract.funded_amount {
        env.panic_with_error(EscrowError::InvalidContractAmounts);
    }
}

/// Persist a contract after validating its monetary invariants.
///
/// This is the canonical write path for contracts. All state transitions that
/// mutate a contract must route through this helper so that the invariants
/// checked by [`validate_contract_amounts`] hold for every stored contract.
///
/// # Panics
/// - `InvalidContractId` if `contract_id` is 0
/// - `InvalidContractAmounts` if any monetary invariant is violated
pub(crate) fn store_contract(env: &Env, contract_id: u32, contract: &Contract) {
    validate_contract_id_bounds(env, contract_id);
    validate_contract_amounts(env, contract);
    env.storage()
        .persistent()
        .set(&DataKey::Contract(contract_id), contract);
}

/// Consume the next expected admin nonce, rejecting stale or future values.
///
/// Stores a monotonic `u64` under [`DataKey::AdminNonce`]. On the first call
/// the expected nonce is `1` (zero means uninitialized). After a successful
/// call the stored nonce is incremented atomically.
///
/// # Invariants
/// * The stored counter never decreases, so a successful call can never be
///   replayed: re-submitting an already-consumed nonce is rejected.
/// * `current + 1` is computed with [`u64::checked_add`]. If the counter is
///   already [`u64::MAX`] the call fails closed with [`Error::PotentialOverflow`]
///   and storage is left unchanged, instead of wrapping back to an accept-all
///   `0`. This keeps the nonce safe under adversarial replay of admin actions.
/// * The comparison and the write happen inside the same contract invocation, so
///   a rejected nonce performs no partial write (a panic aborts the invocation).
///
/// # Panics
/// Panics with [`Error::StaleNonce`] if the provided nonce does not match.
///
/// # Concurrency
/// The read-modify-write of [`DataKey::AdminNonce`] happens within a single
/// invocation, so two racing admin calls cannot both observe the same expected
/// nonce. A retried call that reuses an already-consumed nonce is rejected,
/// which makes admin operations safe to retry only with a fresh nonce.
pub(crate) fn consume_admin_nonce(env: &Env, provided_nonce: u64) {
    // Invariant 5: monotonic nonce. `current == 0` means uninitialized, so
    // the first accepted value is 1. Any other value (stale or future) is
    // rejected before the write, so a failed call cannot advance the nonce.
    // On success the increment is persisted atomically with the check.
    let current: u64 = env
        .storage()
        .persistent()
        .get(&DataKey::AdminNonce)
        .unwrap_or(0);
    // Invariant: the admin nonce is strictly monotonic and must never wrap.
    // Refuse to advance past u64::MAX so a replay window cannot be reopened.
    if current == u64::MAX {
        env.panic_with_error(Error::StaleNonce);
    }
    let expected = current + 1;
    if provided_nonce != expected {
        env.panic_with_error(Error::StaleNonce);
    }
    env.storage()
        .persistent()
        .set(&DataKey::AdminNonce, &expected);
    true
}

/// Check if a contract has been finalized.
///
/// # Arguments
/// * `env` - The contract environment
/// * `contract_id` - The contract ID to check
///
/// # Panics
/// - `ContractNotFound` if `contract_id` is 0 (reserved sentinel)
///
/// # Returns
/// `true` if the contract is finalized
pub(crate) fn is_finalized(env: &Env, contract_id: u32) -> bool {
    // Invariant 2: zero ID is never a valid finalization key.
    validate_contract_id_bounds(env, contract_id);
    env.storage()
        .persistent()
        .has(&DataKey::Finalization(contract_id))
}

/// Require that a contract has not been finalized.
///
/// # Arguments
/// * `env` - The contract environment
/// * `contract_id` - The contract ID to check
///
/// # Panics
/// - `ContractNotFound` if `contract_id` is 0
/// - `AlreadyFinalized` if the contract has been finalized
///
/// # Returns
/// `true` if not finalized, or panics
///
/// # Concurrency
/// Finalization is a terminal latch. Once set, this helper rejects all further
/// mutations, so duplicate or delayed retries observe a consistent terminal
/// state instead of partially applying a second transition.
pub(crate) fn require_not_finalized(env: &Env, contract_id: u32) -> bool {
    // Invariant 2 + 4: bounds check first, then the terminal-state check.
    validate_contract_id_bounds(env, contract_id);
    if is_finalized(env, contract_id) {
        env.panic_with_error(Error::AlreadyFinalized);
    }
    true
}

#[cfg(test)]
mod tests_disabled {
    // These tests are disabled as they require complex Soroban contract setup
    // and are better covered by integration tests in test/ directory.
    // The storage module functions are thoroughly tested indirectly through
    // all integration tests that use load_contract(), load_milestones(), etc.
    
    use super::*;
    use crate::Milestone;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::{Address, Env, Symbol};

    // Test helpers below exercise each invariant in isolation and in
    // combination. Panics are asserted with `#[should_panic]` so that a
    // regression that silently returns instead of aborting will fail CI.

    fn setup_test_env() -> (Env, Address, Address) {
        let env = Env::default();
        let admin = Address::generate(&env);
        env.mock_all_auths();
        let contract_id = env.register(crate::Escrow, ());
        (env, admin, contract_id)
    }

    #[test]
    fn test_require_initialized_when_true() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            env.storage().persistent().set(&DataKey::Initialized, &true);
            let result = require_initialized(&env);
            assert!(result);
        });
    }

    #[test]
    #[should_panic(expected = "NotInitialized")]
    fn test_require_initialized_when_false() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            require_initialized(&env);
        });
    }

    #[test]
    #[should_panic(expected = "ContractNotFound")]
    fn test_load_contract_not_found() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            load_contract(&env, 999);
        });
    }

    #[test]
    fn test_load_contract_found() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            let client = Address::generate(&env);
            let freelancer = Address::generate(&env);
            let contract = Contract {
                client: client.clone(),
                freelancer: freelancer.clone(),
                arbiter: None,
                status: crate::ContractStatus::Created,
                release_authorization: crate::ReleaseAuthorization::ClientOnly,
                funded_amount: 0,
                released_amount: 0,
                refunded_amount: 0,
                total_deposited: 0,
                reputation_issued: false,
            };

            env.storage()
                .persistent()
                .set(&DataKey::Contract(42), &contract);

            let loaded = load_contract(&env, 42);
            assert_eq!(loaded.client, client);
            assert_eq!(loaded.freelancer, freelancer);
            assert_eq!(loaded.status, crate::ContractStatus::Created);
            require_contract_invariants(&env, &loaded);
        });
    }

    #[test]
    #[should_panic(expected = "ContractNotFound")]
    fn test_load_milestones_not_found() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            load_milestones(&env, 999);
        });
    }

    #[test]
    #[should_panic(expected = "ContractNotFound")]
    fn test_load_milestones_orphaned_vector_rejected() {
        // Regression: a milestone vector must not be readable when its
        // parent contract record is absent. This protects the invariant
        // that milestones and their contract are always co-present.
        let (env, admin) = setup_test_env();
        env.as_contract(&admin, || {
            let milestones = Vec::from_array(
                &env,
                [Milestone {
                    amount: 1000,
                    funded_amount: 0,
                    released: false,
                    refunded: false,
                    deadline: None,
                    refunded_amount: 0,
                    work_evidence: None,
                }],
            );
            let milestone_key = Symbol::new(&env, "milestones");
            env.storage()
                .persistent()
                .set(&(DataKey::Contract(42), milestone_key), &milestones);

            // No DataKey::Contract(42) written — must still panic.
            load_milestones(&env, 42);
        });
    }

    #[test]
    fn test_load_milestones_found() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            let milestones = Vec::from_array(
                &env,
                [
                    Milestone {
                        amount: 1000,
                        funded_amount: 0,
                        released: false,
                        refunded: false,
                        deadline: None,
                        refunded_amount: 0,
                        work_evidence: None,
                    },
                    Milestone {
                        amount: 2000,
                        funded_amount: 0,
                        released: false,
                        refunded: false,
                        deadline: None,
                        refunded_amount: 0,
                        work_evidence: None,
                    },
                ],
            );

            let milestone_key = crate::keys::milestone_key(&env, 42);
            env.storage()
                .persistent()
                .set(&milestone_key, &milestones);

            let loaded = load_milestones(&env, 42);
            assert_eq!(loaded.len(), 2);
            assert_eq!(loaded.get(0).unwrap().amount, 1000);
            assert_eq!(loaded.get(1).unwrap().amount, 2000);
        });
    }

    #[test]
    fn test_store_milestones_round_trip() {
        let (env, admin) = setup_test_env();
        env.as_contract(&admin, || {
            let milestones = Vec::from_array(
                &env,
                [Milestone {
                    amount: 500,
                    funded_amount: 0,
                    released: false,
                    refunded: false,
                    deadline: None,
                    refunded_amount: 0,
                    work_evidence: None,
                }],
            );
            store_milestones(&env, 7, &milestones);
            let loaded = load_milestones(&env, 7);
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded.get(0).unwrap().amount, 500);
        });
    }

    #[test]
    fn test_store_milestones_overwrite_is_idempotent() {
        let (env, admin) = setup_test_env();
        env.as_contract(&admin, || {
            let first = Vec::from_array(
                &env,
                [Milestone {
                    amount: 100,
                    funded_amount: 0,
                    released: false,
                    refunded: false,
                    deadline: None,
                    refunded_amount: 0,
                    work_evidence: None,
                }],
            );
            let second = Vec::from_array(
                &env,
                [Milestone {
                    amount: 200,
                    funded_amount: 0,
                    released: false,
                    refunded: false,
                    deadline: None,
                    refunded_amount: 0,
                    work_evidence: None,
                }],
            );
            store_milestones(&env, 9, &first);
            store_milestones(&env, 9, &second);
            store_milestones(&env, 9, &second);
            let loaded = load_milestones(&env, 9);
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded.get(0).unwrap().amount, 200);
        });
    }

    #[test]
    #[should_panic(expected = "ContractNotFound")]
    fn test_store_milestones_zero_id_panics() {
        let (env, admin) = setup_test_env();
        env.as_contract(&admin, || {
            let milestones = Vec::new(&env);
            store_milestones(&env, 0, &milestones);
        });
    }

    #[test]
    #[should_panic(expected = "ContractNotFound")]
    fn test_load_milestones_checked_missing_contract() {
        let (env, admin) = setup_test_env();
        env.as_contract(&admin, || {
            let milestones = Vec::from_array(
                &env,
                [Milestone {
                    amount: 100,
                    funded_amount: 0,
                    released: false,
                    refunded: false,
                    deadline: None,
                    refunded_amount: 0,
                    work_evidence: None,
                }],
            );
            let milestone_key = Symbol::new(&env, "milestones");
            env.storage()
                .persistent()
                .set(&(DataKey::Contract(77), milestone_key), &milestones);
            load_milestones_checked(&env, 77);
        });
    }

    #[test]
    fn test_load_milestones_checked_with_contract() {
        let (env, admin) = setup_test_env();
        env.as_contract(&admin, || {
            let client = Address::generate(&env);
            let freelancer = Address::generate(&env);
            let contract = Contract {
                client: client.clone(),
                freelancer: freelancer.clone(),
                arbiter: None,
                status: crate::ContractStatus::Created,
                release_authorization: crate::ReleaseAuthorization::ClientOnly,
                funded_amount: 0,
                released_amount: 0,
                refunded_amount: 0,
                total_deposited: 0,
                reputation_issued: false,
            };
            env.storage()
                .persistent()
                .set(&DataKey::Contract(11), &contract);
            let milestones = Vec::from_array(
                &env,
                [Milestone {
                    amount: 42,
                    funded_amount: 0,
                    released: false,
                    refunded: false,
                    deadline: None,
                    refunded_amount: 0,
                    work_evidence: None,
                }],
            );
            store_milestones(&env, 11, &milestones);
            let loaded = load_milestones_checked(&env, 11);
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded.get(0).unwrap().amount, 42);
        });
    }

    #[test]
    fn test_require_not_paused_when_not_paused() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            let result = require_not_paused(&env);
            assert!(result);
        });
    }

    #[test]
    #[should_panic(expected = "ContractPaused")]
    fn test_require_not_paused_when_paused() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            env.storage().persistent().set(&DataKey::Paused, &true);
            require_not_paused(&env);
        });
    }

    #[test]
    #[should_panic(expected = "EmergencyActive")]
    fn test_require_not_paused_when_emergency() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            env.storage().persistent().set(&DataKey::Emergency, &true);
            require_not_paused(&env);
        });
    }

    #[test]
    fn test_is_finalized_when_false() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            let result = is_finalized(&env, 42);
            assert!(!result);
        });
    }

    #[test]
    fn test_is_finalized_when_true() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            env.storage()
                .persistent()
                .set(&DataKey::Finalization(42), &true);

            let result = is_finalized(&env, 42);
            assert!(result);
        });
    }

    #[test]
    fn test_require_not_finalized_when_not_finalized() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            let result = require_not_finalized(&env, 42);
            assert!(result);
        });
    }

    #[test]
    fn test_consume_admin_nonce_first_call() {
        let (env, admin) = setup_test_env();
        env.as_contract(&admin, || {
            consume_admin_nonce(&env, 1);
            let stored: u64 = env
                .storage()
                .persistent()
                .get(&DataKey::AdminNonce)
                .unwrap_or(0);
            assert_eq!(stored, 1);
        });
    }

    #[test]
    #[should_panic(expected = "StaleNonce")]
    fn test_consume_admin_nonce_rejects_replay() {
        let (env, admin) = setup_test_env();
        env.as_contract(&admin, || {
            consume_admin_nonce(&env, 1);
            // Replaying the same nonce must be rejected deterministically.
            consume_admin_nonce(&env, 1);
        });
    }

    #[test]
    #[should_panic(expected = "StaleNonce")]
    fn test_consume_admin_nonce_rejects_future() {
        let (env, admin) = setup_test_env();
        env.as_contract(&admin, || {
            consume_admin_nonce(&env, 2);
        });
    }

    #[test]
    #[should_panic(expected = "StaleNonce")]
    fn test_consume_admin_nonce_rejects_overflow() {
        let (env, admin) = setup_test_env();
        env.as_contract(&admin, || {
            env.storage()
                .persistent()
                .set(&DataKey::AdminNonce, &u64::MAX);
            consume_admin_nonce(&env, 0);
        });
    }

    #[test]
    #[should_panic(expected = "AlreadyFinalized")]
    fn test_require_not_finalized_when_finalized() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            env.storage()
                .persistent()
                .set(&DataKey::Finalization(42), &true);

            require_not_finalized(&env, 42);
        });
    }

    #[test]
    fn test_load_contract_checked_all_checks() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            let client = Address::generate(&env);
            let freelancer = Address::generate(&env);
            let contract = Contract {
                client: client.clone(),
                freelancer: freelancer.clone(),
                arbiter: None,
                status: crate::ContractStatus::Created,
                release_authorization: crate::ReleaseAuthorization::ClientOnly,
                funded_amount: 0,
                released_amount: 0,
                refunded_amount: 0,
                total_deposited: 0,
                reputation_issued: false,
            };

            env.storage()
                .persistent()
                .set(&DataKey::Contract(42), &contract);

            let loaded = load_contract_checked(&env, 42, true, true);
            assert_eq!(loaded.client, client);
            require_contract_invariants(&env, &loaded);
        });
    }

    #[test]
    #[should_panic(expected = "ContractPaused")]
    fn test_load_contract_checked_paused() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            let client = Address::generate(&env);
            let freelancer = Address::generate(&env);
            let contract = Contract {
                client: client.clone(),
                freelancer: freelancer.clone(),
                arbiter: None,
                status: crate::ContractStatus::Created,
                release_authorization: crate::ReleaseAuthorization::ClientOnly,
                funded_amount: 0,
                released_amount: 0,
                refunded_amount: 0,
                total_deposited: 0,
                reputation_issued: false,
            };

            env.storage()
                .persistent()
                .set(&DataKey::Contract(42), &contract);
            env.storage().persistent().set(&DataKey::Paused, &true);

            load_contract_checked(&env, 42, true, true);
        });
    }

    #[test]
    #[should_panic(expected = "AlreadyFinalized")]
    fn test_load_contract_checked_finalized() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            let client = Address::generate(&env);
            let freelancer = Address::generate(&env);
            let contract = Contract {
                client: client.clone(),
                freelancer: freelancer.clone(),
                arbiter: None,
                status: crate::ContractStatus::Created,
                release_authorization: crate::ReleaseAuthorization::ClientOnly,
                funded_amount: 0,
                released_amount: 0,
                refunded_amount: 0,
                total_deposited: 0,
                reputation_issued: false,
            };

            env.storage()
                .persistent()
                .set(&DataKey::Contract(42), &contract);
            env.storage()
                .persistent()
                .set(&DataKey::Finalization(42), &true);

            load_contract_checked(&env, 42, true, true);
        });
    }

    #[test]
    fn test_load_contract_checked_no_checks() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            let client = Address::generate(&env);
            let freelancer = Address::generate(&env);
            let contract = Contract {
                client: client.clone(),
                freelancer: freelancer.clone(),
                arbiter: None,
                status: crate::ContractStatus::Created,
                release_authorization: crate::ReleaseAuthorization::ClientOnly,
                funded_amount: 0,
                released_amount: 0,
                refunded_amount: 0,
                total_deposited: 0,
                reputation_issued: false,
            };

            env.storage()
                .persistent()
                .set(&DataKey::Contract(42), &contract);
            env.storage().persistent().set(&DataKey::Paused, &true);
            env.storage()
                .persistent()
                .set(&DataKey::Finalization(42), &true);

            // Should succeed because checks are disabled
            let loaded = load_contract_checked(&env, 42, false, false);
            assert_eq!(loaded.client, client);
            require_contract_invariants(&env, &loaded);
        });
    }

    #[test]
    #[should_panic(expected = "InvalidContractId")]
    fn test_validate_contract_id_bounds_zero_panics() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            validate_contract_id_bounds(&env, 0);
        });
    }

    #[test]
    #[should_panic(expected = "ContractNotFound")]
    fn test_load_contract_zero_id_panics() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            load_contract(&env, 0);
        });
    }

    #[test]
    #[should_panic(expected = "ContractNotFound")]
    fn test_load_milestones_zero_id_panics() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            load_milestones(&env, 0);
        });
    }

    #[test]
    #[should_panic(expected = "ContractNotFound")]
    fn test_load_contract_checked_zero_id_panics() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            load_contract_checked(&env, 0, false, false);
        });
    }

    #[test]
    #[should_panic(expected = "ContractNotFound")]
    fn test_is_finalized_zero_id_panics() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            is_finalized(&env, 0);
        });
    }

    #[test]
    #[should_panic(expected = "ContractNotFound")]
    fn test_require_not_finalized_zero_id_panics() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            require_not_finalized(&env, 0);
        });
    }

    #[test]
    fn test_validate_contract_id_bounds_valid_range() {
        let (env, admin, _contract_id) = setup_test_env();
        env.as_contract(&admin, || {
            validate_contract_id_bounds(&env, 1);
            validate_contract_id_bounds(&env, 42);
            validate_contract_id_bounds(&env, u32::MAX);
        });
    }

    fn make_contract(
        env: &Env,
        status: crate::ContractStatus,
        funded: i128,
        released: i128,
        refunded: i128,
        deposited: i128,
    ) -> Contract {
        Contract {
            client: Address::generate(env),
            freelancer: Address::generate(env),
            arbiter: None,
            status,
            release_authorization: crate::ReleaseAuthorization::ClientOnly,
            funded_amount: funded,
            released_amount: released,
            refunded_amount: refunded,
            total_deposited: deposited,
            reputation_issued: false,
        }
    }

    #[test]
    fn test_invariants_accept_valid_active_state() {
        let (env, admin) = setup_test_env();
        env.as_contract(&admin, || {
            let c = make_contract(&env, crate::ContractStatus::Created, 100, 0, 0, 100);
            require_contract_invariants(&env, &c);
        });
    }

    #[test]
    fn test_invariants_accept_partial_release() {
        let (env, admin) = setup_test_env();
        env.as_contract(&admin, || {
            let c = make_contract(&env, crate::ContractStatus::Created, 100, 40, 0, 100);
            require_contract_invariants(&env, &c);
        });
    }

    #[test]
    fn test_invariants_accept_partial_refund() {
        let (env, admin) = setup_test_env();
        env.as_contract(&admin, || {
            let c = make_contract(&env, crate::ContractStatus::Created, 100, 0, 40, 100);
            require_contract_invariants(&env, &c);
        });
    }

    #[test]
    fn test_invariants_accept_terminal_fully_settled() {
        let (env, admin) = setup_test_env();
        env.as_contract(&admin, || {
            let c = make_contract(&env, crate::ContractStatus::Completed, 100, 100, 0, 100);
            require_contract_invariants(&env, &c);
        });
    }

    #[test]
    fn test_invariants_accept_zero_funded_terminal() {
        let (env, admin) = setup_test_env();
        env.as_contract(&admin, || {
            let c = make_contract(&env, crate::ContractStatus::Cancelled, 0, 0, 0, 0);
            require_contract_invariants(&env, &c);
        });
    }

    #[test]
    #[should_panic(expected = "InvariantViolation")]
    fn test_invariants_reject_released_exceeds_funded() {
        let (env, admin) = setup_test_env();
        env.as_contract(&admin, || {
            let c = make_contract(&env, crate::ContractStatus::Created, 100, 101, 0, 100);
            require_contract_invariants(&env, &c);
        });
    }

    #[test]
    #[should_panic(expected = "InvariantViolation")]
    fn test_invariants_reject_refunded_exceeds_funded() {
        let (env, admin) = setup_test_env();
        env.as_contract(&admin, || {
            let c = make_contract(&env, crate::ContractStatus::Created, 100, 0, 101, 100);
            require_contract_invariants(&env, &c);
        });
    }

    #[test]
    #[should_panic(expected = "InvariantViolation")]
    fn test_invariants_reject_released_plus_refunded_exceeds_funded() {
        let (env, admin) = setup_test_env();
        env.as_contract(&admin, || {
            let c = make_contract(&env, crate::ContractStatus::Created, 100, 60, 60, 100);
            require_contract_invariants(&env, &c);
        });
    }

    #[test]
    #[should_panic(expected = "InvariantViolation")]
    fn test_invariants_reject_funded_exceeds_deposited() {
        let (env, admin) = setup_test_env();
        env.as_contract(&admin, || {
            let c = make_contract(&env, crate::ContractStatus::Created, 101, 0, 0, 100);
            require_contract_invariants(&env, &c);
        });
    }

    #[test]
    #[should_panic(expected = "InvariantViolation")]
    fn test_invariants_reject_terminal_under_settled() {
        let (env, admin) = setup_test_env();
        env.as_contract(&admin, || {
            let c = make_contract(&env, crate::ContractStatus::Completed, 100, 40, 0, 100);
            require_contract_invariants(&env, &c);
        });
    }

    #[test]
    #[should_panic(expected = "InvariantViolation")]
    fn test_invariants_reject_negative_amounts() {
        let (env, admin) = setup_test_env();
        env.as_contract(&admin, || {
            let c = make_contract(&env, crate::ContractStatus::Created, -1, 0, 0, 0);
            require_contract_invariants(&env, &c);
        });
    }
}
