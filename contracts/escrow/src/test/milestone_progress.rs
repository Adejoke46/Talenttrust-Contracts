use super::{register_client, EscrowFixture};
use crate::MilestoneProgress;
use crate::EscrowError;

// ── unknown contract ─────────────────────────────────────────────────────────

/// Unknown contract id returns MilestoneProgress { completed: 0, total: 0 } rather than panicking.
#[test]
fn get_milestone_progress_returns_zero_for_unknown_contract() {
    let env = soroban_sdk::Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let progress = client.get_milestone_progress(&999);
    assert_eq!(
        progress,
        MilestoneProgress {
            completed: 0,
            total: 0
        }
    );
}

/// Zero id (never allocated) also returns MilestoneProgress { completed: 0, total: 0 }.
#[test]
fn get_milestone_progress_returns_zero_for_zero_id() {
    let env = soroban_sdk::Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let progress = client.get_milestone_progress(&0);
    assert_eq!(
        progress,
        MilestoneProgress {
            completed: 0,
            total: 0
        }
    );
}

// ── none complete ────────────────────────────────────────────────────────────

/// Freshly created, unreleased contract: none of its milestones are complete.
#[test]
fn get_milestone_progress_none_complete() {
    let fixture = EscrowFixture::builder().build();
    let escrow = fixture.escrow();

    let progress = escrow.get_milestone_progress(&fixture.escrow_id);
    assert_eq!(
        progress,
        MilestoneProgress {
            completed: 0,
            total: 3
        }
    );
}

// ── some complete ────────────────────────────────────────────────────────────

/// One of several milestones released: progress reflects the partial state.
#[test]
fn get_milestone_progress_some_complete() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0);
    assert!(escrow.release_milestone(&fixture.escrow_id, &fixture.client, &0));

    let progress = escrow.get_milestone_progress(&fixture.escrow_id);
    assert_eq!(
        progress,
        MilestoneProgress {
            completed: 1,
            total: 3
        }
    );
}

// ── all complete ─────────────────────────────────────────────────────────────

/// Fully completed contract: completed count equals total.
#[test]
fn get_milestone_progress_all_complete() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    for milestone_index in 0..3u32 {
        escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &milestone_index);
        assert!(escrow.release_milestone(&fixture.escrow_id, &fixture.client, &milestone_index));
    }

    let progress = escrow.get_milestone_progress(&fixture.escrow_id);
    assert_eq!(
        progress,
        MilestoneProgress {
            completed: 3,
            total: 3
        }
    );
}

// ── purity ───────────────────────────────────────────────────────────────────

/// Repeated reads don't change the result.
#[test]
fn get_milestone_progress_observations_are_pure() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0);
    assert!(escrow.release_milestone(&fixture.escrow_id, &fixture.client, &0));

    let initial = escrow.get_milestone_progress(&fixture.escrow_id);
    for _ in 0..8 {
        assert_eq!(escrow.get_milestone_progress(&fixture.escrow_id), initial);
    }
}

// ── concurrency / idempotency ────────────────────────────────────────────────

/// Releasing the same milestone twice must not double-count progress.
/// The second release attempt must fail and leave `completed` unchanged.
#[test]
fn release_milestone_is_idempotent_under_repeat() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0);

    assert!(escrow.release_milestone(&fixture.escrow_id, &fixture.client, &0));
    let after_first = escrow.get_milestone_progress(&fixture.escrow_id);
    assert_eq!(
        after_first,
        MilestoneProgress {
            completed: 1,
            total: 3
        }
    );

    // Duplicate release must be rejected and must not mutate state.
    let duplicate = escrow.try_release_milestone(&fixture.escrow_id, &fixture.client, &0);
    assert!(duplicate.is_err());
    assert_eq!(
        escrow.get_milestone_progress(&fixture.escrow_id),
        after_first
    );
}

/// Approving the same milestone twice must not enable a second release.
#[test]
fn approve_milestone_release_is_idempotent_under_repeat() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();

    escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0);
    let duplicate = escrow.try_approve_milestone_release(&fixture.escrow_id, &fixture.client, &0);
    assert!(duplicate.is_err());

    // Exactly one release still succeeds.
    assert!(escrow.release_milestone(&fixture.escrow_id, &fixture.client, &0));
    let second = escrow.try_release_milestone(&fixture.escrow_id, &fixture.client, &0);
    assert!(second.is_err());
    assert_eq!(
        escrow.get_milestone_progress(&fixture.escrow_id),
        MilestoneProgress {
            completed: 1,
            total: 3
        }
    );
}

/// Releasing without approval must be rejected and leave progress untouched.
#[test]
fn release_without_approval_is_rejected() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();

    let result = escrow.try_release_milestone(&fixture.escrow_id, &fixture.client, &0);
    assert!(result.is_err());
    assert_eq!(
        escrow.get_milestone_progress(&fixture.escrow_id),
        MilestoneProgress {
            completed: 0,
            total: 3
        }
    );
}

/// Out-of-range milestone index must be rejected without corrupting progress.
#[test]
fn release_out_of_range_milestone_is_rejected() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();
    escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &0);

    let result = escrow.try_release_milestone(&fixture.escrow_id, &fixture.client, &99);
    assert!(result.is_err());
    assert_eq!(
        escrow.get_milestone_progress(&fixture.escrow_id),
        MilestoneProgress {
            completed: 0,
            total: 3
        }
    );
}

/// Interleaved releases across distinct milestones must each count exactly once,
/// regardless of ordering, and never exceed `total`.
#[test]
fn interleaved_releases_count_each_milestone_once() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();

    for milestone_index in 0..3u32 {
        escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &milestone_index);
    }

    // Interleave releases and duplicate attempts.
    assert!(escrow.release_milestone(&fixture.escrow_id, &fixture.client, &1));
    assert!(escrow.try_release_milestone(&fixture.escrow_id, &fixture.client, &1).is_err());
    assert!(escrow.release_milestone(&fixture.escrow_id, &fixture.client, &0));
    assert!(escrow.release_milestone(&fixture.escrow_id, &fixture.client, &2));
    assert!(escrow.try_release_milestone(&fixture.escrow_id, &fixture.client, &2).is_err());

    let progress = escrow.get_milestone_progress(&fixture.escrow_id);
    assert_eq!(
        progress,
        MilestoneProgress {
            completed: 3,
            total: 3
        }
    );
    assert!(progress.completed <= progress.total);
}

/// Progress must never exceed total even after adversarial duplicate attempts.
#[test]
fn progress_completed_never_exceeds_total() {
    let fixture = EscrowFixture::builder().funded().build();
    let escrow = fixture.escrow();

    for milestone_index in 0..3u32 {
        escrow.approve_milestone_release(&fixture.escrow_id, &fixture.client, &milestone_index);
        assert!(escrow.release_milestone(&fixture.escrow_id, &fixture.client, &milestone_index));
        // Duplicate attempts must not inflate the counter.
        let _ = escrow.try_release_milestone(&fixture.escrow_id, &fixture.client, &milestone_index);
    }

    let progress = escrow.get_milestone_progress(&fixture.escrow_id);
    assert!(progress.completed <= progress.total);
    assert_eq!(progress.completed, 3);
    assert_eq!(progress.total, 3);
}

/// Unknown contract id must not be affected by concurrent-style repeated reads.
#[test]
fn unknown_contract_progress_is_stable_under_repeat() {
    let env = soroban_sdk::Env::default();
    env.mock_all_auths();
    let client = register_client(&env);

    let initial = client.get_milestone_progress(&999);
    for _ in 0..8 {
        assert_eq!(client.get_milestone_progress(&999), initial);
    }
    assert_eq!(
        initial,
        MilestoneProgress {
            completed: 0,
            total: 0
        }
    );
}
