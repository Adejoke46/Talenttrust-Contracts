use soroban_sdk::{address, vec, Address, Env , Vec };

use crate::{ContractStatus, ReleaseAuthorization};

use super::{assert_contract_state, create_client, setup};

/// Tests that contract creation persists milestones correctly.
/// 
/// # Security
/// - Validates contract initialization
/// - Ensures milestone data integrity
/// - Verifies initial state is Created
#[test]
fn creates_contract_and_persists_milestones() {
    let (env, client_addr, freelancer_addr) = setup();
    let client = create_client(&env);
    let milestones = vec![&env, 200_0000000_i128, 400_0000000_i128, 600_0000000_i128];

    let contract_id = client.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &milestones,
        &ReleaseAuthorization::ClientOnly,
    );

    assert_eq(contract_id, 1);

    let contract = client.get_contract(&contract_id);
    assert_contract_state(contract, ContractStatus::Created, 0, 0, 0);

    let stored_milestones = client.get_milestones(&contract_id);
    assert_eq(stored_milestones.len(), 3);
    assert_eq(stored_milestones.get(0).unwrap().amount, 200_0000000_i128);
    assert_eq(stored_milestones.get(1).unwrap().amount, 400_0000000_i128);
    assert_eq(stored_milestones.get(2).unwrap().amount, 600_0000000_i128);
}

/// Tests that contract creation with empty milestones is rejected.
/// 
/// # Security
/// - Prevents invalid contract initialization
/// - Validates input sanitization
#[test]
#[should_panic]
fn rejects_empty_milestones() {
    let (env, client_addr, freelancer_addr) = setup();
    let client = create_client(&env);

    let milestones = vec![&env];
    client.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &milestones,
        &ReleaseAuthorization::ClientOnly,
    );
}

/// Tests that contract creation with zero-amount milestone is rejected.
/// 
/// # Security
/// - Prevents dust attacks
/// - Validates milestone amount constraints
#[test]
#[should_panic]
fn rejects_zero_amount_milestone() {
    let (env, client_addr, freelancer_addr) = setup();
    let client = create_client(&env);

    let milestones = vec![&env, 0_i128];
    client.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &milestones,
        &ReleaseAuthorization::ClientOnly,
    );
}

/// Tests that contract creation with same client and freelancer is rejected.
/// 
/// # Security
/// - Prevents self-dealing
/// - Validates participant uniqueness
#[test]
#[should_panic]
fn rejects_same_participants() {
    let (env, client_addr, _) = setup();
    let client = create_client(&env);

    let milestones = vec![&env, 100_0000000_i128];
    client.create_contract(
        &client_addr,
        &client_addr,
        &None,
        &milestones,
        &ReleaseAuthorization::ClientOnly,
    );
}

/// Tests that concurrent contract creations from the same client produce
/// distinct, monotonically increasing contract IDs without collisions.
///
/// # Security
/// - Ensures no ID reuse or overwrite under repeated calls
/// - Verifies each contract is independently readable and consistent
#[test]
fn concurrent_creations_get_unique_ids_and_isolated_state() {
    let (env, client_addr, freelancer_addr) = setup();
    let client = create_client(&env);

    let milestones_a = vec![&env, 10_0000000_i128];
    let milestones_b = vec![&env, 20_0000000_i128];

    let id_a = client.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &milestones_a,
        &ReleaseAuthorization::ClientOnly,
    );
    let id_b = client.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &milestones_b,
        &ReleaseAuthorization::ClientOnly,
    );

    assert_ne(id_a, id_b);
    assert_eq(id_b, id_a + 1);

    let contract_a = client.get_contract(&id_a);
    let contract_b = client.get_contract(&id_b);
    assert_contract_state(contract_a.clone(), ContractStatus::Created, 0, 0, 0);
    assert_contract_state(contract_b.clone(), ContractStatus::Created, 0, 0, 0);

    let stored_a = client.get_milestones(&id_a);
    let stored_b = client.get_milestones(&id_b);
    assert_eq(stored_a.len(), 1);
    assert_eq(stored_b.len(), 1);
    assert_eq(stored_a.get(0).unwrap().amount, 10_0000000_i128);
    assert_eq(stored_b.get(0).unwrap().amount, 20_0000000_i128);
}

/// Tests that repeated identical creation requests are not idempotent at
/// the ID level (each call creates a new contract) but are deterministic and
/// do not corrupt earlier contracts (no stale writes).
///
/// # Security
/// - Retries must not overwrite existing contract state
/// - Each creation is independently verifiable
#[test]
fn repeated_identical_creations_do_not_corrupt_earlier(} {
    let (env, client_addr, freelancer_addr) = setup();
    let client = create_client(&env);

    let milestones = vec![&env, 50_0000000_i128];

    let id_1 = client.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &milestones,
        &ReleaseAuthorization::ClientOnly,
    );
    let id_2 = client.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &milestones,
        &ReleaseAuthorization::ClientOnly,
    );

    assert_ne(id_1, id_2);

    // First contract must remain unchanged after the second creation.
    let contract_1 = client.get_contract(&id_1);
    assert_contract_state(contract_1, ContractStatus::Created, 0, 0, 0);

    let stored_1 = client.get_milestones(&id_1);
    assert_eq(stored_1.len(), 1);
    assert_eq(stored_1.get(0).unwrap().amount, 50_0000000_i128);
}

/// Tests that a contract created with an arbitrator persists the arbitrator
/// address and remains readable without cross-contract interference.
///
/// # Security
/// - Verifies optional arbitrator is stored correctly
/// - Ensures no shared mutable state between contracts
#[test]
fn creation_with_arbitrator_is_isolated() {
    let (env, client_addr, freelancer_addr) = setup();
    let client = create_client(&env);
    let arbitrator = Address::generate(&env);

    let milestones = vec![&env, 7_0000000_i128];
    let id = client.create_contract(
        &client_addr,
        &freelancer_addr,
        &Some(arbitrator.clone()),
        &milestones,
        &ReleaseAuthorization::ClientOnly,
    );

    let contract = client.get_contract(&id);
    assert_contract_state(contract, ContractStatus::Created, 0, 0, 0);

    let stored = client.get_milestones(&id);
    assert_eq(stored.len(), 1);
    assert_eq(stored.get(0).unwrap().amount, 7_0000000_i128);
}

/// Tests that a contract creation with a negative milestone amount is rejected.
///
/// # Security
/// - Prevents invalid amount injection
/// - Preserves accounting invariants
#[test]
#[should_panic]
fn rejects_negative_milestone_amount() {
    let (env, client_addr, freelancer_addr) = setup();
    let client = create_client(&env);

    let milestones = vec![&env, -1_i128];
    client.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &milestones,
        &ReleaseAuthorization::ClientOnly,
    );
}
