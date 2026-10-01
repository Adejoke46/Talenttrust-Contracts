//! Shared validation boundaries for per-milestone refunds.
//!
//! The public refund entrypoint owns authorization, lifecycle, timeout, token,
//! event, and storage behavior. These helpers validate the request and escrow
//! accounting before that entrypoint transfers funds or mutates refund state.
//!
//! A successful request contains at least one unique, in-range milestone that
//! is neither released nor refunded, and its checked total does not exceed the
//! contract's checked available balance. The caller persists changes only
//! after the token transfer succeeds.

use crate::{checked_available_balance, Error, EscrowError, Milestone};
use soroban_sdk::{Env, Vec};

/// Reject an empty refund request and repeated milestone indices.
pub(crate) fn validate_refund_request(env: &Env, milestone_indices: &Vec<u32>) {
    if milestone_indices.is_empty() {
        env.panic_with_error(EscrowError::EmptyRefundRequest);
    }

    // Contract bounds cap milestone count, so this deterministic pairwise
    // check is small and avoids extra allocation in the Soroban host.
    for i in 0..milestone_indices.len() {
        for j in (i + 1)..milestone_indices.len() {
            if milestone_indices.get(i).unwrap() == milestone_indices.get(j).unwrap() {
                env.panic_with_error(EscrowError::DuplicateMilestoneInRefund);
            }
        }
    }
}

/// Validate every requested milestone and return their checked total.
///
/// Validation completes before callers perform token transfers or state writes.
pub(crate) fn validate_and_calculate_refund(
    env: &Env,
    milestones: &Vec<Milestone>,
    milestone_indices: &Vec<u32>,
) -> i128 {
    let mut total_refund_amount: i128 = 0;

    for idx in milestone_indices.iter() {
        if idx >= milestones.len() {
            env.panic_with_error(Error::IndexOutOfBounds);
        }

        let milestone = milestones.get(idx).unwrap();
        if milestone.released {
            env.panic_with_error(Error::AlreadyReleased);
        }
        if milestone.refunded {
            env.panic_with_error(EscrowError::AlreadyRefunded);
        }
        if milestone.amount <= 0 {
            env.panic_with_error(Error::AmountMustBePositive);
        }

        total_refund_amount = total_refund_amount
            .checked_add(milestone.amount)
            .unwrap_or_else(|| env.panic_with_error(EscrowError::PotentialOverflow));
    }

    total_refund_amount
}

/// Reject accounting corruption separately from an ordinary insufficient refund.
pub(crate) fn validate_refundable_balance(
    env: &Env,
    funded_amount: i128,
    released_amount: i128,
    refunded_amount: i128,
    requested_refund: i128,
) {
    let available = checked_available_balance(funded_amount, released_amount, refunded_amount)
        .unwrap_or_else(|error| env.panic_with_error(error));
    if available < requested_refund {
        env.panic_with_error(EscrowError::InsufficientFunds);
    }
}
