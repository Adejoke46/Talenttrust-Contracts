//! Compatibility regression tests for `contracts/escrow/src/lib.rs`.
//!
//! These tests verify that every public entrypoint behaves deterministically
//! for boundary, empty, malformed, and pre-initialization inputs so that
//! callers cannot encounter silent data loss, ambiguous panics, or unexpected
//! success.  They exercise the public ABI via [`EscrowClient`] only — no
//! internal symbols are imported.
//!
//! # Coverage groups
//!
//! 1. `contract_id = 0` panics with the correct error on every read entrypoint.
//! 2. `contract_id = u32::MAX` returns `ContractNotFound`, not a misrouted key.
//! 3. `get_settlement_token` / `is_settlement_token_bound` before and after bind.
//! 4. `get_bounds` and `get_next_contract_id` succeed pre-initialization.
//! 5. Empty `milestone_indices` on `refund_unreleased_milestones`.
//! 6. `get_approval_deadline` returns `None` when no approval exists.
//! 7. Read entrypoints on a `PartiallyFunded` contract return consistent state.
//! 8. Alias entrypoints delegate correctly and return the same data as canonical.

#![cfg(test)]

use crate::{
    ContractStatus, DepositMode, Escrow, EscrowClient, EscrowError, ReleaseAuthorization,
};
use soroban_sdk::{
    testutils::Address as _,
    token::StellarAssetClient,
    vec, Address, Env, Vec,
};

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Register a new Escrow contract and return its client together with a freshly
/// initialized admin.  No settlement token is bound; tests that need one must
/// call `bind_settlement_token` themselves.
fn setup_initialized(env: &Env) -> (EscrowClient<'_>, Address) {
    let addr = env.register(Escrow, ());
    let client = EscrowClient::new(env, &addr);
    let admin = Address::generate(env);
    client.initialize(&admin);
    (client, admin)
}

/// Register + initialize + bind a SAC settlement token.
/// Returns `(client, admin, token_address)`.
fn setup_with_token(env: &Env) -> (EscrowClient<'_>, Address, Address) {
    let (client, admin) = setup_initialized(env);
    let token = env.register_stellar_asset_contract(admin.clone());
    client.bind_settlement_token(&admin, &token);
    (client, admin, token)
}

/// Create a funded 1-milestone contract and return the contract ID.
fn create_funded_contract(
    env: &Env,
    client: &EscrowClient<'_>,
    token: &Address,
    client_addr: &Address,
    freelancer_addr: &Address,
    amount: i128,
) -> u32 {
    let milestones = vec![env, amount];
    let cid = client.create_contract(
        client_addr,
        freelancer_addr,
        &None,
        &milestones,
        &ReleaseAuthorization::ClientOnly,
    );
    StellarAssetClient::new(env, token).mint(client_addr, &amount);
    client.deposit_funds(&cid, client_addr, &amount);
    cid
}

// ─── 1. contract_id = 0 panics on every read entrypoint ─────────────────────

#[test]
fn get_contract_zero_id_returns_contract_not_found() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _) = setup_initialized(&env);

    let result = client.try_get_contract(&0);
    super::assert_contract_error(result, EscrowError::ContractNotFound);
}

#[test]
fn get_milestones_zero_id_returns_contract_not_found() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _) = setup_initialized(&env);

    let result = client.try_get_milestones(&0);
    super::assert_contract_error(result, EscrowError::ContractNotFound);
}

#[test]
fn get_milestone_zero_id_returns_contract_not_found() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _) = setup_initialized(&env);

    let result = client.try_get_milestone(&0, &0);
    super::assert_contract_error(result, EscrowError::ContractNotFound);
}

#[test]
fn get_contract_summary_zero_id_returns_contract_not_found() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _) = setup_initialized(&env);

    let result = client.try_get_contract_summary(&0);
    super::assert_contract_error(result, EscrowError::ContractNotFound);
}

#[test]
fn get_refundable_balance_zero_id_returns_contract_not_found() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _) = setup_initialized(&env);

    let result = client.try_get_refundable_balance(&0);
    super::assert_contract_error(result, EscrowError::ContractNotFound);
}

#[test]
fn get_remaining_balance_zero_id_returns_contract_not_found() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _) = setup_initialized(&env);

    let result = client.try_get_remaining_balance(&0);
    super::assert_contract_error(result, EscrowError::ContractNotFound);
}

// ─── 2. contract_id = u32::MAX returns ContractNotFound, not a key error ─────

#[test]
fn get_contract_max_id_returns_contract_not_found() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _) = setup_initialized(&env);

    let result = client.try_get_contract(&u32::MAX);
    super::assert_contract_error(result, EscrowError::ContractNotFound);
}

#[test]
fn get_milestones_max_id_returns_contract_not_found() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _) = setup_initialized(&env);

    let result = client.try_get_milestones(&u32::MAX);
    super::assert_contract_error(result, EscrowError::ContractNotFound);
}

#[test]
fn get_milestone_max_id_returns_contract_not_found() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _) = setup_initialized(&env);

    let result = client.try_get_milestone(&u32::MAX, &0);
    super::assert_contract_error(result, EscrowError::ContractNotFound);
}

// ─── 3. Settlement token — before and after bind ─────────────────────────────

#[test]
fn get_settlement_token_returns_none_before_bind() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _) = setup_initialized(&env);

    assert_eq!(client.get_settlement_token(), None);
}

#[test]
fn is_settlement_token_bound_returns_false_before_bind() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _) = setup_initialized(&env);

    assert!(!client.is_settlement_token_bound());
}

#[test]
fn get_settlement_token_returns_some_after_bind() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, admin, token) = setup_with_token(&env);

    assert_eq!(client.get_settlement_token(), Some(token));
}

#[test]
fn is_settlement_token_bound_returns_true_after_bind() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _, _) = setup_with_token(&env);

    assert!(client.is_settlement_token_bound());
}

// ─── 4. get_bounds and get_next_contract_id — pre-initialization ──────────────

/// `get_bounds` must succeed even before `initialize` is called.
#[test]
fn get_bounds_succeeds_pre_initialize() {
    let env = Env::default();
    env.mock_all_auths();
    let addr = env.register(Escrow, ());
    let client = EscrowClient::new(&env, &addr);

    // Should not panic — no initialization required.
    let bounds = client.get_bounds();
    assert!(bounds.max_milestones > 0, "max_milestones should be positive");
    assert!(bounds.max_fee_bps > 0, "max_fee_bps should be positive");
    assert!(
        bounds.max_single_milestone_stroops > 0,
        "max_single_milestone_stroops should be positive"
    );
}

/// `get_next_contract_id` must return 1 (the initial value) before any
/// contracts are created, and must succeed even before `initialize`.
#[test]
fn get_next_contract_id_returns_one_pre_initialize() {
    let env = Env::default();
    env.mock_all_auths();
    let addr = env.register(Escrow, ());
    let client = EscrowClient::new(&env, &addr);

    // Should not panic.
    let next = client.get_next_contract_id();
    assert_eq!(next, 1, "next contract id before init should be 1");
}

/// `get_next_contract_id` increments after each `create_contract` call.
#[test]
fn get_next_contract_id_increments_after_create() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, _token) = setup_with_token(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);

    let before = client.get_next_contract_id();

    client.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &vec![&env, 1_000_000_i128],
        &ReleaseAuthorization::ClientOnly,
    );

    let after = client.get_next_contract_id();
    assert_eq!(after, before + 1, "next id must increment after create");
}

// ─── 5. Empty milestone_indices on refund_unreleased_milestones ───────────────

#[test]
fn refund_empty_milestone_indices_returns_empty_refund_request() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, token) = setup_with_token(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);

    let cid = create_funded_contract(
        &env,
        &client,
        &token,
        &client_addr,
        &freelancer_addr,
        1_000_000_i128,
    );

    let empty: Vec<u32> = Vec::new(&env);
    let result = client.try_refund_unreleased_milestones(&cid, &empty);
    super::assert_contract_error(result, EscrowError::EmptyRefundRequest);
}

// ─── 6. get_approval_deadline — None when no approval exists ─────────────────

#[test]
fn get_approval_deadline_returns_none_with_no_approval() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, token) = setup_with_token(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);

    let cid = create_funded_contract(
        &env,
        &client,
        &token,
        &client_addr,
        &freelancer_addr,
        1_000_000_i128,
    );

    // No approval has been submitted yet.
    let deadline = client.get_approval_deadline(&cid, &0);
    assert!(
        deadline.is_none(),
        "get_approval_deadline must return None before any approval is recorded"
    );
}

/// After an approval is submitted, `get_approval_deadline` returns `Some`.
#[test]
fn get_approval_deadline_returns_some_after_approval() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, token) = setup_with_token(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);

    let cid = create_funded_contract(
        &env,
        &client,
        &token,
        &client_addr,
        &freelancer_addr,
        1_000_000_i128,
    );

    client.approve_milestone_release(&cid, &client_addr, &0);

    let deadline = client.get_approval_deadline(&cid, &0);
    assert!(
        deadline.is_some(),
        "get_approval_deadline must return Some after approval is recorded"
    );
}

// ─── 7. PartiallyFunded contract — read entrypoints return consistent state ───

/// Incremental deposit leaves the contract in `PartiallyFunded` status when
/// only part of the total milestone amount has been deposited.  All read
/// entrypoints must return consistent, non-panicking results in this state.
#[test]
fn read_entrypoints_consistent_on_partially_funded_contract() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, token) = setup_with_token(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);

    // Two milestones; we will only partially fund (one of two).
    let total = 2_000_000_i128;
    let partial = 1_000_000_i128;
    let milestones = vec![&env, 1_000_000_i128, 1_000_000_i128];

    let cid = client.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &milestones,
        &ReleaseAuthorization::ClientOnly,
    );

    // Mint and deposit only half the total to get PartiallyFunded status.
    StellarAssetClient::new(&env, &token).mint(&client_addr, &partial);
    client.deposit_funds(&cid, &client_addr, &partial);

    // Verify status is PartiallyFunded.
    let contract = client.get_contract(&cid);
    assert_eq!(
        contract.status,
        ContractStatus::PartiallyFunded,
        "partial deposit must yield PartiallyFunded status"
    );

    // get_milestones must not panic.
    let milestones_read = client.get_milestones(&cid);
    assert_eq!(milestones_read.len(), 2, "must return both milestones");

    // get_milestone must not panic.
    let m0 = client.get_milestone(&cid, &0);
    assert!(m0.is_some(), "milestone 0 must be readable");

    // get_contract_summary must not panic.
    let summary = client.get_contract_summary(&cid);
    assert_eq!(summary.funded_amount, partial);

    // get_refundable_balance must equal funded_amount (no releases yet).
    let refundable = client.get_refundable_balance(&cid);
    assert_eq!(refundable, partial);

    // contract_exists must return true.
    assert!(client.contract_exists(&cid));
}

// ─── 8. Alias entrypoints delegate correctly ─────────────────────────────────

/// `get_authorization_records_page` must return the same result as
/// `get_authorization_records` for the same arguments.
#[test]
#[allow(deprecated)]
fn get_authorization_records_page_matches_canonical() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, token) = setup_with_token(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);

    let cid = create_funded_contract(
        &env,
        &client,
        &token,
        &client_addr,
        &freelancer_addr,
        1_000_000_i128,
    );

    let canonical = client.get_authorization_records(&cid, &0, &10);
    let alias = client.get_authorization_records_page(&cid, &0, &10);
    assert_eq!(
        canonical.len(),
        alias.len(),
        "alias must return same number of records as canonical"
    );
}

/// `list_authorization_records` must also match the canonical result.
#[test]
#[allow(deprecated)]
fn list_authorization_records_matches_canonical() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, token) = setup_with_token(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);

    let cid = create_funded_contract(
        &env,
        &client,
        &token,
        &client_addr,
        &freelancer_addr,
        1_000_000_i128,
    );

    let canonical = client.get_authorization_records(&cid, &0, &10);
    let alias = client.list_authorization_records(&cid, &0, &10);
    assert_eq!(
        canonical.len(),
        alias.len(),
        "list alias must return same number of records as canonical"
    );
}

// ─── 9. contract_exists — boundary behavior ───────────────────────────────────

#[test]
fn contract_exists_returns_false_for_nonexistent_id() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _) = setup_initialized(&env);

    // id=0: even the sentinel value must not panic — just return false.
    assert!(
        !client.contract_exists(&0),
        "contract_exists must return false for id=0"
    );
    assert!(
        !client.contract_exists(&999),
        "contract_exists must return false for unknown id"
    );
    assert!(
        !client.contract_exists(&u32::MAX),
        "contract_exists must return false for u32::MAX"
    );
}

#[test]
fn contract_exists_returns_true_for_created_contract() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, _token) = setup_with_token(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);

    let cid = client.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &vec![&env, 1_000_000_i128],
        &ReleaseAuthorization::ClientOnly,
    );
    assert!(
        client.contract_exists(&cid),
        "contract_exists must return true for a freshly created contract"
    );
}

// ─── 10. get_milestone returns None for out-of-bounds index ──────────────────

#[test]
fn get_milestone_returns_none_for_oob_index() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, _token) = setup_with_token(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);

    let cid = client.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &vec![&env, 1_000_000_i128],
        &ReleaseAuthorization::ClientOnly,
    );

    // Only milestone 0 exists; index 1 and u32::MAX must return None, not panic.
    assert!(
        client.get_milestone(&cid, &1).is_none(),
        "out-of-bounds index must return None"
    );
    assert!(
        client.get_milestone(&cid, &u32::MAX).is_none(),
        "u32::MAX index must return None"
    );
}

// ─── 11. Double-bind settlement token ────────────────────────────────────────

#[test]
fn bind_settlement_token_twice_returns_already_bound() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, admin, token) = setup_with_token(&env);

    // Second bind must fail.
    let result = client.try_bind_settlement_token(&admin, &token);
    super::assert_contract_error(result, EscrowError::SettlementTokenAlreadyBound);
}

// ─── 12. initialize — idempotency guard ──────────────────────────────────────

#[test]
fn double_initialize_returns_already_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, admin) = setup_initialized(&env);

    let result = client.try_initialize(&admin);
    super::assert_contract_error(result, EscrowError::AlreadyInitialized);
}
