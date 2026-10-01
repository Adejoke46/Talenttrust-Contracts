//! Deterministic failure-recovery tests for the pending reputation-credit ledger.
//!
//! `DataKey::PendingReputationCredits(freelancer)` is a freelancer's only
//! on-chain claim on a future reputation issuance: a completed contract adds
//! exactly one credit, and exactly one credit is removed when the client rates
//! that contract. Because the claim is a single `i128`, every failure mode has
//! to be deterministic — the same stored value must always produce the same
//! outcome, and a rejected operation must leave the stored value byte-identical
//! so a retry behaves exactly like the first attempt.
//!
//! These tests pin the policy from both directions:
//!
//! * the pure helpers in [`crate::constants`] (`accrue_pending_credit`,
//!   `consume_pending_credit`, `is_valid_pending_credit_ledger`), and
//! * the entrypoints that drive them (`release_milestone`,
//!   `refund_unreleased_milestones`, `issue_reputation`), including the TTL
//!   durability of the ledger entry.
//!
//! Boundary and corrupted states are seeded straight into storage through
//! `env.as_contract`, because no public entrypoint can be driven there. Those
//! are precisely the states whose recovery used to be undefined.

#![cfg(test)]

use super::{
    assert_contract_error, complete_contract_funded, default_milestones,
    register_client_with_token,
};
use crate::constants::{
    accrue_pending_credit, consume_pending_credit, is_valid_pending_credit_ledger,
    MAX_PENDING_REPUTATION_CREDITS, REPUTATION_CREDIT_INCREMENT,
};
use crate::{ttl, ContractStatus, DataKey, Error, ReleaseAuthorization};
use soroban_sdk::{
    testutils::{storage::Persistent as _, Address as _, Events as _, Ledger as _},
    token::StellarAssetClient,
    vec, Address, Env, String, Symbol, TryIntoVal, Val, Vec,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn credit_key(freelancer: &Address) -> DataKey {
    DataKey::PendingReputationCredits(freelancer.clone())
}

/// Force the pending-credit ledger into an exact value, including values no
/// entrypoint can legitimately produce (negative, or above the ceiling).
fn seed_ledger(env: &Env, escrow: &Address, freelancer: &Address, value: i128) {
    env.as_contract(escrow, || {
        env.storage()
            .persistent()
            .set(&credit_key(freelancer), &value);
    });
}

/// Read the raw stored value, bypassing the getter's TTL bump-on-read.
fn raw_ledger(env: &Env, escrow: &Address, freelancer: &Address) -> i128 {
    env.as_contract(escrow, || {
        env.storage()
            .persistent()
            .get(&credit_key(freelancer))
            .unwrap_or(0)
    })
}

fn comment(env: &Env) -> String {
    String::from_str(env, "Deterministic recovery")
}

/// Create and fund a 3-milestone contract with every milestone pre-approved, so
/// each release can be driven one at a time and the completion branch can be
/// entered deliberately. Returns `(client_addr, freelancer_addr, contract_id)`.
fn funded_approved_contract(
    env: &Env,
    client: &crate::EscrowClient<'_>,
    token: &Address,
) -> (Address, Address, u32) {
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let contract_id = client.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &default_milestones(env),
        &ReleaseAuthorization::ClientOnly,
    );
    let total = super::total_milestone_amount();
    StellarAssetClient::new(env, token).mint(&client_addr, &total);
    assert!(client.deposit_funds(&contract_id, &client_addr, &total));
    for index in 0..3u32 {
        assert!(client.approve_milestone_release(&contract_id, &client_addr, &index));
    }
    (client_addr, freelancer_addr, contract_id)
}

/// Release every milestone of a pre-approved contract, completing it.
fn release_all(client: &crate::EscrowClient<'_>, contract_id: u32, client_addr: &Address) {
    for index in 0..3u32 {
        assert!(
            client
                .try_release_milestone(&contract_id, client_addr, &index)
                .is_ok(),
            "release rejected in test setup"
        );
    }
    assert_eq!(
        client.get_contract(&contract_id).status,
        ContractStatus::Completed
    );
}

/// Collect the payloads of every `("rep_crdt", "granted")` event emitted by the
/// escrow contract, decoded as `(freelancer, credit_count, timestamp)`.
fn credit_accrual_events(env: &Env, escrow: &Address) -> Vec<(Address, i128, u64)> {
    let mut out = Vec::new(env);
    let topic = Symbol::new(env, "rep_crdt");
    let subtopic = Symbol::new(env, "granted");
    for (addr, topics, data) in env.events().all().iter() {
        if &addr != escrow || topics.len() != 2 {
            continue;
        }
        let first: Symbol = topics.get(0).unwrap().try_into_val(env).unwrap();
        let second: Symbol = topics.get(1).unwrap().try_into_val(env).unwrap();
        if first != topic || second != subtopic {
            continue;
        }
        let payload: (Address, i128, u64) = data.try_into_val(env).unwrap();
        out.push_back(payload);
    }
    out
}

// ── Pure policy ───────────────────────────────────────────────────────────────

#[test]
fn pure_accrual_adds_exactly_one_and_rejects_the_ceiling() {
    assert_eq!(REPUTATION_CREDIT_INCREMENT, 1);

    assert_eq!(accrue_pending_credit(0), Some(1));
    assert_eq!(accrue_pending_credit(41), Some(42));
    assert_eq!(
        accrue_pending_credit(MAX_PENDING_REPUTATION_CREDITS - 1),
        Some(MAX_PENDING_REPUTATION_CREDITS)
    );

    // At the ceiling, past it, and below zero the accrual is rejected rather
    // than wrapped, saturated, or clamped.
    assert_eq!(accrue_pending_credit(MAX_PENDING_REPUTATION_CREDITS), None);
    assert_eq!(
        accrue_pending_credit(MAX_PENDING_REPUTATION_CREDITS + 1),
        None
    );
    assert_eq!(accrue_pending_credit(-1), None);
    assert_eq!(accrue_pending_credit(i128::MIN), None);
    assert_eq!(accrue_pending_credit(i128::MAX), None);
}

#[test]
fn pure_consumption_removes_exactly_one_and_rejects_empty_ledgers() {
    assert_eq!(consume_pending_credit(1), Some(0));
    assert_eq!(consume_pending_credit(42), Some(41));
    assert_eq!(
        consume_pending_credit(MAX_PENDING_REPUTATION_CREDITS),
        Some(MAX_PENDING_REPUTATION_CREDITS - 1)
    );

    // An empty ledger, a negative ledger, and an over-ceiling ledger all fail
    // the same way instead of underflowing.
    assert_eq!(consume_pending_credit(0), None);
    assert_eq!(consume_pending_credit(-1), None);
    assert_eq!(consume_pending_credit(i128::MIN), None);
    assert_eq!(
        consume_pending_credit(MAX_PENDING_REPUTATION_CREDITS + 1),
        None
    );
}

#[test]
fn ledger_validity_range_is_the_single_source_of_truth() {
    assert!(is_valid_pending_credit_ledger(0));
    assert!(is_valid_pending_credit_ledger(1));
    assert!(is_valid_pending_credit_ledger(
        MAX_PENDING_REPUTATION_CREDITS
    ));
    assert!(!is_valid_pending_credit_ledger(-1));
    assert!(!is_valid_pending_credit_ledger(
        MAX_PENDING_REPUTATION_CREDITS + 1
    ));

    // Every value the accrual produces is itself a legal ledger value, and
    // consuming one credit always walks the ledger back by exactly one step.
    let mut value = 0_i128;
    for _ in 0..64 {
        let next = accrue_pending_credit(value).expect("accrual below the ceiling must succeed");
        assert!(is_valid_pending_credit_ledger(next));
        assert_eq!(consume_pending_credit(next), Some(value));
        value = next;
    }
    assert_eq!(value, 64);
}

#[test]
fn accrual_and_consumption_meet_exactly_at_the_ceiling() {
    let at_ceiling = MAX_PENDING_REPUTATION_CREDITS;
    assert_eq!(consume_pending_credit(at_ceiling), Some(at_ceiling - 1));
    assert_eq!(
        accrue_pending_credit(at_ceiling - 1),
        Some(at_ceiling),
        "accrual back to the ceiling must be allowed"
    );
    assert_eq!(accrue_pending_credit(at_ceiling), None);
}

// ── Accrual through the contract: milestone release ───────────────────────────

#[test]
fn completion_accrues_exactly_one_credit_and_emits_one_accrual_event() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, token) = register_client_with_token(&env);
    let (client_addr, freelancer, contract_id) = funded_approved_contract(&env, &client, &token);

    assert_eq!(raw_ledger(&env, &client.address, &freelancer), 0);

    release_all(&client, contract_id, &client_addr);

    assert_eq!(raw_ledger(&env, &client.address, &freelancer), 1);
    assert_eq!(client.get_pending_reputation_credits(&freelancer), 1);

    // Exactly one observable accrual, reporting the post-accrual count.
    let events = credit_accrual_events(&env, &client.address);
    assert_eq!(events.len(), 1, "one completion must emit one accrual event");
    let (reported_freelancer, reported_count, _timestamp) = events.get(0).unwrap();
    assert_eq!(reported_freelancer, freelancer);
    assert_eq!(reported_count, 1);
}

#[test]
fn accrual_at_the_ceiling_is_rejected_without_mutating_state() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, token) = register_client_with_token(&env);
    let (client_addr, freelancer, contract_id) = funded_approved_contract(&env, &client, &token);

    seed_ledger(
        &env,
        &client.address,
        &freelancer,
        MAX_PENDING_REPUTATION_CREDITS,
    );

    assert!(client
        .try_release_milestone(&contract_id, &client_addr, &0)
        .is_ok());
    assert!(client
        .try_release_milestone(&contract_id, &client_addr, &1)
        .is_ok());

    // The final release would push the ledger past the ceiling, so it aborts
    // with a typed error instead of wrapping.
    assert_contract_error(
        client.try_release_milestone(&contract_id, &client_addr, &2),
        Error::PotentialOverflow,
    );

    // Nothing moved: the ledger is byte-identical, the contract did not reach
    // `Completed`, and no accrual was advertised to indexers.
    assert_eq!(
        raw_ledger(&env, &client.address, &freelancer),
        MAX_PENDING_REPUTATION_CREDITS
    );
    assert_ne!(
        client.get_contract(&contract_id).status,
        ContractStatus::Completed
    );
    assert_eq!(credit_accrual_events(&env, &client.address).len(), 0);

    // A retry on the untouched state fails in exactly the same way.
    assert_contract_error(
        client.try_release_milestone(&contract_id, &client_addr, &2),
        Error::PotentialOverflow,
    );
    assert_eq!(
        raw_ledger(&env, &client.address, &freelancer),
        MAX_PENDING_REPUTATION_CREDITS
    );

    // Once the ledger is back inside its legal range the *same* call succeeds,
    // so a rejected accrual is recoverable rather than permanently stuck.
    seed_ledger(
        &env,
        &client.address,
        &freelancer,
        MAX_PENDING_REPUTATION_CREDITS - 1,
    );
    assert!(client
        .try_release_milestone(&contract_id, &client_addr, &2)
        .is_ok());
    assert_eq!(
        client.get_contract(&contract_id).status,
        ContractStatus::Completed
    );
    assert_eq!(
        raw_ledger(&env, &client.address, &freelancer),
        MAX_PENDING_REPUTATION_CREDITS
    );
}

#[test]
fn accrual_one_below_the_ceiling_lands_exactly_on_the_ceiling() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, token) = register_client_with_token(&env);
    let (client_addr, freelancer, contract_id) = funded_approved_contract(&env, &client, &token);

    seed_ledger(
        &env,
        &client.address,
        &freelancer,
        MAX_PENDING_REPUTATION_CREDITS - 1,
    );

    release_all(&client, contract_id, &client_addr);

    assert_eq!(
        raw_ledger(&env, &client.address, &freelancer),
        MAX_PENDING_REPUTATION_CREDITS
    );
    assert_eq!(
        client.get_pending_reputation_credits(&freelancer),
        MAX_PENDING_REPUTATION_CREDITS
    );
}

// ── Accrual through the contract: partial-refund completion ───────────────────

#[test]
fn completion_via_partial_refund_accrues_exactly_one_credit() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, token) = register_client_with_token(&env);
    let (client_addr, freelancer, contract_id) = funded_approved_contract(&env, &client, &token);

    assert!(client
        .try_release_milestone(&contract_id, &client_addr, &0)
        .is_ok());
    assert!(client.refund_unreleased_milestones(&contract_id, &vec![&env, 1_u32, 2]) > 0);

    // Settling a contract with a mix of releases and refunds still completes it,
    // so the refund branch must accrue the very same single credit as the
    // release branch — this is one of the paths that used to drift.
    assert_eq!(
        client.get_contract(&contract_id).status,
        ContractStatus::Completed
    );
    assert_eq!(raw_ledger(&env, &client.address, &freelancer), 1);
    assert_eq!(credit_accrual_events(&env, &client.address).len(), 1);
}

// ── Consumption through the contract: issue_reputation ────────────────────────

/// Complete a funded contract between the supplied participants, so a single
/// freelancer can accumulate credits from several clients.
fn complete_contract_for(
    env: &Env,
    client: &crate::EscrowClient<'_>,
    token: &Address,
    client_addr: &Address,
    freelancer_addr: &Address,
) -> u32 {
    let contract_id = client.create_contract(
        client_addr,
        freelancer_addr,
        &None,
        &default_milestones(env),
        &ReleaseAuthorization::ClientOnly,
    );
    let total = super::total_milestone_amount();
    StellarAssetClient::new(env, token).mint(client_addr, &total);
    assert!(client.deposit_funds(&contract_id, client_addr, &total));
    for index in 0..3u32 {
        assert!(client.approve_milestone_release(&contract_id, client_addr, &index));
        assert!(client.release_milestone(&contract_id, client_addr, &index));
    }
    assert_eq!(
        client.get_contract(&contract_id).status,
        ContractStatus::Completed
    );
    contract_id
}

#[test]
fn consumption_on_an_empty_ledger_is_rejected_without_side_effects() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, token) = register_client_with_token(&env);
    let (client_addr, freelancer, contract_id) = complete_contract_funded(&env, &client, &token);

    // Model a ledger that has already been drained, or was never credited.
    seed_ledger(&env, &client.address, &freelancer, 0);

    assert_contract_error(
        client.try_issue_reputation(&contract_id, &client_addr, &5, &comment(&env)),
        Error::NotCompleted,
    );

    // The entire call rolled back: the ledger is untouched, no reputation record
    // exists, and the contract is not flagged as rated.
    assert_eq!(raw_ledger(&env, &client.address, &freelancer), 0);
    assert!(client.get_reputation(&freelancer).is_none());
    assert!(client.get_reputation_comment(&contract_id).is_none());
    assert!(!client.get_contract(&contract_id).reputation_issued);

    // A retry re-observes the same state, so it reproduces the same error rather
    // than reporting `ReputationAlreadyIssued` from a half-applied attempt.
    assert_contract_error(
        client.try_issue_reputation(&contract_id, &client_addr, &5, &comment(&env)),
        Error::NotCompleted,
    );
    assert_eq!(raw_ledger(&env, &client.address, &freelancer), 0);
}

#[test]
fn consumption_from_a_corrupted_ledger_is_rejected_identically() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, token) = register_client_with_token(&env);
    let (client_addr, freelancer, contract_id) = complete_contract_funded(&env, &client, &token);

    for corrupted in [-1_i128, -42, MAX_PENDING_REPUTATION_CREDITS + 1, i128::MAX] {
        seed_ledger(&env, &client.address, &freelancer, corrupted);

        assert_contract_error(
            client.try_issue_reputation(&contract_id, &client_addr, &5, &comment(&env)),
            Error::NotCompleted,
        );

        // Out-of-range state is never silently repaired or partially consumed.
        assert_eq!(raw_ledger(&env, &client.address, &freelancer), corrupted);
        assert!(client.get_reputation(&freelancer).is_none());
    }
}

#[test]
fn each_completed_contract_funds_exactly_one_rating() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, token) = register_client_with_token(&env);
    let freelancer = Address::generate(&env);
    let first_client = Address::generate(&env);
    let second_client = Address::generate(&env);

    let first = complete_contract_for(&env, &client, &token, &first_client, &freelancer);
    let second = complete_contract_for(&env, &client, &token, &second_client, &freelancer);
    assert_eq!(client.get_pending_reputation_credits(&freelancer), 2);

    assert!(client.issue_reputation(&first, &first_client, &5, &comment(&env)));
    assert_eq!(raw_ledger(&env, &client.address, &freelancer), 1);

    assert!(client.issue_reputation(&second, &second_client, &4, &comment(&env)));
    assert_eq!(raw_ledger(&env, &client.address, &freelancer), 0);
    assert_eq!(client.get_pending_reputation_credits(&freelancer), 0);

    let rep = client.get_reputation(&freelancer).unwrap();
    assert_eq!(rep.completed_contracts, 2);
    assert_eq!(rep.last_rating, 4);
}

// ── Durability of the credit ledger ───────────────────────────────────────────

#[test]
fn credit_ledger_lifetime_is_renewed_by_accrual_consumption_and_reads() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, token) = register_client_with_token(&env);
    let (client_addr, freelancer, contract_id) = funded_approved_contract(&env, &client, &token);

    release_all(&client, contract_id, &client_addr);

    let key = credit_key(&freelancer);
    let ledger_ttl = || -> u32 {
        env.as_contract(&client.address, || {
            env.storage().persistent().get_ttl(&key)
        })
    };
    let threshold = ttl::PERSISTENT_BUMP_THRESHOLD;

    // An earned credit is a durable claim: accrual leaves the entry with at
    // least the standard persistent lifetime instead of letting it decay.
    assert!(ledger_ttl() >= threshold);

    // Reading the balance renews it as well.
    assert_eq!(client.get_pending_reputation_credits(&freelancer), 1);
    assert!(ledger_ttl() >= threshold);

    // Consumption renews the (now empty) entry too, so the ledger can still be
    // observed at zero instead of disappearing on the next eviction sweep.
    assert!(client.issue_reputation(&contract_id, &client_addr, &5, &comment(&env)));
    assert!(ledger_ttl() >= threshold);
    assert_eq!(client.get_pending_reputation_credits(&freelancer), 0);
}
