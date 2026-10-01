//! Regression tests for concurrent, repeated and boundary execution of
//! `finalize_contract` / `get_finalization_record`.
//!
//! These tests pin the finalization invariants documented in
//! [`crate::finalize`]:
//!
//! | # | Invariant                                      | Tests                                                     |
//! |---|------------------------------------------------|-----------------------------------------------------------|
//! | 1 | write-once close record                        | `write_finalization_is_write_once`, `write_finalization_rejects_a_duplicate_write` |
//! | 2 | first writer wins, losers have no side effects  | `racing_finalizers_never_overwrite_the_record`, `retry_is_idempotent_and_publishes_no_extra_event`, `unauthorized_racing_finalizer_leaves_no_trace` |
//! | 3 | the guard outlives the entries it protects     | `record_ttl_is_set_on_write`, `guard_outlives_contract_entry_after_a_racing_mutation_attempt`, `guard_survives_past_its_initial_expiry`, `reading_the_record_renews_the_guard` |
//! | 4 | no reentrancy surface                          | follows from 1-3 (single terminal write claim)             |
//! | 5 | freeze gate and authorization precede mutation | `pause_blocks_the_first_finalization`, `emergency_blocks_the_first_finalization`, `retry_while_paused_still_reports_already_finalized`, `disputed_finalization_clears_rollback_once` |
//! | 6 | checked accounting in the snapshot             | `summary_snapshot_matches_contract_state`, `finalize_rejects_inconsistent_accounting` |
//!
//! Every fixture below binds a real settlement token, so the money-flow paths
//! that drive a contract to `Completed`/`Disputed` are exercised for real.
//!
//! Note on event assertions: `env.events().all()` reports the contract events
//! of the *most recent* invocation only, so every event assertion below runs
//! immediately after the invocation it is about, with no intervening call.

use soroban_sdk::{
    testutils::{storage::Persistent as _, Address as _, Events, Ledger as _},
    token::StellarAssetClient,
    vec, Address, Env, Symbol, TryIntoVal, Vec,
};

use super::{
    assert_contract_error, complete_contract_funded, default_milestones,
    register_client_with_token, total_milestone_amount,
};
use crate::{
    finalize::FinalizationRecord,
    settlement::{is_finalized as storage_is_finalized, read_finalization, write_finalization},
    ttl, Contract, ContractStatus, ContractSummary, DataKey, Error, EscrowClient, EscrowError,
    MilestoneSummary, ReleaseAuthorization, CONTRACT_SUMMARY_SCHEMA_VERSION,
};

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Ledger configuration used by the TTL boundary tests.
///
/// `min_persistent_entry_ttl` is deliberately kept *below*
/// [`ttl::PERSISTENT_BUMP_THRESHOLD`] (mirroring networks where a freshly
/// written persistent entry is short lived) so that an entry which is never
/// explicitly renewed really does expire, and the renewal performed by the
/// contract is observable.
fn ttl_env() -> Env {
    let env = Env::default();
    env.ledger().with_mut(|li| {
        li.max_entry_ttl = ttl::PERSISTENT_TTL_LEDGERS * 2;
        li.min_persistent_entry_ttl = ttl::PERSISTENT_BUMP_THRESHOLD - 1;
        li.sequence_number = 1_000;
    });
    env.mock_all_auths();
    env
}

/// An initialized escrow bound to a real settlement token.
fn escrow_with_token(env: &Env) -> (EscrowClient<'_>, Address) {
    register_client_with_token(env)
}

/// Create, fund and fully release a 3-milestone contract.
fn completed_contract(env: &Env) -> (EscrowClient<'_>, Address, Address, Address, u32) {
    let (escrow, token) = escrow_with_token(env);
    let (client_addr, freelancer_addr, contract_id) =
        complete_contract_funded(env, &escrow, &token);
    assert_eq!(
        escrow.get_contract(&contract_id).status,
        ContractStatus::Completed
    );
    (escrow, client_addr, freelancer_addr, token, contract_id)
}

/// Create, fund and dispute a 3-milestone contract that has an arbiter.
fn disputed_contract(env: &Env) -> (EscrowClient<'_>, Address, Address, u32) {
    let (escrow, token) = escrow_with_token(env);
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);

    let contract_id = escrow.create_contract(
        &client_addr,
        &freelancer_addr,
        &Some(arbiter_addr),
        &default_milestones(env),
        &ReleaseAuthorization::ClientOnly,
    );
    StellarAssetClient::new(env, &token).mint(&client_addr, &total_milestone_amount());
    assert!(escrow.deposit_funds(&contract_id, &client_addr, &total_milestone_amount()));
    assert!(escrow.raise_dispute(&contract_id, &client_addr));
    assert_eq!(
        escrow.get_contract(&contract_id).status,
        ContractStatus::Disputed
    );
    (escrow, client_addr, freelancer_addr, contract_id)
}

/// Number of `finalized` events published for `contract_id` by the most recent
/// invocation.
fn finalized_event_count(env: &Env, escrow_address: &Address, contract_id: u32) -> usize {
    let topic = Symbol::new(env, "finalized");
    let mut count = 0;
    for (addr, topics, _data) in env.events().all().iter() {
        if &addr != escrow_address || topics.len() != 2 {
            continue;
        }
        let event_topic: Symbol = topics.get(0).unwrap().try_into_val(env).unwrap();
        let event_contract: u32 = topics.get(1).unwrap().try_into_val(env).unwrap();
        if event_topic == topic && event_contract == contract_id {
            count += 1;
        }
    }
    count
}

fn record_ttl(env: &Env, escrow_address: &Address, contract_id: u32) -> u32 {
    env.as_contract(escrow_address, || {
        env.storage()
            .persistent()
            .get_ttl(&DataKey::Finalization(contract_id))
    })
}

fn contract_entry_ttl(env: &Env, escrow_address: &Address, contract_id: u32) -> u32 {
    env.as_contract(escrow_address, || {
        env.storage()
            .persistent()
            .get_ttl(&DataKey::Contract(contract_id))
    })
}

/// Contract-wide persistent keys written once at setup time.
const CONTROL_KEYS: &[DataKey] = &[
    DataKey::Initialized,
    DataKey::Admin,
    DataKey::PauseScope,
    DataKey::Emergency,
    DataKey::SettlementToken,
    DataKey::TokenScale,
    DataKey::ProtocolParameters,
    DataKey::ProtocolFeeBps,
    DataKey::GovernedParameters,
    DataKey::ContractsParameters,
    DataKey::MaxSettlement,
    DataKey::ReadinessChecklist,
    DataKey::SchemaVersion,
];

/// Ledger jump used to bring a freshly written 30-day entry back into the
/// 7-day bump window, where `extend_ttl` is allowed to renew it again.
const ENTER_BUMP_WINDOW: u32 = ttl::PERSISTENT_TTL_LEDGERS - ttl::PERSISTENT_BUMP_THRESHOLD + 1;

/// Advance the ledger by `by` ledgers.
///
/// Contract-wide keys (instance storage plus the pause/admin/token config) are
/// armed first: they are written once with a bare `set` and the contract never
/// renews them, so without arming them they would be archived mid-test and
/// every entrypoint would fail for reasons unrelated to this module.
fn advance(env: &Env, escrow_address: &Address, by: u32) {
    let max = ttl::PERSISTENT_TTL_LEDGERS * 2;
    env.as_contract(escrow_address, || {
        env.storage().instance().extend_ttl(max, max);
        for key in CONTROL_KEYS {
            if env.storage().persistent().has(key) {
                env.storage().persistent().extend_ttl(key, max, max);
            }
        }
    });
    env.ledger()
        .set_sequence_number(env.ledger().sequence().saturating_add(by));
}

fn milestone_summary(index: u32, amount: i128, released: bool, refunded: bool) -> MilestoneSummary {
    MilestoneSummary {
        index,
        amount,
        released,
        refunded,
    }
}

fn sample_record(env: &Env) -> FinalizationRecord {
    FinalizationRecord {
        finalizer: Address::generate(env),
        timestamp: 42,
        summary: ContractSummary {
            schema_version: CONTRACT_SUMMARY_SCHEMA_VERSION,
            client: Address::generate(env),
            freelancer: Address::generate(env),
            arbiter: None,
            status: ContractStatus::Completed,
            reputation_issued: false,
            total_amount: 100,
            funded_amount: 100,
            released_amount: 100,
            refundable_balance: 0,
            released_milestone_count: 1,
            milestones: Vec::new(env),
        },
    }
}

// ── Invariant 1 & 2: write-once, first writer wins ──────────────────────────

/// Two different finalizers racing the same contract: the first one to land
/// wins, the record is never overwritten, and only one event is published.
#[test]
fn racing_finalizers_never_overwrite_the_record() {
    let env = ttl_env();
    let (escrow, client_addr, freelancer_addr, token, contract_id) = completed_contract(&env);

    // First writer wins.
    env.ledger().with_mut(|li| li.timestamp = 1_000);
    assert!(escrow.finalize_contract(&contract_id, &client_addr));
    assert_eq!(
        finalized_event_count(&env, &escrow.address, contract_id),
        1,
        "the winning call publishes exactly one `finalized` event"
    );

    // Everyone else loses with a typed, side-effect free error.
    assert_contract_error(
        escrow.try_finalize_contract(&contract_id, &freelancer_addr),
        Error::AlreadyFinalized,
    );
    assert_eq!(
        env.events().all().len(),
        0,
        "a rejected racing attempt must not publish any event"
    );

    // A second, later attempt by the winning finalizer is rejected as well.
    env.ledger().with_mut(|li| li.timestamp = 2_000);
    assert_contract_error(
        escrow.try_finalize_contract(&contract_id, &client_addr),
        Error::AlreadyFinalized,
    );
    assert_eq!(env.events().all().len(), 0);

    let record = escrow
        .get_finalization_record(&contract_id)
        .expect("record must survive losing races");
    assert_eq!(record.finalizer, client_addr, "first writer must win");
    assert_eq!(
        record.timestamp, 1_000,
        "the winning timestamp must not be replaced by a later attempt"
    );

    // No custody moved while the race was being resolved.
    let balance = soroban_sdk::token::TokenClient::new(&env, &token).balance(&escrow.address);
    assert_eq!(balance, 0);
}

/// Retrying a finalization is idempotent: the observable state after N retries
/// equals the state after the first successful call.
#[test]
fn retry_is_idempotent_and_publishes_no_extra_event() {
    let env = ttl_env();
    let (escrow, client_addr, _freelancer_addr, _token, contract_id) = completed_contract(&env);

    assert!(escrow.finalize_contract(&contract_id, &client_addr));
    assert_eq!(finalized_event_count(&env, &escrow.address, contract_id), 1);
    let first = escrow.get_finalization_record(&contract_id).unwrap();

    for _ in 0..3 {
        assert_contract_error(
            escrow.try_finalize_contract(&contract_id, &client_addr),
            Error::AlreadyFinalized,
        );
    }
    assert_eq!(
        env.events().all().len(),
        0,
        "retries must not publish any event"
    );
    assert_eq!(
        escrow.get_finalization_record(&contract_id).unwrap(),
        first,
        "retries must be no-ops"
    );
}

/// A finalizer who is not a contract participant loses the race without leaving
/// any trace: no record, no event.
#[test]
fn unauthorized_racing_finalizer_leaves_no_trace() {
    let env = ttl_env();
    let (escrow, _client_addr, _freelancer_addr, _token, contract_id) = completed_contract(&env);

    let outsider = Address::generate(&env);
    assert_contract_error(
        escrow.try_finalize_contract(&contract_id, &outsider),
        Error::UnauthorizedRole,
    );
    assert_eq!(env.events().all().len(), 0);
    assert!(escrow.get_finalization_record(&contract_id).is_none());

    // The legitimate close still works afterwards.
    let client_addr = escrow.get_contract(&contract_id).client;
    assert!(escrow.finalize_contract(&contract_id, &client_addr));
    assert_eq!(finalized_event_count(&env, &escrow.address, contract_id), 1);
}

/// The storage layer accepts the first write and reports the contract as
/// finalized.
#[test]
fn write_finalization_is_write_once() {
    let env = Env::default();
    let escrow_address = env.register(crate::Escrow, ());
    let record = sample_record(&env);

    env.as_contract(&escrow_address, || {
        write_finalization(&env, 1, &record);
        assert!(storage_is_finalized(&env, 1));
        assert_eq!(read_finalization(&env, 1), Some(record));
    });
}

/// ...and fails closed on any second write for the same contract id, so no
/// caller can silently discard an existing close record.
#[test]
#[should_panic(expected = "HostError: Error(Contract, #46)")]
fn write_finalization_rejects_a_duplicate_write() {
    let env = Env::default();
    let escrow_address = env.register(crate::Escrow, ());
    let record = sample_record(&env);
    let mut other = sample_record(&env);
    other.timestamp = 99;
    other.finalizer = Address::generate(&env);

    env.as_contract(&escrow_address, || {
        write_finalization(&env, 1, &record);
        write_finalization(&env, 1, &other);
    });
}

// ── Invariant 3: the guard is durable ───────────────────────────────────────

/// Writing the record gives it the full persistent window instead of the bare
/// minimum a `set()` would receive.
#[test]
fn record_ttl_is_set_on_write() {
    let env = ttl_env();
    let (escrow, client_addr, _freelancer_addr, _token, contract_id) = completed_contract(&env);

    assert!(escrow.finalize_contract(&contract_id, &client_addr));

    let record_window = record_ttl(&env, &escrow.address, contract_id);
    assert_eq!(
        record_window,
        ttl::PERSISTENT_TTL_LEDGERS,
        "the close record must get the full persistent window on write"
    );
    assert!(record_window >= contract_entry_ttl(&env, &escrow.address, contract_id));
}

/// A read of the contract entry (the one every indexer performs) must not be
/// able to keep the contract alive longer than the guard that protects it.
#[test]
fn guard_outlives_contract_entry_after_a_racing_mutation_attempt() {
    let env = ttl_env();
    let (escrow, client_addr, _freelancer_addr, _token, contract_id) = completed_contract(&env);

    assert!(escrow.finalize_contract(&contract_id, &client_addr));

    // Move both entries into the bump window, just short of expiry.
    advance(&env, &escrow.address, ENTER_BUMP_WINDOW);
    let remaining = record_ttl(&env, &escrow.address, contract_id);
    assert!(
        remaining <= ttl::PERSISTENT_BUMP_THRESHOLD,
        "the test must start inside the bump window, got {remaining} ledgers left"
    );

    // A rejected retry still consults the guard and renews it, while a plain
    // read renews the contract entry. The guard must be renewed with it.
    assert_contract_error(
        escrow.try_finalize_contract(&contract_id, &client_addr),
        Error::AlreadyFinalized,
    );
    assert_eq!(env.events().all().len(), 0);
    let _summary = escrow.get_contract_summary(&contract_id);

    let guard = record_ttl(&env, &escrow.address, contract_id);
    let contract_entry = contract_entry_ttl(&env, &escrow.address, contract_id);
    assert!(
        guard >= contract_entry,
        "finalization guard ({guard} ledgers) must never expire before the contract entry ({contract_entry} ledgers)"
    );
}

/// Boundary: once the *original* record expiry has passed, a repeated
/// finalization is still rejected and the record is still readable. Before the
/// guard was renewed on write, the record was evicted at the bare minimum TTL
/// while the contract entry was still live, and the same contract could have
/// been finalized a second time.
#[test]
fn guard_survives_past_its_initial_expiry() {
    let env = ttl_env();
    let (escrow, client_addr, _freelancer_addr, _token, contract_id) = completed_contract(&env);

    assert!(escrow.finalize_contract(&contract_id, &client_addr));
    let original = escrow.get_finalization_record(&contract_id).unwrap();
    let mut min_window = 0_u32;
    env.ledger()
        .with_mut(|li| min_window = li.min_persistent_entry_ttl);

    // Well past the TTL a bare `set()` would have produced, but still inside
    // the window the contract renews its entries on access.
    advance(&env, &escrow.address, ttl::PERSISTENT_TTL_LEDGERS / 2);
    assert!(
        min_window < ttl::PERSISTENT_BUMP_THRESHOLD,
        "the bare-minimum entry TTL must be shorter than the renewal window"
    );

    // The contract is still readable and still closed.
    let _ = escrow.get_contract(&contract_id);
    assert_contract_error(
        escrow.try_finalize_contract(&contract_id, &client_addr),
        Error::AlreadyFinalized,
    );
    assert_eq!(
        escrow.get_finalization_record(&contract_id).unwrap(),
        original
    );
}

/// Reading the record (bump-on-read) keeps the guard alive for consumers that
/// only poll `get_finalization_record`.
#[test]
fn reading_the_record_renews_the_guard() {
    let env = ttl_env();
    let (escrow, client_addr, _freelancer_addr, _token, contract_id) = completed_contract(&env);

    assert!(escrow.finalize_contract(&contract_id, &client_addr));

    advance(&env, &escrow.address, ENTER_BUMP_WINDOW);
    let window_before_read = record_ttl(&env, &escrow.address, contract_id);
    assert!(escrow.get_finalization_record(&contract_id).is_some());

    assert!(
        record_ttl(&env, &escrow.address, contract_id) > window_before_read,
        "reading the record must renew the guard"
    );
}

// ── Invariant 5: freeze gate and authorization ──────────────────────────────

/// A paused escrow cannot be closed for the first time.
#[test]
fn pause_blocks_the_first_finalization() {
    let env = ttl_env();
    let (escrow, client_addr, _freelancer_addr, _token, contract_id) = completed_contract(&env);

    assert!(escrow.pause(&1_u64));
    assert_contract_error(
        escrow.try_finalize_contract(&contract_id, &client_addr),
        Error::ContractPaused,
    );
    assert_eq!(env.events().all().len(), 0);
    assert!(escrow.get_finalization_record(&contract_id).is_none());
}

/// An engaged emergency stop cannot be closed for the first time.
#[test]
fn emergency_blocks_the_first_finalization() {
    let env = ttl_env();
    let (escrow, client_addr, _freelancer_addr, _token, contract_id) = completed_contract(&env);

    // Engage the emergency flag on its own; `activate_emergency_pause` also
    // sets the pause flag, which would mask the emergency branch.
    env.as_contract(&escrow.address, || {
        env.storage().persistent().set(&DataKey::Emergency, &true);
    });
    assert_contract_error(
        escrow.try_finalize_contract(&contract_id, &client_addr),
        Error::EmergencyActive,
    );
    assert_eq!(env.events().all().len(), 0);
    assert!(escrow.get_finalization_record(&contract_id).is_none());
}

/// A retry of an already-finalized contract reports `AlreadyFinalized` even
/// while the escrow is frozen: the duplicate-work guard is evaluated first, so
/// the outcome of a retry never depends on unrelated global state.
#[test]
fn retry_while_paused_still_reports_already_finalized() {
    let env = ttl_env();
    let (escrow, client_addr, _freelancer_addr, _token, contract_id) = completed_contract(&env);

    assert!(escrow.finalize_contract(&contract_id, &client_addr));
    assert!(escrow.pause(&1_u64));

    assert_contract_error(
        escrow.try_finalize_contract(&contract_id, &client_addr),
        Error::AlreadyFinalized,
    );
    assert_eq!(env.events().all().len(), 0);
    assert!(escrow.get_finalization_record(&contract_id).is_some());
}

/// Finalizing a disputed contract clears the rollback record exactly once; the
/// cleared record cannot be resurrected by a repeated call.
#[test]
fn disputed_finalization_clears_rollback_once() {
    let env = ttl_env();
    let (escrow, client_addr, _freelancer_addr, contract_id) = disputed_contract(&env);

    assert!(escrow.finalize_contract(&contract_id, &client_addr));
    assert_eq!(
        escrow
            .get_finalization_record(&contract_id)
            .unwrap()
            .summary
            .status,
        ContractStatus::Disputed,
        "a disputed contract snapshots as disputed"
    );

    // Rollback is no longer reachable once the contract is closed.
    assert_contract_error(
        escrow.try_rollback_dispute(&contract_id),
        Error::AlreadyFinalized,
    );
    assert_contract_error(
        escrow.try_finalize_contract(&contract_id, &client_addr),
        Error::AlreadyFinalized,
    );
    assert_eq!(env.events().all().len(), 0);
}

// ── Invariant 6: checked accounting in the snapshot ─────────────────────────

/// The snapshot agrees with the live contract state it was taken from.
#[test]
fn summary_snapshot_matches_contract_state() {
    let env = ttl_env();
    let (escrow, client_addr, _freelancer_addr, _token, contract_id) = completed_contract(&env);

    let contract = escrow.get_contract(&contract_id);
    let milestones = escrow.get_milestones(&contract_id);

    assert!(escrow.finalize_contract(&contract_id, &client_addr));
    let record = escrow.get_finalization_record(&contract_id).unwrap();

    assert_eq!(
        record.summary.schema_version,
        CONTRACT_SUMMARY_SCHEMA_VERSION
    );
    assert_eq!(record.summary.status, ContractStatus::Completed);
    assert_eq!(record.summary.client, contract.client);
    assert_eq!(record.summary.freelancer, contract.freelancer);
    assert_eq!(record.summary.funded_amount, total_milestone_amount());
    assert_eq!(record.summary.released_amount, total_milestone_amount());
    assert_eq!(record.summary.refundable_balance, 0);
    assert_eq!(record.summary.released_milestone_count, 3);
    assert_eq!(record.summary.milestones.len(), milestones.len());

    for (index, ms) in milestones.iter().enumerate() {
        let expected = milestone_summary(index as u32, ms.amount, ms.released, ms.refunded);
        assert_eq!(
            record.summary.milestones.get(index as u32).unwrap(),
            expected
        );
    }

    // The closed contract remains readable and unchanged.
    assert_eq!(escrow.get_contract(&contract_id), contract);
}

/// A contract whose accounting cannot be represented is not frozen into the
/// immutable record: the close path fails closed instead of writing a summary
/// derived from wrapped arithmetic.
#[test]
fn finalize_rejects_inconsistent_accounting() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_address = env.register(crate::Escrow, ());
    let escrow = EscrowClient::new(&env, &escrow_address);
    escrow.initialize(&Address::generate(&env));

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let contract_id = escrow.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &default_milestones(&env),
        &ReleaseAuthorization::ClientOnly,
    );

    // Corrupt the accounting so that `funded - released - refunded` leaves the
    // representable range. Reachable only through a bug elsewhere; the close
    // path must fail closed anyway.
    env.as_contract(&escrow_address, || {
        let key = DataKey::Contract(contract_id);
        let mut contract: Contract = env.storage().persistent().get(&key).unwrap();
        contract.status = ContractStatus::Completed;
        contract.funded_amount = 0;
        contract.released_amount = i128::MIN;
        contract.refunded_amount = 0;
        env.storage().persistent().set(&key, &contract);
    });

    assert_contract_error(
        escrow.try_finalize_contract(&contract_id, &client_addr),
        Error::AccountingInvariantViolated,
    );
    assert_eq!(env.events().all().len(), 0);
    assert!(escrow.get_finalization_record(&contract_id).is_none());
}

// ── Input validation and boundaries ─────────────────────────────────────────

/// Unknown contract ids are rejected without writing anything.
#[test]
fn finalize_rejects_unknown_contract() {
    let env = ttl_env();
    let (escrow, client_addr, _freelancer_addr, _token, _contract_id) = completed_contract(&env);

    assert_contract_error(
        escrow.try_finalize_contract(&9_999, &client_addr),
        Error::ContractNotFound,
    );
    assert_eq!(env.events().all().len(), 0);

    // Contract ids are allocated from 1, so id 0 is never a live contract.
    assert_contract_error(
        escrow.try_finalize_contract(&0, &client_addr),
        Error::ContractNotFound,
    );
}

/// The highest `u32` id is a boundary input, not a panic or a wrap.
#[test]
fn finalize_rejects_maximum_contract_id() {
    let env = ttl_env();
    let (escrow, client_addr, _freelancer_addr, _token, _contract_id) = completed_contract(&env);

    assert_contract_error(
        escrow.try_finalize_contract(&u32::MAX, &client_addr),
        Error::ContractNotFound,
    );
    assert!(escrow.get_finalization_record(&u32::MAX).is_none());
}

/// Non-terminal statuses are rejected before any record is written.
#[test]
fn finalize_rejects_non_terminal_status() {
    let env = ttl_env();
    let (escrow, token) = escrow_with_token(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);

    let contract_id = escrow.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &default_milestones(&env),
        &ReleaseAuthorization::ClientOnly,
    );

    // `Created`
    assert_contract_error(
        escrow.try_finalize_contract(&contract_id, &client_addr),
        EscrowError::InvalidStatusTransition,
    );

    // `Funded` is still not finalizable.
    StellarAssetClient::new(&env, &token).mint(&client_addr, &total_milestone_amount());
    assert!(escrow.deposit_funds(&contract_id, &client_addr, &total_milestone_amount()));
    assert_contract_error(
        escrow.try_finalize_contract(&contract_id, &client_addr),
        EscrowError::InvalidStatusTransition,
    );

    // `Cancelled` is terminal for funds but not finalizable either.
    assert!(escrow.cancel_contract(&contract_id, &client_addr));
    assert_contract_error(
        escrow.try_finalize_contract(&contract_id, &client_addr),
        EscrowError::InvalidStatusTransition,
    );

    assert_eq!(env.events().all().len(), 0);
    assert!(escrow.get_finalization_record(&contract_id).is_none());
}

/// A partially released / partially refunded contract snapshots the mixed
/// outcome exactly once.
#[test]
fn finalize_snapshots_mixed_release_and_refund() {
    let env = ttl_env();
    let (escrow, token) = escrow_with_token(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);

    let contract_id = escrow.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &default_milestones(&env),
        &ReleaseAuthorization::ClientOnly,
    );
    StellarAssetClient::new(&env, &token).mint(&client_addr, &total_milestone_amount());
    assert!(escrow.deposit_funds(&contract_id, &client_addr, &total_milestone_amount()));

    assert!(escrow.approve_milestone_release(&contract_id, &client_addr, &0));
    assert!(escrow.release_milestone(&contract_id, &client_addr, &0));
    let refunded = escrow.refund_unreleased_milestones(&contract_id, &vec![&env, 1_u32, 2_u32]);
    assert_eq!(refunded, 1_000_0000000);
    assert_eq!(
        escrow.get_contract(&contract_id).status,
        ContractStatus::Completed
    );

    assert!(escrow.finalize_contract(&contract_id, &freelancer_addr));
    let record = escrow.get_finalization_record(&contract_id).unwrap();
    assert_eq!(record.finalizer, freelancer_addr);
    assert_eq!(record.summary.refundable_balance, 0);
    assert_eq!(record.summary.released_milestone_count, 1);
    assert!(record.summary.milestones.get(0).unwrap().released);
    assert!(record.summary.milestones.get(1).unwrap().refunded);

    assert_contract_error(
        escrow.try_finalize_contract(&contract_id, &client_addr),
        Error::AlreadyFinalized,
    );
}

/// Independent contracts finalize independently: closing one must not consume
/// or shadow the other.
#[test]
fn closing_one_contract_does_not_affect_another() {
    let env = ttl_env();
    let (escrow, token) = escrow_with_token(&env);

    let build = |env: &Env| {
        let client_addr = Address::generate(env);
        let id = escrow.create_contract(
            &client_addr,
            &Address::generate(env),
            &None,
            &default_milestones(env),
            &ReleaseAuthorization::ClientOnly,
        );
        StellarAssetClient::new(env, &token).mint(&client_addr, &total_milestone_amount());
        escrow.deposit_funds(&id, &client_addr, &total_milestone_amount());
        for i in 0..3_u32 {
            escrow.approve_milestone_release(&id, &client_addr, &i);
            escrow.release_milestone(&id, &client_addr, &i);
        }
        (id, client_addr)
    };
    let (first_id, first_client) = build(&env);
    let (second_id, second_client) = build(&env);

    assert!(escrow.finalize_contract(&first_id, &first_client));

    assert!(escrow.get_finalization_record(&first_id).is_some());
    assert!(escrow.get_finalization_record(&second_id).is_none());
    assert_contract_error(
        escrow.try_finalize_contract(&first_id, &first_client),
        Error::AlreadyFinalized,
    );

    assert!(escrow.finalize_contract(&second_id, &second_client));
    assert!(escrow.get_finalization_record(&second_id).is_some());
}

/// The record written for one contract id is never readable under another id,
/// even across the whole `u32` range.
#[test]
fn records_are_scoped_to_their_contract_id() {
    let env = ttl_env();
    let (escrow, client_addr, _freelancer_addr, _token, contract_id) = completed_contract(&env);

    assert!(escrow.finalize_contract(&contract_id, &client_addr));

    for other in [0_u32, contract_id - 1, contract_id + 1, u32::MAX] {
        assert!(
            escrow.get_finalization_record(&other).is_none(),
            "contract {other} must not see the record of {contract_id}"
        );
    }

    let first: FinalizationRecord = escrow.get_finalization_record(&contract_id).unwrap();
    let second: FinalizationRecord = escrow.get_finalization_record(&contract_id).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.finalizer, client_addr);
}
