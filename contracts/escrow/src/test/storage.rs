use super::{
    assert_contract_error, complete_contract, create_contract, default_milestones,
    generated_participants, register_client, total_milestone_amount, MILESTONE_ONE, MILESTONE_THREE,
    MILESTONE_TWO,
};
use crate::{ContractStatus, DataKey, EscrowError, ReadinessChecklist, ReleaseAuthorization};
use soroban_sdk::{testutils::Address as _, Address, Env};

// ─── Initialized / Admin ──────────────────────────────────────────────────────

#[test]
fn initialized_written_on_initialize() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let admin = Address::generate(&env);

    assert!(client.initialize(&admin));

    env.as_contract(&client.address, || {
        let v: bool = env
            .storage()
            .persistent()
            .get(&DataKey::Initialized)
            .unwrap();
        assert!(v);
    });
}

#[test]
fn admin_written_on_initialize() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let admin = Address::generate(&env);

    client.initialize(&admin);

    env.as_contract(&client.address, || {
        let stored: Address = env.storage().persistent().get(&DataKey::Admin).unwrap();
        assert_eq!(stored, admin);
    });
}

#[test]
fn double_initialize_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let admin = Address::generate(&env);

    client.initialize(&admin);
    assert_contract_error(
        client.try_initialize(&admin),
        EscrowError::AlreadyInitialized,
    );
}

// ─── Paused ───────────────────────────────────────────────────────────────────

#[test]
fn paused_written_by_pause_and_cleared_by_unpause() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let admin = Address::generate(&env);
    client.initialize(&admin);

    client.pause();
    env.as_contract(&client.address, || {
        let v: bool = env
            .storage()
            .persistent()
            .get(&DataKey::Paused)
            .unwrap_or(false);
        assert!(v);
    });

    client.unpause();
    env.as_contract(&client.address, || {
        let v: bool = env
            .storage()
            .persistent()
            .get(&DataKey::Paused)
            .unwrap_or(false);
        assert!(!v);
    });
}

#[test]
fn paused_blocks_create_contract() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let admin = Address::generate(&env);
    client.initialize(&admin);
    client.pause();

    let (c, f) = generated_participants(&env);
    assert_contract_error(
        client.try_create_contract(
            &c,
            &f,
            &None,
            &default_milestones(&env),
            &ReleaseAuthorization::ClientOnly,
        ),
        EscrowError::ContractPaused,
    );
}

#[test]
fn paused_blocks_deposit_funds() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let (client_addr, _, id) = create_contract(&env, &client);
    client.pause();

    assert_contract_error(
        client.try_deposit_funds(&id, &client_addr, &total_milestone_amount()),
        EscrowError::ContractPaused,
    );
}

#[test]
fn paused_blocks_release_milestone() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let (client_addr, _, id) = create_contract(&env, &client);
    client.deposit_funds(&id, &client_addr, &total_milestone_amount());
    client.pause();

    assert_contract_error(
        client.try_release_milestone(&id, &client_addr, &0),
        EscrowError::ContractPaused,
    );
}

#[test]
fn paused_blocks_cancel_contract() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let (client_addr, _, id) = create_contract(&env, &client);
    client.pause();

    assert_contract_error(
        client.try_cancel_contract(&id, &client_addr),
        EscrowError::ContractPaused,
    );
}

#[test]
fn read_only_queries_not_blocked_by_pause() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let (_, _, id) = create_contract(&env, &client);
    client.pause();

    let record = client.get_contract(&id);
    assert_eq!(record.status, ContractStatus::Created);
    assert!(client.is_paused());
}

// ─── Emergency ────────────────────────────────────────────────────────────────

#[test]
fn emergency_written_by_activate_and_cleared_by_resolve() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let admin = Address::generate(&env);
    client.initialize(&admin);

    client.activate_emergency_pause();
    env.as_contract(&client.address, || {
        let v: bool = env
            .storage()
            .persistent()
            .get(&DataKey::Emergency)
            .unwrap_or(false);
        assert!(v);
    });

    client.resolve_emergency();
    env.as_contract(&client.address, || {
        let v: bool = env
            .storage()
            .persistent()
            .get(&DataKey::Emergency)
            .unwrap_or(false);
        assert!(!v);
    });
}

#[test]
fn unpause_blocked_while_emergency_active() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let admin = Address::generate(&env);
    client.initialize(&admin);

    client.activate_emergency_pause();
    assert_contract_error(client.try_unpause(), EscrowError::EmergencyActive);
}

// ─── Contract / NextContractId ────────────────────────────────────────────────

#[test]
fn contract_written_on_create_and_readable() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let (c, f) = generated_participants(&env);

    let id = client.create_contract(
        &c,
        &f,
        &None,
        &default_milestones(&env),
        &ReleaseAuthorization::ClientOnly,
    );

    let record = client.get_contract(&id);
    assert_eq!(record.client, c);
    assert_eq!(record.freelancer, f);
    assert_eq!(record.status, ContractStatus::Created);
}

#[test]
fn next_contract_id_increments_per_contract() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let (_, _, id1) = create_contract(&env, &client);
    let (_, _, id2) = create_contract(&env, &client);
    assert_eq!(id2, id1 + 1);
}

#[test]
fn get_contract_fails_for_unknown_id() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    assert_contract_error(
        client.try_get_contract(&9999),
        EscrowError::ContractNotFound,
    );
}

// ─── Milestone released flag (milestone vector) ───────────────────────────────

/// `release_milestone` sets `ms.released = true` in the persisted milestone
/// vector. There is no separate `DataKey::MilestoneReleased` storage key; the
/// vector is the single source of truth for released state.
#[test]
fn milestone_released_flag_set_in_vector_on_release() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let (client_addr, _, id) = create_contract(&env, &client);
    client.deposit_funds(&id, &client_addr, &total_milestone_amount());
    client.approve_milestone_release(&id, &client_addr, &0);
    client.release_milestone(&id, &client_addr, &0);

    let milestones = client.get_milestones(&id);
    assert!(milestones.get(0).unwrap().released, "index 0 must be released");
    assert!(!milestones.get(1).unwrap().released, "index 1 must not be released");
    assert!(!milestones.get(2).unwrap().released, "index 2 must not be released");
}

#[test]
fn double_release_same_milestone_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let (client_addr, _, id) = create_contract(&env, &client);
    client.deposit_funds(&id, &client_addr, &total_milestone_amount());
    client.release_milestone(&id, &client_addr, &0);

    assert_contract_error(
        client.try_release_milestone(&id, &client_addr, &0),
        EscrowError::MilestoneAlreadyReleased,
    );
}

#[test]
fn release_out_of_bounds_milestone_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let (client_addr, _, id) = create_contract(&env, &client);
    client.deposit_funds(&id, &client_addr, &total_milestone_amount());

    assert_contract_error(
        client.try_release_milestone(&id, &client_addr, &99),
        EscrowError::IndexOutOfBounds,
    );
}

// ─── ReputationIssued / Reputation / PendingReputationCredits ─────────────────

#[test]
fn reputation_issued_written_and_reputation_updated() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let (c, f, id) = complete_contract(&env, &client);
    client.issue_reputation(&id, &c, &f, &5);

    env.as_contract(&client.address, || {
        let issued: bool = env
            .storage()
            .persistent()
            .get(&DataKey::ReputationIssued(id))
            .unwrap_or(false);
        assert!(issued);
    });

    let rep = client.get_reputation(&f).unwrap();
    assert_eq!(rep.completed_contracts, 1);
    assert_eq!(rep.total_rating, 5);
    assert_eq!(rep.last_rating, 5);
}

#[test]
fn double_issue_reputation_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let (c, f, id) = complete_contract(&env, &client);
    client.issue_reputation(&id, &c, &f, &4);

    assert!(client.issue_reputation(&id, &c, &f, &4));
}

#[test]
fn pending_reputation_credits_incremented_on_completion() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let (_, f, _) = complete_contract(&env, &client);
    assert_eq!(client.get_pending_reputation_credits(&f), 1);
}

#[test]
fn pending_reputation_credits_decremented_on_issue() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let (c, f, id) = complete_contract(&env, &client);
    assert_eq!(client.get_pending_reputation_credits(&f), 1);

    client.issue_reputation(&id, &c, &f, &3);
    assert_eq!(client.get_pending_reputation_credits(&f), 0);
}

#[test]
fn reputation_not_issuable_before_completion() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let (c, f) = generated_participants(&env);
    let id = client.create_contract(
        &c,
        &f,
        &None,
        &default_milestones(&env),
        &ReleaseAuthorization::ClientOnly,
    );

    assert_contract_error(
        client.try_issue_reputation(&id, &c, &f, &5),
        EscrowError::NotCompleted,
    );
}

#[test]
fn reputation_requires_client_caller() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let (c, f, id) = complete_contract(&env, &client);
    let stranger = Address::generate(&env);

    assert_contract_error(
        client.try_issue_reputation(&id, &stranger, &f, &5),
        EscrowError::UnauthorizedRole,
    );
}

// ─── ReadinessChecklist ───────────────────────────────────────────────────────

#[test]
fn readiness_checklist_initialized_flag_set_by_initialize() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let admin = Address::generate(&env);

    client.initialize(&admin);

    env.as_contract(&client.address, || {
        let checklist: ReadinessChecklist = env
            .storage()
            .persistent()
            .get(&DataKey::ReadinessChecklist)
            .unwrap();
        assert!(checklist.initialized);
        assert!(!checklist.governed_params_set);
    });
}

#[test]
fn readiness_checklist_emergency_flag_set_by_activate() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let admin = Address::generate(&env);
    client.initialize(&admin);

    client.activate_emergency_pause();

    env.as_contract(&client.address, || {
        let checklist: ReadinessChecklist = env
            .storage()
            .persistent()
            .get(&DataKey::ReadinessChecklist)
            .unwrap();
        assert!(checklist.emergency_controls_enabled);
    });
}

// ─── Accounting invariant ─────────────────────────────────────────────────────

#[test]
fn released_amount_tracks_milestone_amounts() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let (client_addr, _, id) = create_contract(&env, &client);
    client.deposit_funds(&id, &client_addr, &total_milestone_amount());

    client.release_milestone(&id, &client_addr, &0);
    let r = client.get_contract(&id);
    assert_eq!(r.released_amount, MILESTONE_ONE);

    client.release_milestone(&id, &client_addr, &1);
    let r = client.get_contract(&id);
    assert_eq!(r.released_amount, MILESTONE_ONE + MILESTONE_TWO);

    client.release_milestone(&id, &client_addr, &2);
    let r = client.get_contract(&id);
    assert_eq!(r.released_amount, total_milestone_amount());
    assert_eq!(r.status, ContractStatus::Completed);
}

// ─── get_milestone single-index reader (issue #649) ───────────────────────────

#[test]
fn get_milestone_index_zero_returns_first_milestone() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    // Default contract has three milestones: ONE, TWO, THREE.
    let (_client_addr, _, id) = create_contract(&env, &client);

    let m = client
        .get_milestone(&id, &0u32)
        .expect("index 0 is in bounds");
    assert_eq!(m.amount, MILESTONE_ONE);
    // It must match the entry returned by the full-vector reader.
    assert_eq!(m, client.get_milestones(&id).get(0).unwrap());
}

#[test]
fn get_milestone_last_valid_index_returns_last_milestone() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let (_client_addr, _, id) = create_contract(&env, &client);

    let milestones = client.get_milestones(&id);
    let last = milestones.len() - 1;
    let m = client
        .get_milestone(&id, &last)
        .expect("last index is in bounds");
    assert_eq!(m.amount, MILESTONE_THREE);
    assert_eq!(m, milestones.get(last).unwrap());
}

#[test]
fn get_milestone_out_of_bounds_returns_none() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let (_client_addr, _, id) = create_contract(&env, &client);

    let len = client.get_milestones(&id).len();
    // One past the last valid index must return None, not panic.
    assert!(client.get_milestone(&id, &len).is_none());
    assert!(client.get_milestone(&id, &(len + 5)).is_none());
}

#[test]
fn get_milestone_unknown_contract_panics_contract_not_found() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    // No contract has been created; id 999 was never allocated.
    assert_contract_error(
        client.try_get_milestone(&999u32, &0u32),
        EscrowError::ContractNotFound,
    );
}

#[test]
fn get_milestone_zero_contract_id_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    assert_contract_error(
        client.try_get_milestone(&0u32, &0u32),
        EscrowError::ContractNotFound,
    );
}

#[test]
fn get_milestone_max_contract_id_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    assert_contract_error(
        client.try_get_milestone(&u32::MAX, &0u32),
        EscrowError::ContractNotFound,
    );
}

#[test]
fn deposit_exceeding_total_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let (client_addr, _, id) = create_contract(&env, &client);
    assert_contract_error(
        client.try_deposit_funds(&id, &client_addr, &(total_milestone_amount() + 1)),
        EscrowError::ExactDepositRequired,
    );
}

#[test]
fn deposit_zero_amount_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let (client_addr, _, id) = create_contract(&env, &client);
    assert_contract_error(
        client.try_deposit_funds(&id, &client_addr, &0),
        EscrowError::ExactDepositRequired,
    );
}

#[test]
fn deposit_exact_total_boundary_accepted() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let (client_addr, _, id) = create_contract(&env, &client);
    assert!(client.try_deposit_funds(&id, &client_addr, &total_milestone_amount()).is_ok());

    let record = client.get_contract(&id);
    assert_eq!(record.funded_amount, total_milestone_amount());
}

// ─── Storage Input Bounds Validation (#899) ──────────────────────────────

#[test]
fn storage_entrypoints_reject_zero_contract_id() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let admin = Address::generate(&env);
    client.initialize(&admin);

    assert_contract_error(client.try_get_contract(&0u32), EscrowError::ContractNotFound);
    assert_contract_error(
        client.try_get_contract_summary(&0u32),
        EscrowError::ContractNotFound,
    );
    assert_contract_error(
        client.try_get_milestones(&0u32),
        EscrowError::ContractNotFound,
    );
    assert_contract_error(
        client.try_get_milestone(&0u32, &0u32),
        EscrowError::ContractNotFound,
    );
    assert_contract_error(
        client.try_get_refundable_balance(&0u32),
        EscrowError::ContractNotFound,
    );
    assert_contract_error(
        client.try_set_arbiter(&0u32, &admin, &None),
        EscrowError::InvalidContractId,
    );
}

#[test]
fn storage_entrypoints_reject_max_contract_id() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let admin = Address::generate(&env);
    client.initialize(&admin);

    assert_contract_error(
        client.try_get_milestones(&u32::MAX),
        EscrowError::ContractNotFound,
    );
    assert_contract_error(
        client.try_get_milestone(&u32::MAX, &0u32),
        EscrowError::ContractNotFound,
    );
    assert_contract_error(
        client.try_get_refundable_balance(&u32::MAX),
        EscrowError::ContractNotFound,
    );
    assert_contract_error(
        client.try_set_arbiter(&u32::MAX, &admin, &None),
        EscrowError::ContractNotFound,
    );
}

#[test]
fn storage_entrypoints_boundary_contract_id_valid() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let admin = Address::generate(&env);
    client.initialize(&admin);

    // Min valid contract ID 1 (unallocated) returns ContractNotFound, not InvalidContractId.
    assert_contract_error(client.try_get_contract(&1u32), EscrowError::ContractNotFound);
    assert_contract_error(
        client.try_get_contract_summary(&1u32),
        EscrowError::ContractNotFound,
    );

    // Max u32 contract ID (unallocated) returns ContractNotFound, not InvalidContractId.
    assert_contract_error(
        client.try_get_contract(&u32::MAX),
        EscrowError::ContractNotFound,
    );
    assert_contract_error(
        client.try_get_contract_summary(&u32::MAX),
        EscrowError::ContractNotFound,
    );
}

// ─── State invariant protection (#900) ───────────────────────────────────────
//
// These tests pin the invariants that the escrow storage layer must uphold
// across every entry point. They are intentionally written against the public
// client surface so that any future refactor of `storage_validation.rs` that
// silently weakens a check will fail here.

/// Invariant: `released_amount` must never exceed the sum of milestone
/// amounts, and the contract must never transition to `Completed` until
/// every milestone has been released.
#[test]
fn released_amount_never_exceeds_total_milestone_amount() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let (client_addr, _, id) = create_contract(&env, &client);
    client.deposit_funds(&id, &client_addr, &total_milestone_amount());

    let total = total_milestone_amount();
    for i in 0..3u32 {
        client.release_milestone(&id, &client_addr, &i);
        let r = client.get_contract(&id);
        assert!(
            r.released_amount <= total,
            "released_amount {} exceeded total {} after releasing index {}",
            r.released_amount,
            total,
            i
        );
    }
    assert_eq!(client.get_contract(&id).released_amount, total);
}

/// Invariant: a contract cannot be marked `Completed` while any milestone
/// remains unreleased. Releasing the final milestone is the only transition
/// that flips the status.
#[test]
fn contract_not_completed_until_all_milestones_released() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let (client_addr, _, id) = create_contract(&env, &client);
    client.deposit_funds(&id, &client_addr, &total_milestone_amount());

    client.release_milestone(&id, &client_addr, &0);
    assert_eq!(client.get_contract(&id).status, ContractStatus::Funded);

    client.release_milestone(&id, &client_addr, &1);
    assert_eq!(client.get_contract(&id).status, ContractStatus::Funded);

    client.release_milestone(&id, &client_addr, &2);
    assert_eq!(client.get_contract(&id).status, ContractStatus::Completed);
}

/// Invariant: releasing a milestone must be idempotent-safe — a second
/// attempt on the same index must be rejected without mutating
/// `released_amount` or the milestone vector.
#[test]
fn double_release_does_not_mutate_released_amount() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let (client_addr, _, id) = create_contract(&env, &client);
    client.deposit_funds(&id, &client_addr, &total_milestone_amount());
    client.release_milestone(&id, &client_addr, &0);

    let before = client.get_contract(&id).released_amount;
    assert_contract_error(
        client.try_release_milestone(&id, &client_addr, &0),
        EscrowError::MilestoneAlreadyReleased,
    );
    let after = client.get_contract(&id).released_amount;
    assert_eq!(before, after, "failed release must not mutate released_amount");
}

/// Invariant: `ReputationIssued` is a one-shot latch. A failed second issue
/// must not increment the reputation counters or decrement pending credits.
#[test]
fn double_issue_reputation_does_not_mutate_reputation() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let (c, f, id) = complete_contract(&env, &client);
    client.issue_reputation(&id, &c, &f, &5);

    let before = client.get_reputation(&f).unwrap();
    let pending_before = client.get_pending_reputation_credits(&f);

    assert_contract_error(
        client.try_issue_reputation(&id, &c, &f, &5),
        EscrowError::ReputationAlreadyIssued,
    );

    let after = client.get_reputation(&f).unwrap();
    assert_eq!(before.completed_contracts, after.completed_contracts);
    assert_eq!(before.total_rating, after.total_rating);
    assert_eq!(before.last_rating, after.last_rating);
    assert_eq!(pending_before, client.get_pending_reputation_credits(&f));
}

/// Invariant: pending reputation credits are monotonically non-increasing
/// once a contract completes; issuing reputation must never push the counter
/// below zero or above the number of completed contracts.
#[test]
fn pending_reputation_credits_never_underflow() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let (c, f, id) = complete_contract(&env, &client);
    assert_eq!(client.get_pending_reputation_credits(&f), 1);

    client.issue_reputation(&id, &c, &f, &4);
    assert_eq!(client.get_pending_reputation_credits(&f), 0);

    // A second issue attempt must be rejected and must not underflow.
    assert_contract_error(
        client.try_issue_reputation(&id, &c, &f, &4),
        EscrowError::ReputationAlreadyIssued,
    );
    assert_eq!(client.get_pending_reputation_credits(&f), 0);
}

/// Invariant: `NextContractId` is strictly monotonic. Repeated creates must
/// produce strictly increasing, gap-free ids even when interleaved with
/// read-only queries.
#[test]
fn next_contract_id_is_strictly_monotonic() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let (_, _, id1) = create_contract(&env, &client);
    let _ = client.get_contract(&id1);
    let (_, _, id2) = create_contract(&env, &client);
    let _ = client.get_contract(&id2);
    let (_, _, id3) = create_contract(&env, &client);

    assert!(id1 < id2, "ids must strictly increase");
    assert!(id2 < id3, "ids must strictly increase");
    assert_eq!(id2, id1 + 1);
    assert_eq!(id3, id2 + 1);
}

/// Invariant: pausing must not corrupt persisted state. A paused contract
/// must retain its milestone vector, released flags, and released_amount
/// exactly as they were before the pause.
#[test]
fn pause_preserves_persisted_state() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let (client_addr, _, id) = create_contract(&env, &client);
    client.deposit_funds(&id, &client_addr, &total_milestone_amount());
    client.release_milestone(&id, &client_addr, &0);

    let before = client.get_contract(&id);
    let milestones_before = client.get_milestones(&id);

    client.pause();
    client.unpause();

    let after = client.get_contract(&id);
    let milestones_after = client.get_milestones(&id);

    assert_eq!(before.released_amount, after.released_amount);
    assert_eq!(before.status, after.status);
    assert_eq!(milestones_before.len(), milestones_after.len());
    for i in 0..milestones_before.len() {
        assert_eq!(
            milestones_before.get(i).unwrap(),
            milestones_after.get(i).unwrap()
        );
    }
}

/// Invariant: emergency pause must not silently clear the regular pause
/// flag or the readiness checklist. Resolving the emergency must restore
/// the pre-emergency pause state exactly.
#[test]
fn emergency_pause_preserves_pause_and_checklist_state() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let admin = Address::generate(&env);
    client.initialize(&admin);

    client.pause();
    assert!(client.is_paused());

    client.activate_emergency_pause();
    assert!(client.is_paused(), "regular pause must survive emergency");

    client.resolve_emergency();
    assert!(client.is_paused(), "regular pause must survive resolve");

    env.as_contract(&client.address, || {
        let checklist: ReadinessChecklist = env
            .storage()
            .persistent()
            .get(&DataKey::ReadinessChecklist)
            .unwrap();
        assert!(checklist.initialized);
        assert!(checklist.emergency_controls_enabled);
    });
}

/// Invariant: out-of-bounds milestone reads must not panic and must not
/// mutate any persisted state. Repeated reads at the boundary must be
/// stable.
#[test]
fn out_of_bounds_milestone_read_is_stable_and_side_effect_free() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let (_, _, id) = create_contract(&env, &client);
    let len = client.get_milestones(&id).len();

    let before = client.get_contract(&id);
    for _ in 0..3 {
        assert!(client.get_milestone(&id, &len).is_none());
        assert!(client.get_milestone(&id, &(len + 100)).is_none());
    }
    let after = client.get_contract(&id);
    assert_eq!(before.released_amount, after.released_amount);
    assert_eq!(before.status, after.status);
}

/// Invariant: a rejected deposit must not partially credit the contract.
/// The `released_amount` and status must be identical before and after a
/// failed deposit.
#[test]
fn failed_deposit_does_not_mutate_contract_state() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let (client_addr, _, id) = create_contract(&env, &client);
    let before = client.get_contract(&id);

    assert_contract_error(
        client.try_deposit_funds(&id, &client_addr, &(total_milestone_amount() + 1)),
        EscrowError::ExactDepositRequired,
    );

    let after = client.get_contract(&id);
    assert_eq!(before.released_amount, after.released_amount);
    assert_eq!(before.status, after.status);
    assert_eq!(before.client, after.client);
    assert_eq!(before.freelancer, after.freelancer);
}

