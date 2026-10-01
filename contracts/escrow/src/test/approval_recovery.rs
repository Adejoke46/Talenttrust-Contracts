//! Deterministic failure-recovery tests for milestone release approvals.
//!
//! Covers the recovery paths added in `approvals.rs`:
//!
//! * **Withdrawal** — `revoke_milestone_approval` clears a caller's own flag so
//!   a premature approval is never a seven-day dead end.
//! * **Diagnosis** — `get_milestone_release_readiness` turns an opaque
//!   `InsufficientApprovals` rejection into a named, actionable state.
//! * **Voiding** — dispute open/rollback/resolution and cancellation clear
//!   outstanding approvals, so a pre-dispute consent cannot be resurrected.
//!
//! Every suite uses `EscrowFixture::funded()`, which binds a Stellar Asset
//! Contract before depositing. That binding is required: `deposit_funds` panics
//! with `SettlementTokenNotConfigured` without it.

use super::{assert_contract_error, EscrowFixture, MILESTONE_ONE};
use crate::ttl::PENDING_APPROVAL_TTL_LEDGERS;
use crate::{ContractStatus, DisputeResolution, Error, ReleaseAuthorization};
use soroban_sdk::testutils::{Address as _, Events as _, Ledger as _};
use soroban_sdk::{symbol_short, Address, Env, Symbol, TryIntoVal, Val, Vec};

/// Ledger entry-TTL ceiling for TTL-eviction suites.
///
/// A contract instance's TTL is fixed at registration, so this must be applied
/// to the builder *before* `build()`. It comfortably exceeds the 7-day
/// approval window so the instance outlives the approval under test, letting
/// the approval be the thing that actually expires.
const LEDGER_TTL_BOUND: u32 = PENDING_APPROVAL_TTL_LEDGERS * 6;

/// A funded `ClientOnly` contract with the default three milestones.
fn client_only() -> EscrowFixture {
    EscrowFixture::builder().funded().build()
}

/// A funded contract with an assigned arbiter in the given release mode.
///
/// Role resolution reads `contract.arbiter`, so `MultiSig` and `ArbiterOnly`
/// scenarios need an explicit arbiter; `EscrowFixtureBuilder::with_generated_arbiter`
/// supplies one.
fn arbitrated(mode: ReleaseAuthorization) -> EscrowFixture {
    EscrowFixture::builder()
        .with_generated_arbiter()
        .release_authorization(mode)
        .funded()
        .build()
}

/// A funded `MultiSig` contract with an assigned arbiter.
fn multisig() -> EscrowFixture {
    arbitrated(ReleaseAuthorization::MultiSig)
}

/// A funded `MultiSig` contract whose instance outlives the approval window.
fn multisig_long_lived() -> EscrowFixture {
    EscrowFixture::builder()
        .with_ledger_ttl_bounds(LEDGER_TTL_BOUND)
        .with_generated_arbiter()
        .release_authorization(ReleaseAuthorization::MultiSig)
        .funded()
        .build()
}

/// Advances the test ledger by `by` ledgers.
///
/// Requires a fixture built with [`EscrowFixtureBuilder::with_ledger_ttl_bounds`]
/// so the contract instance outlives the approval window. Reading an archived
/// instance is an uncatchable `Storage(InternalError)` panic in the test host,
/// so the ceiling has to be raised up front rather than repaired afterwards.
fn advance(env: &Env, by: u32) {
    env.ledger().with_mut(|li| {
        li.sequence_number = li.sequence_number.saturating_add(by);
    });
}

/// A funded `ClientOnly` contract whose instance outlives the approval window.
fn client_only_long_lived() -> EscrowFixture {
    EscrowFixture::builder()
        .with_ledger_ttl_bounds(LEDGER_TTL_BOUND)
        .funded()
        .build()
}

/// Reads the live approval record for a milestone, if any.
fn approvals(fixture: &EscrowFixture, index: u32) -> Option<crate::MilestoneApprovals> {
    fixture
        .escrow()
        .get_milestone_approvals(&fixture.escrow_id, &index)
}

/// Collects the payloads of events published under `topic` by this escrow.
fn events_for(fixture: &EscrowFixture, topic: Symbol) -> Vec<Val> {
    let mut out = Vec::new(&fixture.env);
    for (addr, topics, data) in fixture.env.events().all().iter() {
        if &addr != &fixture.escrow_address || topics.len() < 2 {
            continue;
        }
        let t0: Symbol = topics.get(0).unwrap().try_into_val(&fixture.env).unwrap();
        let cid: u32 = topics.get(1).unwrap().try_into_val(&fixture.env).unwrap();
        if t0 == topic && cid == fixture.escrow_id {
            out.push_back(data.clone());
        }
    }
    out
}

// ── Withdrawal: success ───────────────────────────────────────────────────

/// A client that approves by mistake can withdraw its own approval, and the
/// milestone returns to exactly the "never approved" state (invariant I4).
#[test]
fn revoke_clears_callers_own_approval() {
    let fixture = client_only();
    let escrow = fixture.escrow();

    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0));
    assert!(approvals(&fixture, 0).unwrap().client_approved);

    assert!(escrow.revoke_milestone_approval(&fixture.escrow_id, &fixture.client, &0));

    // I4: the record is removed entirely, so the state is indistinguishable
    // from a contract on which no approval was ever given.
    assert_eq!(approvals(&fixture, 0), None);
}

/// A `MultiSig` participant cannot sabotage a set it is not part of: revoking
/// clears only the caller's own flag and leaves the other party's approval
/// live (invariant I2).
#[test]
fn revoke_preserves_other_parties_approvals() {
    let fixture = multisig();
    let escrow = fixture.escrow();

    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0));
    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.freelancer, &0));

    assert!(escrow.revoke_milestone_approval(&fixture.escrow_id, &fixture.client, &0));

    let remaining = approvals(&fixture, 0).expect("freelancer approval keeps the record alive");
    assert!(
        !remaining.client_approved,
        "caller's own flag must be cleared"
    );
    assert!(
        remaining.freelancer_approved,
        "the other party's approval must survive"
    );

    let readiness = escrow.get_milestone_release_readiness(&fixture.escrow_id, &0);
    assert!(
        !readiness.release_authorized,
        "one of two is not sufficient"
    );
    assert_eq!(readiness.approvals_present, 1);
    assert_eq!(readiness.approvals_missing, 1);
    assert!(readiness.has_record);
}

/// The arbiter owns the only flag that matters in `ArbiterOnly` mode, and can
/// withdraw exactly that flag.
#[test]
fn arbiter_can_withdraw_its_own_approval() {
    let fixture = arbitrated(ReleaseAuthorization::ArbiterOnly);
    let escrow = fixture.escrow();
    let arbiter = fixture.arbiter.clone().expect("arbiter assigned");

    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &arbiter, &0));
    assert!(approvals(&fixture, 0).unwrap().arbiter_approved);

    assert!(escrow.revoke_milestone_approval(&fixture.escrow_id, &arbiter, &0));
    assert_eq!(approvals(&fixture, 0), None);
}

/// Withdrawal publishes one event whose payload distinguishes "flag cleared"
/// from "record removed", so a poller needs no follow-up read.
#[test]
fn revoke_publishes_a_distinguishing_event() {
    let fixture = multisig();
    let escrow = fixture.escrow();

    // Both parties approve, so the first revoke keeps the record alive.
    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0));
    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.freelancer, &0));
    assert!(escrow.revoke_milestone_approval(&fixture.escrow_id, &fixture.client, &0));

    let emitted = events_for(&fixture, symbol_short!("mlstn_rvk"));
    assert_eq!(emitted.len(), 1, "expected exactly one mlstn_rvk event");

    // (milestone_index, caller, record_removed, other_approvals_remain, ts)
    let payload: (u32, Address, bool, bool, u64) =
        emitted.get(0).unwrap().try_into_val(&fixture.env).unwrap();
    assert_eq!(payload.0, 0);
    assert_eq!(payload.1, fixture.client);
    assert!(
        !payload.2,
        "record survives while the freelancer still approves"
    );
    assert!(payload.3, "other approvals remain");
    assert_eq!(payload.4, fixture.env.ledger().timestamp());
}

/// A rejected revoke publishes nothing, so indexers never see a phantom
/// revocation for an approval that is still live.
#[test]
fn rejected_revoke_publishes_no_event() {
    let fixture = client_only();
    let escrow = fixture.escrow();
    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0));

    let stranger = Address::generate(&fixture.env);
    assert_contract_error(
        escrow.try_revoke_milestone_approval(&fixture.escrow_id, &stranger, &0),
        Error::UnauthorizedRole,
    );

    assert_eq!(
        events_for(&fixture, symbol_short!("mlstn_rvk")).len(),
        0,
        "a rejected revoke must publish nothing"
    );
    assert!(approvals(&fixture, 0).unwrap().client_approved);
}

// ── Withdrawal: rejection ─────────────────────────────────────────────────

/// Revoking with no record at all is a typed, inert rejection — not a
/// synthesized empty record. Covers "never approved" and "already fully
/// revoked", which share one repair (a fresh approval).
#[test]
fn revoke_without_prior_approval_is_rejected() {
    let fixture = client_only();

    assert_contract_error(
        fixture
            .escrow()
            .try_revoke_milestone_approval(&fixture.escrow_id, &fixture.client, &0),
        Error::InsufficientApprovals,
    );
    assert_eq!(
        approvals(&fixture, 0),
        None,
        "a rejected revoke writes nothing"
    );
}

/// A participant whose own flag is not set cannot clear another party's flag.
#[test]
fn revoke_rejects_participant_without_own_approval() {
    let fixture = multisig();
    let escrow = fixture.escrow();
    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0));

    // The freelancer never approved, so it has nothing to withdraw.
    assert_contract_error(
        escrow.try_revoke_milestone_approval(&fixture.escrow_id, &fixture.freelancer, &0),
        Error::InsufficientApprovals,
    );

    assert!(
        approvals(&fixture, 0).unwrap().client_approved,
        "the client's approval must be untouched"
    );
}

/// Boundary: the highest valid index succeeds and every index at or beyond the
/// milestone count is rejected.
#[test]
fn revoke_enforces_milestone_index_bounds() {
    let fixture = client_only();
    let escrow = fixture.escrow();
    let count = escrow.get_milestones(&fixture.escrow_id).len();

    // Highest valid index.
    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &(count - 1)));
    assert!(escrow.revoke_milestone_approval(&fixture.escrow_id, &fixture.client, &(count - 1)));

    // First invalid index, and one well beyond it.
    for bad_index in [count, count + 1, u32::MAX] {
        assert_contract_error(
            escrow.try_revoke_milestone_approval(&fixture.escrow_id, &fixture.client, &bad_index),
            Error::IndexOutOfBounds,
        );
    }
}

/// An unknown contract is reported distinctly from a missing approval.
#[test]
fn revoke_rejects_unknown_contract() {
    let fixture = client_only();
    assert_contract_error(
        fixture
            .escrow()
            .try_revoke_milestone_approval(&9_999, &fixture.client, &0),
        Error::ContractNotFound,
    );
}

/// A released milestone is terminal. The release consumed the record and the
/// milestone flag, so the terminal state cannot be walked backwards.
#[test]
fn revoke_rejects_after_release() {
    let fixture = client_only();
    let escrow = fixture.escrow();

    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0));
    assert!(escrow.release_milestone(&fixture.escrow_id, &fixture.client, &0));
    assert_eq!(approvals(&fixture, 0), None, "release consumed the record");

    // The released-milestone check precedes record lookup, so a stale caller
    // gets the terminal reason rather than a misleading "no approval" error.
    assert_contract_error(
        escrow.try_revoke_milestone_approval(&fixture.escrow_id, &fixture.client, &0),
        Error::MilestoneAlreadyReleased,
    );
    assert_contract_error(
        escrow.try_approve_milestone_release(&fixture.escrow_id, &fixture.client, &0),
        Error::MilestoneAlreadyReleased,
    );
}

/// Pause rails cover the new mutating entrypoint: recovery cannot be used while
/// an incident is being handled.
#[test]
fn revoke_rejected_while_contract_is_paused() {
    let fixture = client_only();
    let escrow = fixture.escrow();
    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0));

    // `consume_admin_nonce` requires `current + 1`; the first use is nonce 1.
    assert!(escrow.pause(&1u64));

    assert_contract_error(
        escrow.try_revoke_milestone_approval(&fixture.escrow_id, &fixture.client, &0),
        Error::ContractPaused,
    );
    assert!(
        approvals(&fixture, 0).unwrap().client_approved,
        "a paused revoke must not mutate state"
    );
}

/// A finalized contract blocks the new mutating entrypoint, matching every
/// other contract-specific mutation. The finalized check precedes the
/// released-milestone check, so a released index still reports the terminal
/// contract state.
#[test]
fn revoke_rejected_after_finalization() {
    let fixture = EscrowFixture::builder().completed().build();
    let escrow = fixture.escrow();
    assert!(escrow.finalize_contract(&fixture.escrow_id, &fixture.client));

    assert_contract_error(
        escrow.try_revoke_milestone_approval(&fixture.escrow_id, &fixture.client, &0),
        Error::AlreadyFinalized,
    );
}

/// Repeating a withdrawal is an inert no-op rejection rather than a silent
/// success, so the caller can tell "already withdrawn" from "just withdrawn".
#[test]
fn repeated_revoke_leaves_state_unchanged() {
    let fixture = multisig();
    let escrow = fixture.escrow();
    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0));
    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.freelancer, &0));

    // The client withdraws, leaving the freelancer's approval alive.
    assert!(escrow.revoke_milestone_approval(&fixture.escrow_id, &fixture.client, &0));
    let after_first = approvals(&fixture, 0).unwrap();

    // Retrying must be inert: same rejection, same stored state.
    for _ in 0..3 {
        assert_contract_error(
            escrow.try_revoke_milestone_approval(&fixture.escrow_id, &fixture.client, &0),
            Error::InsufficientApprovals,
        );
    }
    assert_eq!(approvals(&fixture, 0).unwrap(), after_first);
}

// ── Recovery: the user-visible loop ───────────────────────────────────────

/// The headline recovery scenario. Withdrawing a premature approval must
/// actually block the release it would have authorized.
#[test]
fn revoke_blocks_the_release_it_would_have_authorized() {
    let fixture = client_only();
    let escrow = fixture.escrow();

    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0));
    assert!(escrow.revoke_milestone_approval(&fixture.escrow_id, &fixture.client, &0));

    assert_contract_error(
        escrow.try_release_milestone(&fixture.escrow_id, &fixture.client, &0),
        Error::InsufficientApprovals,
    );
    assert_eq!(
        escrow.get_contract(&fixture.escrow_id).released_amount,
        0,
        "no funds may move after withdrawal"
    );
}

/// Full recovery: withdraw, reconsider, approve again, release. This is the
/// deterministic path a client follows after a mistaken approval.
#[test]
fn revoke_then_reapprove_restores_the_release() {
    let fixture = client_only();
    let escrow = fixture.escrow();

    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0));
    assert!(escrow.revoke_milestone_approval(&fixture.escrow_id, &fixture.client, &0));
    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0));
    assert!(escrow.release_milestone(&fixture.escrow_id, &fixture.client, &0));

    // Only milestone 0 moves; the contract is still mid-way.
    assert_eq!(
        escrow.get_contract(&fixture.escrow_id).released_amount,
        MILESTONE_ONE
    );
    assert_eq!(
        escrow.get_contract(&fixture.escrow_id).status,
        ContractStatus::Funded
    );
}

/// A partial `MultiSig` set can be rebalanced to either side, and only a
/// sufficient set releases.
#[test]
fn multisig_recovery_reaches_both_sufficient_states() {
    let fixture = multisig();
    let escrow = fixture.escrow();

    // Client-only: insufficient.
    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0));
    assert!(
        !escrow
            .get_milestone_release_readiness(&fixture.escrow_id, &0)
            .release_authorized
    );

    // Both parties: sufficient.
    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.freelancer, &0));
    assert!(
        escrow
            .get_milestone_release_readiness(&fixture.escrow_id, &0)
            .release_authorized
    );

    // The freelancer withdraws after a misunderstanding.
    assert!(escrow.revoke_milestone_approval(&fixture.escrow_id, &fixture.freelancer, &0));
    assert!(
        !escrow
            .get_milestone_release_readiness(&fixture.escrow_id, &0)
            .release_authorized,
        "withdrawing one of two must make the set insufficient again"
    );

    // Re-approving restores sufficiency and the release goes through.
    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.freelancer, &0));
    assert!(
        escrow
            .get_milestone_release_readiness(&fixture.escrow_id, &0)
            .release_authorized
    );
    assert!(escrow.release_milestone(&fixture.escrow_id, &fixture.client, &0));
    assert_eq!(
        escrow.get_contract(&fixture.escrow_id).released_amount,
        MILESTONE_ONE
    );
}

// ── Invariant I3: recovery must not extend deadlines ──────────────────────

/// Withdrawing one flag must not slide the surviving approval's expiry.
/// Otherwise repeated withdraw/re-approve cycles would silently turn a
/// seven-day consent window into an open-ended one.
#[test]
fn revoke_does_not_extend_the_surviving_approval_ttl() {
    let fixture = multisig_long_lived();
    let escrow = fixture.escrow();

    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0));
    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.freelancer, &0));

    // Age the record past the bump threshold, then withdraw one flag.
    advance(&fixture.env, PENDING_APPROVAL_TTL_LEDGERS - 1);
    assert!(escrow.revoke_milestone_approval(&fixture.escrow_id, &fixture.client, &0));

    // Two more ledgers and the surviving approval must be gone. Had the revoke
    // bumped the TTL, the record would still be live here.
    advance(&fixture.env, 2);
    assert_eq!(
        approvals(&fixture, 0),
        None,
        "the freelancer's original deadline must be honoured, not extended"
    );
}

// ── Per-milestone isolation ───────────────────────────────────────────────

/// Approvals are scoped to `(contract_id, milestone_index)`; withdrawing one
/// milestone must not disturb another.
#[test]
fn revoke_is_scoped_to_a_single_milestone() {
    let fixture = multisig();
    let escrow = fixture.escrow();

    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0));
    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &1));

    assert!(escrow.revoke_milestone_approval(&fixture.escrow_id, &fixture.client, &0));

    assert_eq!(approvals(&fixture, 0), None);
    assert!(
        approvals(&fixture, 1)
            .expect("milestone 1 approval survives")
            .client_approved
    );
}

// ── Regression: stale approvals must not resurrect (I6) ───────────────────

/// The load-bearing regression test.
///
/// Before this change, `rollback_dispute` restored the pre-dispute `Funded`
/// status while leaving any pre-dispute approval live in temporary storage. A
/// client approval recorded *before* a dispute could therefore be spent after
/// the rollback, with no party having re-consented. Approvals are now voided,
/// so the parties must re-approve.
#[test]
fn dispute_rollback_does_not_resurrect_pre_dispute_approvals() {
    let fixture = multisig();
    let escrow = fixture.escrow();

    // The client approves on the merits.
    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0));
    assert!(
        escrow
            .get_milestone_release_readiness(&fixture.escrow_id, &0)
            .has_record
    );

    // A dispute is raised over that very milestone.
    assert!(escrow.raise_dispute(&fixture.escrow_id, &fixture.client));

    // The admin rolls the dispute back, restoring `Funded`.
    assert!(escrow.rollback_dispute(&fixture.escrow_id));
    assert_eq!(
        escrow.get_contract(&fixture.escrow_id).status,
        ContractStatus::Funded
    );

    // The stale consent is gone: the release is refused and funds cannot move.
    assert_eq!(
        approvals(&fixture, 0),
        None,
        "a pre-dispute approval must not survive the rollback"
    );
    assert_contract_error(
        escrow.try_release_milestone(&fixture.escrow_id, &fixture.client, &0),
        Error::InsufficientApprovals,
    );
    assert_eq!(escrow.get_contract(&fixture.escrow_id).released_amount, 0);

    // Recovery is deterministic: fresh consent releases normally.
    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0));
    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.freelancer, &0));
    assert!(escrow.release_milestone(&fixture.escrow_id, &fixture.client, &0));
    assert_eq!(
        escrow.get_contract(&fixture.escrow_id).released_amount,
        MILESTONE_ONE
    );
}

/// Opening a dispute voids approvals immediately, so a client polling approval
/// state during a dispute is not told it still holds live consent.
#[test]
fn raising_a_dispute_voids_outstanding_approvals() {
    let fixture = multisig();
    let escrow = fixture.escrow();

    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0));
    assert!(escrow.raise_dispute(&fixture.escrow_id, &fixture.freelancer));

    assert_eq!(approvals(&fixture, 0), None);
    assert!(
        !escrow
            .get_milestone_release_readiness(&fixture.escrow_id, &0)
            .release_authorized
    );
}

/// Resolving a dispute leaves a terminal contract, so no approval may linger.
#[test]
fn resolving_a_dispute_clears_outstanding_approvals() {
    let fixture = multisig();
    let escrow = fixture.escrow();

    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0));
    assert!(escrow.raise_dispute(&fixture.escrow_id, &fixture.client));
    assert!(escrow.resolve_dispute(
        &fixture.escrow_id,
        &fixture.arbiter.clone().unwrap(),
        &DisputeResolution::FullRefund
    ));

    assert_eq!(approvals(&fixture, 0), None);
    assert_eq!(
        escrow.get_contract(&fixture.escrow_id).status,
        ContractStatus::Refunded
    );
}

/// A cancelled contract is never releasable, so its approvals are pure stale
/// state and must not survive to mislead a polling client.
#[test]
fn cancelling_clears_outstanding_approvals() {
    let fixture = multisig();
    let escrow = fixture.escrow();

    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0));
    assert!(escrow.cancel_contract(&fixture.escrow_id, &fixture.client));

    assert_eq!(approvals(&fixture, 0), None);
    assert_eq!(
        escrow.get_contract(&fixture.escrow_id).status,
        ContractStatus::Cancelled
    );
}

// ── Diagnosability: the readiness view ───────────────────────────────────

/// The readiness view names what is missing, which is the difference between
/// an actionable denial and an opaque one.
#[test]
fn readiness_reports_the_outstanding_multisig_approval() {
    let fixture = multisig();
    let escrow = fixture.escrow();

    // Nothing approved yet.
    let readiness = escrow.get_milestone_release_readiness(&fixture.escrow_id, &0);
    assert!(!readiness.release_authorized);
    assert!(!readiness.has_record);
    assert_eq!(readiness.approvals_required, 2);
    assert_eq!(readiness.approvals_present, 0);
    assert_eq!(readiness.approvals_missing, 2);

    // One of two: the client holds live consent, the freelancer is outstanding.
    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0));
    let readiness = escrow.get_milestone_release_readiness(&fixture.escrow_id, &0);
    assert!(readiness.has_record);
    assert_eq!(readiness.approvals_required, 2);
    assert_eq!(readiness.approvals_present, 1);
    assert_eq!(readiness.approvals_missing, 1);

    // Both: releasable now.
    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.freelancer, &0));
    let readiness = escrow.get_milestone_release_readiness(&fixture.escrow_id, &0);
    assert!(readiness.release_authorized);
    assert_eq!(readiness.approvals_missing, 0);
}

/// A record evicted by TTL is reported as "no live record" so the caller knows
/// to re-approve. It is deliberately not reported as a distinct expired state:
/// the host does not retain that history, and both cases need the same repair.
#[test]
fn readiness_reports_an_expired_record_as_absent() {
    let fixture = client_only_long_lived();
    let escrow = fixture.escrow();
    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0));

    advance(&fixture.env, PENDING_APPROVAL_TTL_LEDGERS + 1);

    let readiness = escrow.get_milestone_release_readiness(&fixture.escrow_id, &0);
    assert!(!readiness.has_record);
    assert!(!readiness.release_authorized);
    assert_eq!(readiness.approvals_missing, 1);
    // The milestone itself is still open and needs fresh consent.
    assert!(!readiness.released);
    assert!(!readiness.refunded);
}

/// A settled milestone is reported as terminal so a UI does not prompt the user
/// to approve a release that can never happen.
#[test]
fn readiness_reports_terminal_milestones() {
    let fixture = client_only_long_lived();
    let escrow = fixture.escrow();

    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0));
    assert!(escrow.release_milestone(&fixture.escrow_id, &fixture.client, &0));

    let readiness = escrow.get_milestone_release_readiness(&fixture.escrow_id, &0);
    assert!(readiness.released);
    assert!(!readiness.refunded);
    assert!(
        !readiness.release_authorized,
        "a released milestone is never releasable again"
    );
}

/// The view is a cheap probe: unknown contracts and out-of-range indices report
/// "nothing here" rather than panicking, so a poller needs no error handling.
#[test]
fn readiness_is_empty_safe_for_unknown_input() {
    let fixture = client_only();
    let escrow = fixture.escrow();
    let count = escrow.get_milestones(&fixture.escrow_id).len();

    for (contract_id, index) in [
        (9_999_u32, 0_u32),
        (fixture.escrow_id, count),
        (fixture.escrow_id, u32::MAX),
    ] {
        let readiness = escrow.get_milestone_release_readiness(&contract_id, &index);
        assert!(!readiness.release_authorized);
        assert!(!readiness.has_record);
        assert!(!readiness.released);
        assert!(!readiness.refunded);
    }
}

/// Polling the view must not extend the approval window it is observing.
#[test]
fn readiness_polling_does_not_extend_the_approval_ttl() {
    let fixture = client_only_long_lived();
    let escrow = fixture.escrow();
    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0));

    // Age the record past its bump threshold, then poll repeatedly.
    advance(&fixture.env, PENDING_APPROVAL_TTL_LEDGERS - 1);
    for _ in 0..5 {
        escrow.get_milestone_release_readiness(&fixture.escrow_id, &0);
    }

    advance(&fixture.env, 2);
    assert_eq!(
        approvals(&fixture, 0),
        None,
        "an observing read must not renew a consent the user already gave up"
    );
}

/// `ArbiterOnly` requires one arbiter approval; a client approval never counts.
#[test]
fn readiness_counts_arbiter_only_against_the_arbiter_flag() {
    let fixture = arbitrated(ReleaseAuthorization::ArbiterOnly);
    let escrow = fixture.escrow();
    let arbiter = fixture.arbiter.clone().expect("arbiter assigned");

    // The client cannot approve in this mode, so the milestone stays blocked.
    assert_contract_error(
        escrow.try_approve_milestone_release(&fixture.escrow_id, &fixture.client, &0),
        Error::UnauthorizedRole,
    );
    assert_eq!(
        escrow
            .get_milestone_release_readiness(&fixture.escrow_id, &0)
            .approvals_present,
        0
    );

    assert!(escrow.approve_milestone_release(&fixture.escrow_id, &arbiter, &0));
    let readiness = escrow.get_milestone_release_readiness(&fixture.escrow_id, &0);
    assert_eq!(readiness.approvals_required, 1);
    assert_eq!(readiness.approvals_present, 1);
    assert!(readiness.release_authorized);
}

/// The withdrawal path is bounded by the milestone count, so every milestone's
/// approval can be withdrawn exactly once with no cross-milestone interference.
#[test]
fn revoke_covers_every_milestone_without_interference() {
    let fixture = client_only();
    let escrow = fixture.escrow();
    let count = escrow.get_milestones(&fixture.escrow_id).len();

    for index in 0..count {
        assert!(escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &index));
    }
    for index in 0..count {
        assert!(escrow.revoke_milestone_approval(&fixture.escrow_id, &fixture.client, &index));
        assert_eq!(approvals(&fixture, index), None);
    }
}
