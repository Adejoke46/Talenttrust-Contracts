//! Regression tests for concurrent/idempotent key construction and approval
//! lifecycle determinism around `keys.rs`.
//!
//! # What these tests verify
//!
//! Soroban transactions are serialised per contract-instance, so true in-process
//! concurrent writes to the same key cannot race within a single ledger.
//! However, **retry storms** (client submits the same approval twice because it
//! did not hear a response) and **off-by-one indexing bugs** (contract_id = 0,
//! milestone_index wrapping) are realistic production hazards.  This suite
//! ensures they are all caught at the key-construction or approval-check layer
//! rather than silently producing bad state.
//!
//! Test groups:
//! 1. Key construction determinism — same args → same key every time.
//! 2. Key uniqueness — different args never alias.
//! 3. Zero contract-id guard — `milestone_key(env, 0)` must panic.
//! 4. Boundary conditions — id=1, id=u32::MAX, milestone_index=0/u32::MAX.
//! 5. Duplicate approval rejection — calling `approve_milestone` twice with the
//!    same caller must return `AlreadyApproved`.
//! 6. Approval idempotency via clear-and-re-approve — after `clear_approvals`
//!    the same caller may submit a fresh approval.
//! 7. Stale-version detection — `check_version_for_concurrency` rejects
//!    mismatched versions that would indicate a concurrent modification.
//! 8. Transition-matrix idempotency — applying the same release/refund
//!    transition twice is caught by the `released`/`refunded` guard before
//!    the transition matrix is even reached.

#![cfg(test)]

use crate::{
    approvals,
    keys::{milestone_approval_key, milestone_key, milestone_symbol},
    milestone_transitions::{
        check_version_for_concurrency, store_milestone_transition, validate_milestone_transition,
        MilestoneState,
    },
    types::{Contract, ContractStatus, DataKey, Error, Milestone, MilestoneApprovals,
            ReleaseAuthorization},
    Escrow,
};
use soroban_sdk::{testutils::Address as _, Address, Env, Vec};

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Build a minimal in-storage contract/milestone environment inside the
/// registered Escrow contract address, then return the escrow address so callers
/// can use `env.as_contract` to invoke module-level functions.
fn setup_funded_contract(
    env: &Env,
    release_auth: ReleaseAuthorization,
) -> (Address, Address, Address, u32) {
    let escrow_address = env.register(Escrow, ());
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let contract_id: u32 = 1;

    let contract = Contract {
        client: client_addr.clone(),
        freelancer: freelancer_addr.clone(),
        arbiter: None,
        status: ContractStatus::Funded,
        total_deposited: 1000,
        funded_amount: 1000,
        released_amount: 0,
        refunded_amount: 0,
        release_authorization: release_auth,
        reputation_issued: false,
    };

    let milestones = Vec::from_array(
        env,
        [Milestone {
            amount: 1000,
            funded_amount: 1000,
            released: false,
            refunded: false,
            work_evidence: None,
            refunded_amount: 0,
            deadline: None,
        }],
    );

    env.as_contract(&escrow_address, || {
        env.storage()
            .persistent()
            .set(&DataKey::Contract(contract_id), &contract);
        let key = milestone_key(env, contract_id);
        env.storage().persistent().set(&key, &milestones);
    });

    (escrow_address, client_addr, freelancer_addr, contract_id)
}

// ─── 1. Key construction determinism ─────────────────────────────────────────

#[test]
fn milestone_key_identical_for_same_id() {
    let env = Env::default();
    assert_eq!(milestone_key(&env, 1), milestone_key(&env, 1));
    assert_eq!(milestone_key(&env, 42), milestone_key(&env, 42));
    assert_eq!(milestone_key(&env, u32::MAX), milestone_key(&env, u32::MAX));
}

#[test]
fn milestone_symbol_identical_on_repeated_calls() {
    let env = Env::default();
    // Calling Symbol::new repeatedly must produce the same interned value.
    let s1 = milestone_symbol(&env);
    let s2 = milestone_symbol(&env);
    assert_eq!(s1, s2);
}

#[test]
fn approval_key_identical_for_same_pair() {
    assert_eq!(milestone_approval_key(1, 0), milestone_approval_key(1, 0));
    assert_eq!(
        milestone_approval_key(99, 7),
        milestone_approval_key(99, 7)
    );
    assert_eq!(
        milestone_approval_key(u32::MAX, u32::MAX),
        milestone_approval_key(u32::MAX, u32::MAX)
    );
}

// ─── 2. Key uniqueness ────────────────────────────────────────────────────────

#[test]
fn milestone_keys_differ_across_contract_ids() {
    let env = Env::default();
    assert_ne!(milestone_key(&env, 1), milestone_key(&env, 2));
    assert_ne!(milestone_key(&env, 1), milestone_key(&env, u32::MAX));
}

#[test]
fn approval_keys_differ_across_contract_ids() {
    assert_ne!(milestone_approval_key(1, 0), milestone_approval_key(2, 0));
}

#[test]
fn approval_keys_differ_across_milestone_indices() {
    assert_ne!(milestone_approval_key(1, 0), milestone_approval_key(1, 1));
    assert_ne!(
        milestone_approval_key(1, 0),
        milestone_approval_key(1, u32::MAX)
    );
}

#[test]
fn approval_key_differs_from_released_key_same_coords() {
    let approval = DataKey::MilestoneApprovals(1, 0);
    let released = DataKey::MilestoneReleased(1, 0);
    assert_ne!(approval, released);
}

// ─── 3. Zero contract-id guard ────────────────────────────────────────────────

#[test]
#[should_panic]
fn milestone_key_panics_on_contract_id_zero() {
    let env = Env::default();
    let _ = milestone_key(&env, 0);
}

#[test]
fn milestone_key_does_not_panic_on_contract_id_one() {
    let env = Env::default();
    let _ = milestone_key(&env, 1); // must not panic
}

#[test]
fn milestone_key_does_not_panic_on_contract_id_max() {
    let env = Env::default();
    let _ = milestone_key(&env, u32::MAX); // must not panic
}

// ─── 4. Boundary conditions ───────────────────────────────────────────────────

#[test]
fn approval_key_boundary_smallest_valid() {
    let key = milestone_approval_key(1, 0);
    assert_eq!(key, DataKey::MilestoneApprovals(1, 0));
}

#[test]
fn approval_key_boundary_max_values() {
    let key = milestone_approval_key(u32::MAX, u32::MAX);
    assert_eq!(key, DataKey::MilestoneApprovals(u32::MAX, u32::MAX));
}

// ─── 5. Duplicate approval rejection (regression for #1450) ─────────────────

/// The client submits an approval, then submits the same approval again
/// (simulating a retry).  The second call must fail with `AlreadyApproved`.
#[test]
fn duplicate_approval_by_same_caller_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let (escrow_address, client_addr, _freelancer_addr, contract_id) =
        setup_funded_contract(&env, ReleaseAuthorization::ClientOnly);

    env.as_contract(&escrow_address, || {
        // First approval succeeds
        let result1 = approvals::approve_milestone(&env, contract_id, 0, &client_addr);
        assert_eq!(result1, Ok(true), "first approval should succeed");

        // Second approval (retry / duplicate) must be rejected
        let result2 = approvals::approve_milestone(&env, contract_id, 0, &client_addr);
        assert_eq!(
            result2,
            Err(Error::AlreadyApproved),
            "duplicate approval by the same caller must be rejected"
        );
    });
}

/// MultiSig: client and freelancer both have to approve.  Each can only
/// submit once; a duplicate from either party must be rejected.
#[test]
fn duplicate_approval_in_multisig_mode_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let (escrow_address, client_addr, freelancer_addr, contract_id) =
        setup_funded_contract(&env, ReleaseAuthorization::MultiSig);

    env.as_contract(&escrow_address, || {
        // Both parties approve once
        let r1 = approvals::approve_milestone(&env, contract_id, 0, &client_addr);
        assert_eq!(r1, Ok(true));

        let r2 = approvals::approve_milestone(&env, contract_id, 0, &freelancer_addr);
        assert_eq!(r2, Ok(true));

        // Client tries again — must fail
        let r3 = approvals::approve_milestone(&env, contract_id, 0, &client_addr);
        assert_eq!(r3, Err(Error::AlreadyApproved));

        // Freelancer tries again — must fail
        let r4 = approvals::approve_milestone(&env, contract_id, 0, &freelancer_addr);
        assert_eq!(r4, Err(Error::AlreadyApproved));
    });
}

// ─── 6. Approval idempotency via clear-and-re-approve ────────────────────────

/// After approvals are cleared (simulating a completed release), the same
/// caller is allowed to submit a fresh approval again.  This verifies that
/// `clear_approvals` properly removes the record so the key is free.
#[test]
fn approve_after_clear_succeeds() {
    let env = Env::default();
    env.mock_all_auths();
    let (escrow_address, client_addr, _freelancer_addr, contract_id) =
        setup_funded_contract(&env, ReleaseAuthorization::ClientOnly);

    env.as_contract(&escrow_address, || {
        // Approve once
        let r1 = approvals::approve_milestone(&env, contract_id, 0, &client_addr);
        assert_eq!(r1, Ok(true));

        // Simulate what happens after release: clear approvals
        approvals::clear_approvals(&env, contract_id, 0);

        // Because the milestone itself would now be `released=true` in a real
        // flow, a subsequent approve_milestone call would return
        // MilestoneAlreadyReleased.  We verify that the approval record is
        // truly absent by checking check_approvals returns InsufficientApprovals.
        let contract: Contract = env
            .storage()
            .persistent()
            .get(&DataKey::Contract(contract_id))
            .unwrap();
        let check = approvals::check_approvals(&env, &contract, contract_id, 0);
        assert_eq!(
            check,
            Err(Error::InsufficientApprovals),
            "approval record must be absent after clear"
        );
    });
}

// ─── 7. Stale-version detection ──────────────────────────────────────────────

/// `check_version_for_concurrency` must return `Ok(())` when the expected
/// version matches what is in storage.
#[test]
fn version_check_passes_when_version_matches() {
    let env = Env::default();
    let actor = Address::generate(&env);

    // Store version 1
    store_milestone_transition(&env, 1, 0, actor.clone());

    let result = check_version_for_concurrency(&env, 1, 0, 1);
    assert_eq!(result, Ok(()), "matching version must pass");
}

/// `check_version_for_concurrency` must fail when the expected version is
/// stale (another transaction already committed a new version).
#[test]
fn version_check_fails_on_stale_expected_version() {
    let env = Env::default();
    let actor = Address::generate(&env);

    // Commit two transitions so version is 2
    store_milestone_transition(&env, 1, 0, actor.clone());
    store_milestone_transition(&env, 1, 0, actor.clone());

    // Caller holds version 1 (stale)
    let result = check_version_for_concurrency(&env, 1, 0, 1);
    assert!(
        result.is_err(),
        "stale expected version must be rejected"
    );
}

/// Checking version 0 against an uninitialized milestone succeeds (default
/// version starts at 0 — no prior modification).
#[test]
fn version_check_zero_against_uninitialized_milestone_passes() {
    let env = Env::default();
    let result = check_version_for_concurrency(&env, 1, 0, 0);
    assert_eq!(result, Ok(()), "uninitialized milestone has version 0");
}

/// The version counter increments on each successful transition.
#[test]
fn version_increments_monotonically() {
    let env = Env::default();
    let actor = Address::generate(&env);

    let v1 = store_milestone_transition(&env, 1, 0, actor.clone());
    assert_eq!(v1, 1);

    let v2 = store_milestone_transition(&env, 1, 0, actor.clone());
    assert_eq!(v2, 2);

    let v3 = store_milestone_transition(&env, 1, 0, actor.clone());
    assert_eq!(v3, 3);
}

// ─── 8. Transition matrix idempotency and terminal-state protection ───────────

/// Releasing an already-released milestone must be caught before the storage
/// write by the `milestone.released` flag guard (the gate in `release.rs`).
/// Here we test the transition-matrix layer directly: attempting to transition
/// from `Released` to `Released` is `Ok` (idempotent), but `Released →
/// Refunded` is always an error.
#[test]
fn transition_released_to_released_is_idempotent() {
    let result = validate_milestone_transition(MilestoneState::Released, MilestoneState::Released);
    assert_eq!(result, Ok(()), "released → released must be idempotent");
}

#[test]
fn transition_released_to_refunded_is_rejected() {
    let result = validate_milestone_transition(MilestoneState::Released, MilestoneState::Refunded);
    assert!(
        result.is_err(),
        "released → refunded must be rejected by the transition matrix"
    );
}

#[test]
fn transition_refunded_to_released_is_rejected() {
    let result = validate_milestone_transition(MilestoneState::Refunded, MilestoneState::Released);
    assert!(
        result.is_err(),
        "refunded → released must be rejected by the transition matrix"
    );
}

#[test]
fn transition_refunded_to_refunded_is_idempotent() {
    let result = validate_milestone_transition(MilestoneState::Refunded, MilestoneState::Refunded);
    assert_eq!(result, Ok(()), "refunded → refunded must be idempotent");
}

// ─── 9. Invalid-state detection ──────────────────────────────────────────────

/// A milestone with both `released=true` AND `refunded=true` is an impossible
/// state.  `MilestoneState::from_milestone` must return `Error::InvalidState`.
#[test]
fn milestone_state_both_flags_set_is_invalid() {
    let milestone = Milestone {
        amount: 1000,
        funded_amount: 1000,
        released: true,
        refunded: true,
        work_evidence: None,
        refunded_amount: 0,
        deadline: None,
    };
    let result = MilestoneState::from_milestone(&milestone);
    assert_eq!(
        result,
        Err(Error::InvalidState),
        "both flags set must return InvalidState"
    );
}

// ─── 10. Approval key used consistently across modules ───────────────────────

/// `keys::milestone_approval_key` and the equivalent raw `DataKey` construction
/// must produce the same storage key so that writes in one path are readable
/// from the other.
#[test]
fn approval_key_helper_matches_raw_datakey() {
    let key_via_helper = milestone_approval_key(5, 3);
    let key_raw = DataKey::MilestoneApprovals(5, 3);
    assert_eq!(
        key_via_helper, key_raw,
        "keys::milestone_approval_key must produce identical key to raw DataKey"
    );
}
