//! Typed compatibility tests for `storage_validation` public contracts.
//!
//! # Purpose
//!
//! This module pins the **compatibility contract** of every function in
//! [`crate::storage_validation`] through end-to-end integration assertions that
//! verify not just *that* a rejection occurs, but *which typed error* is
//! returned.
//!
//! The internal `#[cfg(test)]` block in `storage_validation.rs` uses plain
//! `#[should_panic]`, which passes even if the function panics with the **wrong
//! error code**. The tests here use `try_*` entrypoints and pattern-match the
//! returned `soroban_sdk::Error` to ensure the exact `Error` / `EscrowError`
//! variant is preserved through any future refactor.
//!
//! # Coverage matrix
//!
//! | Function                         | Accept boundary | Reject boundary | Error identity |
//! |----------------------------------|-----------------|-----------------|----------------|
//! | `validate_escrow_total_cap`      | 1, i128::MAX    | 0, -1, i128::MIN | `InvalidProtocolParameters` |
//! | `validate_reputation_config_params` | (1,10,1000), (3,3,1) | 0-min, max<min, max>10, 0-comment, comment>1000 | `InvalidProtocolParameters` |
//! | `validate_milestone_count`       | 1, MAX_MILESTONES | 0, MAX+1, u32::MAX | `EmptyMilestones` / `TooManyMilestones` |
//! | `validate_protocol_fee_bps`      | 0, MAX_FEE_BPS  | MAX+1, u32::MAX | `InvalidProtocolParameters` |
//! | `validate_stroop_amount`         | 1, MAX_SINGLE   | 0, -1, MAX+1, i128::MAX | `AmountMustBePositive` / `InvalidMilestoneAmount` |
//!
//! Each rejection test contains a comment noting which constant or invariant
//! the boundary is derived from, so future maintainers know what to update
//! if the constant changes.
//!
//! # How to read a failure
//!
//! If a test in this file fails after a refactor, it means the error code
//! emitted by the entrypoint changed — which is a breaking change for all
//! callers that decode error codes from transaction results. The fix is either:
//!
//! 1. Restore the original error code (if the change was unintentional).
//! 2. Treat the change as a protocol upgrade, update the compat test to the
//!    new error code, and document the migration in the PR description.
//!
//! Run these tests locally with:
//! ```sh
//! cargo test -p escrow --lib storage_validation_compat
//! ```

#![cfg(test)]

use soroban_sdk::{testutils::Address as _, token::StellarAssetClient, vec, Address, Env};

use crate::{
    Error, Escrow, EscrowClient, EscrowError, ReleaseAuthorization, MAX_FEE_BPS, MAX_MILESTONES,
    MAX_SINGLE_AMOUNT_STROOPS,
};

// ── Fixture helpers ───────────────────────────────────────────────────────────

/// Minimal fixture: escrow initialized, no settlement token.
fn setup(env: &Env) -> (EscrowClient<'_>, Address) {
    env.mock_all_auths_allowing_non_root_auth();
    let addr = env.register(Escrow, ());
    let client = EscrowClient::new(env, &addr);
    let admin = Address::generate(env);
    client.initialize(&admin);
    (client, admin)
}

/// Full fixture: initialized escrow + bound SAC token + minted client balance.
#[allow(deprecated)]
fn setup_with_token(env: &Env) -> (EscrowClient<'_>, Address, Address, Address) {
    env.mock_all_auths_allowing_non_root_auth();
    let addr = env.register(Escrow, ());
    let client = EscrowClient::new(env, &addr);
    let admin = Address::generate(env);
    client.initialize(&admin);
    let token = env.register_stellar_asset_contract(admin.clone());
    client.bind_settlement_token(&admin, &token);
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    // Mint enough for any realistic test.
    StellarAssetClient::new(env, &token).mint(&client_addr, &(MAX_SINGLE_AMOUNT_STROOPS * 2));
    (client, client_addr, freelancer_addr, admin)
}

/// Helper: assert a `try_*` result carries exactly `expected` as its contract error.
fn assert_contract_err<T: core::fmt::Debug, E: Into<soroban_sdk::Error> + core::fmt::Debug>(
    result: Result<T, Result<soroban_sdk::Error, soroban_sdk::InvokeError>>,
    expected: E,
) {
    let expected_err: soroban_sdk::Error = expected.into();
    match result {
        Err(Ok(actual)) => assert_eq!(
            actual, expected_err,
            "error code mismatch: expected {:?}, got {:?}",
            expected_err, actual
        ),
        other => panic!(
            "expected contract error {:?}, got unexpected result: {:?}",
            expected_err, other
        ),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// validate_escrow_total_cap
//
// Exercised via `set_governed_params` which is the canonical caller.
// Boundary constant: none (hardcoded `> 0`).
// Error: Error::InvalidProtocolParameters
// ═══════════════════════════════════════════════════════════════════════════════

/// COMPAT: `1` (minimum positive value) must be accepted.
#[test]
fn compat_escrow_total_cap_accepts_1_stroop() {
    let env = Env::default();
    let (escrow, admin) = setup(&env);
    assert!(escrow.set_governed_params(&admin, &0_u32, &1_i128));
    let params = escrow.get_governed_parameters().unwrap();
    assert_eq!(params.max_escrow_total_stroops, 1_i128);
}

/// COMPAT: `i128::MAX` must be accepted (no upper bound on cap).
#[test]
fn compat_escrow_total_cap_accepts_i128_max() {
    let env = Env::default();
    let (escrow, admin) = setup(&env);
    assert!(escrow.set_governed_params(&admin, &0_u32, &i128::MAX));
    let params = escrow.get_governed_parameters().unwrap();
    assert_eq!(params.max_escrow_total_stroops, i128::MAX);
}

/// COMPAT: `0` must produce `InvalidProtocolParameters`.
/// Derived from: hardcoded `<= 0` guard in `validate_escrow_total_cap`.
#[test]
fn compat_escrow_total_cap_rejects_zero_with_typed_error() {
    let env = Env::default();
    let (escrow, admin) = setup(&env);
    let result = escrow.try_set_governed_params(&admin, &0_u32, &0_i128);
    assert_contract_err(result, Error::InvalidProtocolParameters);
}

/// COMPAT: `-1` must produce `InvalidProtocolParameters`.
/// Derived from: hardcoded `<= 0` guard in `validate_escrow_total_cap`.
#[test]
fn compat_escrow_total_cap_rejects_negative_1_with_typed_error() {
    let env = Env::default();
    let (escrow, admin) = setup(&env);
    let result = escrow.try_set_governed_params(&admin, &0_u32, &(-1_i128));
    assert_contract_err(result, Error::InvalidProtocolParameters);
}

/// COMPAT: `i128::MIN` must produce `InvalidProtocolParameters`.
/// Derived from: hardcoded `<= 0` guard in `validate_escrow_total_cap`.
#[test]
fn compat_escrow_total_cap_rejects_i128_min_with_typed_error() {
    let env = Env::default();
    let (escrow, admin) = setup(&env);
    let result = escrow.try_set_governed_params(&admin, &0_u32, &i128::MIN);
    assert_contract_err(result, Error::InvalidProtocolParameters);
}

/// COMPAT: a rejection must not mutate stored parameters.
#[test]
fn compat_escrow_total_cap_rejection_leaves_state_unchanged() {
    let env = Env::default();
    let (escrow, admin) = setup(&env);
    // Set a known good state.
    escrow.set_governed_params(&admin, &500_u32, &1_000_000_i128);
    // Attempt a zero-cap rejection.
    let _ = escrow.try_set_governed_params(&admin, &500_u32, &0_i128);
    // Original state must be preserved.
    let params = escrow.get_governed_parameters().unwrap();
    assert_eq!(params.max_escrow_total_stroops, 1_000_000_i128);
    assert_eq!(params.protocol_fee_bps, 500_u32);
}

// ═══════════════════════════════════════════════════════════════════════════════
// validate_reputation_config_params
//
// Exercised via `set_reputation_config`.
// Boundary constants: MIN_RATING=1, MAX_REPUTATION_CONFIG_RATING_CEILING=10,
//                     MIN_COMMENT_BYTES=1, MAX_REPUTATION_CONFIG_COMMENT_BYTES_CEILING=1_000
// Error: Error::InvalidProtocolParameters (all violations)
// ═══════════════════════════════════════════════════════════════════════════════

/// COMPAT: default config (1, 5, 200) must be accepted.
#[test]
fn compat_rep_config_accepts_default() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    assert!(escrow.set_reputation_config(&1_u32, &5_u32, &200_u32));
    let cfg = escrow.get_reputation_config();
    assert_eq!(cfg.min_rating, 1);
    assert_eq!(cfg.max_rating, 5);
    assert_eq!(cfg.max_comment_bytes, 200);
}

/// COMPAT: degenerate range (min == max) must be accepted.
#[test]
fn compat_rep_config_accepts_equal_min_max_rating() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    assert!(escrow.set_reputation_config(&3_u32, &3_u32, &1_u32));
    let cfg = escrow.get_reputation_config();
    assert_eq!(cfg.min_rating, 3);
    assert_eq!(cfg.max_rating, 3);
}

/// COMPAT: max_comment_bytes = 1 (minimum) must be accepted.
#[test]
fn compat_rep_config_accepts_min_comment_bytes() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    assert!(escrow.set_reputation_config(&1_u32, &5_u32, &1_u32));
    let cfg = escrow.get_reputation_config();
    assert_eq!(cfg.max_comment_bytes, 1);
}

/// COMPAT: max_comment_bytes = 1_000 (ceiling) must be accepted.
#[test]
fn compat_rep_config_accepts_max_comment_bytes() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    assert!(escrow.set_reputation_config(&1_u32, &10_u32, &1_000_u32));
    let cfg = escrow.get_reputation_config();
    assert_eq!(cfg.max_comment_bytes, 1_000);
}

/// COMPAT: min_rating = 0 must produce `InvalidProtocolParameters`.
/// Derived from: `MIN_RATING = 1`.
#[test]
fn compat_rep_config_rejects_zero_min_rating_with_typed_error() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    let result = escrow.try_set_reputation_config(&0_u32, &5_u32, &200_u32);
    assert_contract_err(result, Error::InvalidProtocolParameters);
}

/// COMPAT: max_rating < min_rating must produce `InvalidProtocolParameters`.
/// Derived from: ordering check `max_rating >= min_rating`.
#[test]
fn compat_rep_config_rejects_max_below_min_with_typed_error() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    let result = escrow.try_set_reputation_config(&5_u32, &3_u32, &200_u32);
    assert_contract_err(result, Error::InvalidProtocolParameters);
}

/// COMPAT: max_rating = 11 (one above ceiling) must produce `InvalidProtocolParameters`.
/// Derived from: `MAX_REPUTATION_CONFIG_RATING_CEILING = 10`.
#[test]
fn compat_rep_config_rejects_max_rating_above_ceiling_with_typed_error() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    let result = escrow.try_set_reputation_config(&1_u32, &11_u32, &200_u32);
    assert_contract_err(result, Error::InvalidProtocolParameters);
}

/// COMPAT: max_comment_bytes = 0 must produce `InvalidProtocolParameters`.
/// Derived from: `MIN_COMMENT_BYTES = 1`.
#[test]
fn compat_rep_config_rejects_zero_comment_bytes_with_typed_error() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    let result = escrow.try_set_reputation_config(&1_u32, &5_u32, &0_u32);
    assert_contract_err(result, Error::InvalidProtocolParameters);
}

/// COMPAT: max_comment_bytes = 1_001 (one above ceiling) must produce `InvalidProtocolParameters`.
/// Derived from: `MAX_REPUTATION_CONFIG_COMMENT_BYTES_CEILING = 1_000`.
#[test]
fn compat_rep_config_rejects_comment_above_ceiling_with_typed_error() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    let result = escrow.try_set_reputation_config(&1_u32, &5_u32, &1_001_u32);
    assert_contract_err(result, Error::InvalidProtocolParameters);
}

/// COMPAT: a rejection must not mutate stored reputation config.
#[test]
fn compat_rep_config_rejection_leaves_state_unchanged() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    escrow.set_reputation_config(&2_u32, &8_u32, &150_u32);
    let _ = escrow.try_set_reputation_config(&2_u32, &8_u32, &0_u32);
    let cfg = escrow.get_reputation_config();
    assert_eq!(cfg.min_rating, 2);
    assert_eq!(cfg.max_rating, 8);
    assert_eq!(cfg.max_comment_bytes, 150);
}

/// COMPAT: repeated valid updates preserve the last written value.
#[test]
fn compat_rep_config_multiple_valid_updates() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    escrow.set_reputation_config(&1_u32, &5_u32, &200_u32);
    escrow.set_reputation_config(&2_u32, &8_u32, &500_u32);
    escrow.set_reputation_config(&1_u32, &10_u32, &1_000_u32);
    let cfg = escrow.get_reputation_config();
    assert_eq!(cfg.min_rating, 1);
    assert_eq!(cfg.max_rating, 10);
    assert_eq!(cfg.max_comment_bytes, 1_000);
}

// ═══════════════════════════════════════════════════════════════════════════════
// validate_milestone_count
//
// Exercised via `create_contract` (milestone Vec length).
// Boundary constant: MAX_MILESTONES = 10
// Errors: EscrowError::EmptyMilestones (count=0)
//         EscrowError::TooManyMilestones (count>MAX_MILESTONES)
//
// COMPAT NOTE: these are TWO DISTINCT error codes. Merging them would be a
// breaking change. The tests below verify both codes separately.
// ═══════════════════════════════════════════════════════════════════════════════

fn milestone_vec(env: &Env, count: u32, amount: i128) -> soroban_sdk::Vec<i128> {
    let mut v = soroban_sdk::Vec::new(env);
    for _ in 0..count {
        v.push_back(amount);
    }
    v
}

/// COMPAT: 1 milestone (minimum) must be accepted.
#[test]
fn compat_milestone_count_accepts_1() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    let result = escrow.try_create_contract(
        &c, &f, &None,
        &milestone_vec(&env, 1, 1_i128),
        &ReleaseAuthorization::ClientOnly,
    );
    assert!(result.is_ok(), "1 milestone must be accepted");
}

/// COMPAT: MAX_MILESTONES milestones must be accepted.
#[test]
fn compat_milestone_count_accepts_max() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    let result = escrow.try_create_contract(
        &c, &f, &None,
        &milestone_vec(&env, MAX_MILESTONES, 1_i128),
        &ReleaseAuthorization::ClientOnly,
    );
    assert!(result.is_ok(), "MAX_MILESTONES milestones must be accepted");
}

/// COMPAT: empty milestones must produce `EmptyMilestones`, NOT `TooManyMilestones`.
/// Derived from: guard `count == 0` checked BEFORE `count > MAX_MILESTONES`.
#[test]
fn compat_milestone_count_rejects_zero_with_empty_milestones_error() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    let result = escrow.try_create_contract(
        &c, &f, &None,
        &soroban_sdk::Vec::new(&env),
        &ReleaseAuthorization::ClientOnly,
    );
    assert_contract_err(result, EscrowError::EmptyMilestones);
}

/// COMPAT: MAX_MILESTONES + 1 must produce `TooManyMilestones`, NOT `EmptyMilestones`.
/// Derived from: `MAX_MILESTONES = 10`.
#[test]
fn compat_milestone_count_rejects_over_max_with_too_many_error() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    let result = escrow.try_create_contract(
        &c, &f, &None,
        &milestone_vec(&env, MAX_MILESTONES + 1, 1_i128),
        &ReleaseAuthorization::ClientOnly,
    );
    assert_contract_err(result, EscrowError::TooManyMilestones);
}

/// COMPAT: the two error codes are distinct — zero produces EmptyMilestones,
/// over-max produces TooManyMilestones. Verify they are not the same value.
#[test]
fn compat_milestone_count_empty_and_too_many_errors_are_distinct() {
    let empty: soroban_sdk::Error = EscrowError::EmptyMilestones.into();
    let too_many: soroban_sdk::Error = EscrowError::TooManyMilestones.into();
    assert_ne!(
        empty, too_many,
        "EmptyMilestones and TooManyMilestones must be distinct error codes"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// validate_protocol_fee_bps
//
// Exercised via `set_protocol_fee_bps`.
// Boundary constant: MAX_FEE_BPS = 10_000 (= PROTOCOL_FEE_BPS_DENOMINATOR)
// Error: Error::InvalidProtocolParameters
// ═══════════════════════════════════════════════════════════════════════════════

/// COMPAT: 0 bps (fee disabled) must be accepted.
#[test]
fn compat_protocol_fee_bps_accepts_zero() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    assert!(escrow.set_protocol_fee_bps(&0_u32, &0_u64));
    assert_eq!(escrow.get_protocol_fee_bps(), 0_u32);
}

/// COMPAT: exactly MAX_FEE_BPS (10_000) must be accepted.
#[test]
fn compat_protocol_fee_bps_accepts_max() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    assert!(escrow.set_protocol_fee_bps(&MAX_FEE_BPS, &0_u64));
    assert_eq!(escrow.get_protocol_fee_bps(), MAX_FEE_BPS);
}

/// COMPAT: a typical 2.5% fee (250 bps) must be accepted and stored faithfully.
#[test]
fn compat_protocol_fee_bps_accepts_typical() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    assert!(escrow.set_protocol_fee_bps(&250_u32, &0_u64));
    assert_eq!(escrow.get_protocol_fee_bps(), 250_u32);
}

/// COMPAT: MAX_FEE_BPS + 1 must produce `InvalidProtocolParameters`.
/// Derived from: `MAX_FEE_BPS = 10_000 = PROTOCOL_FEE_BPS_DENOMINATOR`.
#[test]
fn compat_protocol_fee_bps_rejects_over_max_with_typed_error() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    let result = escrow.try_set_protocol_fee_bps(&(MAX_FEE_BPS + 1), &0_u64);
    assert_contract_err(result, Error::InvalidProtocolParameters);
}

/// COMPAT: u32::MAX must produce `InvalidProtocolParameters`.
/// Derived from: `MAX_FEE_BPS = 10_000`.
#[test]
fn compat_protocol_fee_bps_rejects_u32_max_with_typed_error() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    let result = escrow.try_set_protocol_fee_bps(&u32::MAX, &0_u64);
    assert_contract_err(result, Error::InvalidProtocolParameters);
}

/// COMPAT: a rejection must not mutate the stored fee.
#[test]
fn compat_protocol_fee_bps_rejection_leaves_state_unchanged() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    escrow.set_protocol_fee_bps(&300_u32, &0_u64);
    let _ = escrow.try_set_protocol_fee_bps(&(MAX_FEE_BPS + 1), &0_u64);
    assert_eq!(escrow.get_protocol_fee_bps(), 300_u32);
}

/// COMPAT: repeated valid updates preserve the last written value.
#[test]
fn compat_protocol_fee_bps_multiple_valid_updates() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    let mut nonce = 0_u64;
    for &bps in &[0_u32, 100, 500, MAX_FEE_BPS, 0, 250] {
        assert!(escrow.set_protocol_fee_bps(&bps, &nonce));
        nonce += 1;
    }
    assert_eq!(escrow.get_protocol_fee_bps(), 250_u32);
}

// ═══════════════════════════════════════════════════════════════════════════════
// validate_stroop_amount
//
// Exercised via `deposit_funds` which is the only public caller.
// Boundary constant: MAX_SINGLE_AMOUNT_STROOPS = 1_000_000_0000000
// Errors: EscrowError::AmountMustBePositive (amount <= 0)
//         EscrowError::InvalidMilestoneAmount (amount > MAX_SINGLE_AMOUNT_STROOPS)
//
// COMPAT NOTE: these are TWO DISTINCT error codes. The ordering of checks is
// part of the compat contract — non-positive amounts must produce
// AmountMustBePositive even when they also exceed the ceiling (e.g. i128::MIN).
// ═══════════════════════════════════════════════════════════════════════════════

/// COMPAT: 1 stroop (minimum positive) must be accepted.
#[test]
#[allow(deprecated)]
fn compat_stroop_amount_accepts_1() {
    let env = Env::default();
    let (escrow, client_addr, freelancer_addr, _admin) = setup_with_token(&env);
    let milestones = vec![&env, 1_i128];
    let id = escrow.create_contract(
        &client_addr, &freelancer_addr, &None,
        &milestones,
        &ReleaseAuthorization::ClientOnly,
    );
    // 1 stroop deposit must succeed
    assert!(escrow.deposit_funds(&id, &client_addr, &1_i128));
}

/// COMPAT: MAX_SINGLE_AMOUNT_STROOPS must be accepted.
#[test]
#[allow(deprecated)]
fn compat_stroop_amount_accepts_max() {
    let env = Env::default();
    let (escrow, client_addr, freelancer_addr, _admin) = setup_with_token(&env);
    let milestones = vec![&env, MAX_SINGLE_AMOUNT_STROOPS];
    let id = escrow.create_contract(
        &client_addr, &freelancer_addr, &None,
        &milestones,
        &ReleaseAuthorization::ClientOnly,
    );
    assert!(escrow.deposit_funds(&id, &client_addr, &MAX_SINGLE_AMOUNT_STROOPS));
}

/// COMPAT: 0 must produce `AmountMustBePositive`, NOT `InvalidMilestoneAmount`.
/// Derived from: `amount <= 0` guard checked BEFORE `amount > MAX` guard.
#[test]
fn compat_stroop_amount_rejects_zero_with_amount_must_be_positive() {
    let env = Env::default();
    let (escrow, client_addr, freelancer_addr, _admin) = setup_with_token(&env);
    let milestones = vec![&env, 1_000_i128];
    let id = escrow.create_contract(
        &client_addr, &freelancer_addr, &None,
        &milestones,
        &ReleaseAuthorization::ClientOnly,
    );
    let result = escrow.try_deposit_funds(&id, &client_addr, &0_i128);
    assert_contract_err(result, EscrowError::AmountMustBePositive);
}

/// COMPAT: -1 must produce `AmountMustBePositive`.
/// Derived from: `amount <= 0` guard.
#[test]
fn compat_stroop_amount_rejects_negative_1_with_amount_must_be_positive() {
    let env = Env::default();
    let (escrow, client_addr, freelancer_addr, _admin) = setup_with_token(&env);
    let milestones = vec![&env, 1_000_i128];
    let id = escrow.create_contract(
        &client_addr, &freelancer_addr, &None,
        &milestones,
        &ReleaseAuthorization::ClientOnly,
    );
    let result = escrow.try_deposit_funds(&id, &client_addr, &(-1_i128));
    assert_contract_err(result, EscrowError::AmountMustBePositive);
}

/// COMPAT: i128::MIN must produce `AmountMustBePositive` (not `InvalidMilestoneAmount`).
/// Derived from: non-positive guard fires before cap guard regardless of magnitude.
#[test]
fn compat_stroop_amount_rejects_i128_min_with_amount_must_be_positive() {
    let env = Env::default();
    let (escrow, client_addr, freelancer_addr, _admin) = setup_with_token(&env);
    let milestones = vec![&env, 1_000_i128];
    let id = escrow.create_contract(
        &client_addr, &freelancer_addr, &None,
        &milestones,
        &ReleaseAuthorization::ClientOnly,
    );
    let result = escrow.try_deposit_funds(&id, &client_addr, &i128::MIN);
    assert_contract_err(result, EscrowError::AmountMustBePositive);
}

/// COMPAT: MAX_SINGLE_AMOUNT_STROOPS + 1 must produce `InvalidMilestoneAmount`.
/// Derived from: `amount > MAX_SINGLE_AMOUNT_STROOPS` guard.
#[test]
fn compat_stroop_amount_rejects_over_max_with_invalid_milestone_amount() {
    let env = Env::default();
    let (escrow, client_addr, freelancer_addr, _admin) = setup_with_token(&env);
    let milestones = vec![&env, MAX_SINGLE_AMOUNT_STROOPS];
    let id = escrow.create_contract(
        &client_addr, &freelancer_addr, &None,
        &milestones,
        &ReleaseAuthorization::ClientOnly,
    );
    let result = escrow.try_deposit_funds(&id, &client_addr, &(MAX_SINGLE_AMOUNT_STROOPS + 1));
    assert_contract_err(result, EscrowError::InvalidMilestoneAmount);
}

/// COMPAT: i128::MAX must produce `InvalidMilestoneAmount`.
/// Derived from: `amount > MAX_SINGLE_AMOUNT_STROOPS` (i128::MAX >> MAX).
#[test]
fn compat_stroop_amount_rejects_i128_max_with_invalid_milestone_amount() {
    let env = Env::default();
    let (escrow, client_addr, freelancer_addr, _admin) = setup_with_token(&env);
    let milestones = vec![&env, MAX_SINGLE_AMOUNT_STROOPS];
    let id = escrow.create_contract(
        &client_addr, &freelancer_addr, &None,
        &milestones,
        &ReleaseAuthorization::ClientOnly,
    );
    let result = escrow.try_deposit_funds(&id, &client_addr, &i128::MAX);
    assert_contract_err(result, EscrowError::InvalidMilestoneAmount);
}

/// COMPAT: `AmountMustBePositive` and `InvalidMilestoneAmount` are distinct codes.
#[test]
fn compat_stroop_amount_two_error_codes_are_distinct() {
    let positive: soroban_sdk::Error = EscrowError::AmountMustBePositive.into();
    let invalid: soroban_sdk::Error = EscrowError::InvalidMilestoneAmount.into();
    assert_ne!(
        positive, invalid,
        "AmountMustBePositive and InvalidMilestoneAmount must be distinct error codes"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Boundary constant pinning
//
// These tests assert the exact numeric values of the constants used to compute
// validation boundaries. If a constant changes, the test fails loudly — which
// is a signal that all callers and any documented compatibility windows must
// be re-evaluated before merge.
// ═══════════════════════════════════════════════════════════════════════════════

/// COMPAT: pin MAX_FEE_BPS to 10_000.
/// Changing this would shift the accepted range for `validate_protocol_fee_bps`.
#[test]
fn compat_constant_max_fee_bps_is_10000() {
    assert_eq!(
        MAX_FEE_BPS, 10_000_u32,
        "MAX_FEE_BPS changed — review all callers of validate_protocol_fee_bps"
    );
}

/// COMPAT: pin MAX_MILESTONES to 10.
/// Changing this affects `create_contract` and `get_bounds()`.
#[test]
fn compat_constant_max_milestones_is_10() {
    assert_eq!(
        MAX_MILESTONES, 10_u32,
        "MAX_MILESTONES changed — review create_contract, batch_release, and get_bounds()"
    );
}

/// COMPAT: pin MAX_SINGLE_AMOUNT_STROOPS to 1_000_000_0000000.
/// Changing this shifts the accepted range for `validate_stroop_amount`.
#[test]
fn compat_constant_max_single_amount_stroops() {
    assert_eq!(
        MAX_SINGLE_AMOUNT_STROOPS,
        1_000_000_0000000_i128,
        "MAX_SINGLE_AMOUNT_STROOPS changed — review deposit_funds and get_bounds()"
    );
}

/// COMPAT: pin MIN_RATING to 1 (from milestones_consts).
#[test]
fn compat_constant_min_rating_is_1() {
    use crate::milestones_consts::MIN_RATING;
    assert_eq!(
        MIN_RATING, 1_u32,
        "MIN_RATING changed — review validate_reputation_config_params"
    );
}

/// COMPAT: pin MAX_REPUTATION_CONFIG_RATING_CEILING to 10.
#[test]
fn compat_constant_max_rating_ceiling_is_10() {
    use crate::milestones_consts::MAX_REPUTATION_CONFIG_RATING_CEILING;
    assert_eq!(
        MAX_REPUTATION_CONFIG_RATING_CEILING, 10_u32,
        "MAX_REPUTATION_CONFIG_RATING_CEILING changed — review validate_reputation_config_params"
    );
}

/// COMPAT: pin MAX_REPUTATION_CONFIG_COMMENT_BYTES_CEILING to 1_000.
#[test]
fn compat_constant_max_comment_bytes_ceiling_is_1000() {
    use crate::milestones_consts::MAX_REPUTATION_CONFIG_COMMENT_BYTES_CEILING;
    assert_eq!(
        MAX_REPUTATION_CONFIG_COMMENT_BYTES_CEILING, 1_000_u32,
        "MAX_REPUTATION_CONFIG_COMMENT_BYTES_CEILING changed — review validate_reputation_config_params"
    );
}

/// COMPAT: pin MIN_COMMENT_BYTES to 1.
#[test]
fn compat_constant_min_comment_bytes_is_1() {
    use crate::milestones_consts::MIN_COMMENT_BYTES;
    assert_eq!(
        MIN_COMMENT_BYTES, 1_u32,
        "MIN_COMMENT_BYTES changed — review validate_reputation_config_params"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Regression: valid inputs continue to be accepted
// ═══════════════════════════════════════════════════════════════════════════════

/// COMPAT: a 3-milestone contract with typical amounts must still be created.
#[test]
fn compat_regression_standard_3_milestone_contract() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    let id = escrow.create_contract(
        &c, &f, &None,
        &vec![&env, 200_0000000_i128, 400_0000000_i128, 600_0000000_i128],
        &ReleaseAuthorization::ClientOnly,
    );
    // Verify a contract was created.
    let contract = escrow.get_contract(&id);
    assert_eq!(contract.status, crate::ContractStatus::Created);
}

/// COMPAT: fee can be set to 0, then raised, then lowered — all within bounds.
#[test]
fn compat_regression_fee_bps_round_trip() {
    let env = Env::default();
    let (escrow, _admin) = setup(&env);
    let mut nonce = 0_u64;
    for &bps in &[500_u32, 0, 10_000, 250] {
        assert!(escrow.set_protocol_fee_bps(&bps, &nonce));
        nonce += 1;
    }
    assert_eq!(escrow.get_protocol_fee_bps(), 250_u32);
}

/// COMPAT: governed parameters can be set and read back faithfully.
#[test]
fn compat_regression_governed_params_round_trip() {
    let env = Env::default();
    let (escrow, admin) = setup(&env);
    escrow.set_governed_params(&admin, &250_u32, &500_000_0000000_i128);
    let params = escrow.get_governed_parameters().unwrap();
    assert_eq!(params.protocol_fee_bps, 250_u32);
    assert_eq!(params.max_escrow_total_stroops, 500_000_0000000_i128);
}
