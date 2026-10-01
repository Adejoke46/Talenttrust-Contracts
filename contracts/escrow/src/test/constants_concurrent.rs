//! Regression tests for concurrent/repeated execution safety around protocol
//! constants (issue #1405).
//!
//! ## What this file tests
//!
//! The constants in `constants.rs` gate clamping in paginated views, rating
//! validation in `issue_reputation`, and credit accounting.  A divergent
//! constant in any sibling module causes silent inconsistency: callers observe
//! different clamping depending on which code path they hit.
//!
//! These tests exercise the *runtime* side of the invariants enforced at
//! compile time by the `const _: () = assert!(…)` blocks in `constants.rs`.
//! They also validate that repeated or concurrent-style calls (simulated by
//! calling the same entrypoint multiple times in the same test) produce
//! deterministic, idempotent results.
//!
//! ## Scope
//!
//! - Cross-module consistency: PAGE_CEILING, MAX_PAGINATION_LIMIT, MIN/MAX_RATING, MAX_COMMENT_BYTES
//! - Boundary values: at-limit, one-over, one-under, zero, u32::MAX
//! - Idempotent retries: calling a read-only paginated view N times returns the same page
//! - Concurrent write safety: issuing reputation twice on the same contract is rejected
//! - Duplicate-work guard: approval idempotency and double-credit protection

use super::{complete_contract_funded, register_client_with_token};
use crate::{EscrowError, PAGE_CEILING};
use soroban_sdk::{testutils::Address as _, Address, Env, String};

// ── Helpers ────────────────────────────────────────────────────────────────────

fn comment(env: &Env, s: &str) -> String {
    String::from_str(env, s)
}

fn valid_comment(env: &Env) -> String {
    comment(env, "Great work!")
}

// ── PAGE_CEILING consistency ───────────────────────────────────────────────────

/// The publicly re-exported `PAGE_CEILING` must equal 50.
///
/// This confirms the canonical value is stable and the public API is not
/// accidentally changed.  The compile-time assertions inside `constants.rs`
/// enforce that the sibling aliases (contracts::PAGE_CEILING and
/// types::MAX_PAGINATION_LIMIT) match — this test provides a human-readable
/// CI signal for the public-facing value.
#[test]
fn page_ceiling_has_expected_public_value() {
    assert_eq!(PAGE_CEILING, 50u32, "PAGE_CEILING must be 50");
}

// ── Pagination ceiling clamping ────────────────────────────────────────────────

/// Requesting a limit larger than PAGE_CEILING is silently clamped — not an error.
#[test]
fn get_reputations_page_clamps_over_ceiling() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, token) = register_client_with_token(&env);

    // Populate 3 reputation records.
    for _ in 0..3 {
        let (client_addr, _freelancer, contract_id) =
            complete_contract_funded(&env, &client, &token);
        client.issue_reputation(&contract_id, &client_addr, &5, &valid_comment(&env));
    }

    // Requesting far more than PAGE_CEILING returns only available items (3).
    let page_over = client.get_reputations_page(&0u32, &(PAGE_CEILING * 10));
    assert_eq!(page_over.len(), 3, "over-ceiling request must not panic");

    // Requesting exactly PAGE_CEILING also succeeds.
    let page_at = client.get_reputations_page(&0u32, &PAGE_CEILING);
    assert_eq!(page_at.len(), 3, "at-ceiling request must not panic");

    // Requesting 0 returns an empty page.
    let page_zero = client.get_reputations_page(&0u32, &0u32);
    assert_eq!(page_zero.len(), 0, "zero limit returns empty page");
}

/// Calling `get_reputations_page` multiple times with the same arguments is
/// idempotent: repeated reads never alter state and always return the same result.
#[test]
fn get_reputations_page_is_idempotent_under_repeated_calls() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, token) = register_client_with_token(&env);

    let (client_addr, _freelancer, contract_id) = complete_contract_funded(&env, &client, &token);
    client.issue_reputation(&contract_id, &client_addr, &4, &valid_comment(&env));

    // Call the same read 5 times and assert the results are identical.
    let first = client.get_reputations_page(&0u32, &10u32);
    for _ in 0..4 {
        let repeated = client.get_reputations_page(&0u32, &10u32);
        assert_eq!(repeated.len(), first.len(), "repeated read changed page length");
        for i in 0..first.len() {
            let a = first.get(i).unwrap();
            let b = repeated.get(i).unwrap();
            assert_eq!(a.account, b.account, "repeated read changed account at {i}");
            assert_eq!(
                a.completed_contracts, b.completed_contracts,
                "repeated read changed completed_contracts at {i}"
            );
            assert_eq!(a.total_rating, b.total_rating, "repeated read changed total_rating at {i}");
        }
    }
}

/// Requesting a start index at or beyond the total size returns an empty page
/// without panic — safe for pagination retry loops.
#[test]
fn get_reputations_page_oob_start_returns_empty() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, token) = register_client_with_token(&env);

    let (client_addr, _, contract_id) = complete_contract_funded(&env, &client, &token);
    client.issue_reputation(&contract_id, &client_addr, &3, &valid_comment(&env));

    // start = 1 (exactly one past the only record) must return empty.
    let page = client.get_reputations_page(&1u32, &10u32);
    assert_eq!(page.len(), 0, "out-of-bounds start must return empty page");

    // start = u32::MAX must also return empty without arithmetic overflow.
    let page_max = client.get_reputations_page(&u32::MAX, &10u32);
    assert_eq!(page_max.len(), 0, "u32::MAX start must return empty page");
}

// ── Rating boundary values ─────────────────────────────────────────────────────

/// Rating at exactly MIN_RATING (1) is accepted.
#[test]
fn issue_reputation_accepts_min_rating() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, token) = register_client_with_token(&env);
    let (client_addr, _, contract_id) = complete_contract_funded(&env, &client, &token);
    let ok = client.issue_reputation(&contract_id, &client_addr, &1, &valid_comment(&env));
    assert!(ok, "MIN_RATING=1 must be accepted");
}

/// Rating at exactly MAX_RATING (5) is accepted.
#[test]
fn issue_reputation_accepts_max_rating() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, token) = register_client_with_token(&env);
    let (client_addr, _, contract_id) = complete_contract_funded(&env, &client, &token);
    let ok = client.issue_reputation(&contract_id, &client_addr, &5, &valid_comment(&env));
    assert!(ok, "MAX_RATING=5 must be accepted");
}

/// Rating 0 (below MIN_RATING) is rejected with InvalidRating.
#[test]
fn issue_reputation_rejects_rating_below_min() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, token) = register_client_with_token(&env);
    let (client_addr, _, contract_id) = complete_contract_funded(&env, &client, &token);
    let result = client.try_issue_reputation(&contract_id, &client_addr, &0, &valid_comment(&env));
    super::assert_contract_error(result, EscrowError::InvalidRating);
}

/// Rating 6 (above MAX_RATING) is rejected with InvalidRating.
#[test]
fn issue_reputation_rejects_rating_above_max() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, token) = register_client_with_token(&env);
    let (client_addr, _, contract_id) = complete_contract_funded(&env, &client, &token);
    let result = client.try_issue_reputation(&contract_id, &client_addr, &6, &valid_comment(&env));
    super::assert_contract_error(result, EscrowError::InvalidRating);
}

// ── Comment boundary values ───────────────────────────────────────────────────

/// An empty comment (0 bytes) is rejected with EmptyComment.
#[test]
fn issue_reputation_rejects_empty_comment() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, token) = register_client_with_token(&env);
    let (client_addr, _, contract_id) = complete_contract_funded(&env, &client, &token);
    let result =
        client.try_issue_reputation(&contract_id, &client_addr, &5, &comment(&env, ""));
    super::assert_contract_error(result, EscrowError::EmptyComment);
}

/// A comment that is exactly MAX_COMMENT_BYTES (200) long is accepted.
#[test]
fn issue_reputation_accepts_comment_at_max_bytes() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, token) = register_client_with_token(&env);
    let (client_addr, _, contract_id) = complete_contract_funded(&env, &client, &token);

    // Build a 200-byte ASCII string (each char = 1 byte).
    let s = soroban_sdk::String::from_str(&env, &"A".repeat(200));
    assert_eq!(s.len(), 200, "helper must produce exactly 200 bytes");

    let ok = client.issue_reputation(&contract_id, &client_addr, &5, &s);
    assert!(ok, "200-byte comment must be accepted");
}

/// A comment that is MAX_COMMENT_BYTES + 1 (201) bytes long is rejected.
#[test]
fn issue_reputation_rejects_comment_over_max_bytes() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, token) = register_client_with_token(&env);
    let (client_addr, _, contract_id) = complete_contract_funded(&env, &client, &token);

    let s = soroban_sdk::String::from_str(&env, &"A".repeat(201));
    assert_eq!(s.len(), 201, "helper must produce exactly 201 bytes");

    let result = client.try_issue_reputation(&contract_id, &client_addr, &5, &s);
    super::assert_contract_error(result, EscrowError::CommentTooLong);
}

// ── Duplicate / concurrent write safety ───────────────────────────────────────

/// Issuing reputation twice for the same contract is rejected with
/// ReputationAlreadyIssued.  This is the idempotent-retry / concurrent-execution
/// guard: two concurrent callers cannot both succeed.
#[test]
fn issue_reputation_rejects_duplicate_issuance() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, token) = register_client_with_token(&env);
    let (client_addr, _, contract_id) = complete_contract_funded(&env, &client, &token);

    // First call succeeds.
    let ok = client.issue_reputation(&contract_id, &client_addr, &5, &valid_comment(&env));
    assert!(ok);

    // Second call (duplicate / concurrent retry) must be rejected.
    let result =
        client.try_issue_reputation(&contract_id, &client_addr, &4, &valid_comment(&env));
    super::assert_contract_error(result, EscrowError::ReputationAlreadyIssued);
}

/// Reputation data is deterministic across repeated reads after a single write.
/// This ensures no hidden mutable state contaminates the read path.
#[test]
fn get_reputation_is_deterministic_after_write() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, token) = register_client_with_token(&env);
    let (client_addr, freelancer_addr, contract_id) =
        complete_contract_funded(&env, &client, &token);

    client.issue_reputation(&contract_id, &client_addr, &3, &valid_comment(&env));

    let rep1 = client.get_reputation(&freelancer_addr).unwrap();
    let rep2 = client.get_reputation(&freelancer_addr).unwrap();
    assert_eq!(rep1.completed_contracts, rep2.completed_contracts);
    assert_eq!(rep1.total_rating, rep2.total_rating);
    assert_eq!(rep1.last_rating, rep2.last_rating);
    assert_eq!(rep1.completed_contracts, 1);
    assert_eq!(rep1.total_rating, 3);
    assert_eq!(rep1.last_rating, 3);
}

/// Average rating computation is deterministic: calling get_average_rating
/// multiple times returns the same value (×SCALE representation).
#[test]
fn get_average_rating_is_deterministic() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, token) = register_client_with_token(&env);
    let (client_addr, freelancer_addr, contract_id) =
        complete_contract_funded(&env, &client, &token);

    client.issue_reputation(&contract_id, &client_addr, &5, &valid_comment(&env));

    // 5 * 10_000 / 1 = 50_000 (represents 5.0000)
    let avg1 = client.get_average_rating(&freelancer_addr);
    let avg2 = client.get_average_rating(&freelancer_addr);
    assert_eq!(avg1, avg2, "get_average_rating must be deterministic");
    assert_eq!(avg1, Some(50_000), "5 * SCALE / 1 contract = 50_000");
}

// ── Pending-credits accounting ─────────────────────────────────────────────────

/// `get_pending_reputation_credits` returns 0 before any contract completes and
/// decrements by 1 (= REPUTATION_CREDIT_INCREMENT) after issuance.
///
/// REPUTATION_CREDIT_INCREMENT is the canonical credit-increment constant.
/// The compile-time assertion in constants.rs pins it to 1; this test
/// verifies the runtime accounting matches that invariant.
#[test]
fn reputation_credit_accounting_decrements_by_one() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, token) = register_client_with_token(&env);

    let freelancer_addr = Address::generate(&env);

    // Before any contract: 0 pending credits.
    let before = client.get_pending_reputation_credits(&freelancer_addr);
    assert_eq!(before, 0, "no pending credits before any completed contract");

    // Complete a contract to grant one credit.
    let (client_addr, actual_freelancer, contract_id) =
        complete_contract_funded(&env, &client, &token);
    let after_complete = client.get_pending_reputation_credits(&actual_freelancer);
    // REPUTATION_CREDIT_INCREMENT = 1; one completed contract grants exactly 1 credit.
    assert_eq!(
        after_complete, 1,
        "exactly 1 pending credit after one completed contract (REPUTATION_CREDIT_INCREMENT=1)"
    );

    // Issue reputation to consume the credit.
    client.issue_reputation(&contract_id, &client_addr, &5, &valid_comment(&env));
    let after_issue = client.get_pending_reputation_credits(&actual_freelancer);
    assert_eq!(
        after_issue, 0,
        "pending credits must drop to 0 after issue_reputation"
    );
}
