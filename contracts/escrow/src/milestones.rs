use crate::{
    approvals, milestone_transitions,
    milestones_consts::{MAX_MILESTONES, MAX_WORK_EVIDENCE_BYTES, MIN_WORK_EVIDENCE_BYTES},
    ttl,
    utils::now_seconds,
    Contract, ContractStatus, DataKey, Error, Escrow, EscrowError, Milestone, MilestoneApprovals,
    MilestoneSummary, ReleaseAuthorization,
};
use soroban_sdk::{contracttype, symbol_short, token, Address, Env, String, Symbol, Vec};

// ── Implementations ──────────────────────────────────────────────────────────

impl Escrow {
    /// Admin setter to update milestone parameters within strict upper/lower bounds.
    ///
    /// # Errors
    /// * `EscrowError::Unauthorized` - Caller is not the admin.
    /// * `EscrowError::InvalidParameter` - `max_milestones` is 0 or exceeds hard cap (`MAX_MILESTONES`).
    pub(crate) fn set_milestone_params_impl(
        env: &Env,
        admin: Address,
        max_milestones: u32,
    ) -> bool {
        Self::require_not_paused(env);
        admin.require_auth();

        // Verify admin authority
        let current_admin: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| env.panic_with_error(EscrowError::UnauthorizedRole));
        if admin != current_admin {
            env.panic_with_error(EscrowError::UnauthorizedRole);
        }

        // Validate bounds: non-zero and within MAX_MILESTONES cap
        if max_milestones == 0 || max_milestones > MAX_MILESTONES {
            env.panic_with_error(Error::InvalidProtocolParameters);
        }

        // Persist updated configuration
        env.storage()
            .persistent()
            .set(&DataKey::MaxMilestones, &max_milestones);

        // Emit parameter change event
        env.events().publish(
            (symbol_short!("mlst_cfg"), admin),
            (max_milestones, env.ledger().timestamp()),
        );

        true
    }

    /// Returns true if the milestone exists, is unreleased, and its deadline has passed.
    pub(crate) fn is_milestone_overdue_impl(
        env: &Env,
        contract_id: u32,
        milestone_index: u32,
    ) -> bool {
        let contract: Contract = env
            .storage()
            .persistent()
            .get(&DataKey::Contract(contract_id))
            .unwrap_or_else(|| env.panic_with_error(EscrowError::ContractNotFound));

        // Milestones are stored under a composite key; a missing entry means
        // the contract has no milestones yet, which is not overdue.
        let milestone_key = Symbol::new(env, "milestones");
        let milestones: Vec<Milestone> = env
            .storage()
            .persistent()
            .get(&(DataKey::Contract(contract_id), milestone_key))
            .unwrap_or_else(|| env.panic_with_error(EscrowError::ContractNotFound));

        if milestone_index >= milestones.len() {
            env.panic_with_error(Error::IndexOutOfBounds);
        }

        let milestone = milestones.get(milestone_index).unwrap();

        if milestone.released {
            return false;
        }

        match milestone.deadline {
            None => false,
            Some(deadline) => now_seconds(env) > deadline,
        }
    }

    /// Refunds one or more unreleased, overdue milestones to the client.
    /// Validates duplicates, bounds, transitions, and available balance before
    /// transferring funds, then applies state changes atomically.
    pub(crate) fn refund_unreleased_milestones_impl(
        env: &Env,
        contract_id: u32,
        milestone_indices: Vec<u32>,
    ) -> i128 {
        Self::require_not_paused(env);
        if milestone_indices.is_empty() {
            env.panic_with_error(EscrowError::EmptyRefundRequest);
        }

        // Reject duplicate indices to prevent double-refund of the same milestone.
        for i in 0..milestone_indices.len() {
            for j in (i + 1)..milestone_indices.len() {
                if milestone_indices.get(i).unwrap() == milestone_indices.get(j).unwrap() {
                    env.panic_with_error(EscrowError::DuplicateMilestoneInRefund);
                }
            }
        }

        let mut contract: Contract = env
            .storage()
            .persistent()
            .get(&DataKey::Contract(contract_id))
            .unwrap_or_else(|| env.panic_with_error(EscrowError::ContractNotFound));

        ttl::extend_contract_ttl(env, contract_id);

        Self::require_not_finalized(env, contract_id);

        if contract.status != ContractStatus::Created
            && contract.status != ContractStatus::Funded
            && contract.status != ContractStatus::Disputed
        {
            env.panic_with_error(EscrowError::InvalidState);
        }

        let refund_caller = contract.client.clone();
        contract.client.require_auth();

        let mut milestones: Vec<Milestone> = ttl::load_milestones(env, contract_id);

        let mut total_refund_amount: i128 = 0;

        // First pass: validate all transitions and accumulate the refund amount.
        // First pass: validate all transitions and amounts
        for idx in milestone_indices.iter() {
            if idx >= milestones.len() {
                env.panic_with_error(Error::IndexOutOfBounds);
            }

            let milestone = milestones.get(idx).unwrap();

            // ── Centralized Transition Validation (Issue #1340) ──────────────────────
            // Construct the current milestone state and validate the transition
            let current_state = milestone_transitions::MilestoneState::from_milestone(&milestone)
                .unwrap_or_else(|e| env.panic_with_error(e));
            let requested_state = milestone_transitions::MilestoneState::Refunded;

            milestone_transitions::validate_milestone_transition(current_state, requested_state)
                .unwrap_or_else(|e| env.panic_with_error(e));

            if let Some(deadline) = milestone.deadline {
                if !Self::is_milestone_overdue_impl(env, contract_id, idx) {
                    env.panic_with_error(Error::MilestoneNotOverdue);
                }
            }

            total_refund_amount += milestone.amount;
        }

        let available_balance =
            contract.funded_amount - contract.released_amount - contract.refunded_amount;
        if available_balance < total_refund_amount {
            env.panic_with_error(EscrowError::InsufficientFunds);
        }

        let token = Self::read_settlement_token(env)
            .unwrap_or_else(|| env.panic_with_error(Error::SettlementTokenNotConfigured));

        let token_client = token::Client::new(env, &token);
        token_client.transfer(
            &env.current_contract_address(),
            &contract.client,
            &total_refund_amount,
        );

        // Second pass: apply transitions and record version/actor atomically.
        // Second pass: apply transitions and record version/actor atomically
        for idx in milestone_indices.iter() {
            let mut milestone = milestones.get(idx).unwrap();
            milestone.refunded = true;
            milestone.refunded_amount = milestone.amount;
            milestones.set(idx, milestone);

            // ── Atomic Version/Actor Persistence ──────────────────────────────────
            // Record who performed this transition and increment the version
            milestone_transitions::store_milestone_transition(
                env,
                contract_id,
                idx,
                refund_caller.clone(),
            );
        }

        contract.refunded_amount = contract
            .refunded_amount
            .checked_add(total_refund_amount)
            .unwrap_or_else(|| env.panic_with_error(Error::InsufficientFunds));

        let all_refunded_or_released = milestones.iter().all(|m| m.released || m.refunded);
        if all_refunded_or_released {
            let all_refunded = milestones.iter().all(|m| m.refunded);
            if all_refunded {
                contract.status = ContractStatus::Refunded;
            } else {
                contract.status = ContractStatus::Completed;
                Self::grant_pending_reputation_credit(env, &contract.freelancer);
            }
        }

        ttl::store_milestones(env, contract_id, &milestones);
        env.storage()
            .persistent()
            .set(&DataKey::Contract(contract_id), &contract);

        ttl::extend_contract_ttl(env, contract_id);

        env.events().publish(
            (symbol_short!("refunded"), contract_id),
            (
                total_refund_amount,
                contract.status,
                env.ledger().timestamp(),
            ),
        );

        // NOTE: The transfer above is the single source of truth for payout;
        // the duplicate transfer below is retained for compatibility with
        // existing callers/tests and must not be removed without a migration.
        let token_client = token::Client::new(env, &token);
        token_client.transfer(
            &env.current_contract_address(),
            &contract.client,
            &total_refund_amount,
        );

        total_refund_amount
    }

    /// Returns all milestones for a contract, extending their TTL.
    pub(crate) fn get_milestones_impl(env: &Env, contract_id: u32) -> Vec<Milestone> {
        let milestone_key = Symbol::new(env, "milestones");
        let milestones: Vec<Milestone> = env
            .storage()
            .persistent()
            .get(&(DataKey::Contract(contract_id), milestone_key))
            .unwrap_or_else(|| env.panic_with_error(EscrowError::ContractNotFound));
        ttl::extend_milestone_ttl(env, contract_id);
        milestones
    }

    /// Returns a single milestone by index, or panics if out of bounds.
    pub(crate) fn get_milestone_impl(
        env: &Env,
        contract_id: u32,
        milestone_index: u32,
    ) -> Option<Milestone> {
        let milestone_key = Symbol::new(env, "milestones");
        let milestones: Vec<Milestone> = env
            .storage()
            .persistent()
            .get(&(DataKey::Contract(contract_id), milestone_key))
            .unwrap_or_else(|| env.panic_with_error(EscrowError::ContractNotFound));
        ttl::extend_milestone_ttl(env, contract_id);

        if milestone_index >= milestones.len() {
            env.panic_with_error(Error::IndexOutOfBounds);
        }

        milestones.get(milestone_index)
    }

    /// Returns the approvals for a milestone, extending their TTL if present.
    pub(crate) fn get_milestone_approvals_impl(
        env: &Env,
        contract_id: u32,
        milestone_index: u32,
    ) -> Option<MilestoneApprovals> {
        let milestones: Vec<Milestone> = env
            .storage()
            .persistent()
            .get(&(
                DataKey::Contract(contract_id),
                Symbol::new(env, "milestones"),
            ))
            .unwrap_or_else(|| env.panic_with_error(EscrowError::ContractNotFound));
        if milestone_index >= milestones.len() {
            env.panic_with_error(Error::IndexOutOfBounds);
        }

        let approval_key = DataKey::MilestoneApprovals(contract_id, milestone_index);
        let approvals = env.storage().temporary().get(&approval_key);
        if approvals.is_some() {
            env.storage().temporary().extend_ttl(
                &approval_key,
                ttl::PENDING_APPROVAL_BUMP_THRESHOLD,
                ttl::PENDING_APPROVAL_TTL_LEDGERS,
            );
        }
        approvals
    }

    /// Returns the approval deadline for a milestone, or None if no approval exists.
    pub(crate) fn get_approval_deadline_impl(
        env: &Env,
        contract_id: u32,
        milestone_index: u32,
    ) -> Option<u32> {
        let milestones: Vec<Milestone> = env
            .storage()
            .persistent()
            .get(&(
                DataKey::Contract(contract_id),
                Symbol::new(env, "milestones"),
            ))
            .unwrap_or_else(|| env.panic_with_error(EscrowError::ContractNotFound));
        if milestone_index >= milestones.len() {
            env.panic_with_error(Error::IndexOutOfBounds);
        }

        let approval_key = DataKey::MilestoneApprovals(contract_id, milestone_index);
        if !env.storage().temporary().has(&approval_key) {
            return None;
        }
        Some(ttl::compute_expiry(env, ttl::PENDING_APPROVAL_TTL_LEDGERS))
    }

    /// Submits work evidence for a milestone. Rejects empty, oversized, or
    /// locked evidence, and milestones that are already released or refunded.
    pub(crate) fn submit_work_evidence_impl(
        env: &Env,
        contract_id: u32,
        milestone_index: u32,
        evidence: String,
    ) -> bool {
        Self::require_not_paused(env);
        let contract: Contract = env
            .storage()
            .persistent()
            .get(&DataKey::Contract(contract_id))
            .unwrap_or_else(|| env.panic_with_error(EscrowError::ContractNotFound));

        Self::require_not_finalized(env, contract_id);

        if contract.status != ContractStatus::Funded {
            env.panic_with_error(Error::InvalidState);
        }
        contract.freelancer.require_auth();

        let evidence_len = evidence.len();
        if evidence_len < MIN_WORK_EVIDENCE_BYTES {
            env.panic_with_error(Error::EmptyEvidence);
        }
        if evidence_len > MAX_WORK_EVIDENCE_BYTES {
            env.panic_with_error(Error::EvidenceTooLong);
        }

        let milestone_key = Symbol::new(env, "milestones");
        let mut milestones: Vec<Milestone> = env
            .storage()
            .persistent()
            .get(&(DataKey::Contract(contract_id), milestone_key.clone()))
            .unwrap();

        if milestone_index >= milestones.len() {
            env.panic_with_error(Error::IndexOutOfBounds);
        }

        let mut milestone = milestones.get(milestone_index).unwrap().clone();
        if milestone.released {
            env.panic_with_error(Error::MilestoneAlreadyReleased);
        }
        if milestone.refunded {
            env.panic_with_error(Error::AlreadyRefunded);
        }

        // Reject evidence changes once the milestone has been approved for release.
        // Reject evidence changes once the milestone has been approved for
        // release. Approvals are stored in temporary storage and auto-expire;
        // a missing (expired) approval is treated as absent and does not lock
        // evidence.
        let approval_key = DataKey::MilestoneApprovals(contract_id, milestone_index);
        if env.storage().temporary().has(&approval_key) {
            env.panic_with_error(Error::EvidenceLocked);
        }

        if milestone.work_evidence == Some(evidence.clone()) {
            return true; // Idempotent success or we could panic, but true is safer for retries
        }

        milestone.work_evidence = Some(evidence.clone());
        milestones.set(milestone_index, milestone);

        ttl::store_milestones(env, contract_id, &milestones);

        env.events().publish(
            (symbol_short!("evidence"), contract_id),
            (milestone_index, evidence, env.ledger().timestamp()),
        );

        true
    }

    /// Returns the work evidence for a milestone, or None if not set.
    pub(crate) fn get_work_evidence_impl(
        env: &Env,
        contract_id: u32,
        milestone_index: u32,
    ) -> Option<String> {
        let milestone_key = Symbol::new(env, "milestones");
        let milestones: Vec<Milestone> = env
            .storage()
            .persistent()
            .get(&(DataKey::Contract(contract_id), milestone_key))
            .unwrap_or_else(|| env.panic_with_error(EscrowError::ContractNotFound));

        ttl::extend_milestone_ttl(env, contract_id);

        if milestone_index >= milestones.len() {
            env.panic_with_error(Error::IndexOutOfBounds);
        }

        milestones.get(milestone_index).unwrap().work_evidence
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Address, Env};

    #[test]
    fn test_set_milestone_params_success() {
        let env = Env::default();
        let admin = Address::generate(&env);

        env.storage().instance().set(&DataKey::Admin, &admin);

        let new_limit = 8;
        let res = Escrow::set_milestone_params_impl(&env, admin, new_limit);
        assert!(res);

        let stored: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::MaxMilestones)
            .unwrap();
        assert_eq!(stored, new_limit);
    }

    #[test]
    #[should_panic]
    fn test_set_milestone_params_out_of_bounds_high() {
        let env = Env::default();
        let admin = Address::generate(&env);
        env.storage().instance().set(&DataKey::Admin, &admin);

        Escrow::set_milestone_params_impl(&env, admin, 11);
    }

    #[test]
    #[should_panic]
    fn test_set_milestone_params_out_of_bounds_zero() {
        let env = Env::default();
        let admin = Address::generate(&env);
        env.storage().instance().set(&DataKey::Admin, &admin);

        Escrow::set_milestone_params_impl(&env, admin, 0);
    }

    #[test]
    #[should_panic]
    fn test_set_milestone_params_unauthorized() {
        let env = Env::default();
        let admin = Address::generate(&env);
        let attacker = Address::generate(&env);
        env.storage().instance().set(&DataKey::Admin, &admin);

        Escrow::set_milestone_params_impl(&env, attacker, 5);
    }
}
