//! Competing transactions are tested in each serialized order, matching Soroban
//! execution. Separate OS threads sharing a test Env would not model the ledger.
use escrow::{
    DataKey, Error, Escrow, EscrowClient, ADMIN_ROTATION_MIN_DELAY_LEDGERS,
    ADMIN_ROTATION_PROPOSAL_TTL_LEDGERS,
};
use soroban_sdk::testutils::{Address as _, Events, Ledger as _};
use soroban_sdk::{Address, Env, IntoVal, Symbol, TryFromVal};

fn setup() -> (Env, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|ledger| {
        ledger.sequence_number = 1;
        ledger.min_persistent_entry_ttl = ADMIN_ROTATION_PROPOSAL_TTL_LEDGERS * 3;
        ledger.max_entry_ttl = ADMIN_ROTATION_PROPOSAL_TTL_LEDGERS * 3;
    });
    let id = env.register(Escrow, ());
    let admin = Address::generate(&env);
    let proposed = Address::generate(&env);
    EscrowClient::new(&env, &id).initialize(&admin);
    (env, id, admin, proposed)
}

fn advance(env: &Env, ledgers: u32) {
    env.ledger()
        .with_mut(|ledger| ledger.sequence_number += ledgers);
}

fn assert_error<T: core::fmt::Debug, E: core::fmt::Debug>(
    env: &Env,
    result: Result<Result<T, E>, Result<soroban_sdk::Error, soroban_sdk::InvokeError>>,
    error: Error,
) {
    assert!(
        matches!(result, Err(Ok(actual)) if actual == soroban_sdk::Error::from_contract_error(error as u32)),
        "{result:?}"
    );
    assert!(env.events().all().is_empty());
}

#[test]
fn competing_proposals_only_one_wins_in_either_order() {
    for reverse in [false, true] {
        let (env, id, _, first) = setup();
        let second = Address::generate(&env);
        let (winner, loser) = if reverse {
            (second, first)
        } else {
            (first, second)
        };
        let client = EscrowClient::new(&env, &id);
        assert_eq!(client.get_admin_rotation_revision(), 0);
        assert!(client.propose_admin_checked(&winner, &0));
        let events = env.events().all();
        assert_eq!(events.len(), 2);
        let revision_event = events.get(0).unwrap();
        assert_eq!(revision_event.0, id);
        assert_eq!(
            revision_event.1,
            soroban_sdk::vec![
                &env,
                Symbol::new(&env, "admin_rotation_revision").into_val(&env)
            ]
        );
        assert_eq!(u64::try_from_val(&env, &revision_event.2).unwrap(), 1);
        assert_error(
            &env,
            client.try_propose_admin_checked(&loser, &0),
            Error::StaleNonce,
        );
        assert_eq!(client.get_pending_admin(), Some(winner));
        assert_eq!(client.get_admin_rotation_revision(), 1);
        assert!(env.events().all().is_empty());
    }
}

#[test]
fn same_address_same_ledger_aba_rejects_old_cancel_and_accept() {
    let (env, id, admin, proposed) = setup();
    let client = EscrowClient::new(&env, &id);
    client.propose_admin_checked(&proposed, &0);
    client.cancel_admin_checked(&1);
    client.propose_admin_checked(&proposed, &2);
    // Address and proposed_at_ledger are identical; only the revision identifies
    // this as a distinct proposal.
    assert_error(&env, client.try_cancel_admin_checked(&1), Error::StaleNonce);
    advance(&env, ADMIN_ROTATION_MIN_DELAY_LEDGERS);
    assert_error(&env, client.try_accept_admin_checked(&1), Error::StaleNonce);
    assert_eq!(client.get_admin(), Some(admin));
    assert_eq!(client.get_pending_admin(), Some(proposed.clone()));
    client.accept_admin_checked(&3);
    assert_eq!(client.get_admin(), Some(proposed));
    assert_eq!(client.get_admin_rotation_revision(), 4);
}

#[test]
fn accept_and_cancel_race_has_only_one_effect_in_either_order() {
    for accept_first in [false, true] {
        let (env, id, admin, proposed) = setup();
        let client = EscrowClient::new(&env, &id);
        client.propose_admin_checked(&proposed, &0);
        advance(&env, ADMIN_ROTATION_MIN_DELAY_LEDGERS);
        if accept_first {
            client.accept_admin_checked(&1);
            assert_error(&env, client.try_cancel_admin_checked(&1), Error::StaleNonce);
            assert_eq!(client.get_admin(), Some(proposed));
        } else {
            client.cancel_admin_checked(&1);
            assert_error(&env, client.try_accept_admin_checked(&1), Error::StaleNonce);
            assert_eq!(client.get_admin(), Some(admin));
        }
        assert_eq!(client.get_pending_admin(), None);
        assert_eq!(client.get_admin_rotation_revision(), 2);
    }
}

#[test]
fn retries_do_not_restart_timelock_or_repeat_events() {
    let (env, id, _, proposed) = setup();
    let client = EscrowClient::new(&env, &id);
    client.propose_admin_checked(&proposed, &0);
    advance(&env, ADMIN_ROTATION_MIN_DELAY_LEDGERS - 1);
    assert_error(
        &env,
        client.try_propose_admin_checked(&proposed, &0),
        Error::StaleNonce,
    );
    assert_error(
        &env,
        client.try_accept_admin_checked(&1),
        Error::TimelockNotElapsed,
    );
    assert_eq!(client.get_admin_rotation_revision(), 1);
    assert!(env.events().all().is_empty());
    advance(&env, 1);
    assert!(client.accept_admin_checked(&1));
    assert_error(&env, client.try_accept_admin_checked(&1), Error::StaleNonce);
    assert!(env.events().all().is_empty());
}

#[test]
fn legacy_mutations_invalidate_checked_requests_without_changing_legacy_abi() {
    let (env, id, _, proposed) = setup();
    let client = EscrowClient::new(&env, &id);
    client.propose_admin(&proposed);
    assert_eq!(client.get_admin_rotation_revision(), 1);
    client.propose_admin(&Address::generate(&env));
    assert_eq!(client.get_admin_rotation_revision(), 2);
    assert_error(&env, client.try_cancel_admin_checked(&1), Error::StaleNonce);
    client.cancel_admin();
    assert_eq!(client.get_admin_rotation_revision(), 3);
}

#[test]
fn expiry_boundary_allows_accept_but_not_recovery() {
    let (env, id, _, proposed) = setup();
    let client = EscrowClient::new(&env, &id);
    client.propose_admin_checked(&proposed, &0);
    advance(&env, ADMIN_ROTATION_PROPOSAL_TTL_LEDGERS);
    assert_error(
        &env,
        client.try_recover_admin_proposal_checked(&1),
        Error::InvalidState,
    );
    assert_eq!(client.get_admin_rotation_revision(), 1);
    assert!(client.accept_admin_checked(&1));
}

#[test]
fn expired_accept_is_atomic_and_recovery_is_not_replayable() {
    let (env, id, admin, proposed) = setup();
    let client = EscrowClient::new(&env, &id);
    client.propose_admin_checked(&proposed, &0);
    advance(&env, ADMIN_ROTATION_PROPOSAL_TTL_LEDGERS + 1);
    assert_error(
        &env,
        client.try_accept_admin_checked(&1),
        Error::AdminProposalExpired,
    );
    assert_eq!(client.get_admin_rotation_revision(), 1);
    assert_eq!(client.get_admin(), Some(admin));
    assert_eq!(client.get_pending_admin(), Some(proposed));
    assert!(env.events().all().is_empty());
    assert!(client.recover_admin_proposal_checked(&1));
    assert_error(
        &env,
        client.try_recover_admin_proposal_checked(&1),
        Error::StaleNonce,
    );
    assert_eq!(client.get_pending_admin(), None);
    assert_eq!(client.get_admin_rotation_revision(), 2);
}

#[test]
fn stale_recovery_cannot_remove_a_replacement_proposal() {
    let (env, id, _, proposed) = setup();
    let client = EscrowClient::new(&env, &id);
    client.propose_admin_checked(&proposed, &0);
    advance(&env, ADMIN_ROTATION_PROPOSAL_TTL_LEDGERS + 1);
    let replacement = Address::generate(&env);
    client.propose_admin_checked(&replacement, &1);
    assert_error(
        &env,
        client.try_recover_admin_proposal_checked(&1),
        Error::StaleNonce,
    );
    assert_eq!(client.get_pending_admin(), Some(replacement));
    assert_eq!(client.get_admin_rotation_revision(), 2);
}

#[test]
fn invalid_and_future_requests_leave_revision_and_events_unchanged() {
    let (env, id, admin, proposed) = setup();
    let client = EscrowClient::new(&env, &id);
    assert_error(
        &env,
        client.try_propose_admin_checked(&admin, &0),
        Error::CannotProposeSelf,
    );
    assert_error(
        &env,
        client.try_propose_admin_checked(&proposed, &1),
        Error::StaleNonce,
    );
    assert_error(
        &env,
        client.try_cancel_admin_checked(&0),
        Error::InvalidState,
    );
    assert_eq!(client.get_admin_rotation_revision(), 0);
    assert_eq!(client.get_pending_admin(), None);
    assert!(env.events().all().is_empty());
}

#[test]
fn exhausted_revision_fails_closed_without_partial_mutation() {
    let (env, id, _, proposed) = setup();
    let client = EscrowClient::new(&env, &id);
    client.propose_admin_checked(&proposed, &0);
    env.as_contract(&id, || {
        env.storage()
            .instance()
            .set(&DataKey::AdminRotationRevision, &u64::MAX);
    });
    assert_error(
        &env,
        client.try_cancel_admin_checked(&u64::MAX),
        Error::PotentialOverflow,
    );
    assert_eq!(client.get_admin_rotation_revision(), u64::MAX);
    assert_eq!(client.get_pending_admin(), Some(proposed));
    assert!(env.events().all().is_empty());
}

#[test]
fn checked_entrypoints_do_not_bypass_authorization() {
    let (env, id, admin, proposed) = setup();
    let client = EscrowClient::new(&env, &id);
    env.mock_auths(&[]);
    assert!(client.try_propose_admin_checked(&proposed, &0).is_err());
    assert_eq!(client.get_admin_rotation_revision(), 0);
    env.mock_all_auths();
    client.propose_admin_checked(&proposed, &0);
    advance(&env, ADMIN_ROTATION_MIN_DELAY_LEDGERS);
    env.mock_auths(&[]);
    assert!(client.try_accept_admin_checked(&1).is_err());
    assert!(client.try_cancel_admin_checked(&1).is_err());
    assert_eq!(client.get_admin_rotation_revision(), 1);
    assert_eq!(client.get_admin(), Some(admin));
    assert_eq!(client.get_pending_admin(), Some(proposed));
}

#[test]
fn pre_upgrade_pending_proposal_can_be_accepted_at_initial_revision() {
    let (env, id, _, proposed) = setup();
    // Reproduce the existing on-chain encoding with no revision key.
    env.as_contract(&id, || {
        env.storage().persistent().set(
            &DataKey::PendingAdmin,
            &escrow::PendingAdminProposal {
                proposed: proposed.clone(),
                proposed_at_ledger: env.ledger().sequence(),
            },
        );
    });
    let client = EscrowClient::new(&env, &id);
    assert_eq!(client.get_admin_rotation_revision(), 0);
    advance(&env, ADMIN_ROTATION_MIN_DELAY_LEDGERS);
    assert!(client.accept_admin_checked(&0));
    assert_eq!(client.get_admin(), Some(proposed));
    assert_eq!(client.get_admin_rotation_revision(), 1);
}
