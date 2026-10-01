use super::*;
use crate::rollback::{
    clear_dispute_rollback, has_rollback_record, rollback_dispute_impl,
    rollback_milestone_count, store_dispute_rollback, validate_rollback_record,
    MAX_ROLLBACK_MILESTONES,
};
use soroban_sdk::{restutils::Address as _, Address, Env, Vec};

/// Helper that builds a minimal contract with the given status.
fn make_contract(env: &Env, status: ContractStatus) -> Contract {
    Contract {
        client: Address::generate(env),
        freelancer: Address::generate(env),
        arbiter: Address::generate(env),
        total_amount: 1000,
        funded_amount: 0,
        status,
        deadline: 0,
    }
}

/// Helper that builds a milestone with the given index.
fn make_milestone(env: &Env, idx: u32) -> Milestone {
    Milestone {
        idx: idx,
        amount: 10,
        status: MilestoneStatus::Pending,
    }
}

/// Helper that builds a milestone vec with `count` unique indices.
fn make_milestones(env: &Env, count: u32) -> Vec<Milestone> {
    let mut milestones = Vec::new(env);
    for i in 0..count {
        milestones.push_back(make_milestone(env, i));
    }
    milestones
}

///-----------------------------------------------------------------------------
/// Accepted input
///-----------------------------------------------------------------------------

#[test]
fn test_validate_accepts_funded_contract() {
    let env = Env::default();
    let contract = make_contract(&env, ContractStatus::Funded);
    let milestones = make_milestones(&env, 3);
    validate_rollback_record(&env, 1, &contract, &milestones);
}

#[test]
fn test_validate_accepts_partially_funded_contract() {
    let env = Env::default();
    let contract = make_contract(&env, ContractStatus::PartiallyFunded);
    let milestones = make_milestones(&env, 1);
    validate_rollback_record(&env, 1, &contract, &milestones);
}

///-----------------------------------------------------------------------------
/// Rejected input
///-----------------------------------------------------------------------------

#[test]
#[should_panic]
fn test_validate_rejects_disputed_contract() {
    let env = Env::default();
    let contract = make_contract(&env, ContractStatus::Disputed);
    let milestones = make_milestones(&env, 1);
    validate_rollback_record(&env, 1, &contract, &milestones);
}

#[test]
#[should_panic]
fn test_validate_rejects_completed_contract() {
    let env = Env::default();
    let contract = make_contract(&env, ContractStatus::Completed);
    let milestones = make_milestones(&env, 1);
    validate_rollback_record(&env, 1, &contract, &milestones);
}

#[test]
#[should_panic]
fn test_validate_rejects_empty_milestones() {
    let env = Env::default();
    let contract = make_contract(&env, ContractStatus::Funded);
    let milestones = Vec::new(&env);
    validate_rollback_record(&env, 1, &contract, &milestones);
}

#[test]
#[should_panic]
fn test_validate_rejects_too_many_milestones() {
    let env = Env::default();
    let contract = make_contract(&env, ContractStatus::Funded);
    let milestones = make_milestones(&env, MAX_ROLLBACK_MILESTONES + 1);
    validate_rollback_record(&env, 1, &contract, &milestones);
}

///-----------------------------------------------------------------------------
/// Duplicate submissions
///-----------------------------------------------------------------------------

#[test]
#[should_panic]
fn test_validate_rejects_duplicate_milestone_indices() {
    let env = Env::default();
    let contract = make_contract(&env, ContractStatus::Funded);
    let mut milestones = Vec::new(&env);
    milestones.push_back(make_milestone(&env, 0));
    milestones.push_back(make_milestone(&env, 0));
    validate_rollback_record(&env, 1, &contract, &milestones);
}

///-----------------------------------------------------------------------------
/// Boundary values
///-----------------------------------------------------------------------------

#[test]
fn test_validate_accepts_max_milestones() {
    let env = Env::default();
    let contract = make_contract(&env, ContractStatus::Funded);
    let milestones = make_milestones(&env, MAX_ROLLBACK_MILESTONES);
    validate_rollback_record(&env, 1, &contract, &milestones);
}

#[test]
#[should_panic]
fn test_validate_rejects_index_at_bound() {
    let env = Env::default();
    let contract = make_contract(&env, ContractStatus::Funded);
    let mut milestones = Vec::news(&env);
    milestones.push_back(make_milestone(&env, MAX_ROLLBACK_INDEX));
    validate_rollback_record(&env, 1, &contract, &milestones);
}

#[test]
#[should_panic]
fn test_validate_rejects_invalid_contract_id() {
    let env = Env::default();
    let contract = make_contract(&env, ContractStatus::Funded);
    let milestones = make_milestones(&env, 1);
    // 0 is not a valid contract id bound.
    validate_rollback_record(&env, 0, &contract, &milestones);
}

///-----------------------------------------------------------------------------
/// Storage integration and regression
///-----------------------------------------------------------------------------

#[test]
fn test_store_rejects_invalid_record_before_writing() {
    let env = Env::default();
    let contract = make_contract(&env, ContractStatus::Disputed);
    let milestones = make_milestones(&env, 1);
    let result = std::panic::catch_unwind(assert_unwind_safe(|| {
        store_dispute_rollback(&env, 1, &contract, &milestones);
    }));
    assert!(result.is_err());
    // No record must have been written.
    assert!(!has_rollback_record(&env, 1));
}

#[test]
fn test_store_and_clear_roundtrip() {
    let env = Env::default();
    let contract = make_contract(&env, ContractStatus::Funded);
    let milestones = make_milestones(&env, 2);
    store_dispute_rollback(&env, 1, &contract, &milestones);
    assert!(has_rollback_record(&env, 1));
    assert_eq!(rollback_milestone_count(&env, 1), 2);
    clear_dispute_rollback(&env, 1);
    assert!(!has_rollback_record(&env, 1));
    assert_eq!(rollback_milestone_count(&env, 1), 0);
}

#[test]
#[should_panic]
fn test_rollback_rejects_when_no_record() {
    let env = Env::default();
    env.mock_all_auths();
    // Without an initialized escrow, the rollback must fail closed.
    rollback_dispute_impl(&env, 1);
}

#[test]
#[should_panic]
fn test_rollback_rejects_invalid_contract_id() {
    let env = Env::default();
    env.mock_all_auths();
    rollback_dispute_impl(&env, 0);
}

#[test]
#[should_panic]
fn test_rollback_rejects_duplicate_call_after_clear() {
    let env = Env::default();
    env.mock_all_auths();
    // Simulate a completed rollback by clearing the record and then
    // attempting again. The second call must fail closed because the
    // record is gone.
    clear_dispute_rollback(&env, 1);
    rollback_dispute_impl(&env, 1);
}
