//! Typed storage keys and read/write helpers for settlement entries.
//!
//! This module replaces ad-hoc key construction for settlement-related
//! persistent storage with a single, auditable layer. Every settlement
//! read or write in the contract goes through the helpers defined here,
//! guaranteeing that the correct `DataKey` variant and storage bucket
//! (persistent vs. temporary) are always used.
//!
//! # Storage keys
//!
//! | Entry | `DataKey` variant | Bucket |
//! | --- | --- | --- |
//! | Settlement token address | `SettlementToken` | `persistent()` |
//! | Finalization record | `Finalization(contract_id)` | `persistent()` |
//!
//! # Invariants enforced by this module
//!
//! | Invariant | Enforcement point | Error |
//! | --- | --- | --- |
//! | Token is bound at most once | [`write_settlement_token`] | [`Error::SettlementTokenAlreadyBound`] |
//! | Finalization is written at most once per contract | [`write_finalization`] | [`Error::AlreadyFinalized`] |
//! | `contract_id == 0` is always invalid | all finalization helpers | [`Error::InvalidContractId`] |
//! | Summary accounting is consistent | [`write_finalization`] | [`Error::AccountingInvariantViolated`] |
//! | Settlement-token TTL stays live on money-flow reads | [`require_settlement_token`] | (extends TTL silently) |
//!
//! The first two invariants move enforcement *into* the storage layer rather
//! than relying solely on callers. That means a future caller that accidentally
//! bypasses the entry-point guards still cannot corrupt storage.
//!
//! # Round-trip guarantee
//!
//! Every `write_*` followed by the corresponding `read_*` returns the
//! same value.  The `test_settlement_storage` module in `test/` verifies
//! this invariant plus absent-key behaviour.

use crate::{
    finalize::FinalizationRecord,
    ttl::{PERSISTENT_BUMP_THRESHOLD, PERSISTENT_TTL_LEDGERS},
    DataKey, Error,
};
use soroban_sdk::{Address, Env};

// ── Settlement token ────────────────────────────────────────────────────────

/// Read the bound settlement token address from persistent storage.
///
/// Returns `None` when no token has been bound yet (`bind_settlement_token`
/// has not been called).
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

/// Persist the settlement token address under the canonical storage key.
///
/// # Write-once invariant
///
/// This helper **enforces** write-once semantics internally. A second call
/// — regardless of caller — panics with [`Error::SettlementTokenAlreadyBound`].
/// The entry-point guard in `bind_settlement_token` checks the same condition
/// for a user-visible error message, but this function is the authoritative
/// enforcement point so that any direct call from within the crate is also
/// safe.
///
/// # Arguments
///
/// * `env`   – The Soroban environment.
/// * `token` – The SAC token [`Address`] to bind.
///
/// # Errors
///
/// Panics with [`Error::SettlementTokenAlreadyBound`] if a token is already
/// stored.
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
    // Invariant: write-once. Reject any attempt to overwrite an existing binding.
    // This is the canonical enforcement point — callers MUST NOT bypass it.
    if is_settlement_token_bound(env) {
        env.panic_with_error(Error::SettlementTokenAlreadyBound);
    }
    env.storage()
        .persistent()
        .set(&DataKey::SettlementToken, token);
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
    env.storage().persistent().has(&DataKey::SettlementToken)
}

/// Read the bound settlement token, panicking with [`Error::SettlementTokenNotConfigured`]
/// when absent.  Use this in money-flow paths that require a bound token.
///
/// # TTL extension
///
/// This function extends the `SettlementToken` entry's TTL every time it is
/// called, using the same `PERSISTENT_BUMP_THRESHOLD` / `PERSISTENT_TTL_LEDGERS`
/// policy as contract and milestone storage.  Because the settlement token is
/// accessed on every `deposit_funds`, `release_milestone`, and similar
/// money-flow paths, this ensures the key stays live as long as the contract
/// is actively used, preventing eviction-induced `SettlementTokenNotConfigured`
/// panics after periods of inactivity.
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
    let token = read_settlement_token(env)
        .unwrap_or_else(|| env.panic_with_error(Error::SettlementTokenNotConfigured));

    // Extend TTL on every money-flow read so the key survives long-lived
    // contracts and periods of low activity.  This matches the bump policy
    // used by withdraw_protocol_fees and the contract/milestone helpers.
    env.storage().persistent().extend_ttl(
        &DataKey::SettlementToken,
        PERSISTENT_BUMP_THRESHOLD,
        PERSISTENT_TTL_LEDGERS,
    );

    token
}

// ── Finalization record ─────────────────────────────────────────────────────

/// Validate that `contract_id` is a legal non-zero identifier.
///
/// All finalization helpers call this guard first so that callers cannot
/// silently create or read state under contract ID 0 (which is reserved and
/// never assigned by `create_contract`).
///
/// # Panics
///
/// Panics with [`Error::InvalidContractId`] when `contract_id == 0`.
#[inline]
fn require_valid_contract_id(env: &Env, contract_id: u32) {
    if contract_id == 0 {
        env.panic_with_error(Error::InvalidContractId);
    }
}

/// Construct the canonical [`DataKey`] for a finalization record.
///
/// # Arguments
///
/// * `contract_id` – The numeric contract identifier (must be non-zero).
///
/// # Returns
///
/// `DataKey::Finalization(contract_id)`.
///
/// # Errors
///
/// Panics with [`Error::InvalidContractId`] when `contract_id == 0`.
///
/// # Example
///
/// ```no_run
/// use escrow::{DataKey, settlement::finalization_key};
///
/// let key = finalization_key(7);
/// assert_eq!(key, DataKey::Finalization(7));
/// ```
pub fn finalization_key(env: &Env, contract_id: u32) -> DataKey {
    require_valid_contract_id(env, contract_id);
    DataKey::Finalization(contract_id)
}

/// Read a finalization record for `contract_id`, if it exists.
///
/// # Arguments
///
/// * `env`         – The Soroban environment.
/// * `contract_id` – The numeric contract identifier (must be non-zero).
///
/// # Returns
///
/// `Some(FinalizationRecord)` when the contract has been finalized, `None`
/// otherwise.
///
/// # Errors
///
/// Panics with [`Error::InvalidContractId`] when `contract_id == 0`.
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
    env.storage()
        .persistent()
        .get(&finalization_key(env, contract_id))
}

/// Return `true` when a finalization record already exists for `contract_id`.
///
/// # Arguments
///
/// * `env`         – The Soroban environment.
/// * `contract_id` – The numeric contract identifier (must be non-zero).
///
/// # Returns
///
/// `true` if a [`FinalizationRecord`] is stored for `contract_id`, `false`
/// otherwise.
///
/// # Errors
///
/// Panics with [`Error::InvalidContractId`] when `contract_id == 0`.
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
    env.storage()
        .persistent()
        .has(&finalization_key(env, contract_id))
}

/// Persist a finalization record.
///
/// # Write-once invariant
///
/// This helper **enforces** write-once semantics internally. A second call
/// for the same `contract_id` panics with [`Error::AlreadyFinalized`].  The
/// entry-point guard in `finalize_contract_impl` also checks this condition,
/// but this function is the authoritative enforcement point so that any
/// direct call from within the crate cannot accidentally overwrite an
/// immutable close record.
///
/// # Accounting invariant
///
/// Before writing, this helper validates that the summary's accounting fields
/// are internally consistent:
///
/// - `released_amount + refunded_amount <= funded_amount` — the contract
///   cannot have paid out more than was ever deposited.
/// - `refundable_balance == funded_amount - released_amount - refunded_amount`
///   — the snapshot's computed balance matches its declared balance.
///
/// Either failure panics with [`Error::AccountingInvariantViolated`].
///
/// # Arguments
///
/// * `env`         – The Soroban environment.
/// * `contract_id` – The numeric contract identifier (must be non-zero).
/// * `record`      – The [`FinalizationRecord`] to persist.
///
/// # Errors
///
/// - [`Error::InvalidContractId`] when `contract_id == 0`.
/// - [`Error::AlreadyFinalized`] when a record already exists.
/// - [`Error::AccountingInvariantViolated`] when the summary accounting is
///   inconsistent.
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
/// });
/// ```
pub fn write_finalization(env: &Env, contract_id: u32, record: &FinalizationRecord) {
    // Invariant: write-once. Reject any attempt to overwrite an existing record.
    // This is the canonical enforcement point.
    if is_finalized(env, contract_id) {
        env.panic_with_error(Error::AlreadyFinalized);
    }

    // Invariant: accounting consistency.
    //
    // `ContractSummary` stores three of the four accounting fields:
    //   funded_amount, released_amount, refundable_balance
    // The fourth — refunded_amount — is implicit:
    //   implied_refunded = funded_amount - released_amount - refundable_balance
    //
    // We validate the following derived invariants:
    //
    // 1. `released_amount` is non-negative.
    // 2. `refundable_balance` is non-negative.
    // 3. `released_amount + refundable_balance <= funded_amount`
    //    (which also guarantees implied_refunded >= 0 without i128 underflow).
    if record.summary.released_amount < 0 {
        env.panic_with_error(Error::AccountingInvariantViolated);
    }
    if record.summary.refundable_balance < 0 {
        env.panic_with_error(Error::AccountingInvariantViolated);
    }
    let total_accounted = record
        .summary
        .released_amount
        .checked_add(record.summary.refundable_balance)
        .unwrap_or_else(|| env.panic_with_error(Error::AccountingInvariantViolated));
    if total_accounted > record.summary.funded_amount {
        env.panic_with_error(Error::AccountingInvariantViolated);
    }

    env.storage()
        .persistent()
        .set(&finalization_key(env, contract_id), record);
}

/// Panic with [`Error::AlreadyFinalized`] if a record already exists for
/// `contract_id`.
///
/// # Arguments
///
/// * `env`         – The Soroban environment.
/// * `contract_id` – The numeric contract identifier to guard (must be non-zero).
///
/// # Errors
///
/// - [`Error::InvalidContractId`] when `contract_id == 0`.
/// - [`Error::AlreadyFinalized`] when [`is_finalized`] returns `true` for the
///   given `contract_id`.
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
    // require_valid_contract_id is called inside finalization_key, which
    // is called inside is_finalized — no explicit call needed here.
    if is_finalized(env, contract_id) {
        env.panic_with_error(Error::AlreadyFinalized);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finalize::FinalizationRecord;
    use crate::{ContractStatus, ContractSummary, Escrow, CONTRACT_SUMMARY_SCHEMA_VERSION};
    use soroban_sdk::{testutils::Address as _, Address, Env};

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

    // ── Settlement token round-trip ────────────────────────────────────────

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

    // ── Task 1: write_settlement_token write-once guard ────────────────────

    /// Calling `write_settlement_token` a second time must panic with
    /// `SettlementTokenAlreadyBound`, regardless of whether the new token
    /// address is the same as or different from the already-bound one.
    #[test]
    #[should_panic(expected = "HostError: Error(Contract, #61)")]
    fn write_settlement_token_double_write_same_address_panics() {
        let env = Env::default();
        let contract = setup_contract(&env);
        let token = Address::generate(&env);

        env.as_contract(&contract, || {
            write_settlement_token(&env, &token);
            // Second call with the same address must be rejected.
            write_settlement_token(&env, &token);
        });
    }

    #[test]
    #[should_panic(expected = "HostError: Error(Contract, #61)")]
    fn write_settlement_token_double_write_different_address_panics() {
        let env = Env::default();
        let contract = setup_contract(&env);
        let token_a = Address::generate(&env);
        let token_b = Address::generate(&env);

        env.as_contract(&contract, || {
            write_settlement_token(&env, &token_a);
            // Re-bind with a different token must also be rejected.
            write_settlement_token(&env, &token_b);
        });
    }

    /// A single call succeeds; the bound address can be read back.
    #[test]
    fn write_settlement_token_first_write_succeeds() {
        let env = Env::default();
        let contract = setup_contract(&env);
        let token = Address::generate(&env);

        env.as_contract(&contract, || {
            write_settlement_token(&env, &token);
            assert_eq!(read_settlement_token(&env), Some(token.clone()));
        });
    }

    // ── Task 4: require_settlement_token TTL bump ──────────────────────────

    /// Calling `require_settlement_token` after a successful bind returns the
    /// correct address (the TTL bump must not break the read).
    #[test]
    fn require_settlement_token_returns_bound_address() {
        let env = Env::default();
        let contract = setup_contract(&env);
        let token = Address::generate(&env);

        env.as_contract(&contract, || {
            write_settlement_token(&env, &token);
            let returned = require_settlement_token(&env);
            assert_eq!(returned, token);
        });
    }

    /// `require_settlement_token` can be called multiple times safely; each
    /// call extends the TTL without error.
    #[test]
    fn require_settlement_token_repeated_reads_succeed() {
        let env = Env::default();
        let contract = setup_contract(&env);
        let token = Address::generate(&env);

        env.as_contract(&contract, || {
            write_settlement_token(&env, &token);
            let t1 = require_settlement_token(&env);
            let t2 = require_settlement_token(&env);
            assert_eq!(t1, t2);
        });
    }

    #[test]
    #[should_panic(expected = "HostError: Error(Contract, #52)")]
    fn require_settlement_token_absent_panics() {
        let env = Env::default();
        let contract = setup_contract(&env);
        env.as_contract(&contract, || {
            let _ = require_settlement_token(&env);
        });
    }

    // ── Finalization round-trip ────────────────────────────────────────────

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

    // ── Task 2: write_finalization write-once guard ────────────────────────

    /// Writing a finalization record a second time for the same contract ID
    /// must panic with `AlreadyFinalized` (#46).
    #[test]
    #[should_panic(expected = "HostError: Error(Contract, #46)")]
    fn write_finalization_double_write_panics() {
        let env = Env::default();
        let contract = setup_contract(&env);
        let record = FinalizationRecord {
            finalizer: Address::generate(&env),
            timestamp: 100,
            summary: dummy_summary(&env),
        };
        let record2 = FinalizationRecord {
            finalizer: Address::generate(&env),
            timestamp: 200,
            summary: dummy_summary(&env),
        };

        env.as_contract(&contract, || {
            write_finalization(&env, 7, &record);
            // Second write must be rejected even with a different record.
            write_finalization(&env, 7, &record2);
        });
    }

    /// Double-write on one contract ID must not affect a distinct contract ID.
    #[test]
    #[should_panic(expected = "HostError: Error(Contract, #46)")]
    fn write_finalization_double_write_does_not_corrupt_sibling() {
        let env = Env::default();
        let contract = setup_contract(&env);
        let record = FinalizationRecord {
            finalizer: Address::generate(&env),
            timestamp: 100,
            summary: dummy_summary(&env),
        };

        env.as_contract(&contract, || {
            // Write contract 8 once — must succeed.
            write_finalization(&env, 8, &record.clone());
            // Write contract 9 once — must succeed.
            write_finalization(&env, 9, &record.clone());
            // Second write to contract 8 must panic.
            write_finalization(&env, 8, &record);
        });
    }

    // ── Task 3: contract_id == 0 rejection ────────────────────────────────

    /// `finalization_key` must panic with `InvalidContractId` (#4) for id 0.
    #[test]
    #[should_panic(expected = "HostError: Error(Contract, #4)")]
    fn finalization_key_zero_panics() {
        let env = Env::default();
        let contract = setup_contract(&env);
        env.as_contract(&contract, || {
            let _ = finalization_key(&env, 0);
        });
    }

    /// `is_finalized` must reject id 0.
    #[test]
    #[should_panic(expected = "HostError: Error(Contract, #4)")]
    fn is_finalized_zero_panics() {
        let env = Env::default();
        let contract = setup_contract(&env);
        env.as_contract(&contract, || {
            let _ = is_finalized(&env, 0);
        });
    }

    /// `read_finalization` must reject id 0.
    #[test]
    #[should_panic(expected = "HostError: Error(Contract, #4)")]
    fn read_finalization_zero_panics() {
        let env = Env::default();
        let contract = setup_contract(&env);
        env.as_contract(&contract, || {
            let _ = read_finalization(&env, 0);
        });
    }

    /// `write_finalization` must reject id 0.
    #[test]
    #[should_panic(expected = "HostError: Error(Contract, #4)")]
    fn write_finalization_zero_panics() {
        let env = Env::default();
        let contract = setup_contract(&env);
        let record = FinalizationRecord {
            finalizer: Address::generate(&env),
            timestamp: 1,
            summary: dummy_summary(&env),
        };
        env.as_contract(&contract, || {
            write_finalization(&env, 0, &record);
        });
    }

    /// `require_not_finalized` must reject id 0.
    #[test]
    #[should_panic(expected = "HostError: Error(Contract, #4)")]
    fn require_not_finalized_zero_panics() {
        let env = Env::default();
        let contract = setup_contract(&env);
        env.as_contract(&contract, || {
            require_not_finalized(&env, 0);
        });
    }

    /// Boundary: contract_id = 1 (minimum valid) is accepted.
    #[test]
    fn finalization_key_minimum_valid_id_accepted() {
        let env = Env::default();
        let contract = setup_contract(&env);
        env.as_contract(&contract, || {
            let key = finalization_key(&env, 1);
            assert_eq!(key, DataKey::Finalization(1));
        });
    }

    /// Boundary: contract_id = u32::MAX is accepted.
    #[test]
    fn finalization_key_max_id_accepted() {
        let env = Env::default();
        let contract = setup_contract(&env);
        env.as_contract(&contract, || {
            let key = finalization_key(&env, u32::MAX);
            assert_eq!(key, DataKey::Finalization(u32::MAX));
        });
    }

    // ── Task 5: accounting invariant validation ────────────────────────────
    //
    // ContractSummary stores: funded_amount, released_amount, refundable_balance.
    // Invariants checked by write_finalization:
    //   (a) released_amount >= 0
    //   (b) refundable_balance >= 0
    //   (c) released_amount + refundable_balance <= funded_amount
    //       (implies the implicit refunded portion is also >= 0)

    /// `write_finalization` must reject a summary where
    /// `released_amount > funded_amount`.
    #[test]
    #[should_panic(expected = "HostError: Error(Contract, #44)")]
    fn write_finalization_rejects_over_released() {
        let env = Env::default();
        let contract = setup_contract(&env);

        // released_amount (600) > funded_amount (500) — violates invariant (c)
        let bad_summary = ContractSummary {
            schema_version: CONTRACT_SUMMARY_SCHEMA_VERSION,
            client: Address::generate(&env),
            freelancer: Address::generate(&env),
            arbiter: None,
            status: ContractStatus::Completed,
            reputation_issued: false,
            total_amount: 500,
            funded_amount: 500,
            released_amount: 600, // ← exceeds funded_amount
            refundable_balance: 0,
            released_milestone_count: 1,
            milestones: soroban_sdk::Vec::new(&env),
        };
        let record = FinalizationRecord {
            finalizer: Address::generate(&env),
            timestamp: 1,
            summary: bad_summary,
        };

        env.as_contract(&contract, || {
            write_finalization(&env, 3, &record);
        });
    }

    /// `write_finalization` must reject a summary where
    /// `released_amount + refundable_balance > funded_amount`.
    #[test]
    #[should_panic(expected = "HostError: Error(Contract, #44)")]
    fn write_finalization_rejects_over_accounted() {
        let env = Env::default();
        let contract = setup_contract(&env);

        // released(300) + refundable(250) = 550 > funded(500) — violates (c)
        let bad_summary = ContractSummary {
            schema_version: CONTRACT_SUMMARY_SCHEMA_VERSION,
            client: Address::generate(&env),
            freelancer: Address::generate(&env),
            arbiter: None,
            status: ContractStatus::Completed,
            reputation_issued: false,
            total_amount: 500,
            funded_amount: 500,
            released_amount: 300,
            refundable_balance: 250, // 300 + 250 = 550 > 500
            released_milestone_count: 1,
            milestones: soroban_sdk::Vec::new(&env),
        };
        let record = FinalizationRecord {
            finalizer: Address::generate(&env),
            timestamp: 1,
            summary: bad_summary,
        };

        env.as_contract(&contract, || {
            write_finalization(&env, 4, &record);
        });
    }

    /// `write_finalization` must reject a negative `refundable_balance`.
    #[test]
    #[should_panic(expected = "HostError: Error(Contract, #44)")]
    fn write_finalization_rejects_negative_refundable_balance() {
        let env = Env::default();
        let contract = setup_contract(&env);

        // refundable_balance = -1 violates invariant (b)
        let bad_summary = ContractSummary {
            schema_version: CONTRACT_SUMMARY_SCHEMA_VERSION,
            client: Address::generate(&env),
            freelancer: Address::generate(&env),
            arbiter: None,
            status: ContractStatus::Completed,
            reputation_issued: false,
            total_amount: 1_000,
            funded_amount: 1_000,
            released_amount: 500,
            refundable_balance: -1, // ← negative
            released_milestone_count: 1,
            milestones: soroban_sdk::Vec::new(&env),
        };
        let record = FinalizationRecord {
            finalizer: Address::generate(&env),
            timestamp: 1,
            summary: bad_summary,
        };

        env.as_contract(&contract, || {
            write_finalization(&env, 5, &record);
        });
    }

    /// `write_finalization` must accept a summary with partial release where
    /// `refundable_balance > 0` and the invariant holds.
    ///
    /// funded=1000, released=300, refundable=500 → implicit_refunded=200 ✓
    #[test]
    fn write_finalization_accepts_partial_release_with_correct_balance() {
        let env = Env::default();
        let contract = setup_contract(&env);

        let good_summary = ContractSummary {
            schema_version: CONTRACT_SUMMARY_SCHEMA_VERSION,
            client: Address::generate(&env),
            freelancer: Address::generate(&env),
            arbiter: None,
            status: ContractStatus::Disputed,
            reputation_issued: false,
            total_amount: 1_000,
            funded_amount: 1_000,
            released_amount: 300,
            refundable_balance: 500, // 300 + 500 = 800 <= 1000 ✓
            released_milestone_count: 1,
            milestones: soroban_sdk::Vec::new(&env),
        };
        let record = FinalizationRecord {
            finalizer: Address::generate(&env),
            timestamp: 42,
            summary: good_summary,
        };

        env.as_contract(&contract, || {
            write_finalization(&env, 6, &record);
            assert!(is_finalized(&env, 6));
        });
    }

    /// Zero-value summary is valid: 0 + 0 = 0 <= 0.
    #[test]
    fn write_finalization_accepts_zero_value_summary() {
        let env = Env::default();
        let contract = setup_contract(&env);

        let zero_summary = ContractSummary {
            schema_version: CONTRACT_SUMMARY_SCHEMA_VERSION,
            client: Address::generate(&env),
            freelancer: Address::generate(&env),
            arbiter: None,
            status: ContractStatus::Completed,
            reputation_issued: false,
            total_amount: 0,
            funded_amount: 0,
            released_amount: 0,
            refundable_balance: 0,
            released_milestone_count: 0,
            milestones: soroban_sdk::Vec::new(&env),
        };
        let record = FinalizationRecord {
            finalizer: Address::generate(&env),
            timestamp: 1,
            summary: zero_summary,
        };

        env.as_contract(&contract, || {
            write_finalization(&env, 11, &record);
            assert!(is_finalized(&env, 11));
        });
    }
}
