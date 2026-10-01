//! Compatibility-contract tests for `contracts/escrow/src/governance.rs` (#1446).
//!
//! These tests pin the behavior the module's rustdoc declares as the frozen
//! compatibility surface, so any regression fails loudly here instead of
//! silently breaking deployed clients or indexers:
//!
//! 1. **Empty data**: every getter is total — before *and* after
//!    `initialize` it returns the documented default rather than failing.
//! 2. **Boundaries**: each setter accepts its exact min/max and rejects one
//!    step outside with the same typed error it has always used.
//! 3. **Duplicates / replays**: repeated sets are idempotent; a replayed
//!    admin nonce is rejected with `StaleNonce` and leaves state untouched.
//! 4. **Failure atomicity**: a rejected call persists nothing, burns no
//!    nonce, and emits no event (retry with the same nonce must succeed).
//! 5. **Event shapes**: topics and payload arity of every governance event
//!    are stable — indexers branch on them.
//! 6. **Authorization**: setters fail closed pre-initialization and reject a
//!    mismatched admin argument with `UnauthorizedRole`.

#![cfg(test)]

use crate::{
    Error, Escrow, EscrowClient, GovernedParameters, ADMIN_ROTATION_MIN_DELAY_LEDGERS,
    ADMIN_ROTATION_PROPOSAL_TTL_LEDGERS, MAX_FEE_BPS, MAX_MAX_MILESTONES, MIN_MAX_MILESTONES,
};
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Events, Ledger as _, LedgerInfo},
    Address, Env, Symbol, TryIntoVal, Val, Vec,
};

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Fresh `Env` with all auths mocked (entrypoint authorization is checked by
/// the contract itself via `require_auth`, which the mock satisfies).
fn mocked_env() -> Env {
    let env = Env::default();
    env.mock_all_auths();
    env
}

/// Register and initialize an escrow contract; return the client and admin.
fn initialized(env: &Env) -> (EscrowClient<'_>, Address) {
    let id = env.register(Escrow, ());
    let client = EscrowClient::new(env, &id);
    let admin = Address::generate(env);
    assert!(client.initialize(&admin), "initialize must succeed");
    (client, admin)
}

/// Env with a persistent-entry TTL generous enough to survive advancing the
/// ledger past `ADMIN_ROTATION_PROPOSAL_TTL_LEDGERS` without archiving the
/// contract instance out from under the test.
fn generous_ttl_env() -> Env {
    let env = Env::default();
    env.mock_all_auths();
    let initial = env.ledger().get();
    let generous_ttl = (ADMIN_ROTATION_PROPOSAL_TTL_LEDGERS * 2).max(initial.max_entry_ttl);
    env.ledger().set(LedgerInfo {
        min_persistent_entry_ttl: generous_ttl,
        max_entry_ttl: generous_ttl,
        ..initial
    });
    env
}

/// Advance the ledger by `delta` sequences (and proportionally bump time).
fn advance_ledgers(env: &Env, delta: u32) {
    let info = env.ledger().get();
    env.ledger().set(LedgerInfo {
        sequence_number: info.sequence_number + delta,
        timestamp: info.timestamp + (delta as u64) * 5,
        ..info
    });
}

/// Count events published under exactly one topic named `topic`.
fn single_topic_event_count(env: &Env, topic: &str) -> u32 {
    let want = Symbol::new(env, topic);
    let mut count = 0u32;
    for (_contract, topics, _data) in env.events().all().iter() {
        if topics.len() != 1 {
            continue;
        }
        let t0: Symbol = topics.get(0).unwrap().try_into_val(env).unwrap();
        if t0 == want {
            count += 1;
        }
    }
    count
}

/// Payload of the most recent single-topic event named `topic`.
fn last_single_topic_payload(env: &Env, topic: &str) -> Val {
    let want = Symbol::new(env, topic);
    let mut found: Option<Val> = None;
    for (_contract, topics, data) in env.events().all().iter() {
        if topics.len() != 1 {
            continue;
        }
        let t0: Symbol = topics.get(0).unwrap().try_into_val(env).unwrap();
        if t0 == want {
            found = Some(data);
        }
    }
    found.expect("expected governance event not found")
}

/// Payload of the most recent two-topic event matching `(first, second)`.
fn last_double_topic_payload(env: &Env, first: Symbol, second: Symbol) -> Val {
    let mut found: Option<Val> = None;
    for (_contract, topics, data) in env.events().all().iter() {
        if topics.len() != 2 {
            continue;
        }
        let t0: Symbol = topics.get(0).unwrap().try_into_val(env).unwrap();
        let t1: Symbol = topics.get(1).unwrap().try_into_val(env).unwrap();
        if t0 == first && t1 == second {
            found = Some(data);
        }
    }
    found.expect("expected admin-rotation event not found")
}

/// Decode an event payload tuple into its fields. The SDK encodes published
/// tuples as ScVal vectors, so asserting `len()` pins the payload *arity* —
/// the exact property indexers branch on.
fn event_fields(env: &Env, payload: Val) -> Vec<Val> {
    payload
        .try_into_val(env)
        .expect("event payload must decode as a vec of fields")
}

// ── 1. Empty-data defaults (total readers) ──────────────────────────────────

/// Every governance getter must work — and return its documented default —
/// on a contract that was never initialized (fresh deploy, pre-configuration
/// reads by dashboards and indexers).
#[test]
fn readers_return_documented_defaults_before_initialize() {
    let env = mocked_env();
    let cid = env.register(Escrow, ());
    let client = EscrowClient::new(&env, &cid);

    assert_eq!(client.get_protocol_fee_bps(), 0u32);
    assert_eq!(client.get_max_milestones(), crate::MAX_MILESTONES);
    assert_eq!(client.get_fee_withdrawal_cap(), 5_000u32);
    assert_eq!(client.get_fee_withdrawal_cooldown(), 17_280u32);
    assert_eq!(client.get_last_fee_withdrawal_ledger(), 0u32);
    assert_eq!(client.get_governed_parameters(), None);
    assert_eq!(client.get_pending_admin(), None);
}

/// `initialize` must not implicitly write governance values — the defaults
/// survive it unchanged (upgrade-from-empty compatibility).
#[test]
fn readers_return_same_defaults_after_initialize() {
    let env = mocked_env();
    let (client, _) = initialized(&env);

    assert_eq!(client.get_protocol_fee_bps(), 0u32);
    assert_eq!(client.get_max_milestones(), crate::MAX_MILESTONES);
    assert_eq!(client.get_fee_withdrawal_cap(), 5_000u32);
    assert_eq!(client.get_fee_withdrawal_cooldown(), 17_280u32);
    assert_eq!(client.get_last_fee_withdrawal_ledger(), 0u32);
    assert_eq!(client.get_governed_parameters(), None);
    assert_eq!(client.get_pending_admin(), None);
}

/// Readers are side-effect free: repeat calls never emit events.
#[test]
fn readers_emit_no_events_and_are_repeatable() {
    let env = mocked_env();
    let (client, _) = initialized(&env);
    let before = env.events().all().len();

    for _ in 0..2 {
        let _ = client.get_protocol_fee_bps();
        let _ = client.get_max_milestones();
        let _ = client.get_fee_withdrawal_cap();
        let _ = client.get_fee_withdrawal_cooldown();
        let _ = client.get_last_fee_withdrawal_ledger();
        let _ = client.get_governed_parameters();
        let _ = client.get_pending_admin();
    }

    assert_eq!(
        before,
        env.events().all().len(),
        "governance readers must not emit events"
    );
}

// ── 2. Protocol fee bps: boundary, atomicity, replay ────────────────────────

/// The ceiling is inclusive and one step above is rejected; because the
/// rejection is a rollback, the failed call must not burn the admin nonce —
/// the next valid call retries successfully with the *same* nonce.
#[test]
fn protocol_fee_bps_boundary_rejection_does_not_burn_nonce() {
    let env = mocked_env();
    let (client, _) = initialized(&env);

    assert!(client.set_protocol_fee_bps(&MAX_FEE_BPS, &1u64));
    assert_eq!(client.get_protocol_fee_bps(), MAX_FEE_BPS);

    super::assert_contract_error(
        client.try_set_protocol_fee_bps(&(MAX_FEE_BPS + 1), &2u64),
        Error::InvalidProtocolParameters,
    );
    // Rejected call persisted nothing and emitted nothing.
    assert_eq!(client.get_protocol_fee_bps(), MAX_FEE_BPS);
    assert_eq!(single_topic_event_count(&env, "protocol_fee_bps"), 1);

    // Nonce 2 is still the next expected nonce — retries are safe.
    assert!(client.set_protocol_fee_bps(&250u32, &2u64));
    assert_eq!(client.get_protocol_fee_bps(), 250u32);
}

/// A replayed nonce is rejected with `StaleNonce` without touching the
/// stored fee; re-applying the *same* value with the next nonce succeeds
/// and remains idempotent.
#[test]
fn protocol_fee_bps_replay_rejected_and_duplicate_is_idempotent() {
    let env = mocked_env();
    let (client, _) = initialized(&env);

    assert!(client.set_protocol_fee_bps(&100u32, &1u64));

    super::assert_contract_error(
        client.try_set_protocol_fee_bps(&9_000u32, &1u64),
        Error::StaleNonce,
    );
    assert_eq!(client.get_protocol_fee_bps(), 100u32);

    assert!(client.set_protocol_fee_bps(&100u32, &2u64));
    assert_eq!(client.get_protocol_fee_bps(), 100u32);
}

/// Direct-setter event contract: single topic `protocol_fee_bps` with the
/// 4-field payload `(old_bps, new_bps, admin, timestamp)`.
#[test]
fn protocol_fee_bps_event_shape_is_stable() {
    let env = mocked_env();
    let (client, admin) = initialized(&env);

    client.set_protocol_fee_bps(&250u32, &1u64);

    let fields = event_fields(&env, last_single_topic_payload(&env, "protocol_fee_bps"));
    assert_eq!(fields.len(), 4, "payload must keep 4 fields");
    let old_bps: u32 = fields.get(0).unwrap().try_into_val(&env).unwrap();
    let new_bps: u32 = fields.get(1).unwrap().try_into_val(&env).unwrap();
    let emitted_admin: Address = fields.get(2).unwrap().try_into_val(&env).unwrap();
    let timestamp: u64 = fields.get(3).unwrap().try_into_val(&env).unwrap();
    assert_eq!(old_bps, 0u32);
    assert_eq!(new_bps, 250u32);
    assert_eq!(emitted_admin, admin);
    assert_eq!(timestamp, env.ledger().timestamp());
}

// ── 3. Max milestones ───────────────────────────────────────────────────────

/// Default is `crate::MAX_MILESTONES`; both inclusive bounds are accepted and
/// one step outside either end is rejected with `LimitOutOfRange` without
/// persisting.
#[test]
fn max_milestones_bounds_are_enforced_and_rejects_do_not_persist() {
    let env = mocked_env();
    let (client, _) = initialized(&env);

    assert_eq!(client.get_max_milestones(), crate::MAX_MILESTONES);

    assert!(client.set_max_milestones(&MAX_MAX_MILESTONES));
    assert_eq!(client.get_max_milestones(), MAX_MAX_MILESTONES);
    super::assert_contract_error(
        client.try_set_max_milestones(&(MAX_MAX_MILESTONES + 1)),
        Error::LimitOutOfRange,
    );
    assert_eq!(client.get_max_milestones(), MAX_MAX_MILESTONES);

    assert!(client.set_max_milestones(&MIN_MAX_MILESTONES));
    assert_eq!(client.get_max_milestones(), MIN_MAX_MILESTONES);
    super::assert_contract_error(client.try_set_max_milestones(&0u32), Error::LimitOutOfRange);
    assert_eq!(client.get_max_milestones(), MIN_MAX_MILESTONES);
}

// ── 4. Fee-withdrawal rate limits ───────────────────────────────────────────

/// `0` (disabled) and `10_000` (100 %) are legal; `10_001` is rejected and
/// the last accepted value survives. Event keeps its 4-field shape.
#[test]
fn fee_withdrawal_cap_boundaries_and_event_shape() {
    let env = mocked_env();
    let (client, admin) = initialized(&env);

    assert!(client.set_fee_withdrawal_cap(&0u32));
    assert_eq!(client.get_fee_withdrawal_cap(), 0u32);
    assert!(client.set_fee_withdrawal_cap(&10_000u32));
    super::assert_contract_error(
        client.try_set_fee_withdrawal_cap(&10_001u32),
        Error::InvalidProtocolParameters,
    );
    assert_eq!(client.get_fee_withdrawal_cap(), 10_000u32);

    let fields = event_fields(&env, last_single_topic_payload(&env, "fee_cap"));
    assert_eq!(fields.len(), 4, "payload must keep 4 fields");
    let old_cap: u32 = fields.get(0).unwrap().try_into_val(&env).unwrap();
    let new_cap: u32 = fields.get(1).unwrap().try_into_val(&env).unwrap();
    let emitted_admin: Address = fields.get(2).unwrap().try_into_val(&env).unwrap();
    assert_eq!(old_cap, 0u32, "rejected 10_001 must not append an event");
    assert_eq!(new_cap, 10_000u32);
    assert_eq!(emitted_admin, admin);
}

/// `0` (disabled) and the ≈150-day ceiling are legal; one step above the
/// ceiling is rejected without persisting. Event keeps its 4-field shape.
#[test]
fn fee_withdrawal_cooldown_boundaries_and_event_shape() {
    let env = mocked_env();
    let (client, admin) = initialized(&env);

    assert!(client.set_fee_withdrawal_cooldown(&0u32));
    assert_eq!(client.get_fee_withdrawal_cooldown(), 0u32);
    assert!(client.set_fee_withdrawal_cooldown(&2_592_000u32));
    super::assert_contract_error(
        client.try_set_fee_withdrawal_cooldown(&2_592_001u32),
        Error::InvalidProtocolParameters,
    );
    assert_eq!(client.get_fee_withdrawal_cooldown(), 2_592_000u32);

    let fields = event_fields(&env, last_single_topic_payload(&env, "fee_cooldown"));
    assert_eq!(fields.len(), 4, "payload must keep 4 fields");
    let old_cooldown: u32 = fields.get(0).unwrap().try_into_val(&env).unwrap();
    let new_cooldown: u32 = fields.get(1).unwrap().try_into_val(&env).unwrap();
    let emitted_admin: Address = fields.get(2).unwrap().try_into_val(&env).unwrap();
    assert_eq!(old_cooldown, 0u32);
    assert_eq!(new_cooldown, 2_592_000u32);
    assert_eq!(emitted_admin, admin);
}

// ── 5. Governed parameters ──────────────────────────────────────────────────

/// Rejections (wrong admin, fee over ceiling, non-positive cap) must leave
/// the parameter store, the readiness flag, and the event log untouched.
#[test]
fn governed_params_rejections_leave_no_state() {
    let env = mocked_env();
    let (client, admin) = initialized(&env);
    let impostor = Address::generate(&env);

    super::assert_contract_error(
        client.try_set_governed_params(&impostor, &100u32, &1_000_i128),
        Error::UnauthorizedRole,
    );
    super::assert_contract_error(
        client.try_set_governed_params(&admin, &(MAX_FEE_BPS + 1), &1_000_i128),
        Error::InvalidProtocolParameters,
    );
    super::assert_contract_error(
        client.try_set_governed_params(&admin, &0u32, &0_i128),
        Error::InvalidProtocolParameters,
    );
    super::assert_contract_error(
        client.try_set_governed_params(&admin, &0u32, &(-1_i128)),
        Error::InvalidProtocolParameters,
    );

    assert_eq!(client.get_governed_parameters(), None);
    assert!(
        !client.get_mainnet_readiness_info().governed_params_set,
        "rejections must not flip the readiness checklist"
    );
    assert_eq!(single_topic_event_count(&env, "governed_parameters"), 0);
}

/// The struct setter and the legacy flat wrapper must write the *same*
/// storage shape, and the audit event must carry
/// `(Some(old), new, admin, timestamp)` with fields intact.
#[test]
fn governed_params_round_trip_preserves_struct_layout_and_audit_event() {
    let env = mocked_env();
    let (client, admin) = initialized(&env);

    let first = GovernedParameters {
        protocol_fee_bps: 250,
        max_escrow_total_stroops: 1_000_000_i128,
    };
    assert!(client.set_governed_parameters(&admin, &first));
    assert_eq!(client.get_governed_parameters(), Some(first.clone()));
    assert!(client.get_mainnet_readiness_info().governed_params_set);

    // Legacy wrapper writes the identical key/shape.
    assert!(client.set_governed_params(&admin, &300u32, &2_000_i128));
    let second = GovernedParameters {
        protocol_fee_bps: 300,
        max_escrow_total_stroops: 2_000_i128,
    };
    assert_eq!(client.get_governed_parameters(), Some(second.clone()));

    let fields = event_fields(&env, last_single_topic_payload(&env, "governed_parameters"));
    assert_eq!(fields.len(), 4, "payload must keep 4 fields");
    let old_params: Option<GovernedParameters> =
        fields.get(0).unwrap().try_into_val(&env).unwrap();
    let new_params: GovernedParameters = fields.get(1).unwrap().try_into_val(&env).unwrap();
    let emitted_admin: Address = fields.get(2).unwrap().try_into_val(&env).unwrap();
    assert_eq!(old_params, Some(first), "old value must be auditable");
    assert_eq!(new_params, second);
    assert_eq!(emitted_admin, admin);
}

// ── 6. Fail-closed authorization across the whole surface ──────────────────

/// Every governance mutator must reject pre-initialization calls with
/// `NotInitialized`; nothing may be configured before an admin exists.
#[test]
fn every_governance_mutator_fails_closed_before_initialize() {
    let env = mocked_env();
    let cid = env.register(Escrow, ());
    let client = EscrowClient::new(&env, &cid);
    let admin = Address::generate(&env);
    let params = GovernedParameters {
        protocol_fee_bps: 100,
        max_escrow_total_stroops: 1_000_i128,
    };

    super::assert_contract_error(
        client.try_set_protocol_fee_bps(&100u32, &1u64),
        Error::NotInitialized,
    );
    super::assert_contract_error(client.try_set_max_milestones(&5u32), Error::NotInitialized);
    super::assert_contract_error(
        client.try_set_fee_withdrawal_cap(&1_000u32),
        Error::NotInitialized,
    );
    super::assert_contract_error(
        client.try_set_fee_withdrawal_cooldown(&1_000u32),
        Error::NotInitialized,
    );
    super::assert_contract_error(
        client.try_set_governed_params(&admin, &100u32, &1_000_i128),
        Error::NotInitialized,
    );
    super::assert_contract_error(
        client.try_set_governed_parameters(&admin, &params),
        Error::NotInitialized,
    );
    super::assert_contract_error(client.try_propose_admin(&admin), Error::NotInitialized);
    super::assert_contract_error(client.try_accept_admin(), Error::NotInitialized);
    super::assert_contract_error(client.try_cancel_admin(), Error::NotInitialized);
    super::assert_contract_error(client.try_recover_admin_proposal(), Error::NotInitialized);
}

// ── 7. Admin-rotation event topology ────────────────────────────────────────

/// Rotation events must keep the `(symbol_short!("admin"), <verb>)` topic
/// pair and the 3-field `(address, address, timestamp)` payload — the shape
/// the admin-rotation runbook and dashboards decode.
#[test]
fn admin_rotation_event_topics_and_payloads_are_stable() {
    let env = generous_ttl_env();
    let (client, admin) = initialized(&env);
    let proposed = Address::generate(&env);

    assert!(client.propose_admin(&proposed));
    let fields = event_fields(
        &env,
        last_double_topic_payload(&env, symbol_short!("admin"), Symbol::new(&env, "proposed")),
    );
    assert_eq!(fields.len(), 3, "proposed payload must keep 3 fields");
    let event_admin: Address = fields.get(0).unwrap().try_into_val(&env).unwrap();
    let event_proposed: Address = fields.get(1).unwrap().try_into_val(&env).unwrap();
    assert_eq!(event_admin, admin);
    assert_eq!(event_proposed, proposed);

    advance_ledgers(&env, ADMIN_ROTATION_MIN_DELAY_LEDGERS);
    assert!(client.accept_admin());
    let fields = event_fields(
        &env,
        last_double_topic_payload(&env, symbol_short!("admin"), Symbol::new(&env, "accepted")),
    );
    assert_eq!(fields.len(), 3, "accepted payload must keep 3 fields");
    let old_admin: Address = fields.get(0).unwrap().try_into_val(&env).unwrap();
    let new_admin: Address = fields.get(1).unwrap().try_into_val(&env).unwrap();
    assert_eq!(old_admin, admin);
    assert_eq!(new_admin, proposed);
    assert_eq!(client.get_pending_admin(), None);
}
