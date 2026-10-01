#![cfg(test)]

use crate::events::{emit_contract_indexed_event, validate_event_amounts, ContractIndexedEvent};
use crate::EscrowError;
use soroban_sdk::testutils::Events;
use soroban_sdk::{symbol_short, Env, Symbol, TryFromVal};

fn setup_env() -> Env {
    let env = Env::default();
    env.mock_all_auths();
    env
}

fn default_contract() -> crate::Contract {
    crate::Contract {
        status: crate::ContractStatus::Created,
        funded_amount: 0,
        released_amount: 0,
        refunded_amount: 0,
        total_deposited: 0,
        ..Default::default()
    }
}

// ── validate_event_amounts ─────────────────────────────────────────────

#[test]
fn validate_event_amounts_accepts_zero() {
    assert!(validate_event_amounts(0, 0, 0, 0).is_ok());
}

#[test]
fn validate_event_amounts_accepts_positive() {
    assert!(validate_event_amounts(100, 50, 20, 100).is_ok());
}

#[test]
fn validate_event_amounts_accepts_large_values() {
    assert!(validate_event_amounts(i128::MAX, 0, 0, 0).is_ok());
    assert!(validate_event_amounts(0, i128::MAX, 0, 0).is_ok());
}

#[test]
fn validate_event_amounts_rejects_negative_funded() {
    assert_eq!(
        validate_event_amounts(-1, 0, 0, 0),
        Err(EscrowError::AmountMustBePositive)
    );
}

#[test]
fn validate_event_amounts_rejects_negative_released() {
    assert_eq!(
        validate_event_amounts(0, -1, 0, 0),
        Err(EscrowError::AmountMustBePositive)
    );
}

#[test]
fn validate_event_amounts_rejects_negative_refunded() {
    assert_eq!(
        validate_event_amounts(0, 0, -1, 0),
        Err(EscrowError::AmountMustBePositive)
    );
}

#[test]
fn validate_event_amounts_rejects_negative_total_deposited() {
    assert_eq!(
        validate_event_amounts(0, 0, 0, -1),
        Err(EscrowError::AmountMustBePositive)
    );
}

#[test]
fn validate_event_amounts_rejects_multiple_negative() {
    assert_eq!(
        validate_event_amounts(-1, -1, 0, 0),
        Err(EscrowError::AmountMustBePositive)
    );
}

// ── emit_contract_indexed_event bounds ────────────────────────────────

#[test]
#[should_panic(expected = "InvalidContractId")]
fn emit_contract_indexed_event_rejects_zero_id() {
    let env = setup_env();
    let contract = default_contract();
    emit_contract_indexed_event(&env, 0, &contract);
}

#[test]
fn emit_contract_indexed_event_accepts_id_one() {
    let env = setup_env();
    let contract = default_contract();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        emit_contract_indexed_event(&env, 1, &contract);
    }));
    assert!(result.is_ok(), "must accept contract_id == 1");
}

#[test]
fn emit_contract_indexed_event_accepts_id_max() {
    let env = setup_env();
    let contract = default_contract();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        emit_contract_indexed_event(&env, u32::MAX, &contract);
    }));
    assert!(result.is_ok(), "must accept contract_id == u32::MAX");
}

#[test]
fn emit_contract_indexed_event_emits_for_all_status_values() {
    let env = setup_env();
    let statuses = [
        crate::ContractStatus::Created,
        crate::ContractStatus::Funded,
        crate::ContractStatus::Completed,
        crate::ContractStatus::Disputed,
        crate::ContractStatus::Cancelled,
        crate::ContractStatus::Refunded,
        crate::ContractStatus::PartiallyFunded,
    ];
    for status in &statuses {
        let contract = crate::Contract {
            status: *status,
            funded_amount: 100,
            released_amount: 50,
            refunded_amount: 25,
            total_deposited: 100,
            ..Default::default()
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            emit_contract_indexed_event(&env, 1, &contract);
        }));
        assert!(
            result.is_ok(),
            "must emit for status {:?}",
            status
        );
    }
}

// ── deterministic failure recovery ────────────────────────────────────

#[test]
fn emit_contract_indexed_event_is_deterministic_across_repeated_calls() {
    let env = setup_env();
    let contract = crate::Contract {
        status: crate::ContractStatus::Funded,
        funded_amount: 1000,
        released_amount: 300,
        refunded_amount: 100,
        total_deposited: 1000,
        ..Default::default()
    };
    emit_contract_indexed_event(&env, 7, &contract);
    let first = env.events().all();
    emit_contract_indexed_event(&env, 7, &contract);
    let second = env.events().all();
    assert_eq!(first.len(), second.len());
    assert_eq!(first, second, "repeated emissions must be byte-identical");
}

#[test]
fn emit_contract_indexed_event_recovers_after_invalid_id() {
    let env = setup_env();
    let contract = default_contract();
    let bad = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        emit_contract_indexed_event(&env, 0, &contract);
    }));
    assert!(bad.is_err(), "invalid id must be rejected");
    let good = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        emit_contract_indexed_event(&env, 1, &contract);
    }));
    assert!(good.is_ok(), "valid emission must succeed after failure");
    let events = env.events().all();
    assert_eq!(events.len(), 1, "only the successful emission is persisted");
}

#[test]
fn contract_indexed_event_payload_is_stable() {
    let contract = crate::Contract {
        status: crate::ContractStatus::Completed,
        funded_amount: 500,
        released_amount: 500,
        refunded_amount: 0,
        total_deposited: 500,
        ..Default::default()
    };
    let payload = ContractIndexedEvent::from_contract(99, &contract);
    assert_eq!(payload.contract_id, 99);
    assert_eq!(payload.status, crate::ContractStatus::Completed as u32);
    assert_eq!(payload.funded_amount, 500);
    assert_eq!(payload.released_amount, 500);
    assert_eq!(payload.refunded_amount, 0);
    assert_eq!(payload.total_deposited, 500);
}

#[test]
fn emit_contract_indexed_event_emits_at_boundary_amounts() {
    let env = setup_env();
    let contract = crate::Contract {
        status: crate::ContractStatus::Created,
        funded_amount: i128::MAX,
        released_amount: 0,
        refunded_amount: 0,
        total_deposited: i128::MAX,
        ..Default::default()
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        emit_contract_indexed_event(&env, 1, &contract);
    }));
    assert!(result.is_ok(), "must emit for i128::MAX amounts");
}

#[test]
fn emit_contract_indexed_event_emits_with_minimal_contract() {
    let env = setup_env();
    let contract = crate::Contract::default();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        emit_contract_indexed_event(&env, 1, &contract);
    }));
    assert!(result.is_ok(), "must emit for default contract");
}

#[test]
fn emit_contract_indexed_event_publishes_correct_payload_shape() {
    let env = setup_env();
    let contract = crate::Contract {
        status: crate::ContractStatus::Funded,
        funded_amount: 1000,
        released_amount: 300,
        refunded_amount: 100,
        total_deposited: 1000,
        ..Default::default()
    };
    emit_contract_indexed_event(&env, 42, &contract);
    let events = env.events().all();
    let found = events.iter().any(|event| {
        if event.1.len() != 2 {
            return false;
        }
        let t0: Symbol = Symbol::try_from_val(&env, &event.1.get(0).unwrap()).unwrap();
        if t0 != symbol_short!("contract") {
            return false;
        }
        let t1: u32 = TryFromVal::try_from_val(&env, &event.1.get(1).unwrap()).unwrap();
        if t1 != 42 {
            return false;
        }
        let data: (u32, i128, i128, i128, i128) =
            TryFromVal::try_from_val(&env, &event.2).unwrap();
        data == (crate::ContractStatus::Funded as u32, 1000, 300, 100, 1000)
    });
    assert!(found, "event payload must match expected shape");
}

#[test]
fn contract_indexed_topic_no_collision_with_existing_topics() {
    let existing = [
        symbol_short!("init"),
        symbol_short!("created"),
        symbol_short!("mlstn_rls"),
        symbol_short!("ctrct_cmp"),
        symbol_short!("refunded"),
        symbol_short!("pause"),
        symbol_short!("unpaused"),
        symbol_short!("cancelled"),
        symbol_short!("evidence"),
        symbol_short!("fee"),
        symbol_short!("dispute"),
        symbol_short!("admin"),
        symbol_short!("finalized"),
        symbol_short!("deposit"),
        symbol_short!("repr_put"),
        symbol_short!("mlstn_idx"),
        symbol_short!("sttl_bind"),
        symbol_short!("proto_fee"),
    ];
    let contract_topic = symbol_short!("contract");
    for existing_topic in existing.iter() {
        assert_ne!(
            contract_topic, *existing_topic,
            "contract topic must not collide with {:?}",
            existing_topic
        );
    }
}
