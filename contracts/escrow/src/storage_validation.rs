//! Bounds validation for storage entrypoint inputs.
//! Bounds validation for storage entrypoint inputs.
//!
//! This module extracts numeric and length bound checks for storage-mutating
//! entrypoints into a single source of truth. Each function validates one
//! logical parameter and panics with the appropriate typed [`EscrowError`]
//! on rejection.
//!
//! All functions are pure (no side-effects) and intended to be called at the
//! top of the corresponding entrypoint, before any state mutation occurs.
//!
//! # Validation boundaries
//!
//! Each validator defines a closed interval of accepted inputs and rejects
//! everything else deterministically. The boundaries are:
//!
//! * `validate_escrow_total_cap`: `(0, i128::MAX]`
//! * `validate_reputation_config_params`: `min_rating ∈ [1, 10]`,
//!   `max_rating ∈ [min_rating, 10]`, `max_comment_bytes ∈ [1, 1000]`
//! * `validate_milestone_count`: `[1, MAX_MILESTONES]`
//! * `validate_protocol_fee_bps`: `[0, MAX_FEE_BPS]`
//! * `validate_stroop_amount`: `(0, MAX_SINGLE_AMOUNT_STROOPS]`

use crate::milestones_consts::{
    MAX_FEE_BPS, MAX_MILESTONES, MAX_RATING, MAX_REPUTATION_CONFIG_COMMENT_BYTES_CEILING,
    MAX_REPUTATION_CONFIG_RATING_CEILING, MIN_COMMENT_BYTES, MIN_RATING,
};
use crate::types::{Contract, DataKey, Milestone};
use crate::{Error, EscrowError};
use soroban_sdk::panic_with_error;
use soroban_sdk::{Env, Vec};

/// Validate the governed total escrow cap in stroops.
///
/// # Accepted values
/// * Any `i128` in `(0, i128::MAX]`.
///
/// # Rejected values
/// * `0` — a zero cap would block every contract creation.
/// * Negative values — amounts must be positive.
///
/// # Panics
/// Panics with [`Error::InvalidProtocolParameters`] when the cap is out
/// of range.
#[inline]
pub(crate) fn validate_escrow_total_cap(env: &Env, max_escrow_total_stroops: i128) {
    if max_escrow_total_stroops <= 0 {
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
/// # Panics
/// Panics with [`Error::InvalidProtocolParameters`] when any bound is violated.
#[inline]
pub(crate) fn validate_reputation_config_params(
    env: &Env,
    min_rating: u32,
    max_rating: u32,
    max_comment_bytes: u32,
) {
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
/// # Panics
/// Panics with [`EscrowError::EmptyMilestones`] when `count == 0` or
/// [`EscrowError::TooManyMilestones`] when `count > MAX_MILESTONES`.
#[inline]
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
/// # Panics
/// Panics with [`Error::InvalidProtocolParameters`] when `bps > MAX_FEE_BPS`.
#[inline]
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
/// # Panics
/// Panics with [`EscrowError::AmountMustBePositive`] when `amount <= 0` or
/// [`EscrowError::InvalidMilestoneAmount`] when the amount exceeds the cap.
#[inline]
pub(crate) fn validate_stroop_amount(env: &Env, amount: i128) {
    if amount <= 0 {
        env.panic_with_error(crate::EscrowError::AmountMustBePositive);
    }
    if amount > crate::amount_validation::MAX_SINGLE_AMOUNT_STROOPS {
        env.panic_with_error(crate::EscrowError::InvalidMilestoneAmount);
    }
}

// ── Concurrent-execution hardening (issue #1535) ─────────────────────────────
//
// The functions above validate *inputs* before a storage mutation starts.  The
// section below validates the *persisted state* those mutations produce and
// serialises mutation of a single contract so that re-entrant or interleaved
// execution can never observe or leave behind a partially-applied state.
//
// Soroban runs one transaction at a time and rolls every storage write back when
// a call panics, so a classic data race is impossible.  Two hazards remain:
//
// 1. **Re-entrancy** — a malicious settlement token can call back into the
//    escrow while a `transfer` is in flight.  The mutation lock turns any such
//    re-entrant mutation into a deterministic `ConcurrentMutation` failure
//    instead of letting it operate on half-updated state.
// 2. **Corrupt / stale state** — a persisted record that violates the accounting
//    or milestone invariants would otherwise be read, mutated and written back,
//    laundering the corruption into a record that looks valid.  The checked
//    accessors refuse to load or store such a record.

/// Ledgers a mutation-lock entry survives before the host evicts it.
///
/// One day (17 280 ledgers at ~5 s each).  The lock only ever lives for the
/// duration of a single entrypoint call: the TTL exists purely so an entry
/// orphaned by a host-level failure can never keep a contract permanently
/// unusable, because the *presence* of the entry is what blocks mutation.
///
pub const MUTATION_LOCK_TTL_LEDGERS: u32 = crate::ttl::LEDGERS_PER_DAY;

/// Storage key for the per-contract mutation lock.
pub(crate) fn mutation_lock_key(contract_id: u32) -> DataKey {
    DataKey::ContractMutationLock(contract_id)
}

/// Returns `true` when a storage mutation for `contract_id` is in flight.
pub(crate) fn is_contract_locked(env: &Env, contract_id: u32) -> bool {
    env.storage().persistent().has(&mutation_lock_key(contract_id))
}

/// RAII guard holding the per-contract mutation lock.
///
/// The guard owns a clone of the [`Env`] so the lock is released in [`Drop`] on
/// every exit path, including early returns.  A transaction that panics rolls
/// its storage writes back, so a trapped call cannot leak the lock either.
pub struct ContractMutationGuard {
    env: Env,
    contract_id: u32,
}

impl ContractMutationGuard {
    /// Acquires the mutation lock for `contract_id`.
    ///
    /// # Panics
    /// Panics with [`Error::ConcurrentMutation`] when the lock is already held,
    /// which on Soroban means a re-entrant or interleaved call for the same
    /// contract is already mutating state.
    pub fn acquire(env: &Env, contract_id: u32) -> Self {
        if is_contract_locked(env, contract_id) {
            env.panic_with_error(Error::ConcurrentMutation);
        }

        let key = mutation_lock_key(contract_id);
        env.storage().persistent().set(&key, &true);
        env.storage()
            .persistent()
            .extend_ttl(&key, MUTATION_LOCK_TTL_LEDGERS, MUTATION_LOCK_TTL_LEDGERS);

        Self {
            env: env.clone(),
            contract_id,
        }
    }

    /// The contract whose mutation this guard serialises.
    pub fn contract_id(&self) -> u32 {
        self.contract_id
    }
}

/// Acquires the mutation lock for `contract_id` and returns the RAII guard.
///
/// Thin wrapper over [`ContractMutationGuard::acquire`] so entrypoints can bind
/// the guard with `let _mutation_guard = ...;`.
///
/// # Panics
/// Panics with [`Error::ConcurrentMutation`] when the lock is already held.
pub(crate) fn acquire_contract_mutation_lock(
    env: &Env,
    contract_id: u32,
) -> ContractMutationGuard {
    ContractMutationGuard::acquire(env, contract_id)
}

impl Drop for ContractMutationGuard {
    fn drop(&mut self) {
        release_contract_mutation_lock(&self.env, self.contract_id);
    }
}

/// Releases the mutation lock.
///
/// Idempotent: releasing an unlocked (or never locked) contract is a no-op, so
/// a guard that runs after a rolled-back transaction is always safe.
pub(crate) fn release_contract_mutation_lock(env: &Env, contract_id: u32) {
    env.storage()
        .persistent()
        .remove(&mutation_lock_key(contract_id));
}

/// Validates the accounting invariants of a persisted [`Contract`].
///
/// # Invariants
/// * `total_deposited`, `funded_amount`, `released_amount` and
///   `refunded_amount` are all non-negative.
/// * `released_amount + refunded_amount <= funded_amount` — a contract can never
///   have paid out more than it took in.  The sum is checked, so a corrupted
///   pair of maximum `i128`s fails here rather than overflowing.
///
/// # Panics
/// Panics with [`Error::StorageInvariantViolated`] on a violated invariant and
/// [`Error::PotentialOverflow`] when the sum cannot be represented.
pub(crate) fn validate_contract_accounting(env: &Env, contract: &Contract) {
    if contract.total_deposited < 0
        || contract.funded_amount < 0
        || contract.released_amount < 0
        || contract.refunded_amount < 0
    {
        env.panic_with_error(Error::StorageInvariantViolated);
    }

    let settled = contract
        .released_amount
        .checked_add(contract.refunded_amount)
        .unwrap_or_else(|| env.panic_with_error(Error::PotentialOverflow));

    if settled > contract.funded_amount {
        env.panic_with_error(Error::StorageInvariantViolated);
    }
}

/// Validates the invariants of a persisted [`Milestone`].
///
/// # Invariants
/// * `amount`, `funded_amount` and `refunded_amount` are all non-negative.
/// * `released` and `refunded` are mutually exclusive — a milestone is settled by
///   exactly one of the two flows.  This is the same rule
///   [`crate::milestone_transitions::MilestoneState::from_milestone`] enforces.
/// * A refunded milestone is refunded in full, so `refunded_amount == amount`.
///
/// # Panics
/// Panics with [`Error::StorageInvariantViolated`] on a violated invariant.
pub(crate) fn validate_milestone_consistency(env: &Env, milestone: &Milestone) {
    if milestone.amount < 0 || milestone.funded_amount < 0 || milestone.refunded_amount < 0 {
        env.panic_with_error(Error::StorageInvariantViolated);
    }

    if milestone.released && milestone.refunded {
        env.panic_with_error(Error::StorageInvariantViolated);
    }

    if milestone.refunded && milestone.refunded_amount != milestone.amount {
        env.panic_with_error(Error::StorageInvariantViolated);
    }
}

/// Validates every milestone in a persisted vector.
///
/// # Panics
/// Panics with [`Error::StorageInvariantViolated`] on the first milestone that
/// violates an invariant.
pub(crate) fn validate_milestones_consistency(env: &Env, milestones: &Vec<Milestone>) {
    for milestone in milestones.iter() {
        validate_milestone_consistency(env, &milestone);
    }
}

/// Loads a contract and validates it before it can be mutated.
///
/// # Panics
/// Panics with [`Error::ContractNotFound`] when the record is absent and with
/// [`Error::StorageInvariantViolated`] when it is present but inconsistent.
pub(crate) fn load_contract_checked(env: &Env, contract_id: u32) -> Contract {
    let contract: Contract = env
        .storage()
        .persistent()
        .get(&DataKey::Contract(contract_id))
        .unwrap_or_else(|| env.panic_with_error(Error::ContractNotFound));

    validate_contract_accounting(env, &contract);
    contract
}

/// Validates a contract and persists it, so an inconsistent record can never be
/// written to storage.
///
/// # Panics
/// Panics with [`Error::StorageInvariantViolated`] when `contract` violates an
/// accounting invariant; storage is left untouched in that case.
pub(crate) fn store_contract_checked(env: &Env, contract_id: u32, contract: &Contract) {
    validate_contract_accounting(env, contract);
    env.storage()
        .persistent()
        .set(&DataKey::Contract(contract_id), contract);
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
    fn validate_escrow_total_cap_rejects_i128_min_plus_one() {
        let e = env();
        validate_escrow_total_cap(&e, i128::MIN + 1);
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
    fn validate_reputation_config_params_rejects_min_rating_over_10() {
        let e = env();
        validate_reputation_config_params(&e, 11, 11, 200);
    }

    #[test]
    #[should_panic]
    fn validate_reputation_config_params_rejects_max_rating_u32_max() {
        let e = env();
        validate_reputation_config_params(&e, 1, u32::MAX, 200);
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
    fn validate_stroop_amount_rejects_i128_min() {
        let e = env();
        validate_stroop_amount(&e, i128::MIN);
    }
}

/// Tests for the concurrent-execution hardening (issue #1535): the per-contract
/// mutation lock plus the checked load/store accessors.
#[cfg(test)]
mod concurrency_tests {
    use super::*;
    use crate::types::{ContractStatus, ReleaseAuthorization};
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::Address;

    /// A contract record satisfying every accounting invariant.
    fn healthy_contract(env: &Env) -> Contract {
        Contract {
            client: Address::generate(env),
            freelancer: Address::generate(env),
            arbiter: None,
            status: ContractStatus::Funded,
            total_deposited: 1_000,
            funded_amount: 1_000,
            released_amount: 400,
            refunded_amount: 100,
            release_authorization: ReleaseAuthorization::ClientOnly,
            reputation_issued: false,
        }
    }

    /// An unreleased, unrefunded milestone.
    fn pending_milestone() -> Milestone {
        Milestone {
            amount: 500,
            funded_amount: 0,
            released: false,
            refunded: false,
            work_evidence: None,
            refunded_amount: 0,
            deadline: None,
        }
    }

    /// A registered escrow contract address, so storage handles are valid.
    fn registered(env: &Env) -> Address {
        env.register(crate::Escrow, ())
    }

    // ── mutation lock ────────────────────────────────────────────────────────

    #[test]
    fn lock_is_not_held_before_acquire() {
        let env = Env::default();
        let id = registered(&env);
        env.as_contract(&id, || {
            assert!(!is_contract_locked(&env, 7));
        });
    }

    #[test]
    fn guard_releases_lock_when_dropped() {
        let env = Env::default();
        let id = registered(&env);
        env.as_contract(&id, || {
            {
                let guard = ContractMutationGuard::acquire(&env, 7);
                assert_eq!(guard.contract_id(), 7);
                assert!(is_contract_locked(&env, 7));
            }
            // Drop ran, so a later mutation can acquire the lock again.
            assert!(!is_contract_locked(&env, 7));
            let _second = ContractMutationGuard::acquire(&env, 7);
        });
    }

    #[test]
    #[should_panic]
    fn reentrant_acquire_is_rejected() {
        let env = Env::default();
        let id = registered(&env);
        env.as_contract(&id, || {
            let _first = ContractMutationGuard::acquire(&env, 7);
            // Simulates a token callback re-entering the escrow mid-transfer.
            let _reentrant = ContractMutationGuard::acquire(&env, 7);
        });
    }

    #[test]
    fn locks_are_scoped_per_contract() {
        let env = Env::default();
        let id = registered(&env);
        env.as_contract(&id, || {
            let _one = ContractMutationGuard::acquire(&env, 1);
            // A different contract is not blocked by the first contract's lock.
            let _two = ContractMutationGuard::acquire(&env, 2);
            assert!(is_contract_locked(&env, 1));
            assert!(is_contract_locked(&env, 2));
        });
    }

    #[test]
    fn release_is_idempotent() {
        let env = Env::default();
        let id = registered(&env);
        env.as_contract(&id, || {
            release_contract_mutation_lock(&env, 9);
            release_contract_mutation_lock(&env, 9);
            assert!(!is_contract_locked(&env, 9));
        });
    }

    // ── contract accounting invariants ───────────────────────────────────────

    #[test]
    fn validate_contract_accounting_accepts_healthy() {
        let env = Env::default();
        validate_contract_accounting(&env, &healthy_contract(&env));
    }

    #[test]
    fn validate_contract_accounting_accepts_fully_settled() {
        let env = Env::default();
        let mut contract = healthy_contract(&env);
        contract.released_amount = 900;
        contract.refunded_amount = 100;
        validate_contract_accounting(&env, &contract);
    }

    #[test]
    #[should_panic]
    fn validate_contract_accounting_rejects_oversettled() {
        let env = Env::default();
        let mut contract = healthy_contract(&env);
        contract.funded_amount = 100;
        contract.released_amount = 90;
        contract.refunded_amount = 20;
        validate_contract_accounting(&env, &contract);
    }

    #[test]
    #[should_panic]
    fn validate_contract_accounting_rejects_negative_amount() {
        let env = Env::default();
        let mut contract = healthy_contract(&env);
        contract.refunded_amount = -1;
        validate_contract_accounting(&env, &contract);
    }

    #[test]
    #[should_panic]
    fn validate_contract_accounting_rejects_overflowing_settlement() {
        let env = Env::default();
        let mut contract = healthy_contract(&env);
        contract.funded_amount = i128::MAX;
        contract.released_amount = i128::MAX;
        contract.refunded_amount = i128::MAX;
        validate_contract_accounting(&env, &contract);
    }

    // ── milestone invariants ─────────────────────────────────────────────────

    #[test]
    fn validate_milestone_consistency_accepts_pending() {
        let env = Env::default();
        validate_milestone_consistency(&env, &pending_milestone());
    }

    #[test]
    fn validate_milestone_consistency_accepts_released() {
        let env = Env::default();
        let mut milestone = pending_milestone();
        milestone.released = true;
        milestone.funded_amount = milestone.amount;
        validate_milestone_consistency(&env, &milestone);
    }

    #[test]
    fn validate_milestone_consistency_accepts_refunded() {
        let env = Env::default();
        let mut milestone = pending_milestone();
        milestone.refunded = true;
        milestone.refunded_amount = milestone.amount;
        validate_milestone_consistency(&env, &milestone);
    }

    #[test]
    #[should_panic]
    fn validate_milestone_consistency_rejects_both_flags() {
        let env = Env::default();
        let mut milestone = pending_milestone();
        milestone.released = true;
        milestone.funded_amount = milestone.amount;
        milestone.refunded = true;
        milestone.refunded_amount = milestone.amount;
        validate_milestone_consistency(&env, &milestone);
    }

    #[test]
    #[should_panic]
    fn validate_milestone_consistency_rejects_partial_refund() {
        let env = Env::default();
        let mut milestone = pending_milestone();
        milestone.refunded = true;
        milestone.refunded_amount = milestone.amount - 1;
        validate_milestone_consistency(&env, &milestone);
    }

    #[test]
    #[should_panic]
    fn validate_milestone_consistency_rejects_negative_amount() {
        let env = Env::default();
        let mut milestone = pending_milestone();
        milestone.amount = -1;
        validate_milestone_consistency(&env, &milestone);
    }

    #[test]
    #[should_panic]
    fn validate_milestones_consistency_rejects_any_bad_entry() {
        let env = Env::default();
        let mut milestones = Vec::new(&env);
        milestones.push_back(pending_milestone());
        let mut corrupt = pending_milestone();
        corrupt.released = true;
        corrupt.funded_amount = corrupt.amount;
        corrupt.refunded = true;
        corrupt.refunded_amount = corrupt.amount;
        milestones.push_back(corrupt);
        validate_milestones_consistency(&env, &milestones);
    }

    #[test]
    fn validate_milestones_consistency_accepts_healthy_vector() {
        let env = Env::default();
        let mut milestones = Vec::new(&env);
        milestones.push_back(pending_milestone());
        let mut released = pending_milestone();
        released.released = true;
        released.funded_amount = released.amount;
        milestones.push_back(released);
        validate_milestones_consistency(&env, &milestones);
    }

    // ── checked accessors ────────────────────────────────────────────────────

    #[test]
    fn load_contract_checked_accepts_healthy() {
        let env = Env::default();
        let id = registered(&env);
        env.as_contract(&id, || {
            env.storage()
                .persistent()
                .set(&DataKey::Contract(7), &healthy_contract(&env));
            let loaded = load_contract_checked(&env, 7);
            assert_eq!(loaded.funded_amount, 1_000);
        });
    }

    #[test]
    #[should_panic]
    fn load_contract_checked_rejects_corrupt_record() {
        let env = Env::default();
        let id = registered(&env);
        env.as_contract(&id, || {
            let mut corrupt = healthy_contract(&env);
            corrupt.funded_amount = 10;
            corrupt.released_amount = 1_000;
            env.storage()
                .persistent()
                .set(&DataKey::Contract(7), &corrupt);
            let _ = load_contract_checked(&env, 7);
        });
    }

    #[test]
    #[should_panic]
    fn load_contract_checked_rejects_absent_record() {
        let env = Env::default();
        let id = registered(&env);
        env.as_contract(&id, || {
            let _ = load_contract_checked(&env, 7);
        });
    }

    #[test]
    fn store_contract_checked_persists_healthy_record() {
        let env = Env::default();
        let id = registered(&env);
        env.as_contract(&id, || {
            store_contract_checked(&env, 7, &healthy_contract(&env));
            assert!(env.storage().persistent().has(&DataKey::Contract(7)));
        });
    }

    #[test]
    #[should_panic]
    fn store_contract_checked_rejects_corrupt_record() {
        let env = Env::default();
        let id = registered(&env);
        env.as_contract(&id, || {
            let mut corrupt = healthy_contract(&env);
            corrupt.refunded_amount = corrupt.funded_amount + 1;
            // Validation runs before the write, so the panic must happen and the
            // corrupt record must never reach storage.
            store_contract_checked(&env, 7, &corrupt);
        });
    }
}
