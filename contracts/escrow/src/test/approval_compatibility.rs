use soroban_sdk::{testutils::Address as _, testutils::Events as _, Address, Symbol, TryIntoVal};

use super::EscrowFixture;
use crate::{Error, ReleaseAuthorization};

#[test]
fn revoke_milestone_approval_preserves_other_approval_and_event_contract() {
    let fixture = EscrowFixture::builder()
        .release_authorization(ReleaseAuthorization::MultiSig)
        .funded()
        .build();
    let escrow = fixture.escrow();
    let id = fixture.escrow_id;

    assert!(escrow.approve_milestone_release(&id, &fixture.client, &0));
    assert!(escrow.approve_milestone_release(&id, &fixture.freelancer, &0));
    assert!(escrow.revoke_milestone_approval(&id, &fixture.client, &0));

    let revoked = Symbol::new(&fixture.env, "revoked");
    let mut event_count = 0;
    let mut event_payload = None;
    for (_, topics, payload) in fixture.env.events().all().iter() {
        if topics.len() > 0 {
            let topic: Symbol = topics.get(0).unwrap().try_into_val(&fixture.env).unwrap();
            if topic == revoked {
                event_count += 1;
                event_payload = Some(payload);
            }
        }
    }
    assert_eq!(event_count, 1, "events: {:?}", fixture.env.events().all());
    let (event_id, index, caller): (u32, u32, Address) =
        event_payload.unwrap().try_into_val(&fixture.env).unwrap();
    assert_eq!((event_id, index, caller), (id, 0, fixture.client.clone()));

    let record = escrow.get_milestone_approvals(&id, &0).unwrap();
    assert!(!record.client_approved);
    assert!(record.freelancer_approved);
    assert!(!record.arbiter_approved);

    // Revoking again is rejected without erasing the other participant's flag.
    super::assert_contract_error(
        escrow.try_revoke_milestone_approval(&id, &fixture.client, &0),
        Error::InsufficientApprovals,
    );
    let record = escrow.get_milestone_approvals(&id, &0).unwrap();
    assert!(!record.client_approved);
    assert!(record.freelancer_approved);
}

#[test]
fn revoke_milestone_approval_rejects_invalid_index_and_nonparticipant() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    let id = fixture.escrow_id;
    let stranger = Address::generate(&fixture.env);

    super::assert_contract_error(
        escrow.try_revoke_milestone_approval(&id, &fixture.client, &u32::MAX),
        Error::IndexOutOfBounds,
    );
    assert!(fixture.env.events().all().is_empty());
    super::assert_contract_error(
        escrow.try_revoke_milestone_approval(&id, &stranger, &0),
        Error::UnauthorizedRole,
    );
}

#[test]
fn revoking_the_last_approval_removes_the_record() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    let id = fixture.escrow_id;

    assert!(escrow.approve_milestone_release(&id, &fixture.client, &0));
    assert!(escrow.revoke_milestone_approval(&id, &fixture.client, &0));
    assert!(escrow.get_milestone_approvals(&id, &0).is_none());

    super::assert_contract_error(
        escrow.try_revoke_milestone_approval(&id, &fixture.client, &0),
        Error::InsufficientApprovals,
    );
}

#[test]
fn revoke_milestone_approval_rejects_after_release() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    let id = fixture.escrow_id;

    assert!(escrow.approve_milestone_release(&id, &fixture.client, &0));
    assert!(escrow.release_milestone(&id, &fixture.client, &0));
    super::assert_contract_error(
        escrow.try_revoke_milestone_approval(&id, &fixture.client, &0),
        Error::MilestoneAlreadyReleased,
    );
}

#[test]
fn approve_milestone_release_requires_the_named_callers_authentication() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    let id = fixture.escrow_id;

    fixture.env.mock_auths(&[]);
    assert!(escrow
        .try_approve_milestone_release(&id, &fixture.client, &0)
        .is_err());
    assert!(escrow.get_milestone_approvals(&id, &0).is_none());
}
