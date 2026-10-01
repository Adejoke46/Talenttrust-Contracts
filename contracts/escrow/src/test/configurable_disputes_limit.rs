use super::register_client;
use crate::{
    DisputeResolution, Escrow, EscrowClient, EscrowError, ContractStatus, DEFAULT_MAX_DISPUTES,
    MAX_MAX_DISPUTES, MIN_MAX_DISPUTES,
};
use soroban_sdk::{testutils::Address as _, Address, Env};

// ─── Setup ───────────────────────────────────────────

fn setup_initialized() -> (Env, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(Escrow, ());
    let client = EscrowClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    assert!(client.initialize(&admin));
    (env, contract_id, admin)
}

// ─── Default values ──────────────────────────────────

#[test]
fn max_disputes_returns_default_before_any_set() {
    let (env, contract_id, _admin) = setup_initialized();
    let client = EscrowClient::new(&env, &contract_id);

    assert_eq!(client.get_max_disputes(), DEFAULT_MAX_DISPUTES);
}

// ─── Setting limits ─────────────────────────────────────────

#[test]
fn admin_can_set_max_disputes_within_bounds() {
    let (env, contract_id, _admin) = setup_initialized();
    let client = EscrowClient::new(&env, &contract_id);

    assert!(client.set_max_disputes(&20));
    assert_eq!(client.get_max_disputes(), 20);
}

#[test]
fn admin_can_set_max_disputes_to_minimum() {
    let (env, contract_id, _admin) = setup_initialized();
    let client = EscrowClient::new(&env, &contract_id);

    assert!(client.set_max_disputes(&1));
    assert_eq!(client.get_max_disputes(), 1);
}

#[test]
fn admin_can_set_max_disputes_to_maximum() {
    let (env, contract_id, _admin) = setup_initialized();
    let client = EscrowClient::new(&env, &contract_id);

    assert!(client.set_max_disputes(&MAX_MAX_DISPUTES));
    assert_eq!(client.get_max_disputes(), MAX_MAX_DISPUTES);
}

// ─── Out-of-range rejection ─────────────────────────

#[test]
fn set_max_disputes_rejects_zero() {
    let (env, contract_id, _admin) = setup_initialized();
    let client = EscrowClient::new(&env, &contract_id);

    super::assert_contract_error(
        client.try_set_max_disputes(&0),
        EscrowError::LimitOutOfRange,
    );
}

#[test]
fn set_max_disputes_rejects_above_maximum() {
    let (env, contract_id, _admin) = setup_initialized();
    let client = EscrowClient::new(&env, &contract_id);

    let too_high = MAX_MAX_DISPUTES + 1;
    super::assert_contract_error(
        client.try_set_max_disputes(&too_high),
        EscrowError::LimitOutOfRange,
    );
}

// ─── Requires initialization ─────────────────────────

#[test]
fn set_max_disputes_requires_initialization() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(Escrow, ());
    let client = EscrowClient::new(&env, &contract_id);

    super::assert_contract_error(
        client.try_set_max_disputes(&20),
        EscrowError::NotInitialized,
    );
}

#[test]
fn get_max_disputes_returns_default_without_initialization() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(Escrow, ());
    let client = EscrowClient::new(&env, &contract_id);

    assert_eq!(client.get_max_disputes(), DEFAULT_MAX_DISPUTES);
}

// ─── Dispute limit enforcement ─────────────────────────

#[test]
fn raise_dispute_respects_default_max_disputes() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let (client_addr, _freelancer_addr, arbiter_addr, contract_id) =
        super::create_contract_with_arbiter(&env, &client);

    // With default MAX_DISPUTES = 10, we can raise 10 disputes.
    for _ in 0..DEFAULT_MAX_DISPUTES {
        assert!(client.raise_dispute(&contract_id, &client_addr));
        let contract = client.get_contract(&contract_id);
        if contract.status == crate::ContractStatus::Disputed {
            assert!(client.resolve_dispute(
                &contract_id,
                &arbiter_addr,
                &crate::DisputeResolution::FullRefund,
            ));
            assert!(client.deposit_funds(&contract_id, &client_addr, &100_i128));
        }
    }
}

#[test]
fn raise_dispute_rejected_after_reaching_max_disputes() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let (client_addr, _freelancer_addr, arbiter_addr, contract_id) =
        super::create_contract_with_arbiter(&env, &client);

    assert!(client.set_max_disputes(&2));

    assert!(client.raise_dispute(&contract_id, &client_addr));
    assert!(client.resolve_dispute(
        &contract_id,
        &arbiter_addr,
        &crate::DisputeResolution::FullRefund,
    ));
    assert!(client.deposit_funds(&contract_id, &client_addr, &100_i128));

    assert!(client.raise_dispute(&contract_id, &client_addr));
    assert!(client.resolve_dispute(
        &contract_id,
        &arbiter_addr,
        &crate::DisputeResolution::FullRefund,
    ));
    assert!(client.deposit_funds(&contract_id, &client_addr, &100_i128));

    super::assert_contract_error(
        client.try_raise_dispute(&contract_id, &client_addr),
        EscrowError::LimitOutOfRange,
    );
}

#[test]
fn raise_dispute_rejected_after_exactly_max_disputes() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let (client_addr, _freelancer_addr, arbiter_addr, contract_id) =
        super::create_contract_with_arbiter(&env, &client);

    assert!(client.set_max_disputes(&1));

    assert!(client.raise_dispute(&contract_id, &client_addr));
    assert!(client.resolve_dispute(
        &contract_id,
        &arbiter_addr,
        &crate::DisputeResolution::FullRefund,
    ));
    assert!(client.deposit_funds(&contract_id, &client_addr, &100_i128));

    super::assert_contract_error(
        client.try_raise_dispute(&contract_id, &client_addr),
        EscrowError::LimitOutOfRange,
    );
}

// ─── Boundary values ──────────────────────────────────

#[test]
fn set_max_disputes_at_boundary_succeeds() {
    let (env, contract_id, _admin) = setup_initialized();
    let client = EscrowClient::new(&env, &contract_id);

    assert!(client.set_max_disputes(&MIN_MAX_DISPUTES));
    assert_eq!(client.get_max_disputes(), MIN_MAX_DISPUTES);
    assert!(client.set_max_disputes(&MAX_MAX_DISPUTES));
    assert_eq!(client.get_max_disputes(), MAX_MAX_DISPUTES);
}

// ─── Events ───────────────────────────────────────────

#[test]
fn set_max_disputes_emits_event() {
    let (env, contract_id, _admin) = setup_initialized();
    let client = EscrowClient::new(&env, &contract_id);

    assert!(client.set_max_disputes(&15));
    assert_eq!(client.get_max_disputes(), 15);
}

// ─── Get/set symmetry ──────────────────────────────────

#[test]
fn set_and_get_max_disputes_are_symmetric() {
    let (env, contract_id, _admin) = setup_initialized();
    let client = EscrowClient::new(&env, &contract_id);

    for &val in &[1u32, 5, 10, 50, 100] {
        assert!(client.set_max_disputes(&val));
        assert_eq!(client.get_max_disputes(), val);
    }
}

// ─── Dispute count tracking ─────────────────────────────

#[test]
fn dispute_count_increments_per_raise() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let (client_addr, _freelancer_addr, arbiter_addr, contract_id) =
        super::create_contract_with_arbiter(&env, &client);

    assert!(client.set_max_disputes(&3));

    assert!(client.raise_dispute(&contract_id, &client_addr));
    assert!(client.resolve_dispute(
        &contract_id,
        &arbiter_addr,
        &crate::DisputeResolution::FullRefund,
    ));
    assert!(client.deposit_funds(&contract_id, &client_addr, &100_i128));

    assert!(client.raise_dispute(&contract_id, &client_addr));
    assert!(client.resolve_dispute(
        &contract_id,
        &arbiter_addr,
        &crate::DisputeResolution::FullRefund,
    ));
    assert!(client.deposit_funds(&contract_id, &client_addr, &100_i128));

    assert!(client.raise_dispute(&contract_id, &client_addr));
    assert!(client.resolve_dispute(
        &contract_id,
        &arbiter_addr,
        &crate::DisputeResolution::FullRefund,
    ));
    assert!(client.deposit_funds(&contract_id, &client_addr, &100_i128));

    super::assert_contract_error(
        client.try_raise_dispute(&contract_id, &client_addr),
        EscrowError::LimitOutOfRange,
    );
}

// ─── get_bounds includes configurable max_disputes ─────

#[test]
fn get_bounds_returns_configurable_max_disputes() {
    let (env, contract_id, _admin) = setup_initialized();
    let client = EscrowClient::new(&env, &contract_id);

    assert!(client.set_max_disputes(&42));
    let bounds = client.get_bounds();
    assert_eq!(bounds.max_disputes, 42);
}

#[test]
fn get_bounds_returns_default_max_disputes_before_set() {
    let (env, contract_id, _admin) = setup_initialized();
    let client = EscrowClient::new(&env, &contract_id);

    let bounds = client.get_bounds();
    assert_eq!(bounds.max_disputes, DEFAULT_MAX_DISPUTES);
}

// ─── Validation boundaries: duplicate & boundary inputs ─────

#[test]
fn set_max_disputes_is_idempotent_for_same_value() {
    let (env, contract_id, _admin) = setup_initialized();
    let client = EscrowClient::new(&env, &contract_id);

    assert!(client.set_max_disputes(&25));
    assert_eq!(client.get_max_disputes(), 25);
    // Duplicate submission of the same value must remain deterministic.
    assert!(client.set_max_disputes(&25));
    assert_eq!(client.get_max_disputes(), 25);
}

#[test]
fn set_max_disputes_rejects_min_minus_one() {
    let (env, contract_id, _admin) = setup_initialized();
    let client = EscrowClient::new(&env, &contract_id);

    let below_min = MIN_MAX_DISPUTES - 1;
    super::assert_contract_error(
        client.try_set_max_disputes(&below_min),
        EscrowError::LimitOutOfRange,
    );
    // State must be unchanged after a rejected write.
    assert_eq!(client.get_max_disputes(), DEFAULT_MAX_DISPUTES);
}

#[test]
fn set_max_disputes_rejects_max_plus_one() {
    let (env, contract_id, _admin) = setup_initialized();
    let client = EscrowClient::new(&env, &contract_id);

    let above_max = MAX_MAX_DISPUTES + 1;
    super::assert_contract_error(
        client.try_set_max_disputes(&above_max),
        EscrowError::LimitOutOfRange,
    );
    assert_eq!(client.get_max_disputes(), DEFAULT_MAX_DISPUTES);
}

#[test]
fn set_max_disputes_rejects_u32_max() {
    let (env, contract_id, _admin) = setup_initialized();
    let client = EscrowClient::new(&env, &contract_id);

    super::assert_contract_error(
        client.try_set_max_disputes(&u32::MAX),
        EscrowError::LimitOutOfRange,
    );
    assert_eq!(client.get_max_disputes(), DEFAULT_MAX_DISPUTES);
}

#[test]
fn set_max_disputes_rejection_preserves_previous_value() {
    let (env, contract_id, _admin) = setup_initialized();
    let client = EscrowClient::new(&env, &contract_id);

    assert!(client.set_max_disputes(&7));
    assert_eq!(client.get_max_disputes(), 7);

    super::assert_contract_error(
        client.try_set_max_disputes(&0),
        EscrowError::LimitOutOfRange,
    );
    // Failed update must not corrupt prior configuration.
    assert_eq!(client.get_max_disputes(), 7);
}

#[test]
fn raise_dispute_rejected_when_limit_is_one_and_already_used() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let (client_addr, _freelancer_addr, arbiter_addr, contract_id) =
        super::create_contract_with_arbiter(&env, &client);

    assert!(client.set_max_disputes(&1));

    assert!(client.raise_dispute(&contract_id, &client_addr));
    assert!(client.resolve_dispute(
        &contract_id,
        &arbiter_addr,
        &DisputeResolution::FullRefund,
    ));
    assert!(client.deposit_funds(&contract_id, &client_addr, &100_i128));

    // Boundary: exactly at the limit, next raise must be rejected.
    super::assert_contract_error(
        client.try_raise_dispute(&contract_id, &client_addr),
        EscrowError::LimitOutOfRange,
    );
}

#[test]
fn raise_dispute_rejection_does_not_mutate_contract_state() {
    let env = Env::default();
    env.mock_all_auths();
    let client = register_client(&env);
    let (client_addr, _freelancer_addr, arbiter_addr, contract_id) =
        super::create_contract_with_arbiter(&env, &client);

    assert!(client.set_max_disputes(&1));
    assert!(client.raise_dispute(&contract_id, &client_addr));
    assert!(client.resolve_dispute(
        &contract_id,
        &arbiter_addr,
        &DisputeResolution::FullRefund,
    ));
    assert!(client.deposit_funds(&contract_id, &client_addr, &100_i128));

    let before = client.get_contract(&contract_id);
    super::assert_contract_error(
        client.try_raise_dispute(&contract_id, &client_addr),
        EscrowError::LimitOutOfRange,
    );
    let after = client.get_contract(&contract_id);

    // Rejected raise must not change status or any tracked field.
    assert_eq!(before.status, after.status);
    assert_ne!(after.status, ContractStatus::Disputed);
}

#[test]
fn set_max_disputes_requires_admin_auth() {
    let env = Env::default();
    let contract_id = env.register(Escrow, ());
    let client = EscrowClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    assert!(client.initialize(&admin));

    // No auths mocked: admin-only setter must be rejected.
    assert!(client.try_set_max_disputes(&20).is_err());
    assert_eq!(client.get_max_disputes(), DEFAULT_MAX_DISPUTES);
}

#[test]
fn get_bounds_tracks_latest_set_max_disputes() {
    let (env, contract_id, _admin) = setup_initialized();
    let client = EscrowClient::new(&env, &contract_id);

    assert!(client.set_max_disputes(&MIN_MAX_DISPUTES));
    assert_eq!(client.get_bounds().max_disputes, MIN_MAX_DISPUTES);

    assert!(client.set_max_disputes(&MAX_MAX_DISPUTES));
    assert_eq!(client.get_bounds().max_disputes, MAX_MAX_DISPUTES);

    // Duplicate set of the same value must not desync bounds.
    assert!(client.set_max_disputes(&MAX_MAX_DISPUTES));
    assert_eq!(client.get_bounds().max_disputes, MAX_MAX_DISPUTES);
}
