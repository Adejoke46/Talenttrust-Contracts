//! Validation boundary tests for the client migration entrypoints.
//!
//! Covers `propose_client_migration`, `accept_client_migration`, and
//! `cancel_client_migration`. Tests are organized into six categories:
//!
//! 1. **Happy path** – valid proposals, acceptances, and cancellations succeed.
//! 2. **Zero / invalid contract_id** – `contract_id == 0` panics with `ContractNotFound`.
//! 3. **Authorization** – only the current client may propose or cancel;
//!    only the proposed address may accept.
//! 4. **Role overlap** – the proposed address must not alias any existing role
//!    (client, freelancer, arbiter, escrow contract).
//! 5. **Status / state invariants** – terminal statuses block proposals;
//!    duplicate proposals are rejected; double-accept is rejected.
//! 6. **TTL / expiry boundary** – a live proposal is accepted at the last
//!    valid ledger; an expired proposal is evicted and cannot be accepted.
//!
//! All tests use `mock_all_auths()` to satisfy Soroban's `require_auth` calls
//! and register an escrow instance via the shared `register_client` helper.
//! No SAC token binding is needed because the migration path does not transfer
//! funds.

#![cfg(test)]

use crate::migration::PendingClientMigration;
use crate::ttl::PENDING_MIGRATION_TTL_LEDGERS;
use crate::types::{Contract, ContractStatus, DataKey};
use crate::{Error, Escrow, EscrowClient, EscrowError, ReleaseAuthorization};
use soroban_sdk::{
    testutils::{Address as _, Ledger as _, LedgerInfo},
    vec, Address, Env,
};

// ─────────────────────────────────────────────────────────────────────────────
// Shared helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Register, initialize, and return an escrow client.
///
/// Does NOT bind a settlement token — migrations do not move funds and tests
/// here do not require one.
fn register_escrow(env: &Env) -> EscrowClient<'_> {
    let id = env.register(Escrow, ());
    let client = EscrowClient::new(env, &id);
    let admin = Address::generate(env);
    client.initialize(&admin);
    client
}

/// Create a basic 3-milestone contract, return `(client_addr, freelancer_addr, contract_id)`.
fn new_contract(env: &Env, client: &EscrowClient) -> (Address, Address, u32) {
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let milestones = vec![env, 1_000_i128, 2_000_i128, 3_000_i128];
    let id = client.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &milestones,
        &ReleaseAuthorization::ClientOnly,
    );
    (client_addr, freelancer_addr, id)
}

/// Force-inject `status` directly into the on-ledger contract record.
///
/// This bypasses business-logic entrypoints so we can reach terminal states
/// (Completed, Disputed) that have no convenient public path in unit tests.
fn force_status(env: &Env, escrow_addr: &Address, contract_id: u32, status: ContractStatus) {
    env.as_contract(escrow_addr, || {
        let key = DataKey::Contract(contract_id);
        let mut c: Contract = env.storage().persistent().get(&key).unwrap();
        c.status = status;
        env.storage().persistent().set(&key, &c);
    });
}

/// Apply the TTL settings used by every TTL-sensitive test.
///
/// Sets `max_entry_ttl` large enough to accommodate the migration TTL so the
/// temporary storage write does not fail the host's entry-size cap.
fn allow_migration_ttl(env: &Env) {
    let info = env.ledger().get();
    env.ledger().set(LedgerInfo {
        sequence_number: info.sequence_number,
        timestamp: info.timestamp,
        protocol_version: info.protocol_version,
        network_id: info.network_id,
        base_reserve: info.base_reserve,
        min_temp_entry_ttl: 1,
        min_persistent_entry_ttl: PENDING_MIGRATION_TTL_LEDGERS * 4,
        max_entry_ttl: PENDING_MIGRATION_TTL_LEDGERS * 4,
    });
}

/// Advance the ledger sequence past the migration TTL window.
fn advance_past_ttl(env: &Env) {
    let info = env.ledger().get();
    env.ledger().set(LedgerInfo {
        sequence_number: info.sequence_number + PENDING_MIGRATION_TTL_LEDGERS + 1,
        timestamp: info.timestamp + u64::from(PENDING_MIGRATION_TTL_LEDGERS) * 6,
        protocol_version: info.protocol_version,
        network_id: [0u8; 32].into(),
        base_reserve: info.base_reserve,
        min_temp_entry_ttl: 1,
        min_persistent_entry_ttl: 1,
        max_entry_ttl: 65_536,
    });
}

/// Assert that a `try_*` call returns the expected contract-level error.
fn assert_err<T: core::fmt::Debug, IE: core::fmt::Debug, E: Into<soroban_sdk::Error>>(
    result: Result<Result<T, IE>, Result<soroban_sdk::Error, soroban_sdk::InvokeError>>,
    expected: E,
) {
    match result {
        Err(Ok(e)) => assert_eq!(e, expected.into(), "unexpected error code"),
        other => panic!("expected contract error, got: {:?}", other),
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// §1  Happy-path tests
// ═════════════════════════════════════════════════════════════════════════════

/// A complete propose → accept cycle updates `contract.client` and clears
/// the pending record.
#[test]
fn propose_and_accept_updates_client_field() {
    let env = Env::default();
    env.mock_all_auths();
    allow_migration_ttl(&env);

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);
    let new_client = Address::generate(&env);

    assert!(escrow.propose_client_migration(&id, &client_addr, &new_client));
    assert!(escrow.has_pending_client_migration(&id));

    assert!(escrow.accept_client_migration(&id, &new_client));

    // contract.client must be updated
    assert_eq!(escrow.get_contract(&id).client, new_client);
    // pending record must be cleared
    assert!(!escrow.has_pending_client_migration(&id));
}

/// A complete propose → cancel cycle returns the contract to a clean state.
#[test]
fn propose_and_cancel_clears_pending_record() {
    let env = Env::default();
    env.mock_all_auths();
    allow_migration_ttl(&env);

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);
    let new_client = Address::generate(&env);

    assert!(escrow.propose_client_migration(&id, &client_addr, &new_client));
    assert!(escrow.has_pending_client_migration(&id));

    assert!(escrow.cancel_client_migration(&id, &client_addr));

    // pending record must be cleared
    assert!(!escrow.has_pending_client_migration(&id));
    // contract.client must NOT have changed
    assert_eq!(escrow.get_contract(&id).client, client_addr);
}

/// `get_pending_client_migration` returns a record whose fields match what was
/// supplied during proposal.
#[test]
fn pending_record_fields_match_proposal_inputs() {
    let env = Env::default();
    env.mock_all_auths();
    allow_migration_ttl(&env);

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);
    let new_client = Address::generate(&env);

    let seq_before = env.ledger().sequence();
    assert!(escrow.propose_client_migration(&id, &client_addr, &new_client));

    let pending: PendingClientMigration = escrow.get_pending_client_migration(&id);
    assert_eq!(pending.current_client, client_addr);
    assert_eq!(pending.proposed_client, new_client);
    assert_eq!(pending.requested_at_ledger, seq_before);
    assert_eq!(
        pending.expires_at_ledger,
        seq_before.saturating_add(PENDING_MIGRATION_TTL_LEDGERS),
        "expires_at_ledger must equal requested_at + PENDING_MIGRATION_TTL_LEDGERS"
    );
}

/// A second migration may be proposed and accepted after the first one completes.
#[test]
fn sequential_migrations_each_succeed() {
    let env = Env::default();
    env.mock_all_auths();
    allow_migration_ttl(&env);

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);

    // First migration
    let new_client1 = Address::generate(&env);
    assert!(escrow.propose_client_migration(&id, &client_addr, &new_client1));
    assert!(escrow.accept_client_migration(&id, &new_client1));
    assert_eq!(escrow.get_contract(&id).client, new_client1);

    // Second migration (new_client1 is now the current client)
    let new_client2 = Address::generate(&env);
    assert!(escrow.propose_client_migration(&id, &new_client1, &new_client2));
    assert!(escrow.accept_client_migration(&id, &new_client2));
    assert_eq!(escrow.get_contract(&id).client, new_client2);
}

// ═════════════════════════════════════════════════════════════════════════════
// §2  contract_id boundary: zero is always invalid
// ═════════════════════════════════════════════════════════════════════════════

/// `propose_client_migration` with `contract_id == 0` panics with
/// `ContractNotFound` (from `validate_contract_id_bounds`).
#[test]
fn propose_with_zero_contract_id_panics_contract_not_found() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow = register_escrow(&env);
    let client_addr = Address::generate(&env);
    let new_client = Address::generate(&env);

    assert_err(
        escrow.try_propose_client_migration(&0u32, &client_addr, &new_client),
        EscrowError::ContractNotFound,
    );
}

/// `accept_client_migration` with `contract_id == 0` panics with
/// `ContractNotFound`.
#[test]
fn accept_with_zero_contract_id_panics_contract_not_found() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow = register_escrow(&env);
    let new_client = Address::generate(&env);

    assert_err(
        escrow.try_accept_client_migration(&0u32, &new_client),
        EscrowError::ContractNotFound,
    );
}

/// `cancel_client_migration` with `contract_id == 0` panics with
/// `ContractNotFound`.
#[test]
fn cancel_with_zero_contract_id_panics_contract_not_found() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow = register_escrow(&env);
    let client_addr = Address::generate(&env);

    assert_err(
        escrow.try_cancel_client_migration(&0u32, &client_addr),
        EscrowError::ContractNotFound,
    );
}

/// A nonexistent (but non-zero) `contract_id` panics with `ContractNotFound`.
#[test]
fn propose_with_nonexistent_contract_id_panics_contract_not_found() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow = register_escrow(&env);
    let client_addr = Address::generate(&env);
    let new_client = Address::generate(&env);

    // Contract 999 was never created
    assert_err(
        escrow.try_propose_client_migration(&999u32, &client_addr, &new_client),
        EscrowError::ContractNotFound,
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// §3  Authorization checks
// ═════════════════════════════════════════════════════════════════════════════

/// Only the stored `contract.client` may call `propose_client_migration`.
/// A freelancer, arbiter, or random third party is rejected with
/// `UnauthorizedRole`.
#[test]
fn propose_by_freelancer_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow = register_escrow(&env);
    let (_client_addr, freelancer_addr, id) = new_contract(&env, &escrow);
    let new_client = Address::generate(&env);

    assert_err(
        escrow.try_propose_client_migration(&id, &freelancer_addr, &new_client),
        EscrowError::UnauthorizedRole,
    );
}

#[test]
fn propose_by_random_third_party_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow = register_escrow(&env);
    let (_client_addr, _freelancer, id) = new_contract(&env, &escrow);
    let attacker = Address::generate(&env);
    let new_client = Address::generate(&env);

    assert_err(
        escrow.try_propose_client_migration(&id, &attacker, &new_client),
        EscrowError::UnauthorizedRole,
    );
}

/// Only the address named in the proposal may call `accept_client_migration`.
/// The original client, freelancer, and a random third party must all be
/// rejected with `UnauthorizedRole`.
#[test]
fn accept_by_original_client_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    allow_migration_ttl(&env);

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);
    let new_client = Address::generate(&env);

    assert!(escrow.propose_client_migration(&id, &client_addr, &new_client));

    assert_err(
        escrow.try_accept_client_migration(&id, &client_addr),
        EscrowError::UnauthorizedRole,
    );
}

#[test]
fn accept_by_freelancer_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    allow_migration_ttl(&env);

    let escrow = register_escrow(&env);
    let (client_addr, freelancer_addr, id) = new_contract(&env, &escrow);
    let new_client = Address::generate(&env);

    assert!(escrow.propose_client_migration(&id, &client_addr, &new_client));

    assert_err(
        escrow.try_accept_client_migration(&id, &freelancer_addr),
        EscrowError::UnauthorizedRole,
    );
}

#[test]
fn accept_by_third_party_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    allow_migration_ttl(&env);

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);
    let new_client = Address::generate(&env);
    let attacker = Address::generate(&env);

    assert!(escrow.propose_client_migration(&id, &client_addr, &new_client));

    assert_err(
        escrow.try_accept_client_migration(&id, &attacker),
        EscrowError::UnauthorizedRole,
    );
}

/// Only the stored `contract.client` may cancel a pending migration.
/// A random third party is rejected with `UnauthorizedRole`.
#[test]
fn cancel_by_third_party_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    allow_migration_ttl(&env);

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);
    let new_client = Address::generate(&env);
    let attacker = Address::generate(&env);

    assert!(escrow.propose_client_migration(&id, &client_addr, &new_client));

    assert_err(
        escrow.try_cancel_client_migration(&id, &attacker),
        EscrowError::UnauthorizedRole,
    );
}

/// The freelancer cannot cancel a pending migration.
#[test]
fn cancel_by_freelancer_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    allow_migration_ttl(&env);

    let escrow = register_escrow(&env);
    let (client_addr, freelancer_addr, id) = new_contract(&env, &escrow);
    let new_client = Address::generate(&env);

    assert!(escrow.propose_client_migration(&id, &client_addr, &new_client));

    assert_err(
        escrow.try_cancel_client_migration(&id, &freelancer_addr),
        EscrowError::UnauthorizedRole,
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// §4  Role-overlap checks
// ═════════════════════════════════════════════════════════════════════════════

/// Proposing the current client's own address collapses client→client and must
/// be rejected with `RoleOverlap`.
#[test]
fn propose_current_client_as_new_client_is_role_overlap() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);

    assert_err(
        escrow.try_propose_client_migration(&id, &client_addr, &client_addr),
        EscrowError::RoleOverlap,
    );
}

/// Proposing the freelancer's address collapses client-freelancer roles and
/// must be rejected with `RoleOverlap`.
#[test]
fn propose_freelancer_as_new_client_is_role_overlap() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow = register_escrow(&env);
    let (client_addr, freelancer_addr, id) = new_contract(&env, &escrow);

    assert_err(
        escrow.try_propose_client_migration(&id, &client_addr, &freelancer_addr),
        EscrowError::RoleOverlap,
    );
}

/// Proposing the arbiter's address collapses client-arbiter roles and must be
/// rejected with `RoleOverlap`.
#[test]
fn propose_arbiter_as_new_client_is_role_overlap() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow = register_escrow(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let id = escrow.create_contract(
        &client_addr,
        &freelancer_addr,
        &Some(arbiter_addr.clone()),
        &vec![&env, 1_000_i128, 2_000_i128],
        &ReleaseAuthorization::ClientOnly,
    );

    assert_err(
        escrow.try_propose_client_migration(&id, &client_addr, &arbiter_addr),
        EscrowError::RoleOverlap,
    );
}

/// Proposing the escrow contract's own address must be rejected with
/// `RoleOverlap` to prevent circular custody references.
#[test]
fn propose_escrow_address_as_new_client_is_role_overlap() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);
    let escrow_addr = escrow.address.clone();

    assert_err(
        escrow.try_propose_client_migration(&id, &client_addr, &escrow_addr),
        EscrowError::RoleOverlap,
    );
}

/// Late-binding role overlap: the proposed address is valid at proposal time
/// but the freelancer role is updated between proposal and acceptance.
/// `accept_client_migration` must detect this and reject with `RoleOverlap`.
#[test]
fn accept_detects_late_binding_freelancer_overlap() {
    let env = Env::default();
    env.mock_all_auths();
    allow_migration_ttl(&env);

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);
    let new_client = Address::generate(&env);

    // Proposal succeeds — new_client does not overlap any role yet.
    assert!(escrow.propose_client_migration(&id, &client_addr, &new_client));

    // Simulate freelancer being changed to match the proposed client (off-chain
    // storage manipulation via env.as_contract mirrors how roles could shift via
    // other means between propose and accept).
    let escrow_addr = escrow.address.clone();
    env.as_contract(&escrow_addr, || {
        let key = DataKey::Contract(id);
        let mut c: Contract = env.storage().persistent().get(&key).unwrap();
        c.freelancer = new_client.clone();
        env.storage().persistent().set(&key, &c);
    });

    assert_err(
        escrow.try_accept_client_migration(&id, &new_client),
        EscrowError::RoleOverlap,
    );
}

/// Late-binding role overlap: the arbiter is set to match the proposed client
/// between proposal and acceptance.
#[test]
fn accept_detects_late_binding_arbiter_overlap() {
    let env = Env::default();
    env.mock_all_auths();
    allow_migration_ttl(&env);

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);
    let new_client = Address::generate(&env);

    assert!(escrow.propose_client_migration(&id, &client_addr, &new_client));

    let escrow_addr = escrow.address.clone();
    env.as_contract(&escrow_addr, || {
        let key = DataKey::Contract(id);
        let mut c: Contract = env.storage().persistent().get(&key).unwrap();
        c.arbiter = Some(new_client.clone());
        env.storage().persistent().set(&key, &c);
    });

    assert_err(
        escrow.try_accept_client_migration(&id, &new_client),
        EscrowError::RoleOverlap,
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// §5  Status and state invariants
// ═════════════════════════════════════════════════════════════════════════════

/// All four terminal statuses must block proposals with
/// `InvalidStatusTransition`.

#[test]
fn propose_blocked_on_completed_status() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);
    let new_client = Address::generate(&env);

    force_status(&env, &escrow.address.clone(), id, ContractStatus::Completed);

    assert_err(
        escrow.try_propose_client_migration(&id, &client_addr, &new_client),
        EscrowError::InvalidStatusTransition,
    );
}

#[test]
fn propose_blocked_on_cancelled_status() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);
    let new_client = Address::generate(&env);

    force_status(&env, &escrow.address.clone(), id, ContractStatus::Cancelled);

    assert_err(
        escrow.try_propose_client_migration(&id, &client_addr, &new_client),
        EscrowError::InvalidStatusTransition,
    );
}

#[test]
fn propose_blocked_on_refunded_status() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);
    let new_client = Address::generate(&env);

    force_status(&env, &escrow.address.clone(), id, ContractStatus::Refunded);

    assert_err(
        escrow.try_propose_client_migration(&id, &client_addr, &new_client),
        EscrowError::InvalidStatusTransition,
    );
}

#[test]
fn propose_blocked_on_disputed_status() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);
    let new_client = Address::generate(&env);

    force_status(&env, &escrow.address.clone(), id, ContractStatus::Disputed);

    assert_err(
        escrow.try_propose_client_migration(&id, &client_addr, &new_client),
        EscrowError::InvalidStatusTransition,
    );
}

/// A second `propose_client_migration` while a live proposal already exists
/// must be rejected with `InvalidState`.
#[test]
fn duplicate_proposal_while_pending_is_invalid_state() {
    let env = Env::default();
    env.mock_all_auths();
    allow_migration_ttl(&env);

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);
    let new_client1 = Address::generate(&env);
    let new_client2 = Address::generate(&env);

    assert!(escrow.propose_client_migration(&id, &client_addr, &new_client1));

    assert_err(
        escrow.try_propose_client_migration(&id, &client_addr, &new_client2),
        EscrowError::InvalidState,
    );
}

/// After a successful acceptance the pending record is cleared.
/// A second `accept_client_migration` call must fail with `InvalidState`.
#[test]
fn double_accept_after_successful_migration_fails_invalid_state() {
    let env = Env::default();
    env.mock_all_auths();
    allow_migration_ttl(&env);

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);
    let new_client = Address::generate(&env);

    assert!(escrow.propose_client_migration(&id, &client_addr, &new_client));
    assert!(escrow.accept_client_migration(&id, &new_client));

    assert_err(
        escrow.try_accept_client_migration(&id, &new_client),
        EscrowError::InvalidState,
    );
}

/// Cancelling when there is no pending migration must fail with `InvalidState`.
#[test]
fn cancel_without_pending_migration_fails_invalid_state() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);

    assert_err(
        escrow.try_cancel_client_migration(&id, &client_addr),
        EscrowError::InvalidState,
    );
}

/// Accepting when there is no pending migration must fail with `InvalidState`.
#[test]
fn accept_without_pending_migration_fails_invalid_state() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow = register_escrow(&env);
    let (_client_addr, _freelancer, id) = new_contract(&env, &escrow);
    let new_client = Address::generate(&env);

    assert_err(
        escrow.try_accept_client_migration(&id, &new_client),
        EscrowError::InvalidState,
    );
}

/// A contract in `Created` status (the initial state) permits proposals.
#[test]
fn propose_allowed_on_created_status() {
    let env = Env::default();
    env.mock_all_auths();
    allow_migration_ttl(&env);

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);
    assert_eq!(escrow.get_contract(&id).status, ContractStatus::Created);

    let new_client = Address::generate(&env);
    assert!(escrow.propose_client_migration(&id, &client_addr, &new_client));
}

/// A contract in `Accepted` status permits proposals.
#[test]
fn propose_allowed_on_accepted_status() {
    let env = Env::default();
    env.mock_all_auths();
    allow_migration_ttl(&env);

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);

    force_status(&env, &escrow.address.clone(), id, ContractStatus::Accepted);

    let new_client = Address::generate(&env);
    assert!(escrow.propose_client_migration(&id, &client_addr, &new_client));
}

// ═════════════════════════════════════════════════════════════════════════════
// §6  TTL / expiry boundary conditions
// ═════════════════════════════════════════════════════════════════════════════

/// Advancing the ledger past `PENDING_MIGRATION_TTL_LEDGERS` causes the
/// temporary entry to be evicted. Both `has_pending_client_migration` and
/// `accept_client_migration` must reflect the eviction.
#[test]
fn expired_proposal_cannot_be_accepted() {
    let env = Env::default();
    env.mock_all_auths();
    allow_migration_ttl(&env);

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);
    let new_client = Address::generate(&env);

    assert!(escrow.propose_client_migration(&id, &client_addr, &new_client));
    assert!(escrow.has_pending_client_migration(&id));

    advance_past_ttl(&env);

    assert!(
        !escrow.has_pending_client_migration(&id),
        "entry must be evicted after TTL expires"
    );

    assert_err(
        escrow.try_accept_client_migration(&id, &new_client),
        EscrowError::InvalidState,
    );
}

/// An expired proposal cannot be cancelled either — the entry is gone.
#[test]
fn expired_proposal_cannot_be_cancelled() {
    let env = Env::default();
    env.mock_all_auths();
    allow_migration_ttl(&env);

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);
    let new_client = Address::generate(&env);

    assert!(escrow.propose_client_migration(&id, &client_addr, &new_client));
    advance_past_ttl(&env);

    assert_err(
        escrow.try_cancel_client_migration(&id, &client_addr),
        EscrowError::InvalidState,
    );
}

/// A proposal accepted on the ledger just before `expires_at_ledger` must
/// succeed. This pins the inclusive upper boundary of the TTL window to prevent
/// off-by-one eviction bugs.
#[test]
fn proposal_accepted_at_last_ledger_before_expiry_succeeds() {
    let env = Env::default();
    env.mock_all_auths();
    allow_migration_ttl(&env);

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);
    let new_client = Address::generate(&env);

    assert!(escrow.propose_client_migration(&id, &client_addr, &new_client));

    let pending: PendingClientMigration = escrow.get_pending_client_migration(&id);
    let expires_at = pending.expires_at_ledger;

    // Advance to the very last ledger that is still inside the window.
    let info = env.ledger().get();
    env.ledger().set(LedgerInfo {
        sequence_number: expires_at - 1,
        timestamp: info.timestamp + 5,
        protocol_version: info.protocol_version,
        network_id: info.network_id,
        base_reserve: info.base_reserve,
        min_temp_entry_ttl: 1,
        min_persistent_entry_ttl: PENDING_MIGRATION_TTL_LEDGERS * 4,
        max_entry_ttl: PENDING_MIGRATION_TTL_LEDGERS * 4,
    });

    assert!(
        escrow.has_pending_client_migration(&id),
        "proposal must still be live at expires_at - 1"
    );
    assert!(escrow.accept_client_migration(&id, &new_client));
    assert_eq!(escrow.get_contract(&id).client, new_client);
}

/// After the first proposal expires the client may submit a fresh proposal.
#[test]
fn fresh_proposal_allowed_after_previous_expires() {
    let env = Env::default();
    env.mock_all_auths();
    allow_migration_ttl(&env);

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);
    let new_client1 = Address::generate(&env);

    assert!(escrow.propose_client_migration(&id, &client_addr, &new_client1));

    advance_past_ttl(&env);
    assert!(!escrow.has_pending_client_migration(&id));

    // Re-open the TTL window so a new proposal can be stored.
    allow_migration_ttl(&env);

    let new_client2 = Address::generate(&env);
    assert!(
        escrow.propose_client_migration(&id, &client_addr, &new_client2),
        "fresh proposal must succeed after prior proposal expired"
    );
}

/// `expires_at_ledger` stored in the record equals
/// `requested_at_ledger + PENDING_MIGRATION_TTL_LEDGERS`.
#[test]
fn expiry_field_matches_ttl_constant() {
    let env = Env::default();
    env.mock_all_auths();
    allow_migration_ttl(&env);

    let escrow = register_escrow(&env);
    let (client_addr, _freelancer, id) = new_contract(&env, &escrow);
    let new_client = Address::generate(&env);

    let seq_before = env.ledger().sequence();
    assert!(escrow.propose_client_migration(&id, &client_addr, &new_client));

    let pending: PendingClientMigration = escrow.get_pending_client_migration(&id);
    assert_eq!(
        pending.expires_at_ledger,
        seq_before.saturating_add(PENDING_MIGRATION_TTL_LEDGERS),
        "expires_at_ledger must equal seq_before + PENDING_MIGRATION_TTL_LEDGERS"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// §7  Pause gate
// ═════════════════════════════════════════════════════════════════════════════

/// `propose_client_migration` is blocked while the contract is paused.
#[test]
fn propose_blocked_when_contract_is_paused() {
    let env = Env::default();
    env.mock_all_auths();
    allow_migration_ttl(&env);

    let id = env.register(Escrow, ());
    let escrow = EscrowClient::new(&env, &id);
    let admin = Address::generate(&env);
    escrow.initialize(&admin);
    // Consume nonce 1 for pause
    escrow.pause(&1u64);

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let contract_id = {
        // Create the contract via storage bypass since the escrow is paused.
        // We inject the contract directly to avoid the public API gate.
        env.as_contract(&id, || {
            let key = DataKey::Contract(1);
            env.storage().persistent().set(
                &key,
                &Contract {
                    client: client_addr.clone(),
                    freelancer: freelancer_addr.clone(),
                    arbiter: None,
                    status: ContractStatus::Created,
                    total_deposited: 0,
                    funded_amount: 0,
                    released_amount: 0,
                    refunded_amount: 0,
                    release_authorization: ReleaseAuthorization::ClientOnly,
                    reputation_issued: false,
                },
            );
            env.storage()
                .persistent()
                .set(&DataKey::NextContractId, &1u32);
        });
        1u32
    };

    let new_client = Address::generate(&env);
    assert_err(
        escrow.try_propose_client_migration(&contract_id, &client_addr, &new_client),
        Error::ContractPaused,
    );
}

/// `cancel_client_migration` is blocked while the contract is paused.
#[test]
fn cancel_blocked_when_contract_is_paused() {
    let env = Env::default();
    env.mock_all_auths();
    allow_migration_ttl(&env);

    // Set up escrow and create a contract before pausing.
    let id = env.register(Escrow, ());
    let escrow = EscrowClient::new(&env, &id);
    let admin = Address::generate(&env);
    escrow.initialize(&admin);

    let (client_addr, _freelancer, contract_id) = new_contract(&env, &escrow);
    let new_client = Address::generate(&env);
    assert!(escrow.propose_client_migration(&contract_id, &client_addr, &new_client));

    // Now pause — nonce 1 is the first consumed nonce.
    escrow.pause(&1u64);

    assert_err(
        escrow.try_cancel_client_migration(&contract_id, &client_addr),
        Error::ContractPaused,
    );
}
