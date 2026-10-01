use super::{assert_contract_error, complete_contract_funded, register_client_with_token};
use crate::{Contract, DataKey, EscrowClient, EscrowError, Reputation};
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Events},
    token, Address, Env, IntoVal, String, TryFromVal, Val, Vec,
};

fn setup() -> (Env, Address, Address, Address, u32) {
    let env = Env::default();
    env.mock_all_auths();
    let (client, token) = register_client_with_token(&env);
    let (payer, freelancer, id) = complete_contract_funded(&env, &client, &token);
    let address = client.address.clone();
    (env, address, payer, freelancer, id)
}

#[derive(Debug, PartialEq)]
struct ReputationSnapshot {
    contract_issued: bool,
    pending: Option<i128>,
    aggregate: Option<Reputation>,
    issued_marker: Option<bool>,
    index: Vec<Address>,
    comment: Option<String>,
}

fn snapshot(env: &Env, address: &Address, freelancer: &Address, id: u32) -> ReputationSnapshot {
    env.as_contract(address, || {
        let contract: Contract = env
            .storage()
            .persistent()
            .get(&DataKey::Contract(id))
            .unwrap();
        ReputationSnapshot {
            contract_issued: contract.reputation_issued,
            pending: env
                .storage()
                .persistent()
                .get(&DataKey::PendingReputationCredits(freelancer.clone())),
            aggregate: env
                .storage()
                .persistent()
                .get(&DataKey::Reputation(freelancer.clone())),
            issued_marker: env
                .storage()
                .persistent()
                .get(&DataKey::ReputationIssued(id)),
            index: env
                .storage()
                .persistent()
                .get(&DataKey::ReputationIndex)
                .unwrap_or_else(|| Vec::new(env)),
            comment: env
                .storage()
                .persistent()
                .get(&DataKey::ReputationComment(id)),
        }
    })
}

#[test]
fn legacy_marker_blocks_replay_without_consuming_another_credit() {
    let (env, address, payer, freelancer, id) = setup();
    let client = EscrowClient::new(&env, &address);
    env.as_contract(&address, || {
        env.storage()
            .persistent()
            .set(&DataKey::ReputationIssued(id), &true);
        env.storage().persistent().set(
            &DataKey::ReputationComment(id),
            &String::from_str(&env, "legacy"),
        );
    });
    let before = snapshot(&env, &address, &freelancer, id);
    assert_contract_error(
        client.try_issue_reputation(&id, &payer, &5, &String::from_str(&env, "new")),
        EscrowError::ReputationAlreadyIssued,
    );
    assert_eq!(snapshot(&env, &address, &freelancer, id), before);
}

#[test]
fn contract_flag_blocks_replay_when_legacy_marker_is_missing_or_false() {
    let (env, address, payer, freelancer, id) = setup();
    let client = EscrowClient::new(&env, &address);
    env.as_contract(&address, || {
        let mut contract: Contract = env
            .storage()
            .persistent()
            .get(&DataKey::Contract(id))
            .unwrap();
        contract.reputation_issued = true;
        env.storage()
            .persistent()
            .set(&DataKey::Contract(id), &contract);
    });
    let before = snapshot(&env, &address, &freelancer, id);
    assert_contract_error(
        client.try_issue_reputation(&id, &payer, &5, &String::from_str(&env, "new")),
        EscrowError::ReputationAlreadyIssued,
    );
    assert_eq!(snapshot(&env, &address, &freelancer, id), before);
    assert!(client.get_contract_summary(&id).reputation_issued);

    env.as_contract(&address, || {
        env.storage()
            .persistent()
            .set(&DataKey::ReputationIssued(id), &false);
    });
    assert!(client.get_contract_summary(&id).reputation_issued);
    let before = snapshot(&env, &address, &freelancer, id);
    assert_contract_error(
        client.try_issue_reputation(&id, &payer, &5, &String::from_str(&env, "new")),
        EscrowError::ReputationAlreadyIssued,
    );
    assert_eq!(snapshot(&env, &address, &freelancer, id), before);
}

#[test]
fn unversioned_reputation_preserves_totals_and_index_on_success_and_retry() {
    let (env, address, payer, freelancer, id) = setup();
    let client = EscrowClient::new(&env, &address);
    env.as_contract(&address, || {
        env.storage().persistent().set(
            &DataKey::Reputation(freelancer.clone()),
            &Reputation {
                completed_contracts: 2,
                total_rating: 7,
                last_rating: 3,
            },
        );
        let mut index = Vec::new(&env);
        index.push_back(freelancer.clone());
        env.storage()
            .persistent()
            .set(&DataKey::ReputationIndex, &index);
    });
    let comment = String::from_str(&env, "compatible");
    assert!(client.issue_reputation(&id, &payer, &5, &comment));
    let topics: Vec<Val> = soroban_sdk::vec![
        &env,
        symbol_short!("rep_issd").into_val(&env),
        id.into_val(&env),
    ];
    let payload = (freelancer.clone(), 5_u32, env.ledger().timestamp());
    let mut emitted = 0;
    for (event_address, event_topics, event_data) in env.events().all().iter() {
        if event_address == address && event_topics == topics {
            let actual = <(Address, u32, u64)>::try_from_val(&env, &event_data).unwrap();
            assert_eq!(actual, payload);
            emitted += 1;
        }
    }
    assert_eq!(
        emitted, 1,
        "deployed topic and payload must remain compatible"
    );
    assert_eq!(
        client.get_reputation(&freelancer),
        Some(Reputation {
            completed_contracts: 3,
            total_rating: 12,
            last_rating: 5,
        })
    );
    assert_eq!(client.get_average_rating(&freelancer), Some(40_000));
    assert_eq!(client.get_reputations_page(&0, &10).len(), 1);
    assert_eq!(client.get_pending_reputation_credits(&freelancer), 0);
    assert_eq!(client.get_reputation_comment(&id), Some(comment.clone()));
    let before = snapshot(&env, &address, &freelancer, id);
    assert_contract_error(
        client.try_issue_reputation(&id, &payer, &5, &comment),
        EscrowError::ReputationAlreadyIssued,
    );
    assert_eq!(snapshot(&env, &address, &freelancer, id), before);
}

#[test]
fn aggregate_overflow_rejects_without_markers_comments_or_credit_changes() {
    for rep in [
        Reputation {
            completed_contracts: i128::MAX,
            total_rating: 0,
            last_rating: 0,
        },
        Reputation {
            completed_contracts: 1,
            total_rating: i128::MAX,
            last_rating: 5,
        },
    ] {
        let (env, address, payer, freelancer, id) = setup();
        let client = EscrowClient::new(&env, &address);
        env.as_contract(&address, || {
            env.storage()
                .persistent()
                .set(&DataKey::Reputation(freelancer.clone()), &rep);
        });
        let before = snapshot(&env, &address, &freelancer, id);
        assert_contract_error(
            client.try_issue_reputation(&id, &payer, &5, &String::from_str(&env, "overflow")),
            EscrowError::PotentialOverflow,
        );
        assert_eq!(snapshot(&env, &address, &freelancer, id), before);
    }
}

#[test]
fn missing_or_invalid_credit_rejects_without_partial_issuance() {
    for pending in [None, Some(0_i128), Some(-1_i128)] {
        let (env, address, payer, freelancer, id) = setup();
        let client = EscrowClient::new(&env, &address);
        env.as_contract(&address, || {
            let key = DataKey::PendingReputationCredits(freelancer.clone());
            if let Some(value) = pending {
                env.storage().persistent().set(&key, &value);
            } else {
                env.storage().persistent().remove(&key);
            }
        });
        let before = snapshot(&env, &address, &freelancer, id);
        assert_contract_error(
            client.try_issue_reputation(&id, &payer, &5, &String::from_str(&env, "credit")),
            EscrowError::NotCompleted,
        );
        assert_eq!(snapshot(&env, &address, &freelancer, id), before);
    }
}

#[test]
fn unauthorized_caller_cannot_change_reputation_or_credits() {
    let (env, address, _payer, freelancer, id) = setup();
    let client = EscrowClient::new(&env, &address);
    let before = snapshot(&env, &address, &freelancer, id);
    assert_contract_error(
        client.try_issue_reputation(
            &id,
            &freelancer,
            &5,
            &String::from_str(&env, "unauthorized"),
        ),
        EscrowError::UnauthorizedRole,
    );
    assert_eq!(snapshot(&env, &address, &freelancer, id), before);
}

#[test]
fn negative_aggregate_rejects_without_rewriting_legacy_state() {
    for rep in [
        Reputation {
            completed_contracts: -1,
            total_rating: 0,
            last_rating: 0,
        },
        Reputation {
            completed_contracts: 1,
            total_rating: -1,
            last_rating: 0,
        },
        Reputation {
            completed_contracts: 1,
            total_rating: 5,
            last_rating: -1,
        },
    ] {
        let (env, address, payer, freelancer, id) = setup();
        let client = EscrowClient::new(&env, &address);
        env.as_contract(&address, || {
            env.storage()
                .persistent()
                .set(&DataKey::Reputation(freelancer.clone()), &rep);
        });
        let before = snapshot(&env, &address, &freelancer, id);
        assert_contract_error(
            client.try_issue_reputation(&id, &payer, &5, &String::from_str(&env, "invalid")),
            EscrowError::InvalidState,
        );
        assert_eq!(snapshot(&env, &address, &freelancer, id), before);
    }
}

#[test]
fn credit_overflow_rolls_back_final_release_and_token_transfer() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, settlement_token) = register_client_with_token(&env);
    let payer = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let id = client.create_contract(
        &payer,
        &freelancer,
        &None,
        &super::default_milestones(&env),
        &crate::ReleaseAuthorization::ClientOnly,
    );
    let total = super::total_milestone_amount();
    token::StellarAssetClient::new(&env, &settlement_token).mint(&payer, &total);
    client.deposit_funds(&id, &payer, &total);
    for index in 0..2 {
        client.approve_milestone_release(&id, &payer, &index);
        client.release_milestone(&id, &payer, &index);
    }
    client.approve_milestone_release(&id, &payer, &2);
    env.as_contract(&client.address, || {
        env.storage().persistent().set(
            &DataKey::PendingReputationCredits(freelancer.clone()),
            &i128::MAX,
        );
    });
    let before = client.get_contract(&id);
    let balances = token::Client::new(&env, &settlement_token);
    let freelancer_balance = balances.balance(&freelancer);
    let escrow_balance = balances.balance(&client.address);
    assert_contract_error(
        client.try_release_milestone(&id, &payer, &2),
        EscrowError::PotentialOverflow,
    );
    let after = client.get_contract(&id);
    assert_eq!(after.status, before.status);
    assert_eq!(after.released_amount, before.released_amount);
    assert_eq!(after.funded_amount, before.funded_amount);
    assert_eq!(
        client.get_pending_reputation_credits(&freelancer),
        i128::MAX
    );
    assert_eq!(balances.balance(&freelancer), freelancer_balance);
    assert_eq!(balances.balance(&client.address), escrow_balance);
}
