//! Centralized storage precondition checks and contract loading helpers.
//!
//! This module extracts repeated storage validation patterns into a single source
//! of truth, ensuring consistent error handling and reducing code duplication across
//! entrypoints. All contract loading operations should route through these helpers.
//!
//! ## Concurrency and idempotency invariants
//!
//! Soroban smart contracts execute within a single atomic ledger transaction. A
//! given transaction either commits in full or aborts with no state change — there
//! is no partial commit and no interleaving of two concurrent transactions within
//! the same ledger. This means classic "check-then-act" races between two threads
//! are impossible *within* a single invocation, but replay attacks and
//! double-submission at the application layer are real threats.
//!
//! The helpers in this module are therefore hardened against the following adverse
//! patterns:
//!
//! * **Replay attacks (nonce reuse)**: `consume_admin_nonce` stores the *next
//!   expected* nonce immediately after a successful check. A replayed call with
//!   the same nonce will observe the already-incremented value and fail with
//!   [`Error::StaleNonce`]. The stored value is never decremented, so nonces are
//!   strictly monotone.
//!
//! * **Double-initialization**: `require_not_initialized` checks `DataKey::Initialized`
//!   with `.has()` before any write so that a second call to `initialize` from any
//!   code path fails with [`Error::AlreadyInitialized`] regardless of how the check
//!   is reached.
//!
//! * **Double-finalization**: `require_not_finalized` and `is_finalized` are thin
//!   wrappers around a single persistent `.has()` so callers never diverge in how
//!   they interpret the finalization state.
//!
//! * **Pause-then-act gaps**: `load_contract_checked` performs the pause check
//!   *before* loading the contract body. This ensures that no contract data is
//!   visible to the caller when the system is paused, eliminating any ambiguity
//!   about which state the caller should trust.

use crate::{Contract, DataKey, Error, EscrowError};
use soroban_sdk::{Env, Symbol, Vec};

// ── Initialization guards ─────────────────────────────────────────────────────

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
///
/// # Concurrency invariant
/// This is a read-only guard. The initialization flag is set exactly once by
/// `save_initialized` (see below). A subsequent call to `require_initialized`
/// after initialization will always return `true`.
pub(crate) fn require_initialized(env: &Env) -> bool {
    env.storage()
        .persistent()
        .get::<_, bool>(&DataKey::Initialized)
        .unwrap_or(false)
        .then_some(true)
        .ok_or(Error::NotInitialized)
        .unwrap_or_else(|err| env.panic_with_error(err))
}

/// Assert that the contract system has **not** been initialized.
///
/// Call this at the very start of the `initialize` entrypoint to provide a
/// single, consistent double-initialization guard. Every code path that might
/// call into initialization logic should route through this helper rather than
/// performing an inline `.has()` check.
///
/// # Panics
/// - `AlreadyInitialized` if `DataKey::Initialized` is already set to `true`
///
/// # Idempotency invariant
/// Once `save_initialized` has written `DataKey::Initialized = true`, every
/// subsequent call to `require_not_initialized` will panic. There is no
/// operation that clears the initialized flag, so initialization is
/// permanently one-shot.
pub(crate) fn require_not_initialized(env: &Env) {
    // Use `.has()` instead of `.get()` for the presence check so we avoid
    // deserializing the value; the mere existence of the key is sufficient.
    if env.storage().persistent().has(&DataKey::Initialized) {
        env.panic_with_error(Error::AlreadyInitialized);
    }
}

/// Persist the initialized flag and write the admin address in one logical step.
///
/// This helper is the single canonical write path for initialization. Callers
/// MUST call `require_not_initialized` before this function to prevent double
/// writes. The two-step pattern (check then write) is safe within Soroban
/// because a ledger transaction is fully atomic: if the check passes, no other
/// transaction can have set the flag between the check and the write in the
/// same transaction context.
///
/// # Arguments
/// * `env`   - The contract environment
/// * `admin` - The admin address to record under `DataKey::Admin`
///
/// # Invariant
/// After this function returns, `DataKey::Initialized` is `true` and
/// `DataKey::Admin` is `admin`. Both are persistent entries.
pub(crate) fn save_initialized(env: &Env, admin: &crate::Address) {
    env.storage().persistent().set(&DataKey::Initialized, &true);
    env.storage().persistent().set(&DataKey::Admin, admin);
}

// ── Contract ID bounds ────────────────────────────────────────────────────────

/// Validate that `contract_id` is within numeric bounds (non-zero).
///
/// Zero is rejected because contracts are allocated starting from ID 1. Any
/// read or write against ID 0 is a programming error and must fail loudly.
///
/// # Panics
/// - `InvalidContractId` if `contract_id == 0`
///
/// # Correctness note
/// Callers that pass the result of a prior validated allocation (from
/// `create_contract`) will never see a zero here in normal operation. This
/// guard exists to reject malicious or confused client inputs.
pub(crate) fn validate_contract_id_bounds(env: &Env, contract_id: u32) {
    if contract_id == 0 {
        env.panic_with_error(Error::InvalidContractId);
    }
}

// ── Contract loading ──────────────────────────────────────────────────────────

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
/// - `InvalidContractId` if `contract_id` is 0
/// - `ContractNotFound` if no contract exists for this ID
///
/// # Returns
/// The loaded `Contract` or panics with `ContractNotFound`
pub(crate) fn load_contract(env: &Env, contract_id: u32) -> Contract {
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
/// - `InvalidContractId` if `contract_id` is 0
/// - `ContractNotFound` if no milestone vector exists for this contract
///
/// # Returns
/// The loaded milestone vector or panics with `ContractNotFound`
pub(crate) fn load_milestones(env: &Env, contract_id: u32) -> Vec<crate::Milestone> {
    validate_contract_id_bounds(env, contract_id);
    let milestone_key = Symbol::new(env, "milestones");
    env.storage()
        .persistent()
        .get(&(DataKey::Contract(contract_id), milestone_key))
        .unwrap_or_else(|| env.panic_with_error(Error::ContractNotFound))
}

/// Load a contract, optionally with precondition checks for mutation.
///
/// This is the primary helper for loading contracts with optional safety guards:
/// - `check_paused`: If true, verifies pause/emergency flags are not set
/// - `check_finalized`: If true, verifies the contract has not been finalized
///
/// The pause check is performed **before** the contract is loaded from storage.
/// This ordering is intentional: it means callers never receive a contract
/// value in a state where the system is paused, which closes a potential
/// check-then-use ambiguity when the returned value is stored in a local
/// variable and the pause state changes conceptually between the load and the
/// mutation.
///
/// Within a single Soroban transaction the storage is consistent throughout,
/// but this ordering also makes the control flow easier to reason about during
/// code review.
///
/// # Arguments
/// * `env` - The contract environment
/// * `contract_id` - The contract ID to load
/// * `check_paused` - Whether to verify pause/emergency states
/// * `check_finalized` - Whether to verify finalization state
///
/// # Panics
/// - `InvalidContractId` if `contract_id` is 0
/// - `ContractPaused` if `check_paused` is true and pause flag is set
/// - `EmergencyActive` if `check_paused` is true and emergency flag is set
/// - `ContractNotFound` if no contract exists for this ID
/// - `AlreadyFinalized` if `check_finalized` is true and contract is finalized
///
/// # Returns
/// The loaded `Contract` if all preconditions pass
pub(crate) fn load_contract_checked(
    env: &Env,
    contract_id: u32,
    check_paused: bool,
    check_finalized: bool,
) -> Contract {
    // Validate bounds first — this rejects the degenerate zero ID immediately
    // before incurring any storage reads.
    validate_contract_id_bounds(env, contract_id);

    // Pause check happens before the contract load (see doc comment above).
    if check_paused {
        require_not_paused(env);
    }

    // Load the contract body.
    let contract = load_contract(env, contract_id);

    // Finalization check follows the load because the finalization record is
    // stored under a separate key from the contract body. Both are read in the
    // same transaction, so this is consistent. Checking finalization *after*
    // confirming the contract exists avoids a misleading `AlreadyFinalized` on
    // a non-existent contract.
    if check_finalized {
        require_not_finalized(env, contract_id);
    }

    contract
}

// ── Pause and emergency guards ────────────────────────────────────────────────

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
///
/// # Idempotency note
/// This function is read-only and has no side effects. Calling it multiple
/// times within the same transaction always observes the same state.
pub(crate) fn require_not_paused(env: &Env) -> bool {
    if env
        .storage()
        .persistent()
        .get::<_, bool>(&DataKey::Paused)
        .unwrap_or(false)
    {
        env.panic_with_error(Error::ContractPaused);
    }
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
    // Legacy boolean pause acts as Global
    if env
        .storage()
        .persistent()
        .get::<_, bool>(&DataKey::Paused)
        .unwrap_or(false)
    {
        env.panic_with_error(Error::ContractPaused);
    }

    // Emergency always blocks everything
    if env
        .storage()
        .persistent()
        .get::<_, bool>(&DataKey::Emergency)
        .unwrap_or(false)
    {
        env.panic_with_error(Error::EmergencyActive);
    }

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

// ── Admin nonce ───────────────────────────────────────────────────────────────

/// Consume the next expected admin nonce, rejecting stale or future values.
///
/// The nonce is a strictly monotone `u64` counter stored under
/// [`DataKey::AdminNonce`]. On the first call the expected nonce is `1`
/// (zero means "never consumed").
///
/// # Atomicity invariant
/// The read, compare, and increment are performed within a single Soroban
/// transaction. Soroban's ledger guarantees that no other transaction can
/// observe or modify `DataKey::AdminNonce` between the `.get` and the `.set`
/// within the same invocation. This makes the combined read-validate-write
/// effectively atomic.
///
/// A replay of the same call in a later transaction will read the incremented
/// value and immediately fail with [`Error::StaleNonce`].
///
/// # Overflow guard
/// If `current + 1` would overflow `u64`, the function panics with
/// [`Error::PotentialOverflow`]. At one nonce per admin operation, the 2^64
/// ceiling is not reachable in practice, but the check is present to satisfy
/// formal correctness requirements and to make the contract provably panic-safe.
///
/// # Arguments
/// * `env`            - The contract environment
/// * `provided_nonce` - The nonce value supplied by the caller
///
/// # Panics
/// - `StaleNonce` if `provided_nonce != current + 1`
/// - `PotentialOverflow` if `current == u64::MAX`
pub(crate) fn consume_admin_nonce(env: &Env, provided_nonce: u64) {
    let current: u64 = env
        .storage()
        .persistent()
        .get(&DataKey::AdminNonce)
        .unwrap_or(0u64);

    // Guard against nonce counter overflow (defensive; 2^64 is unreachable
    // in any realistic deployment timeline).
    let expected = current
        .checked_add(1)
        .unwrap_or_else(|| env.panic_with_error(Error::PotentialOverflow));

    if provided_nonce != expected {
        env.panic_with_error(Error::StaleNonce);
    }

    // Commit the incremented nonce atomically within this transaction.
    // Any replay using the same `provided_nonce` in a future transaction
    // will find `current = expected` and compute `expected_new = expected + 1`,
    // causing the equality check to fail.
    env.storage()
        .persistent()
        .set(&DataKey::AdminNonce, &expected);
}

// ── Finalization guards ───────────────────────────────────────────────────────

/// Check if a contract has been finalized.
///
/// # Arguments
/// * `env` - The contract environment
/// * `contract_id` - The contract ID to check
///
/// # Returns
/// `true` if the contract is finalized
pub(crate) fn is_finalized(env: &Env, contract_id: u32) -> bool {
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
/// - `InvalidContractId` if `contract_id` is 0
/// - `AlreadyFinalized` if the contract has been finalized
///
/// # Returns
/// `true` if not finalized, or panics
///
/// # Idempotency note
/// Once a finalization record is written, this function will always panic for
/// that contract ID. There is no operation that removes a finalization record.
pub(crate) fn require_not_finalized(env: &Env, contract_id: u32) -> bool {
    validate_contract_id_bounds(env, contract_id);
    if is_finalized(env, contract_id) {
        env.panic_with_error(Error::AlreadyFinalized);
    }
    true
}

// ── Tests ─────────────────────────────────────────────────────────────────────
//
// Unit tests for the storage helpers are located in `src/test/storage_helpers.rs`
// rather than as inline `#[cfg(test)]` tests here. This is necessary because
// Soroban's SDK requires all persistent-storage calls to execute within an active
// contract context (`env.as_contract(&contract_address, || { ... })`), which in
// turn requires a registered contract instance via `env.register(Escrow, ())`.
//
// Registering an `Escrow` from within storage.rs would create a circular
// module dependency. Moving the tests to the `test/` module, which already
// imports `Escrow` and `EscrowClient`, resolves this cleanly.

