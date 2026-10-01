use escrow::{DataKey, Error, Escrow, EscrowClient, Milestone, ReleaseAuthorization};
use soroban_sdk::{testutils::Address as _, vec, Address, Env, Symbol, Vec};

#[test]
fn orphan_milestone_slot_is_preserved_and_creation_can_retry() {
    let env = Env::default();
    env.mock_all_auths();
    let escrow_address = env.register(Escrow, ());
    let client = EscrowClient::new(&env, &escrow_address);
    let participant = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let milestone_key = (DataKey::Contract(1), Symbol::new(&env, "milestones"));

    env.as_contract(&escrow_address, || {
        env.storage()
            .persistent()
            .set(&milestone_key, &Vec::<Milestone>::new(&env));
    });

    let result = client.try_create_contract(
        &participant,
        &freelancer,
        &None,
        &vec![&env, 100_i128],
        &ReleaseAuthorization::ClientOnly,
    );
    let expected_error: soroban_sdk::Error = Error::ContractIdCollision.into();
    match result {
        Err(Ok(error)) => assert_eq!(error, expected_error),
        other => panic!("expected ContractIdCollision, got {:?}", other),
    }

    let (contract_exists, next_id) = env.as_contract(&escrow_address, || {
        let storage = env.storage().persistent();
        (
            storage.has(&DataKey::Contract(1)),
            storage.get::<_, u32>(&DataKey::NextContractId),
        )
    });
    assert!(!contract_exists);
    assert_eq!(next_id, None);

    let record: Vec<Milestone> = env.as_contract(&escrow_address, || {
        env.storage()
            .persistent()
            .get(&milestone_key)
            .expect("existing milestone storage must remain present")
    });
    assert!(record.is_empty());

    env.as_contract(&escrow_address, || {
        env.storage().persistent().remove(&milestone_key);
    });
    assert_eq!(
        client.create_contract(
            &participant,
            &freelancer,
            &None,
            &vec![&env, 100_i128],
            &ReleaseAuthorization::ClientOnly,
        ),
        1
    );
    assert_eq!(client.get_next_contract_id(), 2);
}
