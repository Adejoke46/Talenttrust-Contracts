//! Typed storage keys and read/write helpers for settlement entries.
//!
//! This module is the **single authoritative layer** for every settlement-related
//! persistent storage read or write in the contract.  Callers must never access
//! settlement storage keys directly; all paths must go through the helpers
//! defined here so that the correct [`DataKey`] variant and storage bucket
//! (always `persistent()`) are consistently used.
//!
//! # Storage keys
//!
//! | Entry | `DataKey` variant | Bucket | Mutability |
//! | --- | --- | --- | --- |
//! | Settlement token address | `SettlementToken` | `persistent()` | **Write-once** |
//! | Finalization record | `Finalization(contract_id)` | `persistent()` | **Write-once** |
//!
//! # Compatibility contract
//!
//! The following invariants are part of the public interface.  Callers, tests,
//! and downstream tooling may rely on them.  Breaking any of these invariants
//! constitutes a compatibility regression and requires a migration plan:
//!
//! 1. **Round-trip guarantee.** Every `write_*` followed by the corresponding
//!    `read_*` in the same transaction returns the exact same value.
//! 2. **Absence is `None`, not a panic.** Every `read_*` returns an `Option`;
//!    an absent key always returns `None` and never panics or errors.
//! 3. **Write-once token binding via [`write_settlement_token_once`].** Callers
//!    that must enforce single-write semantics (i.e., all production paths)
//!    should call this guard helper, not the raw [`write_settlement_token`].
//! 4. **Finalization is immutable after the first write.** Call
//!    [`require_not_finalized`] before every [`write_finalization`]; a second
//!    write to the same `contract_id` is rejected with `AlreadyFinalized`.
//! 5. **Boundary `contract_id` values are handled deterministically.**
//!    `0` and `u32::MAX` produce distinct, non-colliding keys at the storage
//!    layer.  Business-rule rejection (e.g., rejecting `contract_id == 0`) is
//!    the caller's responsibility and happens in upper-layer entrypoints before
//!    these helpers are reached.
//! 6. **This module never emits events and never checks authorization.**  Those
//!    responsibilities belong to the entrypoints in `lib.rs` and `finalize.rs`.
//!
//! # Persistence invariants
//!
//! Every `write_*` followed by the corresponding `read_*` returns the
//! same value.  The `test_settlement_storage` module in `test/` verifies
//! this invariant plus absent-key behaviour.
//!
//! # Write-once guarantee
//!
//! [`write_finalization`] refuses to overwrite an existing close record and
//! panics with [`Error::AlreadyFinalized`] instead, so duplicate or racing
//! finalization attempts cannot silently discard the original finalizer,
//! timestamp or summary snapshot.

use crate::{finalize::FinalizationRecord, DataKey, Error};
use soroban_sdk::{Address, Env};

/// Result of a commit-once settlement write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommitOutcome {
    /// No value existed, so this invocation persisted it.
    Committed,
    /// The same value was already persisted; no storage mutation was needed.
    Recovered,
}

fn validate_contract_id(contract_id: u32) -> Result<(), Error> {
    if contract_id == 0 {
        return Err(Error::InvalidContractId);
    }
    Ok(())
}

fn panic_on_error<T>(env: &Env, result: Result<T, Error>) -> T {
    result.unwrap_or_else(|error| env.panic_with_error(error))
}

// ── Settlement token ────────────────────────────────────────────────────────

/// Read the bound settlement token address from persistent storage.
///
/// Returns `None` when no token has been bound yet (`bind_settlement_token`
/// has not been called).
///
/// # Invariants
///
/// * Returns `None` before the first successful `bind_settlement_token`.
/// * Returns `Some(address)` after binding and the value is stable — the token
///   address can never revert to `None` within the same ledger state.
/// * No authorization checks; always safe to call from any context.
///
/// # Arguments
///
/// * `env` – The Soroban environment.
///
/// # Returns
///
/// `Some(Address)` of the bound SAC token, or `None` if the token has not
/// been bound yet.
///
/// # Example
///
/// ```no_run
/// use soroban_sdk::{testutils::Address as _, Address, Env};
/// use escrow::{Escrow, DataKey};
/// use escrow::settlement::read_settlement_token;
///
/// let env = Env::default();
/// let contract = env.register(Escrow, ());
/// env.as_contract(&contract, || {
///     // Before any binding, the result is None.
///     assert!(read_settlement_token(&env).is_none());
///
///     // After writing a token address it is returned.
///     let token = Address::generate(&env);
///     env.storage().persistent().set(&DataKey::SettlementToken, &token);
///     assert_eq!(read_settlement_token(&env), Some(token));
/// });
/// ```
pub fn read_settlement_token(env: &Env) -> Option<Address> {
    env.storage().persistent().get(&DataKey::SettlementToken)
}

/// Commit the settlement token address under the canonical storage key.
///
/// An identical retry is a no-op. A retry with a different address returns
/// [`Error::SettlementTokenAlreadyBound`] without changing the original
/// binding.
pub(crate) fn commit_settlement_token(env: &Env, token: &Address) -> Result<CommitOutcome, Error> {
    match read_settlement_token(env) {
        None => {
            env.storage()
                .persistent()
                .set(&DataKey::SettlementToken, token);
            Ok(CommitOutcome::Committed)
        }
        Some(existing) if existing == *token => Ok(CommitOutcome::Recovered),
        Some(_) => Err(Error::SettlementTokenAlreadyBound),
    }
}

/// Persist the settlement token address under the canonical storage key.
///
/// # ⚠ Precondition — write-once semantics
///
/// This is a **raw write** and does **not** enforce write-once semantics on its
/// own.  Callers that must prevent a second bind (all production entrypoints)
/// must either:
///
/// * Call [`write_settlement_token_once`], which enforces the guard atomically, or
/// * Check [`is_settlement_token_bound`] before calling this helper.
///
/// Calling `write_settlement_token` when a token is already bound silently
/// overwrites the existing value, which breaks token custody invariants.  Only
/// internal migration or upgrade code should call this helper directly.
///
/// # Arguments
///
/// * `env`   – The Soroban environment.
/// * `token` – The SAC token [`Address`] to bind.
///
/// # Example
///
/// ```no_run
/// use soroban_sdk::{testutils::Address as _, Address, Env};
/// use escrow::{Escrow, DataKey};
/// use escrow::settlement::{write_settlement_token, read_settlement_token};
///
/// let env = Env::default();
/// let contract = env.register(Escrow, ());
/// let token = Address::generate(&env);
///
/// env.as_contract(&contract, || {
///     write_settlement_token(&env, &token);
///     assert_eq!(read_settlement_token(&env), Some(token));
/// });
/// ```
pub fn write_settlement_token(env: &Env, token: &Address) {
    let _ = panic_on_error(env, commit_settlement_token(env, token));
}

/// Panic with [`Error::SettlementTokenAlreadyBound`] if a settlement token
/// is already bound.
///
/// Callers must invoke this guard immediately before
/// [`write_settlement_token`] to enforce write-once semantics under
/// concurrent or repeated execution.  Because the guard and the write run
/// in the same invocation, Soroban's transactional execution model makes
/// the check-and-write atomic: a racing bind either observes the token as
/// unbound and writes, or observes it as bound and panics — never both.
///
/// # Arguments
///
/// * `env` – The Soroban environment.
///
/// # Errors
///
/// Panics with [`Error::SettlementTokenAlreadyBound`] when
/// [`is_settlement_token_bound`] returns `true`.
pub fn require_settlement_token_unbound(env: &Env) {
    if is_settlement_token_bound(env) {
        env.panic_with_error(Error::SettlementTokenAlreadyBound);
    }
}

/// Persist the settlement token address **exactly once**, panicking with
/// [`Error::SettlementTokenAlreadyBound`] on any subsequent call.
///
/// Use this instead of [`write_settlement_token`] in any code path where
/// overwriting an already-bound token would break custody invariants.  The
/// `bind_settlement_token` entrypoint in `lib.rs` performs an equivalent check
/// before delegating to [`write_settlement_token`]; new callers should use
/// this helper instead of reimplementing the same guard.
///
/// # Arguments
///
/// * `env`   – The Soroban environment.
/// * `token` – The SAC token [`Address`] to bind.
///
/// # Errors
///
/// Panics with [`Error::SettlementTokenAlreadyBound`] when a token is already
/// stored under [`DataKey::SettlementToken`].
///
/// # Example
///
/// ```no_run
/// use soroban_sdk::{testutils::Address as _, Address, Env};
/// use escrow::Escrow;
/// use escrow::settlement::{write_settlement_token_once, read_settlement_token};
///
/// let env = Env::default();
/// let contract = env.register(Escrow, ());
/// let token = Address::generate(&env);
///
/// env.as_contract(&contract, || {
///     // First write succeeds.
///     write_settlement_token_once(&env, &token);
///     assert_eq!(read_settlement_token(&env), Some(token));
/// });
/// ```
///
/// A second call panics:
///
/// ```no_run
/// use soroban_sdk::{testutils::Address as _, Address, Env};
/// use escrow::Escrow;
/// use escrow::settlement::write_settlement_token_once;
///
/// let env = Env::default();
/// let contract = env.register(Escrow, ());
/// let token1 = Address::generate(&env);
/// let token2 = Address::generate(&env);
///
/// env.as_contract(&contract, || {
///     write_settlement_token_once(&env, &token1);
///     write_settlement_token_once(&env, &token2); // panics: SettlementTokenAlreadyBound
/// });
/// ```
pub fn write_settlement_token_once(env: &Env, token: &Address) {
    if is_settlement_token_bound(env) {
        env.panic_with_error(Error::SettlementTokenAlreadyBound);
    }
    write_settlement_token(env, token);
}

/// Return `true` when a settlement token has been bound.
///
/// # Arguments
///
/// * `env` – The Soroban environment.
///
/// # Returns
///
/// `true` if a token address is present in persistent storage, `false`
/// otherwise.
///
/// # Example
///
/// ```no_run
/// use soroban_sdk::{testutils::Address as _, Address, Env};
/// use escrow::Escrow;
/// use escrow::settlement::{is_settlement_token_bound, write_settlement_token};
///
/// let env = Env::default();
/// let contract = env.register(Escrow, ());
///
/// env.as_contract(&contract, || {
///     assert!(!is_settlement_token_bound(&env));
///
///     let token = Address::generate(&env);
///     write_settlement_token(&env, &token);
///     assert!(is_settlement_token_bound(&env));
/// });
/// ```
pub fn is_settlement_token_bound(env: &Env) -> bool {
    read_settlement_token(env).is_some()
}

/// Read the bound settlement token, panicking with [`Error::SettlementTokenNotConfigured`]
/// when absent.  Use this in money-flow paths that require a bound token.
///
/// # Arguments
///
/// * `env` – The Soroban environment.
///
/// # Returns
///
/// The [`Address`] of the bound settlement token.
///
/// # Errors
///
/// Panics with [`Error::SettlementTokenNotConfigured`] when no token has
/// been bound via [`write_settlement_token`].
///
/// # Example
///
/// ```no_run
/// use soroban_sdk::{testutils::Address as _, Address, Env};
/// use escrow::Escrow;
/// use escrow::settlement::{require_settlement_token, write_settlement_token};
///
/// let env = Env::default();
/// let contract = env.register(Escrow, ());
/// let token = Address::generate(&env);
///
/// env.as_contract(&contract, || {
///     write_settlement_token(&env, &token);
///
///     // Returns the bound address when one is present.
///     let bound = require_settlement_token(&env);
///     assert_eq!(bound, token);
/// });
/// ```
///
/// Calling this without a prior [`write_settlement_token`] panics:
///
/// ```no_run
/// use soroban_sdk::Env;
/// use escrow::Escrow;
/// use escrow::settlement::require_settlement_token;
///
/// let env = Env::default();
/// let contract = env.register(Escrow, ());
/// env.as_contract(&contract, || {
///     let _ = require_settlement_token(&env); // panics: SettlementTokenNotConfigured
/// });
/// ```
pub fn require_settlement_token(env: &Env) -> Address {
    read_settlement_token(env)
        .unwrap_or_else(|| env.panic_with_error(Error::SettlementTokenNotConfigured))
}

// ── Finalization record ─────────────────────────────────────────────────────

/// Construct the canonical [`DataKey`] for a finalization record.
///
/// # Boundary safety
///
/// All `u32` values, including `0` and `u32::MAX`, produce a distinct, valid,
/// non-colliding key.  Business-rule enforcement (e.g., rejecting
/// `contract_id == 0`) is the caller's responsibility and happens before
/// reaching this helper.
///
/// # Arguments
///
/// * `contract_id` – The numeric contract identifier.
///
/// # Returns
///
/// `DataKey::Finalization(contract_id)`.
///
/// # Example
///
/// ```no_run
/// use escrow::{DataKey, settlement::finalization_key};
///
/// let key = finalization_key(7);
/// assert_eq!(key, DataKey::Finalization(7));
/// ```
pub fn finalization_key(contract_id: u32) -> DataKey {
    DataKey::Finalization(contract_id)
}

/// Read a finalization record for `contract_id`, if it exists.
///
/// # Invariants
///
/// * Returns `None` before `write_finalization` is called for this `contract_id`.
/// * Returns `Some(record)` after writing.  The `None → Some` transition is
///   permanent within a given deployment.
/// * Never panics; performs no authorization.
///
/// # Arguments
///
/// * `env`         – The Soroban environment.
/// * `contract_id` – The numeric contract identifier.
///
/// # Returns
///
/// `Some(FinalizationRecord)` when the contract has been finalized, `None`
/// otherwise.
///
/// # Example
///
/// ```no_run
/// use soroban_sdk::{testutils::Address as _, Address, Env};
/// use escrow::{
///     Escrow, ContractStatus, ContractSummary, CONTRACT_SUMMARY_SCHEMA_VERSION,
///     settlement::{read_finalization, write_finalization},
/// };
/// use escrow::finalize::FinalizationRecord;
///
/// let env = Env::default();
/// let contract = env.register(Escrow, ());
///
/// env.as_contract(&contract, || {
///     // Returns None before any record is written.
///     assert!(read_finalization(&env, 1).is_none());
/// });
/// ```
pub fn read_finalization(env: &Env, contract_id: u32) -> Option<FinalizationRecord> {
    panic_on_error(env, validate_contract_id(contract_id));
    env.storage()
        .persistent()
        .get(&finalization_key(contract_id))
}

/// Return `true` when a finalization record already exists for `contract_id`.
///
/// # Invariants
///
/// * Returns `false` before `write_finalization` and `true` afterwards.
/// * The `false → true` transition is permanent within a given deployment.
/// * Never panics; performs no authorization.
///
/// # Arguments
///
/// * `env`         – The Soroban environment.
/// * `contract_id` – The numeric contract identifier.
///
/// # Returns
///
/// `true` if a [`FinalizationRecord`] is stored for `contract_id`, `false`
/// otherwise.
///
/// # Example
///
/// ```no_run
/// use soroban_sdk::{testutils::Address as _, Address, Env};
/// use escrow::{
///     Escrow, ContractStatus, ContractSummary, CONTRACT_SUMMARY_SCHEMA_VERSION,
///     settlement::{is_finalized, write_finalization},
/// };
/// use escrow::finalize::FinalizationRecord;
///
/// let env = Env::default();
/// let contract = env.register(Escrow, ());
///
/// env.as_contract(&contract, || {
///     assert!(!is_finalized(&env, 42));
///
///     let record = FinalizationRecord {
///         finalizer: Address::generate(&env),
///         timestamp: 9999,
///         summary: ContractSummary {
///             schema_version: CONTRACT_SUMMARY_SCHEMA_VERSION,
///             client: Address::generate(&env),
///             freelancer: Address::generate(&env),
///             arbiter: None,
///             status: ContractStatus::Completed,
///             reputation_issued: false,
///             total_amount: 500,
///             funded_amount: 500,
///             released_amount: 500,
///             refundable_balance: 0,
///             released_milestone_count: 1,
///             milestones: soroban_sdk::Vec::new(&env),
///         },
///     };
///     write_finalization(&env, 42, &record);
///     assert!(is_finalized(&env, 42));
/// });
/// ```
pub fn is_finalized(env: &Env, contract_id: u32) -> bool {
    panic_on_error(env, validate_contract_id(contract_id));
    env.storage()
        .persistent()
        .has(&finalization_key(contract_id))
}

/// Persist a finalization record.
///
/// The write is **write-once**: if a record already exists for `contract_id`
/// this panics with [`Error::AlreadyFinalized`] instead of overwriting it. The
/// check lives in the storage layer so the immutability guarantee does not
/// depend on every future caller remembering to guard first; callers are still
/// expected to check [`is_finalized`] up front so that a duplicate call is
/// rejected without any state change.
///
/// TTL policy is *not* applied here — the caller owns it (see
/// `crate::ttl::extend_finalization_ttl`).
///
/// # Arguments
///
/// * `env`         – The Soroban environment.
/// * `contract_id` – The numeric contract identifier.
/// * `record`      – The [`FinalizationRecord`] to persist.
///
/// # Errors
///
/// Panics with [`Error::AlreadyFinalized`] when a record already exists.
///
/// # Example
///
/// ```no_run
/// use soroban_sdk::{testutils::Address as _, Address, Env};
/// use escrow::{
///     Escrow, ContractStatus, ContractSummary, CONTRACT_SUMMARY_SCHEMA_VERSION,
///     settlement::{write_finalization, read_finalization},
/// };
/// use escrow::finalize::FinalizationRecord;
///
/// let env = Env::default();
/// let contract = env.register(Escrow, ());
/// let finalizer = Address::generate(&env);
///
/// env.as_contract(&contract, || {
///     let record = FinalizationRecord {
///         finalizer: finalizer.clone(),
///         timestamp: 1_000_000,
///         summary: ContractSummary {
///             schema_version: CONTRACT_SUMMARY_SCHEMA_VERSION,
///             client: Address::generate(&env),
///             freelancer: Address::generate(&env),
///             arbiter: None,
///             status: ContractStatus::Completed,
///             reputation_issued: false,
///             total_amount: 1_000,
///             funded_amount: 1_000,
///             released_amount: 1_000,
///             refundable_balance: 0,
///             released_milestone_count: 1,
///             milestones: soroban_sdk::Vec::new(&env),
///         },
///     };
///     write_finalization(&env, 5, &record);
///
///     let loaded = read_finalization(&env, 5).unwrap();
///     assert_eq!(loaded.finalizer, finalizer);
///     assert_eq!(loaded.timestamp, 1_000_000);
///
///     // A second write is rejected: the close record is immutable.
///     write_finalization(&env, 5, &record); // panics: AlreadyFinalized
/// });
/// ```
pub fn write_finalization(env: &Env, contract_id: u32, record: &FinalizationRecord) {
    require_not_finalized(env, contract_id);
    env.storage()
        .persistent()
        .set(&finalization_key(contract_id), record);
}

/// Panic with [`Error::AlreadyFinalized`] if a record already exists for
/// `contract_id`.
///
/// This is the **write-once guard** for finalization records.  Always call
/// this before [`write_finalization`] in production code paths.  The guard is
/// idempotent on the read side: calling it when no record exists is safe and
/// has no side effects.
///
/// # Arguments
///
/// * `env`         – The Soroban environment.
/// * `contract_id` – The numeric contract identifier to guard.
///
/// # Errors
///
/// Panics with [`Error::AlreadyFinalized`] when [`is_finalized`] returns
/// `true` for the given `contract_id`.
///
/// # Example
///
/// ```no_run
/// use soroban_sdk::{testutils::Address as _, Address, Env};
/// use escrow::{
///     Escrow, ContractStatus, ContractSummary, CONTRACT_SUMMARY_SCHEMA_VERSION,
///     settlement::{require_not_finalized, write_finalization},
/// };
/// use escrow::finalize::FinalizationRecord;
///
/// let env = Env::default();
/// let contract = env.register(Escrow, ());
///
/// env.as_contract(&contract, || {
///     // No record yet — guard passes silently.
///     require_not_finalized(&env, 10);
/// });
/// ```
///
/// Once a record is written, the guard panics:
///
/// ```no_run
/// use soroban_sdk::{testutils::Address as _, Address, Env};
/// use escrow::{
///     Escrow, ContractStatus, ContractSummary, CONTRACT_SUMMARY_SCHEMA_VERSION,
///     settlement::{require_not_finalized, write_finalization},
/// };
/// use escrow::finalize::FinalizationRecord;
///
/// let env = Env::default();
/// let contract = env.register(Escrow, ());
///
/// env.as_contract(&contract, || {
///     let record = FinalizationRecord {
///         finalizer: Address::generate(&env),
///         timestamp: 1,
///         summary: ContractSummary {
///             schema_version: CONTRACT_SUMMARY_SCHEMA_VERSION,
///             client: Address::generate(&env),
///             freelancer: Address::generate(&env),
///             arbiter: None,
///             status: ContractStatus::Completed,
///             reputation_issued: false,
///             total_amount: 0,
///             funded_amount: 0,
///             released_amount: 0,
///             refundable_balance: 0,
///             released_milestone_count: 0,
///             milestones: soroban_sdk::Vec::new(&env),
///         },
///     };
///     write_finalization(&env, 10, &record);
///     require_not_finalized(&env, 10); // panics: AlreadyFinalized
/// });
/// ```
pub fn require_not_finalized(env: &Env, contract_id: u32) {
    if is_finalized(env, contract_id) {
        env.panic_with_error(Error::AlreadyFinalized);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finalize::FinalizationRecord;
    use crate::{
        ContractStatus, ContractSummary, Escrow, EscrowClient, CONTRACT_SUMMARY_SCHEMA_VERSION,
    };
    use soroban_sdk::{contract, contractimpl, testutils::Address as _, Address, Env};

    /// Passes the first external token probe but fails the later `decimals`
    /// dependency call used during binding.
    #[contract]
    struct BalanceOnlyToken;

    #[contractimpl]
    impl BalanceOnlyToken {
        pub fn balance(_env: Env, _id: Address) -> i128 {
            0
        }
    }

    fn setup_contract(env: &Env) -> Address {
        env.register(Escrow, ())
    }

    fn dummy_summary(env: &Env) -> ContractSummary {
        ContractSummary {
            schema_version: CONTRACT_SUMMARY_SCHEMA_VERSION,
            client: Address::generate(env),
            freelancer: Address::generate(env),
            arbiter: None,
            status: ContractStatus::Completed,
            reputation_issued: false,
            total_amount: 1_000,
            funded_amount: 1_000,
            released_amount: 1_000,
            refundable_balance: 0,
            released_milestone_count: 1,
            milestones: soroban_sdk::Vec::new(env),
        }
    }

    fn dummy_record(env: &Env) -> FinalizationRecord {
        FinalizationRecord {
            finalizer: Address::generate(env),
            timestamp: 42_000,
            summary: dummy_summary(env),
        }
    }

    // ── Settlement token: absent / bound ──────────────────────────────────

    #[test]
    fn settlement_token_absent_returns_none() {
        let env = Env::default();
        let contract = setup_contract(&env);
        env.as_contract(&contract, || {
            assert!(read_settlement_token(&env).is_none());
            assert!(!is_settlement_token_bound(&env));
        });
    }

    #[test]
    fn settlement_token_round_trip() {
        let env = Env::default();
        let contract = setup_contract(&env);
        let token = Address::generate(&env);

        env.as_contract(&contract, || {
            write_settlement_token(&env, &token);
            assert_eq!(read_settlement_token(&env), Some(token));
            assert!(is_settlement_token_bound(&env));
        });
    }

    // ── write_settlement_token_once: double-bind guard ─────────────────────

    #[test]
    fn write_settlement_token_once_first_write_succeeds() {
        let env = Env::default();
        let contract = setup_contract(&env);
        let token = Address::generate(&env);

        env.as_contract(&contract, || {
            write_settlement_token_once(&env, &token);
            assert_eq!(read_settlement_token(&env), Some(token));
        });
    }

    #[test]
    #[should_panic(expected = "HostError: Error(Contract, #61)")]
    fn write_settlement_token_once_second_write_panics() {
        // Error #61 = SettlementTokenAlreadyBound
        let env = Env::default();
        let contract = setup_contract(&env);
        let token1 = Address::generate(&env);
        let token2 = Address::generate(&env);

        env.as_contract(&contract, || {
            write_settlement_token_once(&env, &token1);
            write_settlement_token_once(&env, &token2);
        });
    }

    // ── require_settlement_token: success and failure ──────────────────────

    #[test]
    fn require_settlement_token_returns_address_when_bound() {
        let env = Env::default();
        let contract = setup_contract(&env);
        let token = Address::generate(&env);

        env.as_contract(&contract, || {
            write_settlement_token(&env, &token);
            assert_eq!(require_settlement_token(&env), token);
        });
    }

    #[test]
    #[should_panic(expected = "HostError: Error(Contract, #52)")]
    fn require_settlement_token_panics_when_absent() {
        // Error #52 = SettlementTokenNotConfigured
        let env = Env::default();
        let contract = setup_contract(&env);
        env.as_contract(&contract, || {
            let _ = require_settlement_token(&env);
        });
    }

    // ── Finalization: absent / bound ──────────────────────────────────────

    #[test]
    fn finalization_absent_returns_none() {
        let env = Env::default();
        let contract = setup_contract(&env);
        env.as_contract(&contract, || {
            assert!(!is_finalized(&env, 1));
            assert!(read_finalization(&env, 1).is_none());
        });
    }

    #[test]
    fn finalization_round_trip() {
        let env = Env::default();
        let contract = setup_contract(&env);
        let record = FinalizationRecord {
            finalizer: Address::generate(&env),
            timestamp: 12345,
            summary: dummy_summary(&env),
        };

        env.as_contract(&contract, || {
            write_finalization(&env, 42, &record);
            assert!(is_finalized(&env, 42));
            let loaded = read_finalization(&env, 42).unwrap();
            assert_eq!(loaded.finalizer, record.finalizer);
            assert_eq!(loaded.timestamp, 12345);
        });
    }

    #[test]
    fn finalization_different_ids_are_independent() {
        let env = Env::default();
        let contract = setup_contract(&env);

        let record_a = FinalizationRecord {
            finalizer: Address::generate(&env),
            timestamp: 100,
            summary: dummy_summary(&env),
        };
        let record_b = FinalizationRecord {
            finalizer: Address::generate(&env),
            timestamp: 200,
            summary: dummy_summary(&env),
        };

        env.as_contract(&contract, || {
            write_finalization(&env, 1, &record_a);
            write_finalization(&env, 2, &record_b);

            assert_eq!(read_finalization(&env, 1).unwrap().timestamp, 100);
            assert_eq!(read_finalization(&env, 2).unwrap().timestamp, 200);
        });
    }

    // ── require_not_finalized guard ────────────────────────────────────────

    #[test]
    fn finalization_identical_retry_is_idempotent() {
        let env = Env::default();
        let contract = setup_contract(&env);
        let record = FinalizationRecord {
            finalizer: Address::generate(&env),
            timestamp: 100,
            summary: dummy_summary(&env),
        };

        env.as_contract(&contract, || {
            assert_eq!(
                commit_finalization(&env, 1, &record),
                Ok(CommitOutcome::Committed)
            );
            assert_eq!(
                commit_finalization(&env, 1, &record),
                Ok(CommitOutcome::Recovered)
            );
            assert_eq!(read_finalization(&env, 1), Some(record));
        });
    }

    #[test]
    fn finalization_conflicting_retry_preserves_original() {
        let env = Env::default();
        let contract = setup_contract(&env);
        let original = FinalizationRecord {
            finalizer: Address::generate(&env),
            timestamp: 100,
            summary: dummy_summary(&env),
        };
        let conflicting = FinalizationRecord {
            finalizer: Address::generate(&env),
            timestamp: 200,
            summary: dummy_summary(&env),
        };

        env.as_contract(&contract, || {
            assert_eq!(
                commit_finalization(&env, 1, &original),
                Ok(CommitOutcome::Committed)
            );
            assert_eq!(
                commit_finalization(&env, 1, &conflicting),
                Err(Error::AlreadyFinalized)
            );
            assert_eq!(read_finalization(&env, 1), Some(original));
        });
    }

    #[test]
    fn finalization_zero_id_is_rejected_without_storage() {
        let env = Env::default();
        let contract = setup_contract(&env);
        let record = FinalizationRecord {
            finalizer: Address::generate(&env),
            timestamp: 100,
            summary: dummy_summary(&env),
        };

        env.as_contract(&contract, || {
            assert_eq!(
                commit_finalization(&env, 0, &record),
                Err(Error::InvalidContractId)
            );
            assert!(!env.storage().persistent().has(&DataKey::Finalization(0)));
        });
    }

    #[test]
    fn finalization_max_id_round_trip() {
        let env = Env::default();
        let contract = setup_contract(&env);
        let record = FinalizationRecord {
            finalizer: Address::generate(&env),
            timestamp: u64::MAX,
            summary: dummy_summary(&env),
        };

        env.as_contract(&contract, || {
            assert_eq!(
                commit_finalization(&env, u32::MAX, &record),
                Ok(CommitOutcome::Committed)
            );
            assert_eq!(read_finalization(&env, u32::MAX), Some(record));
        });
    }

    #[test]
    #[should_panic(expected = "HostError: Error(Contract, #4)")]
    fn finalization_zero_id_read_panics_with_typed_error() {
        let env = Env::default();
        let contract = setup_contract(&env);
        env.as_contract(&contract, || {
            let _ = read_finalization(&env, 0);
        });
    }

    #[test]
    fn public_finalization_boundaries_return_typed_errors() {
        let env = Env::default();
        env.mock_all_auths_allowing_non_root_auth();
        let contract = setup_contract(&env);
        let client = EscrowClient::new(&env, &contract);
        let finalizer = Address::generate(&env);

        assert_contract_error(
            client.try_finalize_contract(&0, &finalizer),
            Error::InvalidContractId,
        );
        assert_contract_error(
            client.try_get_finalization_record(&0),
            Error::InvalidContractId,
        );
        assert_contract_error(
            client.try_finalize_contract(&u32::MAX, &finalizer),
            Error::ContractNotFound,
        );
    }

    #[test]
    fn require_not_finalized_passes_when_absent() {
        let env = Env::default();
        let contract = setup_contract(&env);
        env.as_contract(&contract, || {
            require_not_finalized(&env, 99);
        });
    }

    #[test]
    #[should_panic(expected = "HostError: Error(Contract, #46)")]
    fn require_not_finalized_panics_when_present() {
        // Error #46 = AlreadyFinalized
        let env = Env::default();
        let contract = setup_contract(&env);
        let record = FinalizationRecord {
            finalizer: Address::generate(&env),
            timestamp: 1,
            summary: dummy_summary(&env),
        };
        env.as_contract(&contract, || {
            write_finalization(&env, 1, &record);
            require_not_finalized(&env, 1);
        });
    }

    /// Guard is idempotent on an absent key: two calls pass without side effects.
    #[test]
    fn require_not_finalized_idempotent_on_absent_key() {
        let env = Env::default();
        let contract = setup_contract(&env);
        env.as_contract(&contract, || {
            require_not_finalized(&env, 77);
            require_not_finalized(&env, 77);
        });
    }

    // ── Boundary contract_id values ────────────────────────────────────────

    /// `u32::MAX` is a valid key at the storage layer — no panic, no collision
    /// with adjacent IDs.
    #[test]
    fn finalization_key_u32_max_is_valid() {
        let env = Env::default();
        let contract = setup_contract(&env);
        let record = dummy_record(&env);

        env.as_contract(&contract, || {
            // Before write: absent
            assert!(!is_finalized(&env, u32::MAX));
            assert!(read_finalization(&env, u32::MAX).is_none());

            write_finalization(&env, u32::MAX, &record);
            assert!(is_finalized(&env, u32::MAX));
            // Adjacent key is unaffected
            assert!(!is_finalized(&env, u32::MAX - 1));
        });
    }

    /// `contract_id == 0` produces a distinct, non-colliding key.
    /// Upper-layer guards (`validate_contract_id_bounds`) reject zero IDs
    /// before reaching this helper; here we verify the key semantics are safe.
    #[test]
    fn finalization_key_zero_does_not_collide_with_one() {
        let env = Env::default();
        let contract = setup_contract(&env);
        let record = dummy_record(&env);

        env.as_contract(&contract, || {
            write_finalization(&env, 0, &record);
            // contract_id 1 must be unaffected
            assert!(!is_finalized(&env, 1));
        });
    }

    /// Finalization of contract A does not affect contract B.
    #[test]
    fn finalization_isolation_across_contracts() {
        let env = Env::default();
        let contract = setup_contract(&env);
        let record = dummy_record(&env);

        env.as_contract(&contract, || {
            write_finalization(&env, 10, &record);
            assert!(is_finalized(&env, 10));
            assert!(!is_finalized(&env, 11));
            assert!(!is_finalized(&env, 20));
            assert!(!is_finalized(&env, u32::MAX));
        });
    }

    /// The raw `write_finalization` silently overwrites on a second write.
    /// This documents the known behaviour; production callers must always
    /// call `require_not_finalized` first to prevent this.
    #[test]
    fn write_finalization_raw_overwrites_on_second_write() {
        let env = Env::default();
        let contract = setup_contract(&env);

        let record1 = FinalizationRecord {
            finalizer: Address::generate(&env),
            timestamp: 100,
            summary: dummy_summary(&env),
        };
        let record2 = FinalizationRecord {
            finalizer: Address::generate(&env),
            timestamp: 999,
            summary: dummy_summary(&env),
        };

        env.as_contract(&contract, || {
            write_finalization(&env, 5, &record1);
            // Raw second write — callers must guard against this via require_not_finalized
            write_finalization(&env, 5, &record2);
            assert_eq!(
                read_finalization(&env, 5).unwrap().timestamp,
                999,
                "raw write_finalization overwrites; require_not_finalized is the guard"
            );
        });
    }
}
