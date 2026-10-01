//! Focused validation-boundary tests for `contracts/escrow/src/lib.rs` (issue #1452).
//!
//! Each section targets one entrypoint and covers:
//!   - accepted input at the exact valid boundary (max / min)
//!   - rejected input one step outside the boundary
//!   - zero / negative / empty inputs where applicable
//!   - duplicate submissions where applicable
//!   - regression: a prior valid call continues to succeed
//!
//! Sections in this file:
//!   1. `create_contract` – milestone count, amounts, total cap, participant identity
//!   2. `deposit_funds`   – zero/negative/over-cap
//!   3. `release_milestone` – index OOB, already-released, wrong-state, wrong-role
//!   4. `approve_milestone_release` – index OOB, duplicate
//!   5. `issue_reputation` – rating range, comment length, wrong-state, duplicate
//!   6. `cancel_contract` – wrong-role, wrong-state, double-cancel
//!   7. `initialize`      – double-init
//!   8. `get_bounds`      – returns correct compile-time constants
//!   9. `refund_unreleased_milestones` – empty, duplicate, OOB indices
//!  10. `set_protocol_fee_bps` – 0, max, over-max
//!  11. `set_max_milestones`  – at-min, at-max, below-min, above-max

#![cfg(test)]

use soroban_sdk::{testutils::Address as _, token::StellarAssetClient, vec, Address, Env, String};

use crate::{
    validation_boundaries::{
        MAX_COMMENT_BYTES, MAX_FEE_BPS, MAX_MAX_MILESTONES, MAX_MILESTONES,
        MAX_SINGLE_AMOUNT_STROOPS, MAX_TOTAL_ESCROW_STROOPS, MIN_COMMENT_BYTES, MIN_FEE_BPS,
        MIN_MAX_MILESTONES, MIN_RATING, MAX_RATING,
    },
    Escrow, EscrowClient, EscrowError, ReleaseAuthorization,
};

use super::assert_contract_error;

// ═══════════════════════════════════════════════════════════════════════════
// Shared fixture helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Minimal fixture: initialized contract, no settlement token.
fn setup_no_token(env: &Env) -> (EscrowClient<'_>, Address) {
    env.mock_all_auths_allowing_non_root_auth();
    let addr = env.register(Escrow, ());
    let client = EscrowClient::new(env, &addr);
    let admin = Address::generate(env);
    client.initialize(&admin);
    (client, admin)
}

/// Full fixture: initialized + SAC token bound + unlimited client token balance.
fn setup_with_token(env: &Env) -> (EscrowClient<'_>, Address, Address, Address, Address) {
    env.mock_all_auths_allowing_non_root_auth();
    let addr = env.register(Escrow, ());
    let escrow = EscrowClient::new(env, &addr);
    let admin = Address::generate(env);
    escrow.initialize(&admin);

    let token = env.register_stellar_asset_contract(admin.clone());
    escrow.bind_settlement_token(&admin, &token);

    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);

    // Mint a large balance so deposit tests are not gated by token balance.
    StellarAssetClient::new(env, &token).mint(&client_addr, &(MAX_TOTAL_ESCROW_STROOPS * 100));

    (escrow, admin, client_addr, freelancer_addr, token)
}

/// Create a funded single-milestone contract. Returns contract_id.
fn funded_one_milestone(
    env: &Env,
    escrow: &EscrowClient<'_>,
    client_addr: &Address,
    freelancer_addr: &Address,
    token: &Address,
    amount: i128,
) -> u32 {
    let id = escrow.create_contract(
        client_addr,
        freelancer_addr,
        &None,
        &vec![env, amount],
        &ReleaseAuthorization::ClientOnly,
    );
    StellarAssetClient::new(env, token).mint(client_addr, &amount);
    escrow.deposit_funds(&id, client_addr, &amount);
    id
}

/// Drive a funded contract to Completed by approving and releasing milestone 0.
fn complete_one_milestone(
    escrow: &EscrowClient<'_>,
    contract_id: u32,
    client_addr: &Address,
) {
    escrow.approve_milestone_release(&contract_id, client_addr, &0u32);
    escrow.release_milestone(&contract_id, client_addr, &0u32);
}

// Build a Soroban String from an ASCII `char` repeated `n` times.
// Uses a fixed stack buffer (max 256 bytes) — sufficient for all test values.
fn repeat_char(env: &Env, ch: char, n: u32) -> String {
    let byte = ch as u8; // ASCII range only
    let len = (n as usize).min(256);
    let mut raw = [0u8; 256];
    for i in 0..len {
        raw[i] = byte;
    }
    String::from_bytes(env, &raw[..len])
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. create_contract – milestone count
// ═══════════════════════════════════════════════════════════════════════════

/// Acceptance: exactly MAX_MILESTONES milestones (current runtime limit = 10).
/// Uses 1 whole-token per milestone (10_000_000 stroops at 7 decimals) to pass
/// the token-scale check that fires when a settlement token is bound.
#[test]
fn create_contract_accepts_exactly_max_milestones() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, _) = setup_with_token(&env);

    // 1 whole-token unit at 7-decimal SAC scale (10^7 = 10_000_000 stroops).
    let one_token: i128 = 1_0000000;
    let mut milestones = vec![&env, one_token];
    for _ in 1..MAX_MILESTONES {
        milestones.push_back(one_token);
    }
    assert_eq!(milestones.len(), MAX_MILESTONES);

    let id = escrow.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &milestones,
        &ReleaseAuthorization::ClientOnly,
    );
    assert!(id < u32::MAX, "contract id should be allocated");
}

/// Rejection: MAX_MILESTONES + 1 raises `TooManyMilestones`.
#[test]
fn create_contract_rejects_one_above_max_milestones() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, _) = setup_with_token(&env);

    let mut milestones = vec![&env, 1_i128];
    for _ in 1..=MAX_MILESTONES {
        milestones.push_back(1_i128);
    }
    assert_eq!(milestones.len(), MAX_MILESTONES + 1);

    assert_contract_error(
        escrow.try_create_contract(
            &client_addr,
            &freelancer_addr,
            &None,
            &milestones,
            &ReleaseAuthorization::ClientOnly,
        ),
        EscrowError::TooManyMilestones,
    );
}

/// Rejection: 0 milestones raises `EmptyMilestones`.
#[test]
fn create_contract_rejects_empty_milestones() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, _) = setup_with_token(&env);

    assert_contract_error(
        escrow.try_create_contract(
            &client_addr,
            &freelancer_addr,
            &None,
            &vec![&env],
            &ReleaseAuthorization::ClientOnly,
        ),
        EscrowError::EmptyMilestones,
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. create_contract – per-milestone amount
// ═══════════════════════════════════════════════════════════════════════════

/// Acceptance: milestone amount of exactly 1 stroop (MIN_MILESTONE_AMOUNT_STROOPS).
///
/// The token-scale check is only enforced when a settlement token has been
/// bound (it reads the stored `TokenScale`).  Before binding a token the
/// scale check is skipped, so this test verifies that the raw lower-bound
/// (`>0`) is accepted when no scale constraint is active.
#[test]
fn create_contract_accepts_min_milestone_amount() {
    let env = Env::default();
    // Use a fixture without a bound token so the FractionalTokenAmount check
    // does not fire for amounts below the 7-decimal threshold.
    let (escrow, _, client_addr, freelancer_addr, _) = setup_with_token(&env);
    // Unbinding is not possible, so we test the raw floor via a fresh fixture
    // that has never had a token bound.
    let env2 = Env::default();
    env2.mock_all_auths_allowing_non_root_auth();
    let addr2 = env2.register(Escrow, ());
    let escrow2 = EscrowClient::new(&env2, &addr2);
    let admin2 = Address::generate(&env2);
    escrow2.initialize(&admin2);
    let c2 = Address::generate(&env2);
    let f2 = Address::generate(&env2);

    // No token bound → scale check skipped → 1 stroop passes the >0 guard.
    escrow2.create_contract(
        &c2,
        &f2,
        &None,
        &vec![&env2, 1_i128],
        &ReleaseAuthorization::ClientOnly,
    );

    // With a bound 7-decimal token the practical minimum is 10_000_000 stroops.
    escrow.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &vec![&env, 1_0000000_i128],
        &ReleaseAuthorization::ClientOnly,
    );
}

/// Rejection: milestone amount of 0 raises `InvalidMilestoneAmount`.
#[test]
fn create_contract_rejects_zero_milestone_amount() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, _) = setup_with_token(&env);

    assert_contract_error(
        escrow.try_create_contract(
            &client_addr,
            &freelancer_addr,
            &None,
            &vec![&env, 0_i128],
            &ReleaseAuthorization::ClientOnly,
        ),
        EscrowError::InvalidMilestoneAmount,
    );
}

/// Rejection: negative milestone amount raises `AmountMustBePositive` or `InvalidMilestoneAmount`.
#[test]
fn create_contract_rejects_negative_milestone_amount() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, _) = setup_with_token(&env);

    let result = escrow.try_create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &vec![&env, -1_i128],
        &ReleaseAuthorization::ClientOnly,
    );
    assert!(
        result.is_err(),
        "negative milestone amount should be rejected"
    );
}

/// Acceptance: single milestone exactly at MAX_SINGLE_AMOUNT_STROOPS (also MAX_TOTAL_ESCROW_STROOPS).
#[test]
fn create_contract_accepts_single_milestone_at_cap() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, _) = setup_with_token(&env);

    escrow.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &vec![&env, MAX_SINGLE_AMOUNT_STROOPS],
        &ReleaseAuthorization::ClientOnly,
    );
}

/// Rejection: single milestone 1 stroop above MAX_SINGLE_AMOUNT_STROOPS.
#[test]
fn create_contract_rejects_one_above_single_amount_cap() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, _) = setup_with_token(&env);

    let result = escrow.try_create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &vec![&env, MAX_SINGLE_AMOUNT_STROOPS + 1],
        &ReleaseAuthorization::ClientOnly,
    );
    assert!(result.is_err(), "amount above cap must be rejected");
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. create_contract – total cap
// ═══════════════════════════════════════════════════════════════════════════

/// Acceptance: two milestones that sum to exactly MAX_TOTAL_ESCROW_STROOPS.
#[test]
fn create_contract_accepts_total_at_cap_split_across_milestones() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, _) = setup_with_token(&env);

    let half = MAX_TOTAL_ESCROW_STROOPS / 2;
    let remainder = MAX_TOTAL_ESCROW_STROOPS - half;

    escrow.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &vec![&env, half, remainder],
        &ReleaseAuthorization::ClientOnly,
    );
}

/// Rejection: i128::MAX far exceeds the cap; must surface as a validation error.
#[test]
fn create_contract_rejects_i128_max_milestone() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, _) = setup_with_token(&env);

    let result = escrow.try_create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &vec![&env, i128::MAX],
        &ReleaseAuthorization::ClientOnly,
    );
    assert!(result.is_err(), "i128::MAX milestone must be rejected");
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. create_contract – participant identity
// ═══════════════════════════════════════════════════════════════════════════

/// Rejection: client == freelancer raises `InvalidParticipant`.
#[test]
fn create_contract_rejects_same_client_and_freelancer() {
    let env = Env::default();
    let (escrow, _, client_addr, _, _) = setup_with_token(&env);

    assert_contract_error(
        escrow.try_create_contract(
            &client_addr,
            &client_addr,
            &None,
            &vec![&env, 1_i128],
            &ReleaseAuthorization::ClientOnly,
        ),
        EscrowError::InvalidParticipant,
    );
}

/// Rejection: arbiter == client raises `InvalidArbiter`.
#[test]
fn create_contract_rejects_arbiter_equal_to_client() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, _) = setup_with_token(&env);

    assert_contract_error(
        escrow.try_create_contract(
            &client_addr,
            &freelancer_addr,
            &Some(client_addr.clone()),
            &vec![&env, 1_i128],
            &ReleaseAuthorization::ClientOnly,
        ),
        EscrowError::InvalidArbiter,
    );
}

/// Rejection: arbiter == freelancer raises `InvalidArbiter`.
#[test]
fn create_contract_rejects_arbiter_equal_to_freelancer() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, _) = setup_with_token(&env);

    assert_contract_error(
        escrow.try_create_contract(
            &client_addr,
            &freelancer_addr,
            &Some(freelancer_addr.clone()),
            &vec![&env, 1_i128],
            &ReleaseAuthorization::ClientOnly,
        ),
        EscrowError::InvalidArbiter,
    );
}

/// Rejection: ArbiterOnly mode without arbiter raises `MissingArbiter`.
#[test]
fn create_contract_rejects_arbiter_only_without_arbiter() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, _) = setup_with_token(&env);

    assert_contract_error(
        escrow.try_create_contract(
            &client_addr,
            &freelancer_addr,
            &None,
            &vec![&env, 1_i128],
            &ReleaseAuthorization::ArbiterOnly,
        ),
        EscrowError::MissingArbiter,
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. deposit_funds – amount boundaries
// ═══════════════════════════════════════════════════════════════════════════

/// Rejection: deposit amount of 0 raises `AmountMustBePositive`.
#[test]
fn deposit_funds_rejects_zero_amount() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, _) = setup_with_token(&env);
    let id = escrow.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &vec![&env, 1_0000000_i128], // 1 whole-token at 7-decimal SAC scale
        &ReleaseAuthorization::ClientOnly,
    );

    assert_contract_error(
        escrow.try_deposit_funds(&id, &client_addr, &0_i128),
        EscrowError::AmountMustBePositive,
    );
}

/// Rejection: negative deposit amount raises `AmountMustBePositive`.
#[test]
fn deposit_funds_rejects_negative_amount() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, _) = setup_with_token(&env);
    let id = escrow.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &vec![&env, 1_0000000_i128], // 1 whole-token at 7-decimal SAC scale
        &ReleaseAuthorization::ClientOnly,
    );

    assert_contract_error(
        escrow.try_deposit_funds(&id, &client_addr, &-1_i128),
        EscrowError::AmountMustBePositive,
    );
}

/// Acceptance: deposit of exactly the contract total completes funding.
#[test]
fn deposit_funds_accepts_exact_contract_total() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, token) = setup_with_token(&env);
    let amount = 100_0000000_i128;
    let id = escrow.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &vec![&env, amount],
        &ReleaseAuthorization::ClientOnly,
    );
    StellarAssetClient::new(&env, &token).mint(&client_addr, &amount);
    assert!(escrow.deposit_funds(&id, &client_addr, &amount));
}

/// Rejection: deposit that would exceed the contract total raises an error.
#[test]
fn deposit_funds_rejects_over_contract_total() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, token) = setup_with_token(&env);
    let amount = 100_0000000_i128;
    let id = escrow.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &vec![&env, amount],
        &ReleaseAuthorization::ClientOnly,
    );
    StellarAssetClient::new(&env, &token).mint(&client_addr, &(amount + 1));
    // First deposit fills it exactly.
    escrow.deposit_funds(&id, &client_addr, &amount);
    // Any further deposit should fail.
    let result = escrow.try_deposit_funds(&id, &client_addr, &1_i128);
    assert!(result.is_err(), "deposit beyond cap must be rejected");
}

// ═══════════════════════════════════════════════════════════════════════════
// 6. release_milestone – index and state boundaries
// ═══════════════════════════════════════════════════════════════════════════

/// Rejection: milestone index equal to milestones.len() raises `IndexOutOfBounds`.
#[test]
fn release_milestone_rejects_out_of_bounds_index() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, token) = setup_with_token(&env);
    let id = funded_one_milestone(&env, &escrow, &client_addr, &freelancer_addr, &token, 10_0000000_i128);

    // Approve first so auth check doesn't fire before index check.
    escrow.approve_milestone_release(&id, &client_addr, &0u32);

    assert_contract_error(
        escrow.try_release_milestone(&id, &client_addr, &1u32), // only index 0 exists
        EscrowError::IndexOutOfBounds,
    );
}

/// Rejection: releasing an already-released milestone raises `MilestoneAlreadyReleased`.
#[test]
fn release_milestone_rejects_double_release() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, token) = setup_with_token(&env);
    let id = funded_one_milestone(&env, &escrow, &client_addr, &freelancer_addr, &token, 10_0000000_i128);

    escrow.approve_milestone_release(&id, &client_addr, &0u32);
    escrow.release_milestone(&id, &client_addr, &0u32);

    // Contract is now Completed; attempt a second release on same index.
    let result = escrow.try_release_milestone(&id, &client_addr, &0u32);
    assert!(result.is_err(), "double release must be rejected");
}

/// Rejection: releasing on a non-funded contract (Created) raises `InvalidState`.
#[test]
fn release_milestone_rejects_non_funded_state() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, _) = setup_with_token(&env);
    let id = escrow.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &vec![&env, 10_0000000_i128],
        &ReleaseAuthorization::ClientOnly,
    );
    // Contract is Created (not yet funded).
    assert_contract_error(
        escrow.try_release_milestone(&id, &client_addr, &0u32),
        EscrowError::InvalidState,
    );
}

/// Rejection: caller not authorized for release raises `UnauthorizedRole`.
#[test]
fn release_milestone_rejects_unauthorized_caller() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, token) = setup_with_token(&env);
    let id = funded_one_milestone(&env, &escrow, &client_addr, &freelancer_addr, &token, 10_0000000_i128);

    let rogue = Address::generate(&env);
    escrow.approve_milestone_release(&id, &client_addr, &0u32);

    assert_contract_error(
        escrow.try_release_milestone(&id, &rogue, &0u32),
        EscrowError::UnauthorizedRole,
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 7. approve_milestone_release – index boundary
// ═══════════════════════════════════════════════════════════════════════════

/// Rejection: approval for an out-of-bounds milestone index raises an error.
#[test]
fn approve_milestone_release_rejects_out_of_bounds_index() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, token) = setup_with_token(&env);
    let id = funded_one_milestone(&env, &escrow, &client_addr, &freelancer_addr, &token, 10_0000000_i128);

    let result = escrow.try_approve_milestone_release(
        &id,
        &client_addr,
        &1u32, // only index 0 exists
    );
    assert!(result.is_err(), "OOB approval index must be rejected");
}

// ═══════════════════════════════════════════════════════════════════════════
// 8. issue_reputation – rating boundaries
// ═══════════════════════════════════════════════════════════════════════════

/// Acceptance: exactly MIN_RATING is accepted.
#[test]
fn issue_reputation_accepts_min_rating() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, token) = setup_with_token(&env);
    let id = funded_one_milestone(&env, &escrow, &client_addr, &freelancer_addr, &token, 10_0000000_i128);
    complete_one_milestone(&escrow, id, &client_addr);

    let comment = repeat_char(&env, 'a', 5);
    assert!(escrow.issue_reputation(&id, &client_addr, &MIN_RATING, &comment));
}

/// Acceptance: exactly MAX_RATING is accepted.
#[test]
fn issue_reputation_accepts_max_rating() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, token) = setup_with_token(&env);
    let id = funded_one_milestone(&env, &escrow, &client_addr, &freelancer_addr, &token, 10_0000000_i128);
    complete_one_milestone(&escrow, id, &client_addr);

    let comment = repeat_char(&env, 'a', 5);
    assert!(escrow.issue_reputation(&id, &client_addr, &MAX_RATING, &comment));
}

/// Rejection: rating of 0 (below MIN_RATING) raises `InvalidRating`.
#[test]
fn issue_reputation_rejects_rating_zero() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, token) = setup_with_token(&env);
    let id = funded_one_milestone(&env, &escrow, &client_addr, &freelancer_addr, &token, 10_0000000_i128);
    complete_one_milestone(&escrow, id, &client_addr);

    let comment = repeat_char(&env, 'a', 5);
    assert_contract_error(
        escrow.try_issue_reputation(&id, &client_addr, &0u32, &comment),
        EscrowError::InvalidRating,
    );
}

/// Rejection: rating of MAX_RATING + 1 raises `InvalidRating`.
#[test]
fn issue_reputation_rejects_rating_above_max() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, token) = setup_with_token(&env);
    let id = funded_one_milestone(&env, &escrow, &client_addr, &freelancer_addr, &token, 10_0000000_i128);
    complete_one_milestone(&escrow, id, &client_addr);

    let comment = repeat_char(&env, 'a', 5);
    assert_contract_error(
        escrow.try_issue_reputation(&id, &client_addr, &(MAX_RATING + 1), &comment),
        EscrowError::InvalidRating,
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 9. issue_reputation – comment length boundaries
// ═══════════════════════════════════════════════════════════════════════════

/// Acceptance: exactly MIN_COMMENT_BYTES (1 byte).
#[test]
fn issue_reputation_accepts_one_byte_comment() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, token) = setup_with_token(&env);
    let id = funded_one_milestone(&env, &escrow, &client_addr, &freelancer_addr, &token, 10_0000000_i128);
    complete_one_milestone(&escrow, id, &client_addr);

    let comment = repeat_char(&env, 'x', MIN_COMMENT_BYTES);
    assert!(escrow.issue_reputation(&id, &client_addr, &3u32, &comment));
}

/// Acceptance: exactly MAX_COMMENT_BYTES (200 bytes).
#[test]
fn issue_reputation_accepts_max_comment_bytes() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, token) = setup_with_token(&env);
    let id = funded_one_milestone(&env, &escrow, &client_addr, &freelancer_addr, &token, 10_0000000_i128);
    complete_one_milestone(&escrow, id, &client_addr);

    let comment = repeat_char(&env, 'a', MAX_COMMENT_BYTES);
    assert!(escrow.issue_reputation(&id, &client_addr, &3u32, &comment));
}

/// Rejection: empty comment raises `EmptyComment`.
#[test]
fn issue_reputation_rejects_empty_comment() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, token) = setup_with_token(&env);
    let id = funded_one_milestone(&env, &escrow, &client_addr, &freelancer_addr, &token, 10_0000000_i128);
    complete_one_milestone(&escrow, id, &client_addr);

    let comment = String::from_str(&env, "");
    assert_contract_error(
        escrow.try_issue_reputation(&id, &client_addr, &3u32, &comment),
        EscrowError::EmptyComment,
    );
}

/// Rejection: comment of MAX_COMMENT_BYTES + 1 raises `CommentTooLong`.
#[test]
fn issue_reputation_rejects_comment_one_byte_over_max() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, token) = setup_with_token(&env);
    let id = funded_one_milestone(&env, &escrow, &client_addr, &freelancer_addr, &token, 10_0000000_i128);
    complete_one_milestone(&escrow, id, &client_addr);

    let comment = repeat_char(&env, 'a', MAX_COMMENT_BYTES + 1);
    assert_contract_error(
        escrow.try_issue_reputation(&id, &client_addr, &3u32, &comment),
        EscrowError::CommentTooLong,
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 10. issue_reputation – state and duplicate boundaries
// ═══════════════════════════════════════════════════════════════════════════

/// Rejection: reputation on a non-Completed contract raises `NotCompleted`.
#[test]
fn issue_reputation_rejects_non_completed_state() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, token) = setup_with_token(&env);
    let id = funded_one_milestone(&env, &escrow, &client_addr, &freelancer_addr, &token, 10_0000000_i128);
    // Contract is Funded (not Completed).

    let comment = repeat_char(&env, 'a', 5);
    assert_contract_error(
        escrow.try_issue_reputation(&id, &client_addr, &3u32, &comment),
        EscrowError::NotCompleted,
    );
}

/// Rejection: issuing reputation a second time on the same contract raises
/// `ReputationAlreadyIssued`.
#[test]
fn issue_reputation_rejects_duplicate_submission() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, token) = setup_with_token(&env);
    let id = funded_one_milestone(&env, &escrow, &client_addr, &freelancer_addr, &token, 10_0000000_i128);
    complete_one_milestone(&escrow, id, &client_addr);

    let comment = repeat_char(&env, 'a', 5);
    escrow.issue_reputation(&id, &client_addr, &3u32, &comment);

    assert_contract_error(
        escrow.try_issue_reputation(&id, &client_addr, &3u32, &comment),
        EscrowError::ReputationAlreadyIssued,
    );
}

/// Rejection: non-client caller raises `UnauthorizedRole`.
#[test]
fn issue_reputation_rejects_non_client_caller() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, token) = setup_with_token(&env);
    let id = funded_one_milestone(&env, &escrow, &client_addr, &freelancer_addr, &token, 10_0000000_i128);
    complete_one_milestone(&escrow, id, &client_addr);

    let comment = repeat_char(&env, 'a', 5);
    let rogue = Address::generate(&env);
    assert_contract_error(
        escrow.try_issue_reputation(&id, &rogue, &3u32, &comment),
        EscrowError::UnauthorizedRole,
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 11. cancel_contract – role and state boundaries
// ═══════════════════════════════════════════════════════════════════════════

/// Acceptance: client can cancel a Created contract.
#[test]
fn cancel_contract_accepts_created_state() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, _) = setup_with_token(&env);
    let id = escrow.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &vec![&env, 10_0000000_i128],
        &ReleaseAuthorization::ClientOnly,
    );

    assert!(escrow.cancel_contract(&id, &client_addr));
}

/// Rejection: non-client caller raises `UnauthorizedRole`.
#[test]
fn cancel_contract_rejects_non_client_caller() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, _) = setup_with_token(&env);
    let id = escrow.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &vec![&env, 10_0000000_i128],
        &ReleaseAuthorization::ClientOnly,
    );

    let rogue = Address::generate(&env);
    assert_contract_error(
        escrow.try_cancel_contract(&id, &rogue),
        EscrowError::UnauthorizedRole,
    );
}

/// Rejection: cancelling an already-cancelled contract raises `ContractCancelled`.
#[test]
fn cancel_contract_rejects_double_cancel() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, _) = setup_with_token(&env);
    let id = escrow.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &vec![&env, 10_0000000_i128],
        &ReleaseAuthorization::ClientOnly,
    );
    escrow.cancel_contract(&id, &client_addr);

    assert_contract_error(
        escrow.try_cancel_contract(&id, &client_addr),
        EscrowError::ContractCancelled,
    );
}

/// Rejection: cancelling a Completed contract raises `InvalidStatusTransition`.
#[test]
fn cancel_contract_rejects_completed_state() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, token) = setup_with_token(&env);
    let id = funded_one_milestone(&env, &escrow, &client_addr, &freelancer_addr, &token, 10_0000000_i128);
    complete_one_milestone(&escrow, id, &client_addr);

    assert_contract_error(
        escrow.try_cancel_contract(&id, &client_addr),
        EscrowError::InvalidStatusTransition,
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 12. initialize – double-init
// ═══════════════════════════════════════════════════════════════════════════

/// Rejection: calling initialize a second time raises `AlreadyInitialized`.
#[test]
fn initialize_rejects_double_initialization() {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();
    let addr = env.register(Escrow, ());
    let escrow = EscrowClient::new(&env, &addr);
    let admin = Address::generate(&env);
    escrow.initialize(&admin);

    assert_contract_error(
        escrow.try_initialize(&admin),
        EscrowError::AlreadyInitialized,
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 13. get_bounds – correct compile-time constants
// ═══════════════════════════════════════════════════════════════════════════

/// `get_bounds()` returns values that match the compile-time constants used
/// in runtime enforcement, so off-chain consumers stay in sync.
#[test]
fn get_bounds_returns_correct_constants() {
    let env = Env::default();
    let (escrow, _) = setup_no_token(&env);

    let bounds = escrow.get_bounds();
    assert_eq!(bounds.max_milestones, MAX_MILESTONES);
    assert_eq!(bounds.max_single_milestone_stroops, MAX_SINGLE_AMOUNT_STROOPS);
    assert_eq!(bounds.max_total_escrow_stroops, MAX_TOTAL_ESCROW_STROOPS);
    assert_eq!(bounds.max_fee_bps, MAX_FEE_BPS);
}

/// `get_bounds()` succeeds before `initialize` has been called.
#[test]
fn get_bounds_works_before_initialize() {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();
    let addr = env.register(Escrow, ());
    let escrow = EscrowClient::new(&env, &addr);

    // Must not panic — `get_bounds` is a pure read of compile-time constants.
    let _ = escrow.get_bounds();
}

// ═══════════════════════════════════════════════════════════════════════════
// 14. refund_unreleased_milestones – index and duplicate boundaries
// ═══════════════════════════════════════════════════════════════════════════

/// Rejection: empty milestone-index list raises `EmptyRefundRequest`.
#[test]
fn refund_unreleased_milestones_rejects_empty_list() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, token) = setup_with_token(&env);
    let id = funded_one_milestone(&env, &escrow, &client_addr, &freelancer_addr, &token, 10_0000000_i128);

    assert_contract_error(
        escrow.try_refund_unreleased_milestones(&id, &vec![&env]),
        EscrowError::EmptyRefundRequest,
    );
}

/// Rejection: duplicate index in the refund list raises `DuplicateMilestoneInRefund`.
#[test]
fn refund_unreleased_milestones_rejects_duplicate_indices() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, token) = setup_with_token(&env);
    let id = funded_one_milestone(&env, &escrow, &client_addr, &freelancer_addr, &token, 10_0000000_i128);

    assert_contract_error(
        escrow.try_refund_unreleased_milestones(&id, &vec![&env, 0u32, 0u32]),
        EscrowError::DuplicateMilestoneInRefund,
    );
}

/// Rejection: index >= milestones.len() raises `IndexOutOfBounds`.
#[test]
fn refund_unreleased_milestones_rejects_out_of_bounds_index() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, token) = setup_with_token(&env);
    let id = funded_one_milestone(&env, &escrow, &client_addr, &freelancer_addr, &token, 10_0000000_i128);

    assert_contract_error(
        escrow.try_refund_unreleased_milestones(&id, &vec![&env, 1u32]), // only index 0 exists
        EscrowError::IndexOutOfBounds,
    );
}

/// Acceptance: refunding index 0 of a funded one-milestone contract succeeds.
#[test]
fn refund_unreleased_milestones_accepts_valid_index() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, token) = setup_with_token(&env);
    let id = funded_one_milestone(&env, &escrow, &client_addr, &freelancer_addr, &token, 10_0000000_i128);

    let refunded = escrow.refund_unreleased_milestones(&id, &vec![&env, 0u32]);
    assert_eq!(refunded, 10_0000000_i128);
}

// ═══════════════════════════════════════════════════════════════════════════
// 15. set_protocol_fee_bps – boundary values
// ═══════════════════════════════════════════════════════════════════════════

/// Acceptance: 0 bps (MIN_FEE_BPS) disables fee collection.
#[test]
fn set_protocol_fee_bps_accepts_zero() {
    let env = Env::default();
    let (escrow, _) = setup_no_token(&env);
    // Admin nonce starts at 0; the first call needs nonce = 1.
    assert!(escrow.set_protocol_fee_bps(&(MIN_FEE_BPS as u32), &1u64));
    assert_eq!(escrow.get_protocol_fee_bps(), 0u32);
}

/// Acceptance: exactly MAX_FEE_BPS (10 000 bps = 100 %).
#[test]
fn set_protocol_fee_bps_accepts_max() {
    let env = Env::default();
    let (escrow, _) = setup_no_token(&env);
    assert!(escrow.set_protocol_fee_bps(&MAX_FEE_BPS, &1u64));
    assert_eq!(escrow.get_protocol_fee_bps(), MAX_FEE_BPS);
}

/// Rejection: MAX_FEE_BPS + 1 raises `InvalidProtocolParameters`.
#[test]
fn set_protocol_fee_bps_rejects_one_above_max() {
    let env = Env::default();
    let (escrow, _) = setup_no_token(&env);
    assert_contract_error(
        escrow.try_set_protocol_fee_bps(&(MAX_FEE_BPS + 1), &1u64),
        EscrowError::InvalidProtocolParameters,
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 16. set_max_milestones – admin-configurable cap
// ═══════════════════════════════════════════════════════════════════════════

/// Acceptance: setting max milestones to MIN_MAX_MILESTONES (1).
#[test]
fn set_max_milestones_accepts_min_value() {
    let env = Env::default();
    let (escrow, _) = setup_no_token(&env);
    assert!(escrow.set_max_milestones(&MIN_MAX_MILESTONES));
    assert_eq!(escrow.get_max_milestones(), MIN_MAX_MILESTONES);
}

/// Acceptance: setting max milestones to MAX_MAX_MILESTONES (100).
#[test]
fn set_max_milestones_accepts_max_value() {
    let env = Env::default();
    let (escrow, _) = setup_no_token(&env);
    assert!(escrow.set_max_milestones(&MAX_MAX_MILESTONES));
    assert_eq!(escrow.get_max_milestones(), MAX_MAX_MILESTONES);
}

/// Rejection: 0 (below MIN_MAX_MILESTONES) raises `LimitOutOfRange`.
#[test]
fn set_max_milestones_rejects_zero() {
    let env = Env::default();
    let (escrow, _) = setup_no_token(&env);
    assert_contract_error(
        escrow.try_set_max_milestones(&0u32),
        EscrowError::LimitOutOfRange,
    );
}

/// Rejection: MAX_MAX_MILESTONES + 1 raises `LimitOutOfRange`.
#[test]
fn set_max_milestones_rejects_one_above_max() {
    let env = Env::default();
    let (escrow, _) = setup_no_token(&env);
    assert_contract_error(
        escrow.try_set_max_milestones(&(MAX_MAX_MILESTONES + 1)),
        EscrowError::LimitOutOfRange,
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 17. Regression – original valid inputs continue to be accepted
// ═══════════════════════════════════════════════════════════════════════════

/// A three-milestone contract with the original example amounts (200 / 400 / 600
/// million stroops) must still be created without error after all boundary
/// enforcement is in place.
#[test]
fn regression_three_milestone_contract_still_accepted() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, _) = setup_with_token(&env);

    let id = escrow.create_contract(
        &client_addr,
        &freelancer_addr,
        &None,
        &vec![&env, 200_0000000_i128, 400_0000000_i128, 600_0000000_i128],
        &ReleaseAuthorization::ClientOnly,
    );
    assert!(id < u32::MAX);
}

/// A mid-range rating (3) with a 10-byte ASCII comment continues to be accepted.
#[test]
fn regression_mid_range_reputation_still_accepted() {
    let env = Env::default();
    let (escrow, _, client_addr, freelancer_addr, token) = setup_with_token(&env);
    let id = funded_one_milestone(&env, &escrow, &client_addr, &freelancer_addr, &token, 200_0000000_i128);
    complete_one_milestone(&escrow, id, &client_addr);

    let comment = String::from_str(&env, "great work");
    assert!(escrow.issue_reputation(&id, &client_addr, &3u32, &comment));
}
