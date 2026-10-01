//! Contract-level regression tests for the concurrent-execution hardening
//! (issue #1535).
//!
//! The unit tests in `storage_validation` cover the per-contract mutation lock
//! and the state validators in isolation. These tests drive them through the
//! real entrypoints, which is what proves the guard is actually wired in.
//!
//! A Soroban transaction is atomic and single-threaded, so "a mutation for this
//! contract is already in flight" is simulated deterministically by pre-setting
//! the mutation lock and then calling the entrypoint — exactly the state a
//! re-entrant token callback, a duplicated request, or a racing caller would
//! find. Each test also shows that once the in-flight mutation ends the same
//! request succeeds, i.e. the guard is not sticky and retries are safe.

use super::*;
use crate::storage_validation;
use crate::DataKey;

/// Simulate an in-flight mutation for the fixture's contract.
fn begin_in_flight_mutation(fixture: &EscrowFixture) {
    fixture.env.as_contract(&fixture.escrow_address, || {
        fixture
            .env
            .storage()
            .persistent()
            .set(&DataKey::ContractMutationLock(fixture.escrow_id), &true);
    });
}

/// Clear the simulated in-flight mutation, as the holder would on completion.
fn end_in_flight_mutation(fixture: &EscrowFixture) {
    fixture.env.as_contract(&fixture.escrow_address, || {
        fixture
            .env
            .storage()
            .persistent()
            .remove(&DataKey::ContractMutationLock(fixture.escrow_id));
    });
}

#[test]
fn release_milestone_is_rejected_while_a_mutation_is_in_flight() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();

    // Approval is a distinct entrypoint and is not lock-guarded.
    escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0);

    begin_in_flight_mutation(&fixture);
    assert_contract_error(
        escrow.try_release_milestone(&fixture.escrow_id, &fixture.client, &0),
        EscrowError::ConcurrentMutation,
    );

    // The retry after the in-flight mutation completes succeeds, so a rejected
    // request can always be safely retried by the caller.
    end_in_flight_mutation(&fixture);
    assert!(escrow.release_milestone(&fixture.escrow_id, &fixture.client, &0));
}

#[test]
fn deposit_funds_is_rejected_while_a_mutation_is_in_flight() {
    let fixture = EscrowFixture::builder().with_settlement_token().build();
    let escrow = fixture.escrow();
    let token = fixture
        .settlement_token
        .clone()
        .expect("with_settlement_token binds a token");
    let amount = fixture.total_amount();
    StellarAssetClient::new(&fixture.env, &token).mint(&fixture.client, &amount);

    begin_in_flight_mutation(&fixture);
    assert_contract_error(
        escrow.try_deposit_funds(&fixture.escrow_id, &fixture.client, &amount),
        EscrowError::ConcurrentMutation,
    );

    end_in_flight_mutation(&fixture);
    assert!(escrow.deposit_funds(&fixture.escrow_id, &fixture.client, &amount));
}

#[test]
fn refund_is_rejected_while_a_mutation_is_in_flight() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    let indices = vec![&fixture.env, 0_u32];

    begin_in_flight_mutation(&fixture);
    assert_contract_error(
        escrow.try_refund_unreleased_milestones(&fixture.escrow_id, &indices),
        EscrowError::ConcurrentMutation,
    );

    end_in_flight_mutation(&fixture);
    assert_eq!(
        escrow.refund_unreleased_milestones(&fixture.escrow_id, &indices),
        MILESTONE_ONE
    );
}

#[test]
fn cancel_contract_is_rejected_while_a_mutation_is_in_flight() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();

    begin_in_flight_mutation(&fixture);
    assert_contract_error(
        escrow.try_cancel_contract(&fixture.escrow_id, &fixture.client),
        EscrowError::ConcurrentMutation,
    );

    end_in_flight_mutation(&fixture);
    assert!(escrow.cancel_contract(&fixture.escrow_id, &fixture.client));
}

#[test]
fn successful_entrypoint_leaves_no_lock_behind() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0);
    assert!(escrow.release_milestone(&fixture.escrow_id, &fixture.client, &0));

    fixture.env.as_contract(&fixture.escrow_address, || {
        assert!(
            !storage_validation::is_contract_locked(&fixture.env, fixture.escrow_id),
            "the mutation lock must be released when the entrypoint returns"
        );
    });
}

#[test]
fn sequential_releases_are_not_serialised_against_each_other() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();

    for index in 0..2_u32 {
        escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &index);
        assert!(escrow.release_milestone(&fixture.escrow_id, &fixture.client, &index));
    }
}

#[test]
fn release_rejects_a_corrupt_persisted_contract() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0);

    // Launder a corrupt accounting record into storage, as a partially applied
    // or hand-edited write would.
    let mut corrupted = escrow.get_contract(&fixture.escrow_id);
    corrupted.released_amount = corrupted.funded_amount + 1;
    fixture.env.as_contract(&fixture.escrow_address, || {
        fixture
            .env
            .storage()
            .persistent()
            .set(&DataKey::Contract(fixture.escrow_id), &corrupted);
    });

    assert_contract_error(
        escrow.try_release_milestone(&fixture.escrow_id, &fixture.client, &0),
        EscrowError::StorageInvariantViolated,
    );
}
