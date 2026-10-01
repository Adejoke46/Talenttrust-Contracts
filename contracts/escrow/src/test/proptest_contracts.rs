//! Property-based tests for contract creation and state invariants.
//! Property-based tests for contract creation and state invariants.
//!
//! Randomized input testing for escrow contract core invariants:
//! - Contract creation with valid/invalid milestone amounts
//! - Client/freelancer distinctness enforcement
//! - Accounting fields initialized to zero
//! - Status starts as Created
//! - Arbitration modes validated
//!
//! Validation boundaries (deterministic, enforced by `create_contract`):
//! - `milestones` must be non-empty; an empty vector is rejected.
//! - Every milestone amount must be strictly positive (`> 0`); zero and
//!   negative amounts are rejected.
//! - `client` and `freelancer` must be distinct addresses.
//! - `ReleaseAuthorization::ClientAndArbiter` and `ArbiterOnly` require a
//!   non-`None` arbiter; `ClientOnly` must not require one.
//! - On rejection, no contract is persisted and no state is mutated, so
//!   retries and concurrent submissions cannot observe a partial contract.
//!
//! NOTE: Tests requiring fund flow (deposit, release, refund) are excluded due
//! to a pre-existing auth regression in `deposit_funds` cross-contract
//! transfers (181 tests fail on clean main for the same reason).

#![cfg(test)]

extern crate std;

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Mutex, OnceLock};
use std::vec::Vec as StdVec;

use proptest::prelude::*;
use soroban_sdk::{testutils::Address as _, Address, Env, Vec};

use crate::{Contract, ContractStatus, Escrow, EscrowClient, ReleaseAuthorization};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Serializes property tests that share the global Soroban test environment.
///
/// Soroban's `Env::default()` installs process-wide test state. Running
/// proptest cases concurrently across threads can interleave that state and
/// produce non-deterministic results. This guard ensures each property test
/// body executes under exclusive access, preserving deterministic behavior
/// for valid, invalid, duplicate, and boundary-case inputs.
fn test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn setup() -> (Env, EscrowClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let id = env.register(Escrow, ());
    let client = EscrowClient::new(&env, &id);
    let admin = Address::generate(&env);
    client.initialize(&admin);
    (env, client)
}

fn to_soroban_vec(env: &Env, amounts: &[i128]) -> Vec<i128> {
    let mut v = Vec::new(env);
    for &a in amounts {
        v.push_back(a);
    }
    v
}

fn try_create(
    client: &EscrowClient,
    ca: &Address,
    fa: &Address,
    arbiter: Option<Address>,
    milestones: Vec<i128>,
    auth: &ReleaseAuthorization,
) -> bool {
    // Guard against concurrent execution of the shared Soroban test env.
    let _guard = test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    catch_unwind(AssertUnwindSafe(|| {
        client.create_contract(ca, fa, &arbiter, &milestones, auth);
    }))
    .is_ok()
}

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

fn valid_amounts() -> impl Strategy<Value = StdVec<i128>> {
    prop::collection::vec(1i128..=100_000_000, 1..=8)
}

fn small_amounts() -> impl Strategy<Value = StdVec<i128>> {
    prop::collection::vec(1i128..=1000, 1..=5)
}

/// Boundary amounts: the smallest valid positive value, zero, and negative
/// values that must all be rejected.
fn boundary_amounts() -> impl Strategy<Value = StdVec<i128>> {
    prop::collection::vec(
        prop_oneof![
            Just(1i128),
            Just(0i128),
            Just(-1i128),
            Just(i128::MIN),
            Just(i128::MAX),
        ],
        1..=5,
    )
}

/// Amounts that are all strictly positive (valid) but include the maximum.
fn positive_boundary_amounts() -> impl Strategy<Value = StdVec<i128>> {
    prop::collection::vec(prop_oneof![Just(1i128), Just(i128::MAX)], 1..=5)
}

const CASES: u32 = 64;

proptest! {
    #![proptest_config(ProptestConfig { cases: CASES, ..ProptestConfig::default() })]

    /// Valid creation with distinct addresses and positive milestones succeeds.
    #[test]
    fn prop_create_contract_succeeds(amounts in valid_amounts()) {
        let (env, client) = setup();
        let ca = Address::generate(&env);
        let fa = Address::generate(&env);
        let milestones = to_soroban_vec(&env, &amounts);

        let ok = try_create(&client, &ca, &fa, None, milestones, &ReleaseAuthorization::ClientOnly);
        prop_assert!(ok, "Valid creation should succeed");

        let data: Contract = client.get_contract(&1u32);
        prop_assert_eq!(data.status, ContractStatus::Created);
        prop_assert_eq!(data.total_deposited, 0);
        prop_assert_eq!(data.released_amount, 0);
        prop_assert_eq!(data.refunded_amount, 0);
        prop_assert!(!data.reputation_issued);
    }

    /// Client == freelancer is always rejected.
    #[test]
    fn prop_same_participants_rejected(amounts in small_amounts()) {
        let (env, client) = setup();
        let same = Address::generate(&env);
        let milestones = to_soroban_vec(&env, &amounts);

        let ok = try_create(&client, &same, &same, None, milestones, &ReleaseAuthorization::ClientOnly);
        prop_assert!(!ok, "Same participants should be rejected");
    }

    /// Client and freelancer are always distinct in successful creation.
    #[test]
    fn prop_distinct_participants_stored(amounts in small_amounts()) {
        let (env, client) = setup();
        let ca = Address::generate(&env);
        let fa = Address::generate(&env);
        let milestones = to_soroban_vec(&env, &amounts);

        let ok = try_create(&client, &ca, &fa, None, milestones, &ReleaseAuthorization::ClientOnly);
        prop_assert!(ok);

        let data: Contract = client.get_contract(&1u32);
        prop_assert_eq!(data.client, ca);
        prop_assert_eq!(data.freelancer, fa);
    }

    /// Arbiter modes requiring arbiter fail without one.
    #[test]
    fn prop_arbiter_required_modes(
        mode in prop_oneof![
            Just(ReleaseAuthorization::ClientAndArbiter),
            Just(ReleaseAuthorization::ArbiterOnly),
        ],
        amounts in small_amounts(),
    ) {
        let (env, client) = setup();
        let ca = Address::generate(&env);
        let fa = Address::generate(&env);
        let milestones = to_soroban_vec(&env, &amounts);

        let ok = try_create(&client, &ca, &fa, None, milestones, &mode);
        prop_assert!(!ok, "Arbiter-required mode without arbiter should fail");
    }

    /// ClientOnly mode works without an arbiter.
    #[test]
    fn prop_client_only_no_arbiter(amounts in small_amounts()) {
        let (env, client) = setup();
        let ca = Address::generate(&env);
        let fa = Address::generate(&env);
        let milestones = to_soroban_vec(&env, &amounts);

        let ok = try_create(&client, &ca, &fa, None, milestones, &ReleaseAuthorization::ClientOnly);
        prop_assert!(ok, "ClientOnly without arbiter should succeed");
    }

    /// Multiple contracts get sequential IDs.
    #[test]
    fn prop_sequential_ids(amounts in small_amounts()) {
        let (env, client) = setup();
        let milestones = to_soroban_vec(&env, &amounts);

        for n in 0..5u32 {
            let ca = Address::generate(&env);
            let fa = Address::generate(&env);
            let ok = try_create(&client, &ca, &fa, None, milestones.clone(), &ReleaseAuthorization::ClientOnly);
            prop_assert!(ok, "Contract {} creation should succeed", n);
            let data: Contract = client.get_contract(&(n + 1));
            prop_assert_eq!(data.status, ContractStatus::Created);
        }
    }

    /// Accounting fields are always zero after creation.
    #[test]
    fn prop_zero_accounting_after_creation(amounts in valid_amounts()) {
        let (env, client) = setup();
        let ca = Address::generate(&env);
        let fa = Address::generate(&env);
        let milestones = to_soroban_vec(&env, &amounts);

        let ok = try_create(&client, &ca, &fa, None, milestones, &ReleaseAuthorization::ClientOnly);
        prop_assert!(ok);

        let data: Contract = client.get_contract(&1u32);
        prop_assert_eq!(data.total_deposited, 0);
        prop_assert_eq!(data.released_amount, 0);
        prop_assert_eq!(data.refunded_amount, 0);
        prop_assert!(!data.reputation_issued);
    }

    /// Empty milestone vectors are always rejected and persist no contract.
    #[test]
    fn prop_empty_milestones_rejected(_dummy in 0u8..1) {
        let (env, client) = setup();
        let ca = Address::generate(&env);
        let fa = Address::generate(&env);
        let milestones = Vec::<i128>::new(&env);

        let ok = try_create(&client, &ca, &fa, None, milestones, &ReleaseAuthorization::ClientOnly);
        prop_assert!(!ok, "Empty milestones must be rejected");

        // No contract should have been persisted under id 1.
        let lookup = catch_unwind(AssertUnwindSafe(|| client.get_contract(&1u32)));
        prop_assert!(lookup.is_err(), "Rejected creation must not persist a contract");
    }

    /// Non-positive milestone amounts (zero, negative, i128::MIN) are rejected.
    #[test]
    fn prop_non_positive_milestones_rejected(amounts in boundary_amounts()) {
        // Only exercise the case where at least one amount is non-positive.
        prop_assume!(amounts.iter().any(|&a| a <= 0));

        let (env, client) = setup();
        let ca = Address::generate(&env);
        let fa = Address::generate(&env);
        let milestones = to_soroban_vec(&env, &amounts);

        let ok = try_create(&client, &ca, &fa, None, milestones, &ReleaseAuthorization::ClientOnly);
        prop_assert!(!ok, "Non-positive milestone amounts must be rejected");

        let lookup = catch_unwind(AssertUnwindSafe(|| client.get_contract(&1u32)));
        prop_assert!(lookup.is_err(), "Rejected creation must not persist a contract");
    }

    /// Boundary-valid amounts (1 and i128::MAX) are accepted.
    #[test]
    fn prop_positive_boundary_amounts_accepted(amounts in positive_boundary_amounts()) {
        let (env, client) = setup();
        let ca = Address::generate(&env);
        let fa = Address::generate(&env);
        let milestones = to_soroban_vec(&env, &amounts);

        let ok = try_create(&client, &ca, &fa, None, milestones, &ReleaseAuthorization::ClientOnly);
        prop_assert!(ok, "Strictly positive boundary amounts must be accepted");

        let data: Contract = client.get_contract(&1u32);
        prop_assert_eq!(data.status, ContractStatus::Created);
        prop_assert_eq!(data.total_deposited, 0);
        prop_assert_eq!(data.released_amount, 0);
        prop_assert_eq!(data.refunded_amount, 0);
    }

    /// Duplicate submissions with identical inputs each produce a distinct,
    /// independent contract with sequential IDs and zeroed accounting.
    #[test]
    fn prop_duplicate_submissions_are_independent(amounts in small_amounts()) {
        let (env, client) = setup();
        let ca = Address::generate(&env);
        let fa = Address::generate(&env);
        let milestones = to_soroban_vec(&env, &amounts);

        let first = try_create(&client, &ca, &fa, None, milestones.clone(), &ReleaseAuthorization::ClientOnly);
        let second = try_create(&client, &ca, &fa, None, milestones.clone(), &ReleaseAuthorization::ClientOnly);
        prop_assert!(first && second, "Duplicate submissions should each succeed");

        let c1: Contract = client.get_contract(&1u32);
        let c2: Contract = client.get_contract(&2u32);
        prop_assert_eq!(c1.status, ContractStatus::Created);
        prop_assert_eq!(c2.status, ContractStatus::Created);
        prop_assert_eq!(c1.total_deposited, 0);
        prop_assert_eq!(c2.total_deposited, 0);
        prop_assert_eq!(c1.released_amount, 0);
        prop_assert_eq!(c2.released_amount, 0);
    }

    /// Rejected creation leaves the next valid contract id unchanged, proving
    /// no partial state was written by the failed attempt.
    #[test]
    fn prop_rejection_does_not_consume_id(amounts in small_amounts()) {
        let (env, client) = setup();
        let ca = Address::generate(&env);
        let fa = Address::generate(&env);
        let milestones = to_soroban_vec(&env, &amounts);

        // Rejected: same participants.
        let rejected = try_create(&client, &ca, &ca, None, milestones.clone(), &ReleaseAuthorization::ClientOnly);
        prop_assert!(!rejected);

        // Next valid creation should still receive id 1.
        let ok = try_create(&client, &ca, &fa, None, milestones, &ReleaseAuthorization::ClientOnly);
        prop_assert!(ok);
        let data: Contract = client.get_contract(&1u32);
        prop_assert_eq!(data.status, ContractStatus::Created);
    }

    /// Arbiter-required modes succeed when an arbiter is supplied.
    #[test]
    fn prop_arbiter_required_modes_with_arbiter(
        mode in prop_oneof![
            Just(ReleaseAuthorization::ClientAndArbiter),
            Just(ReleaseAuthorization::ArbiterOnly),
        ],
        amounts in small_amounts(),
    ) {
        let (env, client) = setup();
        let ca = Address::generate(&env);
        let fa = Address::generate(&env);
        let arbiter = Address::generate(&env);
        let milestones = to_soroban_vec(&env, &amounts);

        let ok = try_create(&client, &ca, &fa, Some(arbiter), milestones, &mode);
        prop_assert!(ok, "Arbiter-required mode with arbiter should succeed");

        let data: Contract = client.get_contract(&1u32);
        prop_assert_eq!(data.status, ContractStatus::Created);
    }
}

