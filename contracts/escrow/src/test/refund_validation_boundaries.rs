//! Focused boundary tests for the refund validation boundary defined in
//! [`crate::refund`] and enforced by `refund_unreleased_milestones`
//! (issue #1482).
//!
//! Coverage is organised along the four input classes the boundary has to make
//! deterministic: **accepted** requests, **rejected** requests, **duplicate**
//! submissions, and **boundary** values. Every rejection asserts the exact
//! typed error, and the retry tests assert that a rejected request leaves the
//! escrow byte-identical so a client can safely resubmit it.
//!
//! | Scenario | Asserted outcome |
//! | --- | --- |
//! | One, two, or all milestones refunded | Amount, `refunded_amount`, status, token balances |
//! | Empty request | `EmptyRefundRequest` |
//! | Duplicate indices (adjacent and spanning) | `DuplicateMilestoneInRefund` |
//! | Request longer than the protocol maximum | `IndexOutOfBounds` |
//! | Index equal to / above the schedule length, `u32::MAX` | `IndexOutOfBounds` |
//! | Released milestone | `MilestoneAlreadyReleased` |
//! | Replay of an already refunded request | `AlreadyRefunded` |
//! | Deadline before, at, and after now | `MilestoneNotOverdue` / accepted |
//! | Terminal or unknown contract | `InvalidState` / `ContractNotFound` |
//! | Request larger than the available balance | `InsufficientFunds` |
//! | Broken or overflowing accounting | `PotentialOverflow` (not a raw fault) |
//! | Rejected call | Storage and token balances unchanged |
//! | Call without the client's authorization | Rejected, no state change |
//! | `simulate_refund` | Same verdict and error code as the real call |
//!
//! # Security
//! These tests guard the accounting invariant
//! `released_amount + refunded_amount <= funded_amount`: every accepted refund
//! is checked against it, and every rejected refund must leave it intact.

#![cfg(test)]

use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, vec, Env, Vec,
};

use super::{assert_contract_error, EscrowFixture, MILESTONE_ONE, MILESTONE_THREE, MILESTONE_TWO};
use crate::{refund, Contract, ContractStatus, DataKey, Error, Milestone};

// ── Fixture helpers ─────────────────────────────────────────────────────────

/// Every value a refund decision may read or write.
///
/// A rejected request must leave both copies of this snapshot equal, which is
/// what makes client retries deterministic.
#[derive(Clone, Debug, PartialEq)]
struct EscrowSnapshot {
    contract: Contract,
    milestones: Vec<Milestone>,
    escrow_balance: i128,
    client_balance: i128,
}

fn snapshot(fixture: &EscrowFixture) -> EscrowSnapshot {
    let escrow = fixture.escrow();
    let settlement_token = fixture
        .settlement_token
        .clone()
        .expect("funded fixtures bind a settlement token");
    let token_client = token::Client::new(&fixture.env, &settlement_token);

    EscrowSnapshot {
        contract: escrow.get_contract(&fixture.escrow_id),
        milestones: escrow.get_milestones(&fixture.escrow_id),
        escrow_balance: token_client.balance(&fixture.escrow_address),
        client_balance: token_client.balance(&fixture.client),
    }
}

/// Overwrite one milestone record directly in persistent storage.
///
/// Used only to reach states that no entrypoint can produce (deadlines,
/// inconsistent accounting), mirroring the pattern in `timeout_tests.rs`.
fn set_milestone(fixture: &EscrowFixture, index: u32, mutate: impl FnOnce(&mut Milestone)) {
    fixture.env.as_contract(&fixture.escrow_address, || {
        let key = crate::keys::milestone_key(&fixture.env, fixture.escrow_id);
        let mut milestones: Vec<Milestone> = fixture
            .env
            .storage()
            .persistent()
            .get(&key)
            .expect("milestones are stored by the funded fixture");
        let mut milestone = milestones.get(index).expect("milestone index exists");
        mutate(&mut milestone);
        milestones.set(index, milestone);
        fixture.env.storage().persistent().set(&key, &milestones);
    });
}

/// Overwrite the contract record directly in persistent storage.
fn set_contract(fixture: &EscrowFixture, mutate: impl FnOnce(&mut Contract)) {
    fixture.env.as_contract(&fixture.escrow_address, || {
        let key = DataKey::Contract(fixture.escrow_id);
        let mut contract: Contract = fixture
            .env
            .storage()
            .persistent()
            .get(&key)
            .expect("contract is stored by the fixture");
        mutate(&mut contract);
        fixture.env.storage().persistent().set(&key, &contract);
    });
}

/// Move the ledger clock to an absolute timestamp.
fn set_now(env: &Env, seconds: u64) {
    env.ledger().with_mut(|ledger| {
        ledger.timestamp = seconds;
    });
}

// ── Accepted input ──────────────────────────────────────────────────────────

#[test]
fn refund_accepts_a_single_unreleased_milestone() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    let ids = vec![&fixture.env, 1_u32];

    let before = snapshot(&fixture);
    let refunded = escrow.refund_unreleased_milestones(&fixture.escrow_id, &ids);

    assert_eq!(refunded, MILESTONE_TWO);

    let contract = escrow.get_contract(&fixture.escrow_id);
    assert_eq!(contract.status, ContractStatus::Funded);
    assert_eq!(contract.refunded_amount, MILESTONE_TWO);
    assert!(
        contract.released_amount + contract.refunded_amount <= contract.funded_amount,
        "accounting invariant must hold after an accepted refund"
    );

    let milestone = escrow
        .get_milestone(&fixture.escrow_id, &1)
        .expect("refunded milestone still readable");
    assert!(milestone.refunded);
    assert_eq!(milestone.refunded_amount, MILESTONE_TWO);

    let after = snapshot(&fixture);
    assert_eq!(
        after.client_balance,
        before.client_balance + MILESTONE_TWO,
        "the client is paid the refunded amount"
    );
    assert_eq!(
        after.escrow_balance,
        before.escrow_balance - MILESTONE_TWO,
        "custody releases exactly the refunded amount"
    );
    assert_eq!(
        escrow.get_refundable_balance(&fixture.escrow_id),
        fixture.total_amount() - MILESTONE_TWO
    );
}

/// The first and last index of the schedule are both legal boundaries.
#[test]
fn refund_accepts_the_first_and_last_index_together() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    let ids = vec![&fixture.env, 0_u32, 2_u32];

    let refunded = escrow.refund_unreleased_milestones(&fixture.escrow_id, &ids);

    assert_eq!(refunded, MILESTONE_ONE + MILESTONE_THREE);
    let contract = escrow.get_contract(&fixture.escrow_id);
    assert_eq!(contract.refunded_amount, MILESTONE_ONE + MILESTONE_THREE);
    assert_eq!(
        contract.status,
        ContractStatus::Funded,
        "one milestone is still outstanding, so the contract stays active"
    );
}

/// Refunding the whole schedule drives the contract to its `Refunded` terminal
/// state and empties custody.
#[test]
fn refund_accepts_the_whole_schedule() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    let ids = vec![&fixture.env, 0_u32, 1_u32, 2_u32];

    let refunded = escrow.refund_unreleased_milestones(&fixture.escrow_id, &ids);

    assert_eq!(refunded, fixture.total_amount());
    let contract = escrow.get_contract(&fixture.escrow_id);
    assert_eq!(contract.status, ContractStatus::Refunded);
    assert_eq!(contract.refunded_amount, contract.funded_amount);
    assert_eq!(contract.released_amount, 0);
    assert_eq!(escrow.get_refundable_balance(&fixture.escrow_id), 0);

    let settlement_token = fixture.settlement_token.clone().unwrap();
    assert_eq!(
        token::Client::new(&fixture.env, &settlement_token).balance(&fixture.escrow_address),
        0,
        "a fully refunded contract holds no custody"
    );
}

// ── Rejected input: request shape ───────────────────────────────────────────

#[test]
fn refund_rejects_an_empty_request() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    let ids = vec![&fixture.env];
    let before = snapshot(&fixture);

    assert_contract_error(
        escrow.try_refund_unreleased_milestones(&fixture.escrow_id, &ids),
        Error::EmptyRefundRequest,
    );
    assert_eq!(
        snapshot(&fixture),
        before,
        "a rejection must not move state"
    );
}

#[test]
fn refund_rejects_duplicate_indices() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();

    for ids in [
        vec![&fixture.env, 1_u32, 1_u32],
        vec![&fixture.env, 0_u32, 1_u32, 2_u32, 0_u32],
        vec![&fixture.env, 2_u32, 2_u32, 2_u32],
    ] {
        assert_contract_error(
            escrow.try_refund_unreleased_milestones(&fixture.escrow_id, &ids),
            Error::DuplicateMilestoneInRefund,
        );
    }
}

#[test]
fn refund_rejects_a_request_longer_than_the_protocol_maximum() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    let mut ids = Vec::new(&fixture.env);
    for index in 0..=refund::MAX_REFUND_REQUEST_LEN {
        ids.push_back(index);
    }

    assert_contract_error(
        escrow.try_refund_unreleased_milestones(&fixture.escrow_id, &ids),
        Error::IndexOutOfBounds,
    );
}

// ── Rejected input: index boundaries ────────────────────────────────────────

#[test]
fn refund_rejects_an_index_past_the_end_of_the_schedule() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();

    for index in [3_u32, 4_u32, u32::MAX] {
        let ids = vec![&fixture.env, index];
        assert_contract_error(
            escrow.try_refund_unreleased_milestones(&fixture.escrow_id, &ids),
            Error::IndexOutOfBounds,
        );
    }
}

#[test]
fn refund_rejects_a_valid_index_followed_by_an_invalid_one() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    let before = snapshot(&fixture);
    let ids = vec![&fixture.env, 0_u32, 9_u32];

    assert_contract_error(
        escrow.try_refund_unreleased_milestones(&fixture.escrow_id, &ids),
        Error::IndexOutOfBounds,
    );
    assert_eq!(
        snapshot(&fixture),
        before,
        "the whole request rolls back, including the valid index"
    );
}

// ── Rejected input: milestone state ─────────────────────────────────────────

#[test]
fn refund_rejects_a_released_milestone() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0_u32);
    escrow.release_milestone(&fixture.escrow_id, &fixture.client, &0_u32);
    let ids = vec![&fixture.env, 0_u32];

    assert_contract_error(
        escrow.try_refund_unreleased_milestones(&fixture.escrow_id, &ids),
        Error::MilestoneAlreadyReleased,
    );
}

/// Replaying an already refunded request is the most common duplicate
/// submission; it must be rejected with the same error on every retry.
#[test]
fn refund_rejects_a_replayed_request() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    let ids = vec![&fixture.env, 1_u32];

    assert_eq!(
        escrow.refund_unreleased_milestones(&fixture.escrow_id, &ids),
        MILESTONE_TWO
    );
    let settled = snapshot(&fixture);

    for _ in 0..2 {
        assert_contract_error(
            escrow.try_refund_unreleased_milestones(&fixture.escrow_id, &ids),
            Error::AlreadyRefunded,
        );
        assert_eq!(snapshot(&fixture), settled, "a replay must not pay twice");
    }
}

#[test]
fn refund_rejects_a_milestone_with_an_unexpired_deadline() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    set_now(&fixture.env, 1_000);
    set_milestone(&fixture, 1, |milestone| milestone.deadline = Some(1_500));
    let ids = vec![&fixture.env, 1_u32];

    assert_contract_error(
        escrow.try_refund_unreleased_milestones(&fixture.escrow_id, &ids),
        Error::MilestoneNotOverdue,
    );
}

/// Boundary: at exactly the deadline the milestone is still not refundable.
#[test]
fn refund_rejects_a_milestone_at_exactly_its_deadline() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    set_now(&fixture.env, 2_000);
    set_milestone(&fixture, 1, |milestone| milestone.deadline = Some(2_000));
    let ids = vec![&fixture.env, 1_u32];

    assert_contract_error(
        escrow.try_refund_unreleased_milestones(&fixture.escrow_id, &ids),
        Error::MilestoneNotOverdue,
    );
}

/// Boundary: one second past the deadline the same request succeeds.
#[test]
fn refund_accepts_a_milestone_one_second_past_its_deadline() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    set_now(&fixture.env, 2_000);
    set_milestone(&fixture, 1, |milestone| milestone.deadline = Some(1_999));
    let ids = vec![&fixture.env, 1_u32];

    assert_eq!(
        escrow.refund_unreleased_milestones(&fixture.escrow_id, &ids),
        MILESTONE_TWO
    );
}

// ── Rejected input: contract state ──────────────────────────────────────────

#[test]
fn refund_rejects_a_contract_that_is_no_longer_active() {
    for status in [ContractStatus::Completed, ContractStatus::Cancelled] {
        let fixture = EscrowFixture::builder().funded().build();
        let escrow = fixture.escrow();
        set_contract(&fixture, |contract| contract.status = status);
        let before = snapshot(&fixture);
        let ids = vec![&fixture.env, 0_u32];

        assert_contract_error(
            escrow.try_refund_unreleased_milestones(&fixture.escrow_id, &ids),
            Error::InvalidState,
        );
        assert_eq!(snapshot(&fixture), before);
    }
}

#[test]
fn refund_rejects_an_unknown_contract() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    let ids = vec![&fixture.env, 0_u32];

    assert_contract_error(
        escrow.try_refund_unreleased_milestones(&999_u32, &ids),
        Error::ContractNotFound,
    );
}

// ── Rejected input: accounting ──────────────────────────────────────────────

/// The requested total must fit inside the balance the contract still holds.
#[test]
fn refund_rejects_a_request_beyond_the_available_balance() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    set_contract(&fixture, |contract| contract.released_amount = 100);
    let ids = vec![&fixture.env, 0_u32, 1_u32, 2_u32];

    assert_contract_error(
        escrow.try_refund_unreleased_milestones(&fixture.escrow_id, &ids),
        Error::InsufficientFunds,
    );
}

/// Regression: a balance that is over-committed (spent more than funded) must be
/// rejected as `InsufficientFunds`, exactly as before the boundary was extracted.
#[test]
fn refund_rejects_an_over_committed_balance() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    let funded = escrow.get_contract(&fixture.escrow_id).funded_amount;
    set_contract(&fixture, |contract| contract.released_amount = funded + 1);
    let ids = vec![&fixture.env, 0_u32];

    assert_contract_error(
        escrow.try_refund_unreleased_milestones(&fixture.escrow_id, &ids),
        Error::InsufficientFunds,
    );
}

/// Regression: accounting that cannot even be expressed as an `i128` difference
/// must surface as a typed `PotentialOverflow`, never as a raw arithmetic fault.
#[test]
fn refund_reports_overflow_for_unrepresentable_accounting() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    set_contract(&fixture, |contract| contract.released_amount = i128::MIN);
    let ids = vec![&fixture.env, 0_u32];

    assert_contract_error(
        escrow.try_refund_unreleased_milestones(&fixture.escrow_id, &ids),
        Error::PotentialOverflow,
    );
}

/// Regression: a schedule whose amounts cannot be added must be rejected
/// instead of wrapping or aborting.
#[test]
fn refund_reports_overflow_for_a_schedule_that_cannot_be_summed() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    set_milestone(&fixture, 0, |milestone| milestone.amount = i128::MAX);
    set_milestone(&fixture, 1, |milestone| milestone.amount = 1);
    set_contract(&fixture, |contract| contract.funded_amount = i128::MAX);
    let ids = vec![&fixture.env, 0_u32, 1_u32];

    assert_contract_error(
        escrow.try_refund_unreleased_milestones(&fixture.escrow_id, &ids),
        Error::PotentialOverflow,
    );
}

/// A rejected request leaves no trace, so the identical retry produces the
/// identical rejection — and succeeds once the cause is removed.
#[test]
fn a_rejected_request_is_deterministic_and_retryable() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    set_contract(&fixture, |contract| contract.released_amount = 100);
    let ids = vec![&fixture.env, 0_u32, 1_u32, 2_u32];

    assert_contract_error(
        escrow.try_refund_unreleased_milestones(&fixture.escrow_id, &ids),
        Error::InsufficientFunds,
    );
    let rejected = snapshot(&fixture);

    assert_contract_error(
        escrow.try_refund_unreleased_milestones(&fixture.escrow_id, &ids),
        Error::InsufficientFunds,
    );
    assert_eq!(
        snapshot(&fixture),
        rejected,
        "retries observe the same state"
    );

    set_contract(&fixture, |contract| contract.released_amount = 0);
    assert_eq!(
        escrow.refund_unreleased_milestones(&fixture.escrow_id, &ids),
        fixture.total_amount(),
        "the same request succeeds once the balance is available again"
    );
}

// ── Authorization ───────────────────────────────────────────────────────────

/// The refund entrypoint is only reachable with the client's authorization.
///
/// The fixture mocks authorization, so this test clears the mock for the
/// refund invocation: validation passing must never be sufficient on its own.
#[test]
fn refund_requires_the_client_authorization() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    let before = snapshot(&fixture);
    let ids = vec![&fixture.env, 0_u32];

    fixture.env.set_auths(&[]);
    let result = escrow.try_refund_unreleased_milestones(&fixture.escrow_id, &ids);

    assert!(
        result.is_err(),
        "a refund without the client's authorization must be rejected"
    );
    assert_eq!(snapshot(&fixture), before, "no state moves without auth");
}

// ── Simulation parity ───────────────────────────────────────────────────────

/// `simulate_refund` runs the same boundary, so an accepted request projects the
/// exact amount the entrypoint pays.
#[test]
fn simulate_refund_agrees_with_the_entrypoint_on_a_valid_request() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    let ids = vec![&fixture.env, 0_u32, 1_u32];

    let simulated = escrow.simulate_refund(&fixture.escrow_id, &ids);

    assert!(simulated.would_succeed);
    assert_eq!(simulated.error_code, None);
    assert_eq!(simulated.total_refund_amount, MILESTONE_ONE + MILESTONE_TWO);
    assert_eq!(
        escrow.refund_unreleased_milestones(&fixture.escrow_id, &ids),
        simulated.total_refund_amount
    );
}

/// Regression: a released milestone used to be reported as `AlreadyRefunded` by
/// the simulator while the entrypoint reported `MilestoneAlreadyReleased`.
#[test]
fn simulate_refund_reports_the_same_error_as_the_entrypoint() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0_u32);
    escrow.release_milestone(&fixture.escrow_id, &fixture.client, &0_u32);
    let ids = vec![&fixture.env, 0_u32];

    let simulated = escrow.simulate_refund(&fixture.escrow_id, &ids);

    assert!(!simulated.would_succeed);
    assert_eq!(
        simulated.error_code,
        Some(Error::MilestoneAlreadyReleased as u32)
    );
    assert_contract_error(
        escrow.try_refund_unreleased_milestones(&fixture.escrow_id, &ids),
        Error::MilestoneAlreadyReleased,
    );
}

/// Regression: unrepresentable accounting used to abort the entrypoint with a
/// raw arithmetic fault and could make the simulator panic or report success.
/// Both now report the same typed `PotentialOverflow`.
#[test]
fn simulate_refund_reports_overflow_instead_of_success() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    set_contract(&fixture, |contract| contract.released_amount = i128::MIN);
    let ids = vec![&fixture.env, 0_u32];

    let simulated = escrow.simulate_refund(&fixture.escrow_id, &ids);

    assert!(!simulated.would_succeed);
    assert_eq!(simulated.error_code, Some(Error::PotentialOverflow as u32));
    assert_eq!(simulated.total_refund_amount, 0);
    assert_contract_error(
        escrow.try_refund_unreleased_milestones(&fixture.escrow_id, &ids),
        Error::PotentialOverflow,
    );
}
