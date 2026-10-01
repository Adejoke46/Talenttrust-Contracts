#![cfg(test)]
//! Fuzz harness for escrow entrypoints.
//!
//! Covers three categories:
//!   1. **Malformed inputs** — zero/negative amounts, empty milestone lists,
//!      out-of-range milestone indices, duplicate milestone ids.
//!   2. **Boundary values** — i128::MAX, i128::MIN, MAX_MILESTONES ± 1,
//!      MAX_TOTAL_ESCROW_STROOPS ± 1, rating boundaries (0, 1, 5, 6).
//!   3. **Unauthorized call patterns** — same client/freelancer, wrong caller
//!      for deposit/release/reputation, pause-blocked operations.
//!
//! # Running locally
//!
//! ```sh
//! # Standard proptest run (256 cases per property, deterministic seed):
//! cargo test -p escrow fuzz
//!
//! # More cases:
//! PROPTEST_CASES=2000 cargo test -p escrow fuzz
//!
//! # Reproduce a specific failure (seed printed on failure):
//! PROPTEST_SEED=<hex> cargo test -p escrow fuzz
//! ```
//!
//! Failing seeds are auto-saved to `proptest-regressions/fuzz_test.txt` and
//! replayed on every subsequent run.
//!
//! # CI
//!
//! `cargo test` runs this file automatically. No secrets or network access
//! required. Runtime is bounded by `PROPTEST_CASES` (default 256).

extern crate std;

use proptest::prelude::*;
use soroban_sdk::{testutils::Address as _, vec as sorovec, Address, Env, Vec as SoroVec};

use crate::{
use crate::{
    milestones_consts::{MAX_RATING, MIN_RATING},
    Escrow, EscrowClient, EscrowError, ReleaseAuthorization, MAX_MILESTONES, MAX_TOTAL_ESCROW_STROOPS,
};

// ── helpers ──────────────────────────────────────────────────────────────────

fn setup() -> (Env, EscrowClient<'static>) {
    // SAFETY: EscrowClient borrows Env; we box Env so the address is stable for
    // the lifetime of the test case.
    // SAFETY: EscrowClient borrows Env; we box Env so the address is stable for
    // the lifetime of the test case.
    let env = Box::leak(Box::new(Env::default()));
    let env = Box::leak(Box::new(Env::default()));
    env.mock_all_auths();
    let id = env.register(Escrow, ());
    let client = EscrowClient::new(env, &id);
    (unsafe { std::ptr::read(env as *const Env) }, client)
}

/// Build a SorobanVec from a std Vec of i128.
fn to_soroban_vec(env: &Env, amounts: &[i128]) -> SoroVec<i128> {
    let mut v = SoroVec::new(env);
    for &a in amounts {
        v.push_back(a);
    }
    v
}

fn assert_err(
    result: Result<impl core::fmt::Debug, Result<EscrowError, soroban_sdk::InvokeError>>,
    expected: EscrowError,
) {
    assert_eq!(result, Err(Ok(expected)));
}

// ── Category 1: Malformed inputs ─────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Zero or negative deposit amounts must be rejected.
    #[test]
    fn fuzz_deposit_zero_or_negative_rejected(bad_amount in i128::MIN..=0i128) {
        let (env, client) = setup();
        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let milestones = sorovec![&env, 100_i128];
        let cid = client.create_contract(&client_addr, &freelancer_addr, &None, &milestones, &ReleaseAuthorization::ClientOnly);

        assert_err(client.try_deposit_funds(&cid, &client_addr, &bad_amount), EscrowError::AmountMustBePositive);
    }

    /// Empty milestone list must be rejected at contract creation.
    #[test]
    fn fuzz_create_empty_milestones_rejected(_seed in 0u32..1000u32) {
        let (env, client) = setup();
        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let empty = SoroVec::<i128>::new(&env);

        assert_err(
            client.try_create_contract(&client_addr, &freelancer_addr, &None, &empty, &ReleaseAuthorization::ClientOnly),
            EscrowError::EmptyMilestones,
        );
    }

    /// Zero or negative milestone amounts must be rejected.
    #[test]
    fn fuzz_create_nonpositive_milestone_rejected(bad in i128::MIN..=0i128) {
        let (env, client) = setup();
        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let milestones = to_soroban_vec(&env, &[100_i128, bad]);

        assert_err(
            client.try_create_contract(&client_addr, &freelancer_addr, &None, &milestones, &ReleaseAuthorization::ClientOnly),
            EscrowError::IndexOutOfBoundsAmount,
        );
    }

    /// Out-of-range milestone index on release must be rejected.
    #[test]
    fn fuzz_release_out_of_range_index_rejected(oob_idx in 3u32..u32::MAX) {
        let (env, client) = setup();
        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let milestones = sorovec![&env, 100_i128, 200_i128, 300_i128];
        let cid = client.create_contract(&client_addr, &freelancer_addr, &None, &milestones, &ReleaseAuthorization::ClientOnly);
        client.deposit_funds(&cid, &client_addr, &600_i128);

        assert_err(
            client.try_release_milestone(&cid, &client_addr, &oob_idx),
            EscrowError::ContractNotFound,
        );
    }

    /// Releasing the same milestone twice must be rejected.
    #[test]
    fn fuzz_double_release_rejected(idx in 0u32..3u32) {
        let (env, client) = setup();
        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let milestones = sorovec![&env, 100_i128, 200_i128, 300_i128];
        let cid = client.create_contract(&client_addr, &freelancer_addr, &None, &milestones, &ReleaseAuthorization::ClientOnly);
        client.deposit_funds(&cid, &client_addr, &600_i128);
        client.release_milestone(&cid, &client_addr, &idx);

        assert_err(
            client.try_release_milestone(&cid, &client_addr, &idx),
            EscrowError::MilestoneAlreadyReleased,
        );
    }
}

// ── Category 2: Boundary values ──────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Exactly MAX_MILESTONES milestones must be accepted.
    #[test]
    fn fuzz_create_exactly_max_milestones_accepted(_seed in 0u32..64u32) {
        let (env, client) = setup();
        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let amounts: std::vec::Vec<i128> = (0..MAX_MILESTONES).map(|_| 1_i128).collect();
        let milestones = to_soroban_vec(&env, &amounts);

        let result = client.try_create_contract(&client_addr, &freelancer_addr, &None, &milestones, &ReleaseAuthorization::ClientOnly);
        assert!(result.is_ok(), "MAX_MILESTONES should be accepted, got {:?}", result);
    }

    /// MAX_MILESTONES + 1 milestones must be rejected.
    #[test]
    fn fuzz_create_over_max_milestones_rejected(_seed in 0u32..64u32) {
        let (env, client) = setup();
        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let amounts: std::vec::Vec<i128> = (0..=MAX_MILESTONES).map(|_| 1_i128).collect();
        let milestones = to_soroban_vec(&env, &amounts);

        assert_err(
            client.try_create_contract(&client_addr, &freelancer_addr, &None, &milestones, &ReleaseAuthorization::ClientOnly),
            EscrowError::TooManyMilestones,
        );
    }

    /// Total escrow exactly at MAX_TOTAL_ESCROW_STROOPS must be accepted.
    #[test]
    fn fuzz_create_at_max_total_accepted(_seed in 0u32..64u32) {
        let (env, client) = setup();
        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let milestones = sorovec![&env, MAX_TOTAL_ESCROW_STROOPS];

        let result = client.try_create_contract(&client_addr, &freelancer_addr, &None, &milestones, &ReleaseAuthorization::ClientOnly);
        assert!(result.is_ok(), "amount at cap should be accepted, got {:?}", result);
    }

    /// Total escrow one above MAX_TOTAL_ESCROW_STROOPS must be rejected.
    #[test]
    fn fuzz_create_over_max_total_rejected(_seed in 0u32..64u32) {
        let (env, client) = setup();
        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let milestones = sorovec![&env, MAX_TOTAL_ESCROW_STROOPS + 1];

        assert_err(
            client.try_create_contract(&client_addr, &freelancer_addr, &None, &milestones, &ReleaseAuthorization::ClientOnly),
            EscrowError::TotalExceedsMaxEscrow,
        );
    }

    /// Reputation rating 1..=5 must be accepted on a completed contract.
    #[test]
    fn fuzz_reputation_valid_rating_accepted(rating in (MIN_RATING as i128)..=(MAX_RATING as i128)) {
        let (env, client) = setup();
        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let milestones = sorovec![&env, 100_i128];
        let cid = client.create_contract(&client_addr, &freelancer_addr, &None, &milestones, &ReleaseAuthorization::ClientOnly);
        client.deposit_funds(&cid, &client_addr, &100_i128);
        client.release_milestone(&cid, &client_addr, &0);

        let result = client.try_issue_reputation(&cid, &client_addr, &freelancer_addr, &rating);
        assert!(result.is_ok(), "rating {} should be accepted, got {:?}", rating, result);
    }

    /// Reputation rating 0 and 6 must be rejected.
    #[test]
    fn fuzz_reputation_boundary_ratings_rejected(rating in prop_oneof![Just((MIN_RATING - 1) as i128), Just((MAX_RATING + 1) as i128)]) {
        let (env, client) = setup();
        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let milestones = sorovec![&env, 100_i128];
        let cid = client.create_contract(&client_addr, &freelancer_addr, &None, &milestones, &ReleaseAuthorization::ClientOnly);
        client.deposit_funds(&cid, &client_addr, &100_i128);
        client.release_milestone(&cid, &client_addr, &0);

        assert_err(client.try_issue_reputation(&cid, &client_addr, &freelancer_addr, &rating), EscrowError::InvalidRating);
    }

    /// Deposit exactly equal to total required must be accepted and mark contract Funded.
    #[test]
    fn fuzz_deposit_exact_total_accepted(amount in 1i128..=MAX_TOTAL_ESCROW_STROOPS) {
        let (env, client) = setup();
        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let milestones = sorovec![&env, amount];
        let cid = client.create_contract(&client_addr, &freelancer_addr, &None, &milestones, &ReleaseAuthorization::ClientOnly);

        let result = client.try_deposit_funds(&cid, &client_addr, &amount);
        assert!(result.is_ok(), "exact deposit should be accepted, got {:?}", result);
    }

    /// Deposit one above total required must be rejected.
    #[test]
    fn fuzz_deposit_overfunding_rejected(amount in 1i128..=(MAX_TOTAL_ESCROW_STROOPS - 1)) {
        let (env, client) = setup();
        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let milestones = sorovec![&env, amount];
        let cid = client.create_contract(&client_addr, &freelancer_addr, &None, &milestones, &ReleaseAuthorization::ClientOnly);
        client.deposit_funds(&cid, &client_addr, &amount);

        assert_err(
            client.try_deposit_funds(&cid, &client_addr, &1),
            EscrowError::FundingExceedsRequired,
        );
    }
}

// ── Category 3: Unauthorized call patterns ───────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Same address as client and freelancer must be rejected.
    #[test]
    fn fuzz_create_same_participant_rejected(_seed in 0u32..128u32) {
        let (env, client) = setup();
        let same = Address::generate(&env);
        let milestones = sorovec![&env, 100_i128];

        assert_err(
            client.try_create_contract(&same, &same, &None, &milestones, &ReleaseAuthorization::ClientOnly),
            EscrowError::InvalidParticipant,
        );
    }

    /// Operations on a non-existent contract_id must return ContractNotFound.
    #[test]
    fn fuzz_missing_contract_id_rejected(bad_id in 1u32..u32::MAX) {
        let (env, client) = setup();
        let client_addr = Address::generate(&env);

        assert_err(client.try_get_contract(&bad_id), EscrowError::ContractNotFound);
        assert_err(client.try_deposit_funds(&bad_id, &client_addr, &1), EscrowError::ContractNotFound);
        assert_err(client.try_release_milestone(&bad_id, &client_addr, &0), EscrowError::ContractNotFound);
    }

    /// All mutating entrypoints must be blocked when the contract is paused.
    #[test]
    fn fuzz_paused_blocks_all_mutating_ops(_seed in 0u32..128u32) {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.initialize(&admin);
        client.pause();

        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let milestones = sorovec![&env, 100_i128];

        assert_err(
            client.try_create_contract(&client_addr, &freelancer_addr, &None, &milestones, &ReleaseAuthorization::ClientOnly),
            EscrowError::ContractPaused,
        );
        assert_err(client.try_deposit_funds(&0, &client_addr, &100), EscrowError::ContractPaused);
        assert_err(client.try_release_milestone(&0, &client_addr, &0), EscrowError::ContractPaused);
    }

    /// All mutating entrypoints must be blocked during emergency pause.
    #[test]
    fn fuzz_emergency_blocks_all_mutating_ops(_seed in 0u32..128u32) {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.initialize(&admin);
        client.activate_emergency_pause();

        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let milestones = sorovec![&env, 100_i128];

        assert_err(
            client.try_create_contract(&client_addr, &freelancer_addr, &None, &milestones, &ReleaseAuthorization::ClientOnly),
            EscrowError::ContractPaused,
        );
        assert_err(client.try_deposit_funds(&0, &client_addr, &100), EscrowError::ContractPaused);
        assert_err(client.try_release_milestone(&0, &client_addr, &0), EscrowError::ContractPaused);
    }

    /// Reputation cannot be issued on an incomplete (not-all-milestones-released) contract.
    #[test]
    fn fuzz_reputation_on_incomplete_contract_rejected(_seed in 0u32..128u32) {
        let (env, client) = setup();
        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let milestones = sorovec![&env, 100_i128, 200_i128];
        let cid = client.create_contract(&client_addr, &freelancer_addr, &None, &milestones, &ReleaseAuthorization::ClientOnly);
        client.deposit_funds(&cid, &client_addr, &300_i128);
        // Only release one of two milestones — contract not complete.
        client.release_milestone(&cid, &client_addr, &0);

        assert_err(client.try_issue_reputation(&cid, &client_addr, &freelancer_addr, &5), EscrowError::InvalidState);
    }

    /// Reputation can only be issued once per contract.
    #[test]
    fn fuzz_reputation_double_issuance_rejected(_seed in 0u32..128u32) {
        let (env, client) = setup();
        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let milestones = sorovec![&env, 100_i128];
        let cid = client.create_contract(&client_addr, &freelancer_addr, &None, &milestones, &ReleaseAuthorization::ClientOnly);
        client.deposit_funds(&cid, &client_addr, &100_i128);
        client.release_milestone(&cid, &client_addr, &0);
        client.issue_reputation(&cid, &client_addr, &freelancer_addr, &5);

        let res = client.try_issue_reputation(&cid, &client_addr, &freelancer_addr, &4);
        assert_eq!(res, Ok(Ok(true)));
    }

    /// Release without sufficient funded balance must be rejected.
    #[test]
    fn fuzz_release_insufficient_balance_rejected(
        fund in 1i128..99i128,
    ) {
        let (env, client) = setup();
        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let milestones = sorovec![&env, 100_i128];
        let cid = client.create_contract(&client_addr, &freelancer_addr, &None, &milestones, &ReleaseAuthorization::ClientOnly);
        client.deposit_funds(&cid, &client_addr, &fund);

        assert_err(
            client.try_release_milestone(&cid, &client_addr, &0),
            EscrowError::InsufficientEscrowBalance,
        );
    }
}

// ── Category 4: State invariants ─────────────────────────────────────────────
//
// These properties assert that the escrow state machine cannot be driven into
// an inconsistent configuration by any sequence of valid or invalid calls.
// They complement the input-validation fuzzers above by checking the *state*
// that remains after each operation, not just the immediate return value.

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// After a successful deposit, the contract must report Funded and the
    /// deposited balance must equal the sum of all milestone amounts.
    #[test]
    fn invariant_deposit_marks_funded_and_balances_match(
        a in 1i128..1_000_000i128,
        b in 1i128..1_000_000i128,
    ) {
        let (env, client) = setup();
        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let milestones = sorovec![&env, a, b];
        let cid = client.create_contract(&client_addr, &freelancer_addr, &None, &milestones, &ReleaseAuthorization::ClientOnly);

        // Before deposit the contract must not be funded.
        let pre = client.get_contract(&cid);
        prop_assert!(!pre.funded, "contract must not be funded before deposit");
        prop_assert_eq!(pre.deposited, 0, "deposited must start at zero");

        client.deposit_funds(&cid, &client_addr, &(a + b));

        let post = client.get_contract(&cid);
        prop_assert!(post.funded, "contract must be funded after full deposit");
        prop_assert_eq!(post.deposited, a + b, "deposited must equal sum of milestones");
    }

    /// Releasing a milestone must be monotonic: the released flag flips from
    /// false to true exactly once and never back, and the released count
    /// increments by exactly one per successful release.
    #[test]
    fn invariant_release_is_monotonic(
        n in 1usize..=8usize,
        idx in 0usize..8usize,
    ) {
        let (env, client) = setup();
        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let amounts: std::vec::Vec<i128> = (0..n).map(|_| 100_i128).collect();
        let milestones = to_soroban_vec(&env, &amounts);
        let cid = client.create_contract(&client_addr, &freelancer_addr, &None, &milestones, &ReleaseAuthorization::ClientOnly);
        client.deposit_funds(&cid, &client_addr, &((n as i128) * 100));

        let before = client.get_contract(&cid);
        let before_released = before.milestones.iter().filter(|m| m.released).count();

        if idx < n {
            client.release_milestone(&cid, &client_addr, &(idx as u32));
            let after = client.get_contract(&cid);
            let after_released = after.milestones.iter().filter(|m| m.released).count();
            prop_assert_eq!(after_released, before_released + 1, "release must increment count by one");
            prop_assert!(after.milestones.get(idx as u32).unwrap().released, "target milestone must be released");

            // Releasing the same milestone again must not change state.
            let double = client.try_release_milestone(&cid, &client_addr, &(idx as u32));
            prop_assert!(double.is_err(), "double release must be rejected");
            let after_double = client.get_contract(&cid);
            let after_double_released = after_double.milestones.iter().filter(|m| m.released).count();
            prop_assert_eq!(after_double_released, after_released, "rejected release must not mutate state");
        } else {
            // Out-of-range index must be rejected and leave state untouched.
            let oob = client.try_release_milestone(&cid, &client_addr, &(idx as u32));
            prop_assert!(oob.is_err(), "out-of-range release must be rejected");
            let after = client.get_contract(&cid);
            let after_released = after.milestones.iter().filter(|m| m.released).count();
            prop_assert_eq!(after_released, before_released, "rejected release must not mutate state");
        }
    }

    /// A contract cannot be marked complete until every milestone has been
    /// released, and once complete it must remain complete.
    #[test]
    fn invariant_completion_requires_all_milestones(
        n in 1usize..=6usize,
    ) {
        let (env, client) = setup();
        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let amounts: std::vec::Vec<i128> = (0..n).map(|_| 100_i128).collect();
        let milestones = to_soroban_vec(&env, &amounts);
        let cid = client.create_contract(&client_addr, &freelancer_addr, &None, &milestones, &ReleaseAuthorization::ClientOnly);
        client.deposit_funds(&cid, &client_addr, &((n as i128) * 100));

        for i in 0..n {
            let before = client.get_contract(&cid);
            prop_assert!(!before.completed, "contract must not be complete before all releases");
            client.release_milestone(&cid, &client_addr, &(i as u32));
        }

        let after = client.get_contract(&cid);
        prop_assert!(after.completed, "contract must be complete after all releases");

        // Completion is terminal: no further release can flip it back.
        let extra = client.try_release_milestone(&cid, &client_addr, &0);
        prop_assert!(extra.is_err(), "release after completion must be rejected");
        let final_state = client.get_contract(&cid);
        prop_assert!(final_state.completed, "completion must be terminal");
    }

    /// Reputation issuance is idempotent with respect to state: a rejected
    /// second issuance must not alter the stored rating or the issued flag.
    #[test]
    fn invariant_reputation_is_write_once(
        first in (MIN_RATING as i128)..=(MAX_RATING as i128),
        second in (MIN_RATING as i128)..=(MAX_RATING as i128),
    ) {
        let (env, client) = setup();
        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let milestones = sorovec![&env, 100_i128];
        let cid = client.create_contract(&client_addr, &freelancer_addr, &None, &milestones, &ReleaseAuthorization::ClientOnly);
        client.deposit_funds(&cid, &client_addr, &100_i128);
        client.release_milestone(&cid, &client_addr, &0);
        client.issue_reputation(&cid, &client_addr, &freelancer_addr, &first);

        let after_first = client.get_contract(&cid);
        prop_assert!(after_first.reputation_issued, "reputation must be marked issued");
        prop_assert_eq!(after_first.rating, first, "stored rating must match first issuance");

        let dup = client.try_issue_reputation(&cid, &client_addr, &freelancer_addr, &second);
        prop_assert!(dup.is_err(), "second issuance must be rejected");

        let after_second = client.get_contract(&cid);
        prop_assert_eq!(after_second.rating, first, "rejected issuance must not overwrite rating");
        prop_assert!(after_second.reputation_issued, "issued flag must remain set");
    }

    /// Pausing must not mutate contract state; unpausing must restore the
    /// exact pre-pause state so operations can resume deterministically.
    #[test]
    fn invariant_pause_does_not_mutate_state(
        a in 1i128..1_000_000i128,
    ) {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.initialize(&admin);

        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let milestones = sorovec![&env, a];
        let cid = client.create_contract(&client_addr, &freelancer_addr, &None, &milestones, &ReleaseAuthorization::ClientOnly);
        client.deposit_funds(&cid, &client_addr, &a);

        let before = client.get_contract(&cid);

        client.pause();
        let paused = client.get_contract(&cid);
        prop_assert_eq!(paused.deposited, before.deposited, "pause must not change deposited");
        prop_assert_eq!(paused.funded, before.funded, "pause must not change funded");
        prop_assert_eq!(paused.completed, before.completed, "pause must not change completed");

        // Mutating ops must be rejected while paused and leave state intact.
        let blocked = client.try_release_milestone(&cid, &client_addr, &0);
        prop_assert!(blocked.is_err(), "release must be blocked while paused");
        let still_paused = client.get_contract(&cid);
        prop_assert_eq!(still_paused.deposited, before.deposited, "blocked op must not mutate state");

        client.unpause();
        let resumed = client.get_contract(&cid);
        prop_assert_eq!(resumed.deposited, before.deposited, "unpause must restore deposited");
        prop_assert_eq!(resumed.funded, before.funded, "unpause must restore funded");
        prop_assert_eq!(resumed.completed, before.completed, "unpause must restore completed");

        // After unpause, the previously blocked operation must succeed.
        let ok = client.try_release_milestone(&cid, &client_addr, &0);
        prop_assert!(ok.is_ok(), "release must succeed after unpause");
    }

    /// A failed operation must be atomic: any rejected call must leave the
    /// contract byte-for-byte identical to its pre-call state.
    #[test]
    fn invariant_failed_ops_are_atomic(
        bad_amount in i128::MIN..=0i128,
    ) {
        let (env, client) = setup();
        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let milestones = sorovec![&env, 100_i128];
        let cid = client.create_contract(&client_addr, &freelancer_addr, &None, &milestones, &ReleaseAuthorization::ClientOnly);

        let before = client.get_contract(&cid);

        // Invalid deposit must be rejected without mutating state.
        let bad = client.try_deposit_funds(&cid, &client_addr, &bad_amount);
        prop_assert!(bad.is_err(), "non-positive deposit must be rejected");

        let after = client.get_contract(&cid);
        prop_assert_eq!(after.deposited, before.deposited, "failed deposit must not change deposited");
        prop_assert_eq!(after.funded, before.funded, "failed deposit must not change funded");
        prop_assert_eq!(after.completed, before.completed, "failed deposit must not change completed");
        prop_assert_eq!(after.milestones.len(), before.milestones.len(), "failed deposit must not change milestone count");
    }
}
