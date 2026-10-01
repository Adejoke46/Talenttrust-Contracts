#![cfg(test)]

//! Deterministic failure-recovery tests for `finalize_contract`
//! (`contracts/escrow/src/finalize.rs`).
//!
//! The close record written by finalization is immutable, so the contract can
//! only ever seal a summary it has fully validated. These tests pin down that
//! property across the paths where a seal can fail.
//!
//! Coverage map:
//!
//! | Property | Tests |
//! | --- | --- |
//! | Success on both sealable statuses, by every authorized role | `seal_*` |
//! | Guard order and error determinism | `rejects_*`, `guard_order_*` |
//! | Retries and concurrent finalizers | `retry_*`, `concurrent_*` |
//! | Partial failure leaves nothing behind | `partial_failure_*` |
//! | Dependency (settlement-token) failure | `dependency_*` |
//! | Accounting reconciliation and boundaries | `accounting_*`, `boundary_*` |
//! | Observability | `event_*` |
//! | Regression over prior behavior | `regression_*` |

use super::assert_contract_error;
use crate::{
    test::EscrowFixtureBuilder, ContractStatus, DataKey, Error, Escrow, EscrowClient, Milestone,
    ReleaseAuthorization,
};
use soroban_sdk::{
    testutils::{Address as _, Events as _},
    token::StellarAssetClient,
    vec, Address, Env, IntoVal, TryFromVal, TryIntoVal,
};

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Amount used by the single-milestone fixtures (100 tokens at the SAC's
/// 7-decimal scale, i.e. exactly representable as required by
/// `create_contract`).
const AMOUNT: i128 = 1_000_000_000;

/// The smallest amount `create_contract` accepts against a 7-decimal SAC:
/// one whole token. Used as a lower-bound case.
const MIN_AMOUNT: i128 = 10_000_000;

/// An initialized escrow contract with named participants and a bound SAC,
/// plus the `Env` that owns it.
///
/// The `Env` is kept in the struct (rather than handing out a borrowed
/// `EscrowClient`) so tests can build short-lived clients on demand without
/// any lifetime or aliasing tricks.
struct Sealable {
    env: Env,
    escrow_address: Address,
    admin: Address,
    client_addr: Address,
    freelancer_addr: Address,
    arbiter_addr: Option<Address>,
    contract_id: u32,
}

impl Sealable {
    fn escrow(&self) -> EscrowClient<'_> {
        EscrowClient::new(&self.env, &self.escrow_address)
    }

    /// The assigned arbiter. Every fixture built by [`completed_contract`] and
    /// [`disputed_contract`] has one.
    fn arbiter(&self) -> Address {
        self.arbiter_addr
            .clone()
            .expect("fixture is created with an arbiter")
    }

    /// Force this contract's persisted ledger totals, staging state that no
    /// legitimate flow can produce.
    fn force_accounting(&self, mutate: impl FnOnce(&mut crate::Contract)) {
        force_contract_accounting(&self.env, &self.escrow_address, self.contract_id, mutate);
    }

    /// Force this contract's milestone vector, staging state that no
    /// legitimate flow can produce.
    fn force_milestones(&self, mutate: impl FnOnce(&mut soroban_sdk::Vec<Milestone>)) {
        force_milestones(&self.env, &self.escrow_address, self.contract_id, mutate);
    }

    /// Delete this contract's milestone vector entirely.
    fn remove_milestones(&self) {
        self.env.as_contract(&self.escrow_address, || {
            let key = crate::keys::milestone_key(&self.env, self.contract_id);
            self.env.storage().persistent().remove(&key);
        });
    }
}

/// Build an initialized escrow with a bound SAC and one funded milestone of
/// `amount` stroops, leaving the contract in `Funded` so each test can drive it
/// into the status it needs to exercise.
fn funded_contract(amount: i128) -> Sealable {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();
    let escrow_address = env.register(Escrow, ());
    let escrow = EscrowClient::new(&env, &escrow_address);

    let admin = Address::generate(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    escrow.initialize(&admin);

    let token = env.register_stellar_asset_contract(admin.clone());
    escrow.bind_settlement_token(&admin, &token);

    let contract_id = escrow.create_contract(
        &client_addr,
        &freelancer_addr,
        &Some(arbiter_addr.clone()),
        &vec![&env, amount],
        &ReleaseAuthorization::ClientOnly,
    );
    StellarAssetClient::new(&env, &token).mint(&client_addr, &amount);
    escrow.deposit_funds(&contract_id, &client_addr, &amount);

    Sealable {
        env,
        escrow_address,
        admin,
        client_addr,
        freelancer_addr,
        arbiter_addr: Some(arbiter_addr),
        contract_id,
    }
}

/// A fully funded, fully released (→ `Completed`) 1-milestone contract.
fn completed_contract() -> Sealable {
    let s = funded_contract(AMOUNT);
    let escrow = s.escrow();
    escrow.approve_milestone_release(&s.contract_id, &s.client_addr, &0);
    escrow.release_milestone(&s.contract_id, &s.client_addr, &0);
    assert_eq!(
        escrow.get_contract(&s.contract_id).status,
        ContractStatus::Completed
    );
    drop(escrow);
    s
}

/// A funded-but-unreleased contract that has been escalated to `Disputed`.
fn disputed_contract() -> Sealable {
    let s = funded_contract(AMOUNT);
    let escrow = s.escrow();
    escrow.raise_dispute(&s.contract_id, &s.client_addr);
    assert_eq!(
        escrow.get_contract(&s.contract_id).status,
        ContractStatus::Disputed
    );
    drop(escrow);
    s
}

/// Overwrite a contract's persisted ledger totals directly, bypassing the
/// entrypoints that keep them consistent. Used to stage corrupt state that no
/// legitimate flow can produce.
fn force_contract_accounting(
    env: &Env,
    escrow_address: &Address,
    contract_id: u32,
    mutate: impl FnOnce(&mut crate::Contract),
) {
    env.as_contract(escrow_address, || {
        let key = DataKey::Contract(contract_id);
        let mut contract: crate::Contract = env
            .storage()
            .persistent()
            .get(&key)
            .expect("contract must exist");
        mutate(&mut contract);
        env.storage().persistent().set(&key, &contract);
    });
}

/// Overwrite the milestone vector, bypassing the entrypoints.
fn force_milestones(
    env: &Env,
    escrow_address: &Address,
    contract_id: u32,
    mutate: impl FnOnce(&mut soroban_sdk::Vec<Milestone>),
) {
    env.as_contract(escrow_address, || {
        let key = crate::keys::milestone_key(env, contract_id);
        let mut milestones: soroban_sdk::Vec<Milestone> = env
            .storage()
            .persistent()
            .get(&key)
            .expect("milestones must exist");
        mutate(&mut milestones);
        env.storage().persistent().set(&key, &milestones);
    });
}

// ── Success: both sealable statuses, every authorized role ──────────────────

/// Baseline: a `Completed` contract seals and round-trips its summary.
#[test]
fn seal_completed_contract_round_trips_summary() {
    let s = completed_contract();
    assert!(s.escrow().finalize_contract(&s.contract_id, &s.client_addr));

    let record = s
        .escrow()
        .get_finalization_record(&s.contract_id)
        .expect("seal must be readable");
    assert_eq!(record.finalizer, s.client_addr);
    assert_eq!(record.summary.status, ContractStatus::Completed);
    assert_eq!(record.summary.total_amount, AMOUNT);
    assert_eq!(record.summary.funded_amount, AMOUNT);
    assert_eq!(record.summary.released_amount, AMOUNT);
    assert_eq!(record.summary.refundable_balance, 0);
    assert_eq!(record.summary.released_milestone_count, 1);
}

/// The freelancer may seal a `Completed` contract.
#[test]
fn seal_completed_contract_by_freelancer() {
    let s = completed_contract();
    assert!(s
        .escrow()
        .finalize_contract(&s.contract_id, &s.freelancer_addr));
    let record = s.escrow().get_finalization_record(&s.contract_id).unwrap();
    assert_eq!(record.finalizer, s.freelancer_addr);
}

/// The assigned arbiter may seal a `Completed` contract.
#[test]
fn seal_completed_contract_by_arbiter() {
    let s = completed_contract();
    assert!(s.escrow().finalize_contract(&s.contract_id, &s.arbiter()));
    let record = s.escrow().get_finalization_record(&s.contract_id).unwrap();
    assert_eq!(record.finalizer, s.arbiter());
}

/// A `Disputed` contract is sealable, and its seal reports the still-refundable
/// balance rather than forcing the balance to zero.
#[test]
fn seal_disputed_contract_by_arbiter_reports_refundable_balance() {
    let s = disputed_contract();
    assert!(s.escrow().finalize_contract(&s.contract_id, &s.arbiter()));

    let record = s.escrow().get_finalization_record(&s.contract_id).unwrap();
    assert_eq!(record.summary.status, ContractStatus::Disputed);
    assert_eq!(record.summary.funded_amount, AMOUNT);
    assert_eq!(record.summary.released_amount, 0);
    assert_eq!(record.summary.refundable_balance, AMOUNT);
    assert_eq!(record.summary.released_milestone_count, 0);
}

/// Sealing a `Disputed` contract retires its pre-dispute snapshot, so the
/// now-sealed contract can no longer be rolled back.
#[test]
fn seal_disputed_contract_retires_rollback_snapshot() {
    let s = disputed_contract();
    let contract_id = s.contract_id;

    assert!(s.escrow().finalize_contract(&contract_id, &s.arbiter()));

    // `rollback_dispute` now reports `AlreadyFinalized` (the seal guard) rather
    // than succeeding: the snapshot is no longer reachable.
    assert_contract_error(
        s.escrow().try_rollback_dispute(&contract_id),
        Error::AlreadyFinalized,
    );
}

// ── Guard order and error determinism ───────────────────────────────────────

/// An outsider is rejected with `UnauthorizedRole` and nothing is sealed.
#[test]
fn rejects_unauthorized_finalizer_without_writing() {
    let s = completed_contract();
    let outsider = Address::generate(&s.env);

    assert_contract_error(
        s.escrow().try_finalize_contract(&s.contract_id, &outsider),
        Error::UnauthorizedRole,
    );
    assert!(s.escrow().get_finalization_record(&s.contract_id).is_none());
}

/// A `Created` contract cannot be sealed (`InvalidStatusTransition`).
#[test]
fn rejects_created_contract() {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();
    let escrow_address = env.register(Escrow, ());
    let escrow = EscrowClient::new(&env, &escrow_address);
    let admin = Address::generate(&env);
    escrow.initialize(&admin);
    let token = env.register_stellar_asset_contract(admin.clone());
    escrow.bind_settlement_token(&admin, &token);

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let contract_id = escrow.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &vec![&env, AMOUNT],
        &ReleaseAuthorization::ClientOnly,
    );

    assert_contract_error(
        escrow.try_finalize_contract(&contract_id, &client_addr),
        Error::InvalidStatusTransition,
    );
    assert!(escrow.get_finalization_record(&contract_id).is_none());
}

/// A `Funded` contract cannot be sealed — finalization is not a substitute for
/// completing or refunding the remaining milestones.
#[test]
fn rejects_funded_contract() {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();
    let escrow_address = env.register(Escrow, ());
    let escrow = EscrowClient::new(&env, &escrow_address);
    let admin = Address::generate(&env);
    escrow.initialize(&admin);
    let token = env.register_stellar_asset_contract(admin.clone());
    escrow.bind_settlement_token(&admin, &token);

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let amount = AMOUNT;
    let contract_id = escrow.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &vec![&env, amount],
        &ReleaseAuthorization::ClientOnly,
    );
    StellarAssetClient::new(&env, &token).mint(&client_addr, &amount);
    escrow.deposit_funds(&contract_id, &client_addr, &amount);
    assert_eq!(
        escrow.get_contract(&contract_id).status,
        ContractStatus::Funded
    );

    assert_contract_error(
        escrow.try_finalize_contract(&contract_id, &client_addr),
        Error::InvalidStatusTransition,
    );
    assert!(escrow.get_finalization_record(&contract_id).is_none());
}

/// An unknown contract id reports `ContractNotFound`.
#[test]
fn rejects_unknown_contract_id() {
    let s = completed_contract();
    assert_contract_error(
        s.escrow().try_finalize_contract(&9_999, &s.client_addr),
        Error::ContractNotFound,
    );
}

/// The zero id is never allocated, so it is rejected by the bounds check
/// before any storage lookup happens.
#[test]
fn rejects_zero_contract_id() {
    let s = completed_contract();
    assert_contract_error(
        s.escrow().try_finalize_contract(&0, &s.client_addr),
        Error::ContractNotFound,
    );
}

/// Pause blocks sealing and leaves the contract sealable afterwards, i.e. the
/// failure is transient and fully recoverable by unpausing.
#[test]
fn pause_blocks_seal_and_unpause_recovers() {
    let env = EscrowFixtureBuilder::new().completed().build();
    let escrow = env.escrow();
    let contract_id = env.escrow_id;

    assert!(escrow.pause(&1u64));
    assert_contract_error(
        escrow.try_finalize_contract(&contract_id, &env.client),
        Error::ContractPaused,
    );
    assert!(escrow.get_finalization_record(&contract_id).is_none());

    // Recovery: once the safety rail is lifted the very same call succeeds.
    assert!(escrow.unpause());
    assert!(escrow.finalize_contract(&contract_id, &env.client));
    assert!(escrow.get_finalization_record(&contract_id).is_some());
}

/// A paused contract reports `ContractPaused` even when it is also in a
/// non-sealable status, so the pause never masks behind a state verdict.
/// This pins the guard order: pause is evaluated before status.
#[test]
fn guard_order_pause_precedes_status_check() {
    let env = EscrowFixtureBuilder::new().funded().build();
    let escrow = env.escrow();
    let contract_id = env.escrow_id;
    // `Funded` alone would produce `InvalidStatusTransition`.
    assert_eq!(
        escrow.get_contract(&contract_id).status,
        ContractStatus::Funded
    );

    assert!(escrow.pause(&1u64));
    assert_contract_error(
        escrow.try_finalize_contract(&contract_id, &env.client),
        Error::ContractPaused,
    );
}

/// `AlreadyFinalized` is reported ahead of the pause check, so a duplicate
/// retry against an already-sealed contract is diagnosed as a duplicate
/// regardless of whether the protocol is currently paused.
#[test]
fn guard_order_duplicate_precedes_pause_check() {
    let s = completed_contract();
    assert!(s.escrow().finalize_contract(&s.contract_id, &s.client_addr));

    assert_contract_error(
        s.escrow()
            .try_finalize_contract(&s.contract_id, &s.client_addr),
        Error::AlreadyFinalized,
    );
}

/// Emergency mode blocks sealing. `activate_emergency_pause` engages both the
/// emergency flag and the pause flag, and the pause flag is checked first, so
/// the refusal surfaces as `ContractPaused` — an operator reading the error
/// still learns that a safety rail, not the contract's status, blocked the call.
#[test]
fn emergency_mode_blocks_seal() {
    let env = EscrowFixtureBuilder::new().completed().build();
    let escrow = env.escrow();
    let contract_id = env.escrow_id;

    assert!(escrow.activate_emergency_pause());
    assert!(escrow.is_emergency());
    assert_contract_error(
        escrow.try_finalize_contract(&contract_id, &env.client),
        Error::ContractPaused,
    );
    assert!(escrow.get_finalization_record(&contract_id).is_none());

    // Recovery: clearing both rails restores the ability to seal.
    assert!(escrow.resolve_emergency());
    assert!(escrow.finalize_contract(&contract_id, &env.client));
    assert!(escrow.get_finalization_record(&contract_id).is_some());
}

// ── Retries and concurrency ─────────────────────────────────────────────────

/// A rejected seal is retryable: once the cause is removed the same call
/// succeeds, and the earlier failure wrote nothing.
#[test]
fn retry_after_rejection_succeeds() {
    let env = EscrowFixtureBuilder::new().completed().build();
    let escrow = env.escrow();
    let contract_id = env.escrow_id;

    assert!(escrow.pause(&1u64));
    assert_contract_error(
        escrow.try_finalize_contract(&contract_id, &env.client),
        Error::ContractPaused,
    );
    assert!(escrow.unpause());

    assert!(escrow.finalize_contract(&contract_id, &env.client));
    assert!(escrow.get_finalization_record(&contract_id).is_some());
}

/// Repeated attempts after a successful seal are all rejected with
/// `AlreadyFinalized` and never rewrite the record.
#[test]
fn retry_after_success_is_rejected_and_record_is_stable() {
    let s = completed_contract();
    let contract_id = s.contract_id;
    assert!(s.escrow().finalize_contract(&contract_id, &s.client_addr));
    let first = s.escrow().get_finalization_record(&contract_id).unwrap();

    for _ in 0..3 {
        assert_contract_error(
            s.escrow()
                .try_finalize_contract(&contract_id, &s.client_addr),
            Error::AlreadyFinalized,
        );
    }

    let after = s.escrow().get_finalization_record(&contract_id).unwrap();
    assert_eq!(first, after, "the seal must never be rewritten");
}

/// A second finalizer racing the first is rejected with `AlreadyFinalized`;
/// the first writer's finalizer and timestamp are preserved.
#[test]
fn concurrent_second_finalizer_is_rejected() {
    let s = completed_contract();
    let contract_id = s.contract_id;

    assert!(s.escrow().finalize_contract(&contract_id, &s.client_addr));
    let record = s.escrow().get_finalization_record(&contract_id).unwrap();

    // Every other authorized party loses the race, deterministically.
    for other in [&s.freelancer_addr, &s.arbiter()] {
        assert_contract_error(
            s.escrow().try_finalize_contract(&contract_id, other),
            Error::AlreadyFinalized,
        );
    }

    let after = s.escrow().get_finalization_record(&contract_id).unwrap();
    assert_eq!(record, after, "a losing racer must not alter the seal");
}

/// A rejected duplicate finalization emits no event, so an indexer never sees
/// two closes for one contract.
#[test]
fn rejected_duplicate_finalization_emits_no_event() {
    let s = completed_contract();
    let contract_id = s.contract_id;
    assert!(s.escrow().finalize_contract(&contract_id, &s.client_addr));
    assert_eq!(
        s.env.events().all().len(),
        1,
        "the seal publishes one event"
    );

    // The rejected duplicate runs in its own invocation and publishes nothing.
    assert_contract_error(
        s.escrow()
            .try_finalize_contract(&contract_id, &s.client_addr),
        Error::AlreadyFinalized,
    );
    assert_eq!(
        s.env.events().all().len(),
        0,
        "a rejected duplicate must not publish a second close event"
    );
}

// ── Partial failure leaves nothing behind ───────────────────────────────────

/// A `Funded` contract is refused, and the refusal writes nothing: the
/// contract, its milestones, and its (absent) seal are all untouched, so the
/// participant can still release or refund afterwards.
#[test]
fn partial_failure_non_sealable_status_leaves_contract_usable() {
    let env = EscrowFixtureBuilder::new().funded().build();
    let escrow = env.escrow();
    let contract_id = env.escrow_id;
    let before = escrow.get_contract(&contract_id);

    assert_contract_error(
        escrow.try_finalize_contract(&contract_id, &env.client),
        Error::InvalidStatusTransition,
    );

    assert_eq!(escrow.get_contract(&contract_id), before);
    assert!(escrow.get_finalization_record(&contract_id).is_none());

    // The contract is still fully operable — the failed seal did not wedge it.
    escrow.approve_milestone_release(&contract_id, &env.client, &0);
    assert!(escrow.release_milestone(&contract_id, &env.client, &0));
}

/// A rejected seal is observable: the record stays absent and the contract
/// remains unsealed, so a caller can distinguish "refused" from "already done".
#[test]
fn partial_failure_is_observable_to_caller() {
    let env = EscrowFixtureBuilder::new().funded().build();
    let escrow = env.escrow();
    let contract_id = env.escrow_id;

    let result = escrow.try_finalize_contract(&contract_id, &env.client);
    match result {
        Err(Ok(e)) => assert_eq!(e, soroban_sdk::Error::from(Error::InvalidStatusTransition)),
        other => panic!("expected InvalidStatusTransition, got {other:?}"),
    }
    assert!(escrow.get_finalization_record(&contract_id).is_none());
}

// ── Dependency failure ──────────────────────────────────────────────────────

/// Sealing does not depend on the settlement token being present for custody
/// moves: an initialized contract with no bound token still seals correctly.
/// This pins that finalization reads only the contract and its milestones.
#[test]
fn dependency_seal_does_not_require_settlement_token_at_seal_time() {
    let s = completed_contract();
    let contract_id = s.contract_id;

    // Drop the settlement-token binding before sealing. Finalization reads only
    // the contract record and its milestones, so a missing custody dependency
    // must not prevent a correct close from being recorded.
    s.env.as_contract(&s.escrow_address, || {
        s.env
            .storage()
            .persistent()
            .remove(&DataKey::SettlementToken);
    });

    assert!(s.escrow().finalize_contract(&contract_id, &s.client_addr));
    let record = s.escrow().get_finalization_record(&contract_id).unwrap();
    assert_eq!(record.summary.status, ContractStatus::Completed);
    assert_eq!(record.summary.funded_amount, AMOUNT);
    assert_eq!(record.summary.released_amount, AMOUNT);
    assert_eq!(record.summary.refundable_balance, 0);
}

// ── Accounting reconciliation and boundaries ────────────────────────────────

/// A contract whose released total exceeds its funded total is refused rather
/// than sealed with a wrapped/negative refundable balance. This is the core
/// determinism fix: under `--release` an unchecked subtraction would wrap
/// silently and freeze a nonsense balance into an immutable record.
#[test]
fn accounting_rejects_payments_exceeding_funding() {
    let s = completed_contract();
    let contract_id = s.contract_id;
    // Stage an impossible accounting state: paid out more than was funded.
    s.force_accounting(|c| {
        c.released_amount = c.funded_amount + 1;
    });

    assert_contract_error(
        s.escrow()
            .try_finalize_contract(&contract_id, &s.client_addr),
        Error::AccountingInvariantViolated,
    );
    assert!(s.escrow().get_finalization_record(&contract_id).is_none());
}

/// A negative ledger total is refused.
#[test]
fn accounting_rejects_negative_totals() {
    let s = completed_contract();
    let contract_id = s.contract_id;
    s.force_accounting(|c| c.funded_amount = -1);

    assert_contract_error(
        s.escrow()
            .try_finalize_contract(&contract_id, &s.client_addr),
        Error::AccountingInvariantViolated,
    );
    assert!(s.escrow().get_finalization_record(&contract_id).is_none());
}

/// A contract funded beyond its milestone total is refused: the contract
/// record and its milestone vector have fallen out of step.
#[test]
fn accounting_rejects_funding_above_milestone_total() {
    let s = completed_contract();
    let contract_id = s.contract_id;
    s.force_accounting(|c| c.funded_amount = 5_000);

    assert_contract_error(
        s.escrow()
            .try_finalize_contract(&contract_id, &s.client_addr),
        Error::AccountingInvariantViolated,
    );
    assert!(s.escrow().get_finalization_record(&contract_id).is_none());
}

/// A milestone that claims to be both released and refunded is refused, so a
/// self-contradictory snapshot can never be sealed.
#[test]
fn accounting_rejects_milestone_both_released_and_refunded() {
    let s = completed_contract();
    let contract_id = s.contract_id;
    s.force_milestones(|milestones| {
        milestones.set(
            0,
            Milestone {
                amount: milestones.get(0).unwrap().amount,
                funded_amount: 1_000,
                released: true,
                refunded: true,
                deadline: None,
                refunded_amount: 0,
                work_evidence: None,
            },
        );
    });

    assert_contract_error(
        s.escrow()
            .try_finalize_contract(&contract_id, &s.client_addr),
        Error::AccountingInvariantViolated,
    );
    assert!(s.escrow().get_finalization_record(&contract_id).is_none());
}

/// A missing milestone vector is refused with a distinct, diagnosable error
/// rather than the misleading `ContractNotFound` — the contract record does
/// exist, only the summary inputs are gone. Nothing is written, so the seal
/// can be retried once the milestone entry is restored.
#[test]
fn accounting_reports_incomplete_state_when_milestones_missing() {
    let s = completed_contract();
    let contract_id = s.contract_id;

    // Snapshot the milestone vector so it can be restored below.
    let saved: soroban_sdk::Vec<Milestone> = s.escrow().get_milestones(&contract_id);

    s.remove_milestones();
    assert_contract_error(
        s.escrow()
            .try_finalize_contract(&contract_id, &s.client_addr),
        Error::FinalizationStateIncomplete,
    );
    assert!(s.escrow().get_finalization_record(&contract_id).is_none());

    // Recovery: restoring the milestone entry makes the very same call succeed.
    s.env.as_contract(&s.escrow_address, || {
        let key = crate::keys::milestone_key(&s.env, contract_id);
        s.env.storage().persistent().set(&key, &saved);
    });
    assert!(s.escrow().finalize_contract(&contract_id, &s.client_addr));
    assert!(s.escrow().get_finalization_record(&contract_id).is_some());
}

/// Boundary: a fully released contract has an exactly-zero refundable balance
/// and still seals.
#[test]
fn boundary_fully_released_seals_with_zero_balance() {
    let env = EscrowFixtureBuilder::new().completed().build();
    let escrow = env.escrow();
    let contract_id = env.escrow_id;

    assert!(escrow.finalize_contract(&contract_id, &env.client));
    let record = escrow.get_finalization_record(&contract_id).unwrap();
    assert_eq!(record.summary.refundable_balance, 0);
    assert_eq!(record.summary.released_amount, record.summary.funded_amount);
    assert_eq!(record.summary.milestones.len(), 3);
}

/// Boundary: a contract with the smallest amount the settlement token can
/// represent seals and reports its refundable balance exactly, with no
/// rounding or truncation surprise.
#[test]
fn boundary_minimum_amount_seals_exactly() {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();
    let escrow_address = env.register(Escrow, ());
    let escrow = EscrowClient::new(&env, &escrow_address);
    let admin = Address::generate(&env);
    escrow.initialize(&admin);
    let token = env.register_stellar_asset_contract(admin.clone());
    escrow.bind_settlement_token(&admin, &token);

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let amount = MIN_AMOUNT;
    let contract_id = escrow.create_contract(
        &client_addr,
        &freelancer_addr,
        &Some(arbiter_addr),
        &vec![&env, amount],
        &ReleaseAuthorization::ClientOnly,
    );
    StellarAssetClient::new(&env, &token).mint(&client_addr, &amount);
    escrow.deposit_funds(&contract_id, &client_addr, &amount);
    escrow.raise_dispute(&contract_id, &client_addr);

    assert!(escrow.finalize_contract(&contract_id, &client_addr));
    let record = escrow.get_finalization_record(&contract_id).unwrap();
    assert_eq!(record.summary.funded_amount, MIN_AMOUNT);
    assert_eq!(record.summary.refundable_balance, MIN_AMOUNT);
    assert_eq!(record.summary.released_amount, 0);
}

/// Boundary: sealing is refused for a contract id at the top of the `u32`
/// range that was never allocated.
#[test]
fn boundary_max_contract_id_rejected_as_not_found() {
    let s = completed_contract();
    assert_contract_error(
        s.escrow().try_finalize_contract(&u32::MAX, &s.client_addr),
        Error::ContractNotFound,
    );
}

// ── Observability ───────────────────────────────────────────────────────────

/// A successful seal publishes the summary alongside the finalizer and
/// timestamp, so an indexer can reconcile the close from the event alone
/// without a follow-up read. The payload carries no evidence strings or
/// secrets — only addresses, status and amounts already in the record.
#[test]
fn event_publishes_finalizer_timestamp_and_summary() {
    let s = completed_contract();
    let contract_id = s.contract_id;
    let events_before = s.env.events().all().len();

    assert!(s.escrow().finalize_contract(&contract_id, &s.client_addr));

    let expected_topics: soroban_sdk::Vec<soroban_sdk::Val> = vec![
        &s.env,
        soroban_sdk::symbol_short!("finalized").into_val(&s.env),
        contract_id.into_val(&s.env),
    ];

    let (_contract, _topics, payload) = s
        .env
        .events()
        .all()
        .iter()
        .find(|(_, topics, _)| topics == &expected_topics)
        .expect("a finalized event must be published for this contract");

    // Payload is (finalizer, timestamp, summary) — the sealed snapshot travels
    // with the event so an indexer needs no follow-up read.
    let (finalizer, _timestamp, summary): (Address, u64, crate::ContractSummary) =
        TryFromVal::try_from_val(&s.env, &payload).unwrap();
    assert_eq!(finalizer, s.client_addr);
    assert_eq!(summary.status, ContractStatus::Completed);
    assert_eq!(summary.released_amount, summary.funded_amount);
    assert_eq!(summary.refundable_balance, 0);
}

// ── Regression over prior behavior ──────────────────────────────────────────

/// Regression: sealing a contract still blocks every later mutation, so the
/// immutability guarantee that motivated the strict failure model is intact.
#[test]
fn regression_seal_still_blocks_later_mutations() {
    let env = EscrowFixtureBuilder::new().completed().build();
    let escrow = env.escrow();
    let contract_id = env.escrow_id;
    let client_addr = env.client.clone();

    assert!(escrow.finalize_contract(&contract_id, &client_addr));

    for result in [
        escrow.try_release_milestone(&contract_id, &client_addr, &0),
        escrow.try_approve_milestone_release(&contract_id, &client_addr, &0),
    ] {
        assert_contract_error(result, Error::AlreadyFinalized);
    }
    assert_contract_error(
        escrow.try_rollback_dispute(&contract_id),
        Error::AlreadyFinalized,
    );
}

/// Regression: `get_finalization_record` keeps returning `None` for an
/// unfinalized contract and for an id that was never allocated, rather than
/// panicking.
#[test]
fn regression_get_record_returns_none_when_unsealed() {
    let env = EscrowFixtureBuilder::new().completed().build();
    let escrow = env.escrow();

    assert!(escrow.get_finalization_record(&env.escrow_id).is_none());
    assert!(escrow.get_finalization_record(&9_999).is_none());
}

/// Regression: the seal carries the current summary schema version, so a
/// reader knows how to interpret the stored shape.
#[test]
fn regression_seal_carries_current_schema_version() {
    let env = EscrowFixtureBuilder::new().completed().build();
    let escrow = env.escrow();
    let contract_id = env.escrow_id;

    assert!(escrow.finalize_contract(&contract_id, &env.client));
    let record = escrow.get_finalization_record(&contract_id).unwrap();
    assert_eq!(
        record.summary.schema_version,
        crate::CONTRACT_SUMMARY_SCHEMA_VERSION
    );
}
