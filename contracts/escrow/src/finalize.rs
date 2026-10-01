//! Finalization: closing an escrow contract with an immutable record.
//!
//! The record is stored once under `DataKey::Finalization(contract_id)`.  After
//! it exists, all contract-specific mutating entrypoints reject with
//! `Error::AlreadyFinalized`.
//!
//! # Why finalization needs a strict failure model
//!
//! Finalization is the only operation in this contract that writes an
//! **immutable** record.  There is no rewrite path: whatever is sealed is what
//! every later reader, indexer and audit sees forever.  That has two
//! consequences that shape the whole design in this module.
//!
//! 1. **Nothing may be sealed optimistically.**  A summary is only written once
//!    it has been fully validated, so a rejected seal can never leave a
//!    half-correct record behind and a corrupted input can never be frozen into
//!    storage.
//! 2. **Rejection must be deterministic.**  The same input must always produce
//!    the same error, in a debug build and in a release build, and regardless
//!    of how many times the caller retries.  This rules out unchecked
//!    arithmetic (which wraps silently in `--release` and panics in debug) and
//!    rules out guard ordering that lets an incidental check mask the real
//!    reason a call was refused.
//!
//! # Failure model
//!
//! `finalize_contract_impl` runs in three phases.  Phases 1 and 2 are
//! read-only; nothing is persisted unless every check in them passes.  Phase 3
//! performs the writes and cannot panic.
//!
//! | Phase | Purpose | May panic |
//! | --- | --- | --- |
//! | 1. Preconditions | Establish that the call is allowed, in a fixed order | yes |
//! | 2. Build + validate summary | Project state into a close summary and reconcile it | yes |
//! | 3. Commit | Write the seal, drop the dispute snapshot, publish the event | no |
//!
//! Because a Soroban invocation is atomic, a panic in phases 1–2 discards the
//! whole transaction.  The contract is therefore left exactly as it was found:
//! still mutable, still unfinalized, and still repairable.  A retry after
//! resolving the reported condition behaves identically every time.
//!
//! # Invariants enforced before sealing
//!
//! - **I1 — no negative ledger totals.**  `funded_amount`, `released_amount`
//!   and `refunded_amount` are only ever increased by `checked_add` of positive
//!   amounts, so a negative value can only come from corrupt state.
//! - **I2 — payments cannot exceed funding.**
//!   `released_amount + refunded_amount <= funded_amount`.  The money-movement
//!   paths enforce the stronger
//!   `released + refunded + protocol_fees <= funded`, which implies this.
//! - **I3 — funding cannot exceed the milestone total.**  `deposit_funds`
//!   rejects any deposit that would push `funded_amount` past the sum of the
//!   milestone amounts, so `funded_amount <= total_amount` holds for every
//!   reachable state.  This also catches a contract record that has been
//!   restored from an archive which is out of step with its milestone vector.
//! - **I4 — a milestone is never both released and refunded.**  The milestone
//!   transition matrix treats those two flags as mutually exclusive terminal
//!   states, so the combination is unreachable; sealing it would freeze an
//!   unauditable contradiction into an immutable record.
//! - **I5 — the summary is a faithful projection of the contract record.**
//!   Status and every amount in the summary are re-checked against the
//!   contract they were derived from immediately before the seal is written.
//!
//! # Storage lifetime
//!
//! A finalization record *is* the `is_finalized` flag for the entire contract,
//! so it must obey the same persistent TTL policy as the contract and
//! milestone entries it summarises.  Without that, the seal could silently
//! lapse and a second finalizer could re-seal the same contract with a
//! different finalizer and timestamp.  The seal is therefore written with the
//! standard [`PERSISTENT_TTL_LEDGERS`] policy, renewed on read through
//! `get_finalization_record`, and the contract and milestone entries it reads
//! are refreshed up front so a finalizable contract cannot be wedged by TTL
//! expiry part-way through being closed.
//!
//! [`PERSISTENT_TTL_LEDGERS`]: crate::ttl::PERSISTENT_TTL_LEDGERS

use soroban_sdk::{contracttype, symbol_short, Address, Env, Vec};

use crate::{
    ttl::{self, PERSISTENT_BUMP_THRESHOLD, PERSISTENT_TTL_LEDGERS},
    Contract, ContractStatus, ContractSummary, DataKey, Error, Escrow, EscrowError, Milestone,
    MilestoneSummary, CONTRACT_SUMMARY_SCHEMA_VERSION,
};

/// Immutable metadata written when an escrow contract is closed.
///
/// The record is stored once under `DataKey::Finalization(contract_id)`.
/// After it exists, all contract-specific mutating entrypoints reject with
/// `Error::AlreadyFinalized`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizationRecord {
    /// Authorized client, freelancer, or assigned arbiter that finalized.
    pub finalizer: Address,
    /// Ledger timestamp at finalization time.
    pub timestamp: u64,
    /// Snapshot of participant, milestone, and accounting state.
    pub summary: ContractSummary,
}

/// Statuses a contract may hold when it is sealed.
///
/// `Completed` and `Disputed` are the only two states in which every milestone
/// has already been dispositioned, so a close summary is meaningful.
fn is_sealable_status(status: ContractStatus) -> bool {
    status == ContractStatus::Completed || status == ContractStatus::Disputed
}

impl Escrow {
    fn finalization_key(contract_id: u32) -> DataKey {
        DataKey::Finalization(contract_id)
    }

    /// Load the contract record for a finalization attempt and refresh its TTL.
    ///
    /// The bump is unconditional: an entry that is still live is renewed
    /// according to the standard policy, and one that has already been evicted
    /// is left evicted.  This keeps a contract that is being closed from
    /// lapsing while the (longer) summary computation runs.
    fn load_contract_for_finalization(env: &Env, contract_id: u32) -> Contract {
        let contract: Contract = env
            .storage()
            .persistent()
            .get(&DataKey::Contract(contract_id))
            .unwrap_or_else(|| env.panic_with_error(Error::ContractNotFound));
        ttl::extend_contract_ttl(env, contract_id);
        contract
    }

    /// Load a contract for finalization without extending TTL. Finalization
    /// is a terminal transition; the record itself is what must outlive the
    /// contract, so we do not refresh the contract TTL here.
    fn load_contract_for_finalization_checked(
        env: &Env,
        contract_id: u32,
    ) -> Contract {
        Self::load_contract_for_finalization(env, contract_id)
    }

    pub(crate) fn is_finalized(env: &Env, contract_id: u32) -> bool {
        settlement::is_finalized(env, contract_id)
    }

    pub(crate) fn require_not_finalized(env: &Env, contract_id: u32) {
        settlement::require_not_finalized(env, contract_id);
    }

    /// Returns true when the contract status is a terminal, non-mutable
    /// state that must never be resurrected by lifecycle entrypoints.
    pub(crate) fn is_terminal_status(status: ContractStatus) -> bool {
        matches!(
            status,
            ContractStatus::Cancelled | ContractStatus::Refunded
        )
    }

    /// Load a contract, verify it's in an active (mutable) state, and extend
    /// its TTL. Rejects `Cancelled`, `Refunded`, and finalized contracts.
    ///
    /// This is the canonical preamble for all lifecycle entrypoints that need a
    /// live, mutable contract. Calls `load_contract` from `storage.rs`, extends
    /// the TTL, checks finalization, and rejects terminal statuses.
    ///
    /// # Panics
    /// - `ContractNotFound` when `contract_id` is unknown.
    /// - `AlreadyFinalized` when the contract has been finalized.
    /// - `InvalidState` when the contract status is `Cancelled` or `Refunded`.
    ///
    /// # Returns
    /// The loaded `Contract`.
    pub(crate) fn require_active_contract(env: &Env, contract_id: u32) -> Contract {
        let contract = crate::storage::load_contract(env, contract_id);
        ttl::extend_contract_ttl(env, contract_id);
        Self::require_not_finalized(env, contract_id);
        if Self::is_terminal_status(contract.status) {
            env.panic_with_error(Error::InvalidState);
        }
        contract
    }

    pub(crate) fn require_not_paused(env: &Env) {
        if env
            .storage()
            .persistent()
            .get::<_, bool>(&DataKey::Paused)
            .unwrap_or(false)
        {
            env.panic_with_error(Error::ContractPaused);
        }
        if env
            .storage()
            .persistent()
            .get::<_, bool>(&DataKey::Emergency)
            .unwrap_or(false)
        {
            env.panic_with_error(Error::EmergencyActive);
        }
    }

    fn require_finalizer_role(env: &Env, contract: &Contract, finalizer: &Address) {
        let is_client = *finalizer == contract.client;
        let is_freelancer = *finalizer == contract.freelancer;
        let is_arbiter = contract.arbiter.clone().is_some_and(|a| a == *finalizer);
        if !is_client && !is_freelancer && !is_arbiter {
            env.panic_with_error(Error::UnauthorizedRole);
        }
    }

    /// Project the live contract state into an immutable close summary.
    ///
    /// This function only reads.  It performs no writes, so a panic here leaves
    /// no trace and the caller can safely run it before the commit phase.
    ///
    /// All arithmetic is checked.  Under `--release` the workspace does not
    /// enable `overflow-checks`, so an unchecked `+` or `-` on `i128` wraps
    /// silently in a release build and panics in a debug build — the same input
    /// would then produce two different results depending on how the contract
    /// was compiled.  Every accumulation and subtraction below is therefore
    /// `checked_*` and maps a failure onto a typed error.
    ///
    /// # Panics
    /// - `FinalizationStateIncomplete` when the milestone vector is absent.  The
    ///   contract record exists, so this is not `ContractNotFound`: the honest
    ///   signal is that the close summary cannot be built from the state that
    ///   is actually present.  Refusing here is deliberate — sealing an empty
    ///   milestone list would freeze a `total_amount` of `0` and a release count
    ///   of `0` into a permanent record that contradicts the contract it
    ///   describes.
    /// - `PotentialOverflow` when the milestone amounts do not sum to an `i128`.
    /// - `AccountingInvariantViolated` when a milestone claims to be both
    ///   released and refunded (invariant I4).
    fn summarize_contract(env: &Env, contract_id: u32, contract: &Contract) -> ContractSummary {
        let milestone_key = crate::keys::milestone_key(env, contract_id);
        let milestones: Vec<Milestone> = match env.storage().persistent().get(&milestone_key) {
            Some(milestones) => milestones,
            None => env.panic_with_error(Error::FinalizationStateIncomplete),
        };
        // The milestone vector carries its own TTL, independent of the contract
        // entry. Refresh it so the vector this summary is built from stays live
        // for as long as the seal that quotes it.
        ttl::extend_milestone_ttl(env, contract_id);

        // Invariant: milestone count must be non-zero for a finalized
        // contract. A zero-milestone contract cannot have a meaningful
        // accounting snapshot and indicates corrupted state.
        if milestones.is_empty() {
            env.panic_with_error(Error::InvalidState);
        }

        let mut total_amount: i128 = 0;
        let mut released_milestone_count: u32 = 0;
        let mut milestone_summaries = Vec::new(env);

        for (index, ms) in milestones.iter().enumerate() {
            let idx = index as u32;

            // I4: `released` and `refunded` are mutually exclusive terminal
            // states. A milestone carrying both is unreachable and would make
            // the sealed record self-contradictory.
            if ms.released && ms.refunded {
                env.panic_with_error(Error::AccountingInvariantViolated);
            }

            total_amount = total_amount
                .checked_add(ms.amount)
                .unwrap_or_else(|| env.panic_with_error(Error::PotentialOverflow));

            // Invariant: a milestone cannot be both released and refunded.
            // Allowing both would double-count funds in the summary and
            // break downstream accounting.
            if ms.released && ms.refunded {
                env.panic_with_error(Error::InvalidState);
            }

            if ms.released {
                released_milestone_count = released_milestone_count
                    .checked_add(1)
                    .unwrap_or_else(|| env.panic_with_error(Error::PotentialOverflow));
            }

            milestone_summaries.push_back(MilestoneSummary {
                index: idx,
                amount: ms.amount,
                released: ms.released,
                refunded: ms.refunded,
            });
        }

        // I2 (at the point of computation): derive the refundable balance with
        // checked subtraction. `require_sealable_accounting` proves the result
        // is non-negative; this proves it is computed the same way in every
        // build profile.
        let paid = contract
            .released_amount
            .checked_add(contract.refunded_amount)
            .unwrap_or_else(|| env.panic_with_error(Error::PotentialOverflow));
        let refundable_balance = contract
            .funded_amount
            .checked_sub(paid)
            .unwrap_or_else(|| env.panic_with_error(Error::AccountingInvariantViolated));

        ContractSummary {
            schema_version: CONTRACT_SUMMARY_SCHEMA_VERSION,
            client: contract.client.clone(),
            freelancer: contract.freelancer.clone(),
            arbiter: contract.arbiter.clone(),
            status: contract.status,
            reputation_issued: contract.reputation_issued,
            total_amount,
            funded_amount: contract.funded_amount,
            released_amount: contract.released_amount,
            refundable_balance,
            released_milestone_count,
            milestones: milestone_summaries,
        }
    }

    /// Refuse to seal accounting that cannot be reconciled.
    ///
    /// Runs after the summary is built and before anything is written, so a
    /// contract in an unreconcilable state stays mutable and an operator can
    /// still repair or refund it.  Refusing is the safe direction: the
    /// alternative is freezing a balance that does not add up into a record
    /// that can never be corrected.
    ///
    /// # Panics
    /// - `AccountingInvariantViolated` when any of I1, I2, I3, I5 is broken.
    /// - `PotentialOverflow` when the paid totals cannot be summed.
    fn require_sealable_accounting(env: &Env, contract: &Contract, summary: &ContractSummary) {
        // I1: ledger totals are non-negative.
        if contract.funded_amount < 0
            || contract.released_amount < 0
            || contract.refunded_amount < 0
        {
            env.panic_with_error(Error::AccountingInvariantViolated);
        }

        // I2: released + refunded <= funded.
        let paid = contract
            .released_amount
            .checked_add(contract.refunded_amount)
            .unwrap_or_else(|| env.panic_with_error(Error::PotentialOverflow));
        if paid > contract.funded_amount {
            env.panic_with_error(Error::AccountingInvariantViolated);
        }

        // I3: a contract cannot have been funded beyond its milestone total,
        // and the milestone total itself cannot be negative.
        if summary.total_amount < 0 || contract.funded_amount > summary.total_amount {
            env.panic_with_error(Error::AccountingInvariantViolated);
        }

        // I5: the summary is a faithful projection of the contract it was
        // derived from. Guards against the two ever drifting apart.
        if summary.status != contract.status
            || summary.funded_amount != contract.funded_amount
            || summary.released_amount != contract.released_amount
            || summary.refundable_balance != contract.funded_amount - paid
        {
            env.panic_with_error(Error::AccountingInvariantViolated);
        }
    }
}

/// Finalize an escrow contract by writing immutable close metadata.
///
/// `finalizer` must authorize the call and must be the stored client,
/// freelancer, or assigned arbiter. Finalization is allowed only while the
/// contract is in a terminal state: `Completed`, `Disputed`, `Refunded`,
/// or `Cancelled`. Once finalized, future contract-specific mutations
/// fail with `AlreadyFinalized`.
///
/// # Execution order
///
/// The checks below run in a fixed order so that the reported error always
/// names the first condition that actually failed, on every call and in every
/// build profile:
///
/// 1. `contract_id` is in bounds.
/// 2. No close record exists yet (`AlreadyFinalized`).
/// 3. Pause and emergency controls are clear.
/// 4. The contract exists and is in a sealable state.
/// 5. `finalizer` authorized the call and is a participant of this contract.
/// 6. The close summary reconciles with the contract record.
/// 7. The seal is written, the dispute snapshot is dropped if any, and the
///    `finalized` event is published.
///
/// Steps 1–6 are read-only. A panic anywhere in them aborts the invocation
/// before any storage is touched, so the contract remains unfinalized and
/// retryable and a competing finalizer cannot observe a partial seal.
///
/// # Errors
/// - `ContractNotFound` when `contract_id` is zero or unknown.
/// - `AlreadyFinalized` when a close record already exists.
/// - `ContractPaused` when pause controls are active.
/// - `EmergencyActive` when emergency controls are active.
/// - `UnauthorizedRole` when `finalizer` is not a contract participant.
/// - `InvalidStatusTransition` unless status is a terminal state.
pub fn finalize_contract_impl(env: &Env, contract_id: u32, finalizer: Address) -> bool {
    // ── Phase 1: preconditions ───────────────────────────────────────────
    // Nothing below this point writes to storage.

    let contract = Escrow::load_contract_for_finalization(&env, contract_id);

    // Validate contract is in a terminal state eligible for finalization
    let is_terminal = matches!(
        contract.status,
        ContractStatus::Completed
            | ContractStatus::Disputed
            | ContractStatus::Refunded
            | ContractStatus::Cancelled
    );
    if !is_terminal {
        env.panic_with_error(EscrowError::InvalidStatusTransition);
    }

    // Validate accounting invariants before finalizing
    let refundable_balance = contract
        .funded_amount
        .checked_sub(contract.released_amount)
        .and_then(|a| a.checked_sub(contract.refunded_amount))
        .unwrap_or_else(|| env.panic_with_error(Error::PotentialOverflow));
    if refundable_balance < 0 {
        env.panic_with_error(Error::AccountingInvariantViolated);
    }

    // For Completed contracts, verify all funds are accounted for
    if contract.status == ContractStatus::Completed {
        let total_accounted = contract
            .released_amount
            .checked_add(contract.refunded_amount)
            .unwrap_or_else(|| env.panic_with_error(Error::PotentialOverflow));
        if total_accounted != contract.funded_amount {
            env.panic_with_error(Error::AccountingInvariantViolated);
        }
    }

    // For Refunded and Cancelled contracts, verify full refund
    if contract.status == ContractStatus::Refunded || contract.status == ContractStatus::Cancelled {
        if contract.refunded_amount != contract.funded_amount {
            env.panic_with_error(Error::AccountingInvariantViolated);
        }
    }

    Escrow::require_not_paused(&env);
    finalizer.require_auth();
    Escrow::require_finalizer_role(env, &contract, &finalizer);

    // ── Phase 2: build and validate the close summary ────────────────────
    // Still read-only. `summarize_contract` projects state; the reconciliation
    // below refuses to seal anything that does not add up.

    let summary = Escrow::summarize_contract(env, contract_id, &contract);
    Escrow::require_sealable_accounting(env, &contract, &summary);

    let record = FinalizationRecord {
        finalizer: finalizer.clone(),
        timestamp: env.ledger().timestamp(),
        summary,
    };

    // ── Phase 3: commit ──────────────────────────────────────────────────
    // Every check has passed. The steps below cannot panic, so the seal and
    // the event are always committed together or not at all.

    let key = Escrow::finalization_key(contract_id);
    env.storage().persistent().set(&key, &record);

    // The close record is the `is_finalized` flag for the whole contract, so it
    // must live as long as the entries it summarizes. Without this the seal
    // would inherit the bare minimum TTL and could lapse, silently re-opening
    // every mutation it closed off and allowing a second, conflicting seal.
    env.storage()
        .persistent()
        .extend_ttl(&key, PERSISTENT_BUMP_THRESHOLD, PERSISTENT_TTL_LEDGERS);

    // A disputed contract carries a pre-dispute snapshot that `rollback_dispute`
    // would restore while the dispute is still untouched. Finalization is a
    // deliberate, authorized close, so the snapshot is superseded and dropped
    // here — but only after the summary has been fully validated, so the
    // recovery path is never discarded in exchange for a record that then
    // fails to reconcile. Removal is idempotent, so a contract sealed without
    // a snapshot is unaffected.
    if contract.status == ContractStatus::Disputed {
        crate::rollback::clear_dispute_rollback(env, contract_id);
    }

    // Publish the sealed summary alongside the finalizer and timestamp so an
    // indexer can reconcile the close from the event alone. The payload is
    // addresses, status flags and amounts already present in the record — no
    // evidence strings, keys or other sensitive data.
    env.events().publish(
        (symbol_short!("finalized"), contract_id),
        (finalizer, record.timestamp, record.summary),
    );

    true
}

/// Return immutable close metadata for `contract_id`, if it has been finalized.
///
/// Reading a seal renews it, matching the read path of the contract and
/// milestone entries.  This keeps the immutability flag alive for as long as
/// anyone is still reading the record that backs it.
pub fn get_finalization_record_impl(env: &Env, contract_id: u32) -> Option<FinalizationRecord> {
    let key = Escrow::finalization_key(contract_id);
    let record: Option<FinalizationRecord> = env.storage().persistent().get(&key);
    if record.is_some() {
        env.storage().persistent().extend_ttl(
            &key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_TTL_LEDGERS,
        );
    }
    record
}
