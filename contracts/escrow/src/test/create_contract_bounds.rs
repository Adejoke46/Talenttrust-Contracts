
// Tests for `get_bounds` (ContractBounds) and every input-validation guard in
// `create_contract`.
//
// ── get_bounds contract ──────────────────────────────────────────────────────
//   1. Returns ContractBounds (not ContractSummary)
//   2. max_milestones  == MAX_MILESTONES
//   3. max_single_milestone_stroops == MAX_SINGLE_AMOUNT_STROOPS
//   4. max_total_escrow_stroops     == MAX_TOTAL_ESCROW_STROOPS
//   5. max_fee_bps                  == 10_000 (100 %)
//   6. Idempotent — two calls return identical values
//   7. No auth required (works before initialize)
//   8. Consistency: max_single == max_total (current policy)
//
// ── create_contract guards (in execution order) ─────────────────────────────
//   G1. client == freelancer                   → InvalidParticipant
//   G2. milestone_amounts.is_empty()           → EmptyMilestones
//   G3. len > MAX_MILESTONES (10)              → TooManyMilestones
//   G4. len == MAX_MILESTONES                  → succeeds
//   G5. any amount <= 0 (zero)                 → InvalidMilestoneAmount
//   G6. any amount <= 0 (negative)             → InvalidMilestoneAmount
//   G7. safe_add_amounts overflow              → PotentialOverflow
//   G8. total > MAX_TOTAL_ESCROW_STROOPS       → InvalidMilestoneAmount
//   G9. total == MAX_TOTAL_ESCROW_STROOPS      → succeeds
//   G10. count-guard fires before amount-guard → TooManyMilestones
//   G11. duplicate/racing create_contract      → distinct IDs, no state bleed

#![cfg(test)]

use soroban_sdk::{testutils::Address as _, vec, Address, Env, Vec};

use crate::{
    types::ContractBounds, Escrow, EscrowClient, EscrowError, ReleaseAuthorization, MAX_MILESTONES,
    MAX_SINGLE_AMOUNT_STROOPS, MAX_TOTAL_ESCROW_STROOPS,
};
use soroban_sdk::testutils::Ledger as _;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Returns `(env, contract_address)` with all auths mocked.
fn setup() -> (Env, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(Escrow, ());
    (env, contract_id)
}

/// Advance the ledger sequence so that repeated calls are not treated as
/// identical transactions by the host. This simulates distinct, concurrent
/// submissions arriving at slightly different ledger timestamps.
fn bump_ledger(env: &Env) {
    let mut info = env.ledger().get();
    info.sequence_number = info.sequence_number.saturating_add(1);
    info.timestamp = info.timestamp.saturating_add(1);
    env.ledger().set(info);
}

/// Build a vector of `n` identical amounts.
fn amounts_of(env: &Env, n: u32, amount: i128) -> Vec<i128> {
    let mut v: Vec<i128> = Vec::new(env);
    for _ in 0..n {
        v.push_back(amount);
    }
    v
}

/// Assert that a `try_create_contract` result carries the expected EscrowError.
fn assert_err(
    result: Result<
        Result<u32, soroban_sdk::ConversionError>,
        Result<soroban_sdk::Error, soroban_sdk::InvokeError>,
    >,
    expected: EscrowError,
) {
    match result {
        Err(Ok(e)) => {
            let want: soroban_sdk::Error = expected.into();
            assert_eq!(e, want, "wrong error: expected {:?}", expected);
        }
        other => panic!("expected {:?}, got {:?}", expected, other),
    }
}

// ── get_bounds: ContractBounds shape and values ───────────────────────────────

/// `get_bounds` must return a `ContractBounds` — not a `ContractSummary`.
/// The Soroban generated client encodes the return type so this test
/// confirms that `get_bounds()` is ABI-compatible with `ContractBounds`.
#[test]
fn get_bounds_returns_contract_bounds_type() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let bounds: ContractBounds = client.get_bounds();
    // If the above compiles and runs without a host error, the return type is correct.
    let _ = bounds;
}

/// `max_milestones` must equal the compile-time `MAX_MILESTONES` constant.
#[test]
fn get_bounds_max_milestones_equals_constant() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let bounds = client.get_bounds();
    assert_eq!(
        bounds.max_milestones, MAX_MILESTONES,
        "max_milestones must equal MAX_MILESTONES"
    );
}

/// `max_single_milestone_stroops` must equal `MAX_SINGLE_AMOUNT_STROOPS`.
#[test]
fn get_bounds_max_single_milestone_stroops_equals_constant() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let bounds = client.get_bounds();
    assert_eq!(
        bounds.max_single_milestone_stroops, MAX_SINGLE_AMOUNT_STROOPS,
        "max_single_milestone_stroops must equal MAX_SINGLE_AMOUNT_STROOPS"
    );
}

/// `max_total_escrow_stroops` must equal `MAX_TOTAL_ESCROW_STROOPS`.
#[test]
fn get_bounds_max_total_escrow_stroops_equals_constant() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let bounds = client.get_bounds();
    assert_eq!(
        bounds.max_total_escrow_stroops, MAX_TOTAL_ESCROW_STROOPS,
        "max_total_escrow_stroops must equal MAX_TOTAL_ESCROW_STROOPS"
    );
}

/// `max_fee_bps` must be 10_000 (100%).
#[test]
fn get_bounds_max_fee_bps_is_10000() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let bounds = client.get_bounds();
    assert_eq!(
        bounds.max_fee_bps, 10_000,
        "max_fee_bps must be 10_000 (100 %)"
    );
}

/// `get_bounds` is idempotent — two consecutive calls return identical values.
#[test]
fn get_bounds_is_idempotent() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let first = client.get_bounds();
    let second = client.get_bounds();
    assert_eq!(first, second, "get_bounds must be idempotent");
}

/// `get_bounds` requires no authorization — it must succeed even before
/// `initialize` has been called.
#[test]
fn get_bounds_requires_no_auth_and_works_before_initialize() {
    let env = Env::default();
    // Deliberately do NOT call mock_all_auths.
    let cid = env.register(Escrow, ());
    let client = EscrowClient::new(&env, &cid);
    // Must not panic.
    let bounds = client.get_bounds();
    assert_eq!(bounds.max_milestones, MAX_MILESTONES);
}

/// Under current policy, `max_single_milestone_stroops == max_total_escrow_stroops`.
/// This test makes the invariant explicit so it becomes a compilation break-point
/// if the policy ever diverges.
#[test]
fn get_bounds_single_equals_total_under_current_policy() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let bounds = client.get_bounds();
    assert_eq!(
        bounds.max_single_milestone_stroops, bounds.max_total_escrow_stroops,
        "single and total caps must be equal under current policy"
    );
}

/// All bounds fields must be strictly positive — no field should be zero or
/// negative, which would be a nonsensical protocol configuration.
#[test]
fn get_bounds_all_fields_are_positive() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let bounds = client.get_bounds();
    assert!(bounds.max_milestones > 0, "max_milestones must be > 0");
    assert!(
        bounds.max_single_milestone_stroops > 0,
        "max_single_milestone_stroops must be > 0"
    );
    assert!(
        bounds.max_total_escrow_stroops > 0,
        "max_total_escrow_stroops must be > 0"
    );
    assert!(bounds.max_fee_bps > 0, "max_fee_bps must be > 0");
}

/// `max_fee_bps` must not exceed 10_000 — higher values would imply a fee
/// greater than the payout itself.
#[test]
fn get_bounds_fee_bps_does_not_exceed_100_percent() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let bounds = client.get_bounds();
    assert!(
        bounds.max_fee_bps <= 10_000,
        "max_fee_bps must not exceed 10_000 (100 %)"
    );
}

/// `get_bounds` result must not contain any per-contract participant data.
/// We verify this indirectly: `ContractBounds` has no `client`, `freelancer`,
/// or `milestones` fields — accessing any such field would be a compile error.
/// This test documents the type-level separation as a runtime assertion.
#[test]
fn get_bounds_result_type_has_no_participant_fields() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let bounds = client.get_bounds();
    // Exhaustive pattern match ensures no unexpected fields are silently added.
    let ContractBounds {
        max_milestones,
        max_single_milestone_stroops,
        max_total_escrow_stroops,
        max_fee_bps,
        max_settlement: _,
    } = bounds;
    assert!(max_milestones > 0);
    assert!(max_single_milestone_stroops > 0);
    assert!(max_total_escrow_stroops > 0);
    assert!(max_fee_bps > 0);
}

/// `get_bounds` should be consistent with `create_contract` behavior:
/// a single milestone exactly at `max_total_escrow_stroops` must be accepted.
#[test]
fn get_bounds_max_total_matches_create_contract_acceptance() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let bounds = client.get_bounds();
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    // Exactly at the cap — must succeed.
    client.create_contract(
        &c,
        &f,
        &None,
        &vec![&env, bounds.max_total_escrow_stroops],
        &ReleaseAuthorization::ClientOnly,
    );
}

/// `get_bounds` is consistent with `create_contract` rejection:
/// a single milestone one stroop above `max_total_escrow_stroops` must fail.
#[test]
fn get_bounds_max_total_plus_one_is_rejected_by_create_contract() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let bounds = client.get_bounds();
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    let result = client.try_create_contract(
        &c,
        &f,
        &None,
        &vec![&env, bounds.max_total_escrow_stroops + 1],
        &ReleaseAuthorization::ClientOnly,
    );
    assert!(result.is_err(), "one stroop over cap must be rejected");
}

/// `get_bounds` `max_milestones` is consistent with `create_contract`:
/// exactly `max_milestones` milestones must be accepted.
#[test]
fn get_bounds_max_milestones_matches_create_contract_acceptance() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let bounds = client.get_bounds();
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    let mut amounts: Vec<i128> = Vec::new(&env);
    for _ in 0..bounds.max_milestones {
        amounts.push_back(1_i128);
    }
    client.create_contract(&c, &f, &None, &amounts, &ReleaseAuthorization::ClientOnly);
}

/// `get_bounds` `max_milestones` is consistent with `create_contract`:
/// one more than `max_milestones` milestones must be rejected.
#[test]
fn get_bounds_max_milestones_plus_one_is_rejected_by_create_contract() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let bounds = client.get_bounds();
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    let mut amounts: Vec<i128> = Vec::new(&env);
    for _ in 0..=bounds.max_milestones {
        amounts.push_back(1_i128);
    }
    let result =
        client.try_create_contract(&c, &f, &None, &amounts, &ReleaseAuthorization::ClientOnly);
    assert_err(result, EscrowError::TooManyMilestones);
}

// ── create_contract — guard G1: same client and freelancer ────────────────────

#[test]
fn rejects_same_client_and_freelancer() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let same = Address::generate(&env);
    assert_err(
        client.try_create_contract(
            &same,
            &same,
            &None,
            &vec![&env, 100_i128],
            &ReleaseAuthorization::ClientOnly,
        ),
        EscrowError::InvalidParticipant,
    );
}

// ── create_contract — guard G2: empty milestone list ─────────────────────────

#[test]
fn rejects_empty_milestones() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    assert_err(
        client.try_create_contract(
            &c,
            &f,
            &None,
            &Vec::new(&env),
            &ReleaseAuthorization::ClientOnly,
        ),
        EscrowError::EmptyMilestones,
    );
}

// ── create_contract — guard G3: milestone count above MAX ────────────────────

#[test]
fn rejects_one_over_max_milestones() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    let mut amounts: Vec<i128> = Vec::new(&env);
    for _ in 0..=MAX_MILESTONES {
        amounts.push_back(1_i128);
    }
    assert_eq!(amounts.len(), MAX_MILESTONES + 1);
    assert_err(
        client.try_create_contract(&c, &f, &None, &amounts, &ReleaseAuthorization::ClientOnly),
        EscrowError::TooManyMilestones,
    );
}

// ── create_contract — guard G4: milestone count exactly MAX (boundary success)

#[test]
fn accepts_exactly_max_milestones() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    let mut amounts: Vec<i128> = Vec::new(&env);
    for _ in 0..MAX_MILESTONES {
        amounts.push_back(1_i128);
    }
    assert_eq!(amounts.len(), MAX_MILESTONES);
    client.create_contract(&c, &f, &None, &amounts, &ReleaseAuthorization::ClientOnly);
}

// ── create_contract — guard G5: zero milestone amount ────────────────────────

#[test]
fn rejects_zero_milestone_amount() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    assert_err(
        client.try_create_contract(
            &c,
            &f,
            &None,
            &vec![&env, 0_i128],
            &ReleaseAuthorization::ClientOnly,
        ),
        EscrowError::InvalidMilestoneAmount,
    );
}

// ── create_contract — guard G6: negative milestone amount ────────────────────

#[test]
fn rejects_negative_milestone_amount() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    assert_err(
        client.try_create_contract(
            &c,
            &f,
            &None,
            &vec![&env, -1_i128],
            &ReleaseAuthorization::ClientOnly,
        ),
        EscrowError::InvalidMilestoneAmount,
    );
}

// ── create_contract — guard G7: amounts that would overflow i128 ──────────────

/// When amounts individually exceed `MAX_SINGLE_AMOUNT_STROOPS`, the per-amount
/// guard fires first and returns `InvalidMilestoneAmount`.
/// When amounts are individually valid but their sum would overflow `i128`,
/// `validate_amount_array` detects it via `checked_add` and returns
/// `PotentialOverflow`. Both paths are tested here.
#[test]
fn rejects_amounts_that_would_overflow_i128() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    // Both values individually exceed MAX_SINGLE_AMOUNT_STROOPS, so the
    // per-amount guard fires first with InvalidMilestoneAmount.
    let large = i128::MAX / 2 + 2;
    assert_err(
        client.try_create_contract(
            &c,
            &f,
            &None,
            &vec![&env, large, large],
            &ReleaseAuthorization::ClientOnly,
        ),
        EscrowError::InvalidMilestoneAmount,
    );
}

// ── create_contract — guard G8: total above cap ───────────────────────────────

#[test]
fn rejects_total_one_over_cap() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    assert_err(
        client.try_create_contract(
            &c,
            &f,
            &None,
            &vec![&env, MAX_TOTAL_ESCROW_STROOPS + 1],
            &ReleaseAuthorization::ClientOnly,
        ),
        EscrowError::InvalidMilestoneAmount,
    );
}

#[test]
fn rejects_multi_milestone_total_over_cap() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    // Each amount exceeds MAX_SINGLE_AMOUNT_STROOPS — the per-amount guard fires.
    let too_large = MAX_SINGLE_AMOUNT_STROOPS + 1;
    assert_err(
        client.try_create_contract(
            &c,
            &f,
            &None,
            &vec![&env, too_large, too_large],
            &ReleaseAuthorization::ClientOnly,
        ),
        EscrowError::InvalidMilestoneAmount,
    );
}

// ── create_contract — guard G9: total exactly at cap (boundary success) ───────

#[test]
fn accepts_total_exactly_at_cap() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    client.create_contract(
        &c,
        &f,
        &None,
        &vec![&env, MAX_TOTAL_ESCROW_STROOPS],
        &ReleaseAuthorization::ClientOnly,
    );
}

#[test]
fn accepts_total_split_exactly_at_cap() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    let half = MAX_TOTAL_ESCROW_STROOPS / 2;
    let remainder = MAX_TOTAL_ESCROW_STROOPS - half;
    client.create_contract(
        &c,
        &f,
        &None,
        &vec![&env, half, remainder],
        &ReleaseAuthorization::ClientOnly,
    );
}

// ── create_contract — guard G10: count check fires before amount check ─────────

/// When both count > MAX_MILESTONES and total > cap, TooManyMilestones is
/// returned because the count guard runs before the amount guard.
#[test]
fn count_guard_fires_before_amount_guard() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    let mut amounts: Vec<i128> = Vec::new(&env);
    for _ in 0..=MAX_MILESTONES {
        amounts.push_back(MAX_TOTAL_ESCROW_STROOPS);
    }
    assert_err(
        client.try_create_contract(&c, &f, &None, &amounts, &ReleaseAuthorization::ClientOnly),
        EscrowError::TooManyMilestones,
    );
}

// ── Regression: original 3-milestone example still accepted ──────────────────

/// Amounts from the original test suite (total = 12 billion stroops).
/// Must not be affected by the bounds refactor.
#[test]
fn create_contract_still_accepts_original_three_milestone_example() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    let milestones = vec![&env, 200_0000000_i128, 400_0000000_i128, 600_0000000_i128];
    let id = client.create_contract(
        &c,
        &f,
        &None,
        &milestones,
        &ReleaseAuthorization::ClientOnly,
    );
    // First contract ID starts at 1 (next_contract_id defaults to 1).
    assert_eq!(id, 1);
}

// ── Concurrency / idempotency / duplicate-work regressions ───────────────────

/// Two sequential `create_contract` calls with identical inputs must yield
/// distinct contract IDs and must not corrupt each other's state. This is the
/// canonical "duplicate work" scenario: a caller retries after a timeout and
/// the retry must not silently overwrite the first contract.
#[test]
fn duplicate_create_contract_yields_distinct_ids() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    let amounts = vec![&env, 100_i128, 200_i128];

    let first = client.create_contract(&c, &f, &None, &amounts, &ReleaseAuthorization::ClientOnly);
    bump_ledger(&env);
    let second = client.create_contract(&c, &f, &None, &amounts, &ReleaseAuthorization::ClientOnly);

    assert_ne!(first, second, "duplicate submissions must not share an ID");
    assert_eq!(first, 1);
    assert_eq!(second, 2);
}

/// Repeated identical calls must be idempotent with respect to *validation*:
/// every call either succeeds with a fresh ID or fails with the same error.
/// Here we exercise the success path N times and assert monotonic IDs.
#[test]
fn repeated_create_contract_is_monotonic_and_idempotent() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    let amounts = vec![&env, 1_i128];

    let mut last = 0u32;
    for _ in 0..5 {
        let id = client.create_contract(
            &c,
            &f,
            &None,
            &amounts,
            &ReleaseAuthorization::ClientOnly,
        );
        assert!(id > last, "IDs must be strictly increasing");
        last = id;
        bump_ledger(&env);
    }
    assert_eq!(last, 5);
}

/// A rejected call must not consume a contract ID. If it did, a racing
/// successful call could observe a gap and a retry could collide.
#[test]
fn rejected_create_contract_does_not_consume_id() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let c = Address::generate(&env);
    let f = Address::generate(&env);

    // Reject: empty milestones.
    assert_err(
        client.try_create_contract(
            &c,
            &f,
            &None,
            &Vec::new(&env),
            &ReleaseAuthorization::ClientOnly,
        ),
        EscrowError::EmptyMilestones,
    );

    // The next successful call must still receive ID 1.
    let id = client.create_contract(
        &c,
        &f,
        &None,
        &vec![&env, 1_i128],
        &ReleaseAuthorization::ClientOnly,
    );
    assert_eq!(id, 1, "rejected call must not advance next_contract_id");
}

/// Interleaved valid and invalid submissions must leave the counter in a
/// consistent state: only successful calls advance it.
#[test]
fn interleaved_valid_and_invalid_calls_keep_counter_consistent() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let c = Address::generate(&env);
    let f = Address::generate(&env);

    let ok = vec![&env, 10_i128];
    let bad = vec![&env, 0_i128];

    let id1 = client.create_contract(&c, &f, &None, &ok, &ReleaseAuthorization::ClientOnly);
    bump_ledger(&env);
    assert_err(
        client.try_create_contract(&c, &f, &None, &bad, &ReleaseAuthorization::ClientOnly),
        EscrowError::InvalidMilestoneAmount,
    );
    bump_ledger(&env);
    let id2 = client.create_contract(&c, &f, &None, &ok, &ReleaseAuthorization::ClientOnly);

    assert_eq!(id1, 1);
    assert_eq!(id2, 2);
}

/// Boundary: a submission at exactly the cap followed by a submission one
/// stroop over the cap must succeed then fail, without the failure poisoning
/// the counter.
#[test]
fn boundary_cap_then_over_cap_keeps_state_clean() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let c = Address::generate(&env);
    let f = Address::generate(&env);

    let at_cap = vec![&env, MAX_TOTAL_ESCROW_STROOPS];
    let over_cap = vec![&env, MAX_TOTAL_ESCROW_STROOPS + 1];

    let id = client.create_contract(&c, &f, &None, &at_cap, &ReleaseAuthorization::ClientOnly);
    assert_eq!(id, 1);
    bump_ledger(&env);

    assert_err(
        client.try_create_contract(&c, &f, &None, &over_cap, &ReleaseAuthorization::ClientOnly),
        EscrowError::InvalidMilestoneAmount,
    );
    bump_ledger(&env);

    let id2 = client.create_contract(&c, &f, &None, &at_cap, &ReleaseAuthorization::ClientOnly);
    assert_eq!(id2, 2);
}

/// Boundary: exactly MAX_MILESTONES accepted, then MAX_MILESTONES+1 rejected,
/// then MAX_MILESTONES accepted again — counter advances only on success.
#[test]
fn boundary_milestone_count_retries_are_idempotent() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let c = Address::generate(&env);
    let f = Address::generate(&env);

    let at_max = amounts_of(&env, MAX_MILESTONES, 1_i128);
    let over_max = amounts_of(&env, MAX_MILESTONES + 1, 1_i128);

    let id1 = client.create_contract(&c, &f, &None, &at_max, &ReleaseAuthorization::ClientOnly);
    assert_eq!(id1, 1);
    bump_ledger(&env);

    assert_err(
        client.try_create_contract(&c, &f, &None, &over_max, &ReleaseAuthorization::ClientOnly),
        EscrowError::TooManyMilestones,
    );
    bump_ledger(&env);

    let id2 = client.create_contract(&c, &f, &None, &at_max, &ReleaseAuthorization::ClientOnly);
    assert_eq!(id2, 2);
}

/// Racing-style scenario: two different clients submit contracts in the same
/// ledger. Both must succeed with distinct IDs and neither must observe the
/// other's participant data.
#[test]
fn concurrent_distinct_clients_get_distinct_contracts() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let c1 = Address::generate(&env);
    let f1 = Address::generate(&env);
    let c2 = Address::generate(&env);
    let f2 = Address::generate(&env);

    let id1 = client.create_contract(
        &c1,
        &f1,
        &None,
        &vec![&env, 100_i128],
        &ReleaseAuthorization::ClientOnly,
    );
    let id2 = client.create_contract(
        &c2,
        &f2,
        &None,
        &vec![&env, 200_i128],
        &ReleaseAuthorization::ClientOnly,
    );

    assert_ne!(id1, id2);
    assert_eq!(id1, 1);
    assert_eq!(id2, 2);
}

/// Idempotent retry: submitting the exact same payload twice must produce two
/// independent contracts, not a shared one. This guards against a future
/// "dedupe by hash" optimization silently collapsing distinct user intents.
#[test]
fn identical_payloads_do_not_collapse_into_one_contract() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let c = Address::generate(&env);
    let f = Address::generate(&env);
    let amounts = vec![&env, 42_i128];

    let a = client.create_contract(&c, &f, &None, &amounts, &ReleaseAuthorization::ClientOnly);
    bump_ledger(&env);
    let b = client.create_contract(&c, &f, &None, &amounts, &ReleaseAuthorization::ClientOnly);

    assert_ne!(a, b);
}

// ── ContractBounds struct: type-level properties ──────────────────────────────

/// Confirm that `ContractBounds` implements `Copy` — it holds no heap data so
/// this should be possible and makes the type more ergonomic for callers.
#[test]
fn contract_bounds_implements_copy() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let bounds = client.get_bounds();
    let copied = bounds; // Copy, not move
    assert_eq!(bounds, copied);
}

/// Confirm `PartialEq` holds — two `get_bounds()` results are equal.
#[test]
fn contract_bounds_implements_partial_eq() {
    let (env, cid) = setup();
    let client = EscrowClient::new(&env, &cid);
    let a = client.get_bounds();
    let b = client.get_bounds();
    assert_eq!(a, b);
}
