//! Tests to validate the authorization documentation matrix against source code.
//!
//! This test module ensures that the documented authorization rules in
//! docs/escrow/authorization.md match the actual implementation in
//! contracts/escrow/src/approvals.rs and contracts/escrow/src/lib.rs.
//!
//! The tests verify:
//! - Allowed approvers per mode
//! - Required approval logic per mode
//! - Allowed release callers per mode
//! - Error codes returned for unauthorized attempts
//! - State invariants: approvals are scoped per milestone, cannot be replayed
//!   after release, and cannot be granted for non-existent or released milestones.

#![cfg(test)]

use soroban_sdk::{testutils::Address as _, vec, Address, Env};
use crate::{Escrow, EscrowClient, EscrowError, ReleaseAuthorization};

use super::assert_contract_error;

const MILESTONE_AMOUNT: i128 = 500_0000000_i128;

fn setup(env: &Env) -> (EscrowClient<'_>, Address, Address, Address) {
    let contract_id = env.register(Escrow, ());
    let client = EscrowClient::new(env, &contract_id);

    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);

    (client, client_addr, freelancer_addr, arbiter_addr)
}

/// Creates a contract with two milestones and funds it to cover both.
/// Invariant: the returned contract id has exactly two milestones, both unreleased.
fn create_funded_contract(
    env: &Env,
    client: &EscrowClient<'_>,
    client_addr: &Address,
    freelancer_addr: &Address,
    arbiter: Option<&Address>,
    auth: &ReleaseAuthorization,
) -> u32 {
    let milestones = vec![env, MILESTONE_AMOUNT, 300_0000000_i128];
    let id = client.create_contract(client_addr, freelancer_addr, &arbiter.cloned(), &milestones, auth);
    client.deposit_funds(&id, client_addr, &800_0000000_i128);
    id
}

// ===========================================================================
// ClientOnly Mode Validation
// ===========================================================================

// Invariant: in ClientOnly mode, only the client may approve or release, and
// exactly one client approval is required per milestone.
#[test]
fn clientonly_matrix_allowed_approvers() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, client_addr, freelancer_addr, arbiter_addr) = setup(&env);

    let id = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        None,
        &ReleaseAuthorization::ClientOnly,
    );

    // Client can approve
    let result = client.try_approve_milestone_release(&id, &client_addr, &0);
    assert!(result.is_ok(), "Client should be allowed to approve in ClientOnly mode");

    // Freelancer cannot approve
    let result = client.try_approve_milestone_release(&id, &freelancer_addr, &0);
    assert_contract_error(result, EscrowError::UnauthorizedRole);

    // Arbiter cannot approve
    let result = client.try_approve_milestone_release(&id, &arbiter_addr, &0);
    assert_contract_error(result, EscrowError::UnauthorizedRole);
}

// Invariant: release without the required client approval must fail with
// InsufficientApprovals and must not mutate milestone state.
#[test]
fn clientonly_matrix_required_approvals() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, client_addr, freelancer_addr, _) = setup(&env);

    let id = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        None,
        &ReleaseAuthorization::ClientOnly,
    );

    // Without approvals, release fails
    let result = client.try_release_milestone(&id, &client_addr, &0);
    assert_contract_error(result, EscrowError::InsufficientApprovals);

    // With client approval, release succeeds
    assert!(client.approve_milestone_release(&id, &client_addr, &0));
    let result = client.try_release_milestone(&id, &client_addr, &0);
    assert!(result.is_ok(), "Release should succeed with client approval");
}

// Invariant: only the client may call release in ClientOnly mode; freelancer
// and arbiter attempts must be rejected with UnauthorizedRole.
#[test]
fn clientonly_matrix_allowed_release_callers() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, client_addr, freelancer_addr, arbiter_addr) = setup(&env);

    let id = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        None,
        &ReleaseAuthorization::ClientOnly,
    );

    assert!(client.approve_milestone_release(&id, &client_addr, &0));

    // Client can release
    let result = client.try_release_milestone(&id, &client_addr, &0);
    assert!(result.is_ok(), "Client should be allowed to release in ClientOnly mode");

    // Freelancer cannot release
    let result = client.try_release_milestone(&id, &freelancer_addr, &0);
    assert_contract_error(result, EscrowError::UnauthorizedRole);

    // Arbiter cannot release
    let result = client.try_release_milestone(&id, &arbiter_addr, &0);
    assert_contract_error(result, EscrowError::UnauthorizedRole);
}

// ===========================================================================
// ArbiterOnly Mode Validation
// ===========================================================================

// Invariant: in ArbiterOnly mode, only the arbiter may approve, and an arbiter
// must be configured at creation time.
#[test]
fn arbiteronly_matrix_allowed_approvers() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, client_addr, freelancer_addr, arbiter_addr) = setup(&env);

    let id = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        Some(&arbiter_addr),
        &ReleaseAuthorization::ArbiterOnly,
    );

    // Arbiter can approve
    let result = client.try_approve_milestone_release(&id, &arbiter_addr, &0);
    assert!(result.is_ok(), "Arbiter should be allowed to approve in ArbiterOnly mode");

    // Client cannot approve
    let result = client.try_approve_milestone_release(&id, &client_addr, &0);
    assert_contract_error(result, EscrowError::UnauthorizedRole);

    // Freelancer cannot approve
    let result = client.try_approve_milestone_release(&id, &freelancer_addr, &0);
    assert_contract_error(result, EscrowError::UnauthorizedRole);
}

// Invariant: release without arbiter approval must fail with
// InsufficientApprovals and must not mutate milestone state.
#[test]
fn arbiteronly_matrix_required_approvals() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, client_addr, freelancer_addr, arbiter_addr) = setup(&env);

    let id = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        Some(&arbiter_addr),
        &ReleaseAuthorization::ArbiterOnly,
    );

    // Without approvals, release fails
    let result = client.try_release_milestone(&id, &arbiter_addr, &0);
    assert_contract_error(result, EscrowError::InsufficientApprovals);

    // With arbiter approval, release succeeds
    assert!(client.approve_milestone_release(&id, &arbiter_addr, &0));
    let result = client.try_release_milestone(&id, &arbiter_addr, &0);
    assert!(result.is_ok(), "Release should succeed with arbiter approval");
}

// Invariant: only the arbiter may call release in ArbiterOnly mode; client and
// freelancer attempts must be rejected with UnauthorizedRole.
#[test]
fn arbiteronly_matrix_allowed_release_callers() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, client_addr, freelancer_addr, arbiter_addr) = setup(&env);

    let id = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        Some(&arbiter_addr),
        &ReleaseAuthorization::ArbiterOnly,
    );

    assert!(client.approve_milestone_release(&id, &arbiter_addr, &0));

    // Arbiter can release
    let result = client.try_release_milestone(&id, &arbiter_addr, &0);
    assert!(result.is_ok(), "Arbiter should be allowed to release in ArbiterOnly mode");

    // Client cannot release
    let result = client.try_release_milestone(&id, &client_addr, &0);
    assert_contract_error(result, EscrowError::UnauthorizedRole);

    // Freelancer cannot release
    let result = client.try_release_milestone(&id, &freelancer_addr, &0);
    assert_contract_error(result, EscrowError::UnauthorizedRole);
}

// ===========================================================================
// ClientAndArbiter Mode Validation
// ===========================================================================

// Invariant: in ClientAndArbiter mode, both client and arbiter may approve,
// but the freelancer may not.
#[test]
fn clientandarbiter_matrix_allowed_approvers() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, client_addr, freelancer_addr, arbiter_addr) = setup(&env);

    let id = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        Some(&arbiter_addr),
        &ReleaseAuthorization::ClientAndArbiter,
    );

    // Client can approve
    let result = client.try_approve_milestone_release(&id, &client_addr, &0);
    assert!(result.is_ok(), "Client should be allowed to approve in ClientAndArbiter mode");

    // Arbiter can approve
    let result = client.try_approve_milestone_release(&id, &arbiter_addr, &0);
    assert!(result.is_ok(), "Arbiter should be allowed to approve in ClientAndArbiter mode");

    // Freelancer cannot approve
    let result = client.try_approve_milestone_release(&id, &freelancer_addr, &0);
    assert_contract_error(result, EscrowError::UnauthorizedRole);
}

// Invariant: ClientAndArbiter uses OR semantics — either a client approval or
// an arbiter approval is sufficient to release a milestone.
#[test]
fn clientandarbiter_matrix_required_approvals_or_logic() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, client_addr, freelancer_addr, arbiter_addr) = setup(&env);

    // Test with client approval only
    let id1 = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        Some(&arbiter_addr),
        &ReleaseAuthorization::ClientAndArbiter,
    );
    assert!(client.approve_milestone_release(&id1, &client_addr, &0));
    let result = client.try_release_milestone(&id1, &client_addr, &0);
    assert!(result.is_ok(), "Release should succeed with only client approval (OR logic)");

    // Test with arbiter approval only
    let id2 = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        Some(&arbiter_addr),
        &ReleaseAuthorization::ClientAndArbiter,
    );
    assert!(client.approve_milestone_release(&id2, &arbiter_addr, &0));
    let result = client.try_release_milestone(&id2, &arbiter_addr, &0);
    assert!(result.is_ok(), "Release should succeed with only arbiter approval (OR logic)");
}

// Invariant: in ClientAndArbiter mode, both client and arbiter may call
// release; the freelancer may not.
#[test]
fn clientandarbiter_matrix_allowed_release_callers() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, client_addr, freelancer_addr, arbiter_addr) = setup(&env);

    let id = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        Some(&arbiter_addr),
        &ReleaseAuthorization::ClientAndArbiter,
    );

    assert!(client.approve_milestone_release(&id, &client_addr, &0));

    // Client can release
    let result = client.try_release_milestone(&id, &client_addr, &0);
    assert!(result.is_ok(), "Client should be allowed to release in ClientAndArbiter mode");

    // Arbiter can release
    let id2 = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        Some(&arbiter_addr),
        &ReleaseAuthorization::ClientAndArbiter,
    );
    assert!(client.approve_milestone_release(&id2, &arbiter_addr, &0));
    let result = client.try_release_milestone(&id2, &arbiter_addr, &0);
    assert!(result.is_ok(), "Arbiter should be allowed to release in ClientAndArbiter mode");

    // Freelancer cannot release
    let result = client.try_release_milestone(&id, &freelancer_addr, &0);
    assert_contract_error(result, EscrowError::UnauthorizedRole);
}

// ===========================================================================
// MultiSig Mode Validation
// ===========================================================================

// Invariant: in MultiSig mode, client and freelancer may approve; the arbiter
// may not.
#[test]
fn multisig_matrix_allowed_approvers() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, client_addr, freelancer_addr, arbiter_addr) = setup(&env);

    let id = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        None,
        &ReleaseAuthorization::MultiSig,
    );

    // Client can approve
    let result = client.try_approve_milestone_release(&id, &client_addr, &0);
    assert!(result.is_ok(), "Client should be allowed to approve in MultiSig mode");

    // Freelancer can approve
    let result = client.try_approve_milestone_release(&id, &freelancer_addr, &0);
    assert!(result.is_ok(), "Freelancer should be allowed to approve in MultiSig mode");

    // Arbiter cannot approve
    let result = client.try_approve_milestone_release(&id, &arbiter_addr, &0);
    assert_contract_error(result, EscrowError::UnauthorizedRole);
}

// Invariant: MultiSig uses AND semantics — both client and freelancer
// approvals are required before release can succeed.
#[test]
fn multisig_matrix_required_approvals_and_logic() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, client_addr, freelancer_addr, _) = setup(&env);

    let id = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        None,
        &ReleaseAuthorization::MultiSig,
    );

    // With only client approval, release fails
    assert!(client.approve_milestone_release(&id, &client_addr, &0));
    let result = client.try_release_milestone(&id, &client_addr, &0);
    assert_contract_error(result, EscrowError::InsufficientApprovals);

    // With both approvals, release succeeds
    assert!(client.approve_milestone_release(&id, &freelancer_addr, &0));
    let result = client.try_release_milestone(&id, &client_addr, &0);
    assert!(result.is_ok(), "Release should succeed with both client and freelancer approval (AND logic)");
}

// Invariant: in MultiSig mode, both client and freelancer may call release
// once both approvals are present; the arbiter may not.
#[test]
fn multisig_matrix_allowed_release_callers() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, client_addr, freelancer_addr, arbiter_addr) = setup(&env);

    let id = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        None,
        &ReleaseAuthorization::MultiSig,
    );

    assert!(client.approve_milestone_release(&id, &client_addr, &0));
    assert!(client.approve_milestone_release(&id, &freelancer_addr, &0));

    // Client can release
    let result = client.try_release_milestone(&id, &client_addr, &0);
    assert!(result.is_ok(), "Client should be allowed to release in MultiSig mode");

    // Freelancer can release
    let id2 = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        None,
        &ReleaseAuthorization::MultiSig,
    );
    assert!(client.approve_milestone_release(&id2, &client_addr, &0));
    assert!(client.approve_milestone_release(&id2, &freelancer_addr, &0));
    let result = client.try_release_milestone(&id2, &freelancer_addr, &0);
    assert!(result.is_ok(), "Freelancer should be allowed to release in MultiSig mode");

    // Arbiter cannot release
    let result = client.try_release_milestone(&id, &arbiter_addr, &0);
    assert_contract_error(result, EscrowError::UnauthorizedRole);
}

// ===========================================================================
// Error Code Validation
// ===========================================================================

// Invariant: unauthorized approvers receive UnauthorizedRole, not a generic
// failure, so callers can distinguish permission errors from state errors.
#[test]
fn matrix_error_codes_unauthorized_role() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, client_addr, freelancer_addr, arbiter_addr) = setup(&env);

    // ClientOnly: freelancer unauthorized
    let id = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        None,
        &ReleaseAuthorization::ClientOnly,
    );
    let result = client.try_approve_milestone_release(&id, &freelancer_addr, &0);
    assert_contract_error(result, EscrowError::UnauthorizedRole);
}

// Invariant: duplicate approvals from the same address must be rejected with
// AlreadyApproved and must not change approval state.
#[test]
fn matrix_error_codes_already_approved() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, client_addr, freelancer_addr, _) = setup(&env);

    let id = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        None,
        &ReleaseAuthorization::ClientOnly,
    );

    // First approval succeeds
    assert!(client.approve_milestone_release(&id, &client_addr, &0));

    // Duplicate approval fails
    let result = client.try_approve_milestone_release(&id, &client_addr, &0);
    assert_contract_error(result, EscrowError::AlreadyApproved);
}

// Invariant: releasing without the required approvals must fail with
// InsufficientApprovals and must not mutate milestone state.
#[test]
fn matrix_error_codes_insufficient_approvals() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, client_addr, freelancer_addr, _) = setup(&env);

    let id = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        None,
        &ReleaseAuthorization::ClientOnly,
    );

    // Release without approval fails
    let result = client.try_release_milestone(&id, &client_addr, &0);
    assert_contract_error(result, EscrowError::InsufficientApprovals);
}

// Invariant: ArbiterOnly mode requires an arbiter at creation time; creation
// without one must fail before any state is persisted.
#[test]
fn matrix_error_codes_missing_arbiter() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, client_addr, freelancer_addr, _) = setup(&env);

    // ArbiterOnly without arbiter should fail at creation
    let milestones = vec![&env, 500_0000000_i128];
    let result = client.try_create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &milestones,
        &ReleaseAuthorization::ArbiterOnly,
    );
    assert!(result.is_err(), "ArbiterOnly mode should require arbiter at contract creation");
}

// ===========================================================================
// State Invariant Validation
// ===========================================================================

// Invariant: approvals are scoped per milestone. Approving milestone 0 must
// not satisfy the approval requirement for milestone 1.
#[test]
fn invariant_approvals_are_scoped_per_milestone() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, client_addr, freelancer_addr, _) = setup(&env);

    let id = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        None,
        &ReleaseAuthorization::ClientOnly,
    );

    // Approve only milestone 0.
    assert!(client.approve_milestone_release(&id, &client_addr, &0));

    // Milestone 1 must still require its own approval.
    let result = client.try_release_milestone(&id, &client_addr, &1);
    assert_contract_error(result, EscrowError::InsufficientApprovals);

    // Approving milestone 1 then releasing it must succeed.
    assert!(client.approve_milestone_release(&id, &client_addr, &1));
    let result = client.try_release_milestone(&id, &client_addr, &1);
    assert!(result.is_ok(), "Milestone 1 release should succeed after its own approval");
}

// Invariant: once a milestone is released, its approval state must not be
// reusable and further approvals for that milestone must be rejected.
#[test]
fn invariant_no_replay_after_release() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, client_addr, freelancer_addr, _) = setup(&env);

    let id = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        None,
        &ReleaseAuthorization::ClientOnly,
    );

    assert!(client.approve_milestone_release(&id, &client_addr, &0));
    assert!(client.try_release_milestone(&id, &client_addr, &0).is_ok());

    // Releasing the same milestone again must fail; it is already released.
    let result = client.try_release_milestone(&id, &client_addr, &0);
    assert!(result.is_err(), "Releasing an already-released milestone must fail");

    // Re-approving a released milestone must not silently succeed.
    let result = client.try_approve_milestone_release(&id, &client_addr, &0);
    assert!(result.is_err(), "Approving an already-released milestone must fail");
}

// Invariant: approvals for out-of-range milestone indices must be rejected
// and must not create phantom approval entries.
#[test]
fn invariant_out_of_range_milestone_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, client_addr, freelancer_addr, _) = setup(&env);

    let id = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        None,
        &ReleaseAuthorization::ClientOnly,
    );

    // Index 2 is out of range for a two-milestone contract.
    let result = client.try_approve_milestone_release(&id, &client_addr, &2);
    assert!(result.is_err(), "Approving an out-of-range milestone must fail");

    let result = client.try_release_milestone(&id, &client_addr, &2);
    assert!(result.is_err(), "Releasing an out-of-range milestone must fail");
}

// Invariant: repeated identical approvals must be idempotent in effect —
// the first succeeds, subsequent attempts fail with AlreadyApproved, and the
// approval count does not change.
#[test]
fn invariant_duplicate_approval_does_not_change_state() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, client_addr, freelancer_addr, _) = setup(&env);

    let id = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        None,
        &ReleaseAuthorization::ClientOnly,
    );

    assert!(client.approve_milestone_release(&id, &client_addr, &0));
    let result = client.try_approve_milestone_release(&id, &client_addr, &0);
    assert_contract_error(result, EscrowError::AlreadyApproved);

    // The single approval is still sufficient and release succeeds.
    let result = client.try_release_milestone(&id, &client_addr, &0);
    assert!(result.is_ok(), "Single approval should still be sufficient after duplicate rejection");
}

// Invariant: a failed release attempt must not consume or clear approvals,
// so a subsequent valid release still succeeds.
#[test]
fn invariant_failed_release_preserves_approvals() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, client_addr, freelancer_addr, arbiter_addr) = setup(&env);

    let id = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        Some(&arbiter_addr),
        &ReleaseAuthorization::ClientOnly,
    );

    // Approve milestone 0 as client.
    assert!(client.approve_milestone_release(&id, &client_addr, &0));

    // Unauthorized caller attempts release; must fail without consuming approval.
    let result = client.try_release_milestone(&id, &freelancer_addr, &0);
    assert_contract_error(result, EscrowError::UnauthorizedRole);

    // Authorized caller can still release using the preserved approval.
    let result = client.try_release_milestone(&id, &client_addr, &0);
    assert!(result.is_ok(), "Preserved approval should allow subsequent valid release");
}

// Invariant: unauthorized approval attempts must not create approval state,
// so a later authorized approval is still required and sufficient.
#[test]
fn invariant_unauthorized_approval_does_not_create_state() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, client_addr, freelancer_addr, _) = setup(&env);

    let id = create_funded_contract(
        &env,
        &client,
        &client_addr,
        &freelancer_addr,
        None,
        &ReleaseAuthorization::ClientOnly,
    );

    // Freelancer is not allowed to approve in ClientOnly mode.
    let result = client.try_approve_milestone_release(&id, &freelancer_addr, &0);
    assert_contract_error(result, EscrowError::UnauthorizedRole);

    // Release must still fail because no valid approval exists.
    let result = client.try_release_milestone(&id, &client_addr, &0);
    assert_contract_error(result, EscrowError::InsufficientApprovals);

    // Client approval then enables release.
    assert!(client.approve_milestone_release(&id, &client_addr, &0));
    let result = client.try_release_milestone(&id, &client_addr, &0);
    assert!(result.is_ok(), "Release should succeed after authorized approval");
}

