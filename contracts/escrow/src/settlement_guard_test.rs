//! Settlement guard tests: double-spend prevention, milestone isolation, and
//! cross-contract independence.
//!
//! These tests exercise the invariant that each milestone can only be released
//! once (double-spend rejection), that releasing one milestone does not affect
//! others, and that completely separate contracts are fully isolated from each
//! other.
//!
//! All tests bind a real Stellar Asset Contract (SAC) and mint tokens to the
//! client, matching the production money-flow path.

#![cfg(test)]

use crate::types::ReleaseAuthorization;
use crate::{Escrow, EscrowClient};
use soroban_sdk::{testutils::Address as _, token::StellarAssetClient, Address, Env, Vec};

/// Build a fully initialized escrow with a bound SAC, create a contract with
/// the given milestone amounts, mint the required tokens to the client, and
/// deposit the full amount.
///
/// Returns `(client, admin, client_addr, freelancer_addr, contract_id)`.
fn setup_and_create_escrow<'a>(
    env: &'a Env,
    milestone_amounts: &[i128],
) -> (EscrowClient<'a>, Address, Address, Address, u32) {
    let contract_id = env.register(Escrow, ());
    let client = EscrowClient::new(env, &contract_id);

    let admin = Address::generate(env);
    client.initialize(&admin);

    // Bind a real SAC so deposit_funds and release_milestone can transfer tokens.
    let sac = env.register_stellar_asset_contract(admin.clone());
    client.bind_settlement_token(&admin, &sac);

    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);

    let mut milestones = Vec::new(env);
    let mut total_amount = 0i128;
    for &amount in milestone_amounts {
        milestones.push_back(amount);
        total_amount += amount;
    }

    let c_id = client.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &milestones,
        &ReleaseAuthorization::ClientOnly,
    );

    // Mint tokens to the client and deposit the full escrow amount.
    StellarAssetClient::new(env, &sac).mint(&client_addr, &total_amount);
    client.deposit_funds(&c_id, &client_addr, &total_amount);

    (client, admin, client_addr, freelancer_addr, c_id)
}

/// Releasing a milestone for the first time must succeed and record the correct
/// released_amount.
#[test]
fn test_milestone_settlement_succeeds_first_time() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, client_addr, _freelancer_addr, c_id) =
        setup_and_create_escrow(&env, &[1_000_0000000, 2_000_0000000]);

    client.approve_milestone_release(&c_id, &client_addr, &0u32);
    let res = client.release_milestone(&c_id, &client_addr, &0u32);
    assert!(res);

    let summary = client.get_contract(&c_id);
    assert_eq!(summary.released_amount, 1_000_0000000);
}

/// A second release of the same milestone must be rejected (double-spend guard).
/// The released_amount must remain at the value set by the first release.
#[test]
fn test_milestone_settlement_rejects_second_settlement_double_spend() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, client_addr, _freelancer_addr, c_id) =
        setup_and_create_escrow(&env, &[1_000_0000000, 2_000_0000000]);

    // First release succeeds.
    client.approve_milestone_release(&c_id, &client_addr, &0u32);
    assert!(client.release_milestone(&c_id, &client_addr, &0u32));

    // Second release of the same milestone must fail.
    let res = client.try_release_milestone(&c_id, &client_addr, &0u32);
    assert!(res.is_err(), "double-release must be rejected");

    // released_amount must not have changed.
    let summary = client.get_contract(&c_id);
    assert_eq!(
        summary.released_amount, 1_000_0000000,
        "released_amount must not change after double-spend rejection"
    );
}

/// Releasing milestone 0 must not affect milestone 1; both can be released
/// independently in sequence.
#[test]
fn test_milestone_settlement_unrelated_milestones_unaffected() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, client_addr, _freelancer_addr, c_id) =
        setup_and_create_escrow(&env, &[1_000_0000000, 2_000_0000000]);

    // Release milestone 0.
    client.approve_milestone_release(&c_id, &client_addr, &0u32);
    assert!(client.release_milestone(&c_id, &client_addr, &0u32));

    // Milestone 1 is unaffected and can still be released.
    client.approve_milestone_release(&c_id, &client_addr, &1u32);
    assert!(client.release_milestone(&c_id, &client_addr, &1u32));

    let summary = client.get_contract(&c_id);
    assert_eq!(summary.released_amount, 3_000_0000000);
}

/// Two entirely separate contracts are fully isolated: releasing a milestone
/// on contract 1 must have no effect on contract 2.
#[test]
fn test_milestone_settlement_different_contracts_isolated() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, client_addr1, _freelancer_addr1, c_id1) =
        setup_and_create_escrow(&env, &[5_000_0000000]);

    // Create a second contract under the same escrow instance.
    let client_addr2 = Address::generate(&env);
    let freelancer_addr2 = Address::generate(&env);
    let mut milestones2 = Vec::new(&env);
    milestones2.push_back(5_000_0000000_i128);

    let sac = client.get_settlement_token().expect("SAC must be bound");
    StellarAssetClient::new(&env, &sac).mint(&client_addr2, &5_000_0000000_i128);

    let c_id2 = client.create_contract(
        &client_addr2,
        &freelancer_addr2,
        &None,
        &milestones2,
        &ReleaseAuthorization::ClientOnly,
    );
    client.deposit_funds(&c_id2, &client_addr2, &5_000_0000000_i128);

    // Release milestone on contract 1.
    client.approve_milestone_release(&c_id1, &client_addr1, &0u32);
    assert!(client.release_milestone(&c_id1, &client_addr1, &0u32));

    // Contract 2 is unaffected: its milestone can be released independently.
    client.approve_milestone_release(&c_id2, &client_addr2, &0u32);
    assert!(client.release_milestone(&c_id2, &client_addr2, &0u32));

    assert_eq!(client.get_contract(&c_id1).released_amount, 5_000_0000000);
    assert_eq!(client.get_contract(&c_id2).released_amount, 5_000_0000000);
}
