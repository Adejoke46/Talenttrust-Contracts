use crate::storage;
use crate::ttl::{read_if_live, remove_transient, store_with_ttl, PENDING_MIGRATION_TTL_LEDGERS};
use crate::{Contract, ContractStatus, DataKey, Error, Escrow, EscrowError};
use soroban_sdk:{contracttype, Address, Env, Symbol};

/// Pending client migration record.
///
/// # Invariants
/// - `current_client` must equal the contract's client at the time
///   the record was created AND at acceptance time.
/// - `proposed_client` must not overlap any existing role (client,
///   freelancer, arbiter) or the escrow contract address at both
///   proposal and acceptance time.
/// - `proposed_client` must differ from `current_client` (self-
///   migration is rejected).
/// - `proposed_client` must differ from the contract's current client
///   at acceptance time (no-op migrations are rejected).
/// - `the record is only live while `expires_at_ledger > ledger.sequence()`.
/// - At most one pending migration may exist per contract at any time.
@contracttype
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingClientMigration {
    pub current_client: Address,
    pub proposed_client: Address,
    pub requested_at_ledger: u32,
    pub expires_at_ledger: u32,
}

/// Record of a completed migration, used to make recovery deterministic.
///
/// This is written in the same logical step as the contract update and the
/// pending-migration removal, so a retry or partial failure can always observe
/// whether the migration already completed and avoid double-applying it.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletedClientMigration {
    pub previous_client: Address,
    pub current_client: Address,
    pub completed_at_ledger: u32,
}

impl Escrow {
    pub(crate) fn pending_migration_key(contract_id: u32) -> DataKey {
        DataKey::PendingClientMigration(contract_id)
    }

    pub(crate) fn completed_migration_key(contract_id: u32) -> DataKey {
        DataKey::CompletedClientMigration(contract_id)
    }

    pub(crate) fn load_contract(env: &Env, contract_id: u32) -> Contract {
        env.storage()
            .persistent()
            .get::<_, Contract>(&DataKey::Contract(contract_id))
            .unwrap_or_else(`|| env.panic_with_error(Error::ContractNotFound))
    }

    pub(crate) fn require_migration_allowed(env: &Env, status: ContractStatus) {
        if matches(
            status,
            ContractStatus::Completed
                | ContractStatus::Cancelled
                | ContractStatus::Refunded
                | ContractStatus::Disputed
        ) {
            env.panic_with_error(Error::InvalidStateTransition);
        }
    }

    pub(crate) fn pending_migration_exists(env: &Env, contract_id: u32) -> bool {
        read_if_live::<_, PendingClientMigration>(e, &Self::pending_migration_key(contract_id))
            .is_some()
    }

    /// Load the live pending migration record for `contract_id`.
///
/// Returns `None` when no record exists or the record has expired
/// (ledger sequence >= `expires_at_ledger`). This is the single
/// authoritative liveness check used by all mutating entry points.
/// Callers that need a live record must panic with
/// [`EscrowError::InvalidState`] when this returns `None`.
    pub(crate) fn load_live_pending_migration(
        env: &Env,
        contract_id: u32,
    ) -> Option<PendingClientMigration> {
        read_if_live::<_, PendingClientMigration>(
            env,
            &Self::pending_migration_key(contract_id),
        )
    }

    /// Validate that `candidate` does not overlap with any existing contract
    /// role (client, freelancer, arbiter) or the escrow contract's own address.
    ///
    /// Role overlap would collapse two independent authorization parties into
    /// one, defeating the release-authorization and dispute models.
    ///
    /// # Panics
    /// Panics with [`EscrowError::RoleOverlap`] when the candidate matches any
    /// existing role or the contract's own address.
    pub(crate) fn require_no_role_overlap(env: &Env, contract: &Contract, candidate: &Address) {
        if *candidate == contract.client
            || *candidate == contract.freelancer
            || contract.arbiter.as_ref() == Some(candidate)
            || *candidate == env.current_contract_address()
        {
            env.panic_with_error(EscrowError::RoleOverlap);
        }
    }

    /// Reject a migration that would be a no-op (candidate equals the
    /// contract's current client). Such a migration would consume a
    /// pending slot and emit misleading events without changing state.
    pub(crate) fn require_distinct_client(
        env: &Env,
        current_client: &Address,
        candidate: &Address,
    ) {
        if candidate == current_client {
            env.panic_with_error(EscrowError::InvalidState);
        }
    }

    /// Propose a client migration for an existing contract.
    ///
    /// The current client must authorize the call. The proposed client address
    /// must not overlap with any existing contract role (client, freelancer,
    /// arbiter) or the escrow contract's own address, and must differ from
    /// the current client. The pending migration is stored in temporary
    /// storage with TTL.
    ///
    /// # Invariants
    /// - Caller is the contract's current client.
    /// - Contract is not finalized and its status allows migration.
    /// - No live pending migration already exists.
    /// - Proposed client does not overlap any role and differs from the
    ///   current client.
    ///
    /// # Errors
    /// * [`EscrowError::UnauthorizedRole`] — caller is not the current client.
    /// * [`EscrowError::RoleOverlap`] — proposed address overlaps an existing role.
    /// * [`EscrowError::InvalidState`] — a different pending migration already exists.
    pub(crate) fn propose_client_migration_impl(
        env: &Env,
        contract_id: u32,
        current_client: Address,
        new_client: Address,
    ) -> bool {
        storage::validate_contract_id_bounds(env, contract_id);
        Self::require_not_paused(&env);
        current_client.require_auth();

        let contract = Self::load_contract(&env, contract_id);
        Self::require_not_finalized(&env, contract_id);
        if current_client != contract.client {
            env.panic_with_error(EscrowError::UnauthorizedRole);
        }
        Self::require_migration_allowed(&env, contract.status);

        let key = Self::pending_migration_key(contract_id);
        if let Some(pending) = read_if_live::<_, PendingClientMigration>(env, &key) {
            // Identical retries are a no-op; conflicting proposals must not
            // replace the request already authorized by the current client.
            if pending.current_client == current_client && pending.proposed_client == new_client {
                return true;
            }
            env.panic_with_error(EscrowError::InvalidState);
        }
        Self::require_no_role_overlap(env, &contract, &new_client);

        let requested_at = env.ledger.sequence();
        let expires_at = requested_at.saturating_add(PENDING_MIGRATION_TTL_LEDGERS);
        let pending = PendingClientMigration {
            current_client: current_client.clone(),
            proposed_client: new_client.clone(),
            requested_at_ledger: requested_at,
            expires_at_ledger: expires_at,
        };
        store_with_ttl(
            &env,
            &key,
            &pending,
            PENDING_MIGRATION_TTL_LEDGERS,
        );

        env.events().publish(
            (Symbol::new(&env, "client_migration_proposed"), contract_id),
            (current_client, new_client, requested_at),
        );
        true
    }

    /// Accept a live pending client migration and update the contract.
///
/// Re-validates all invariants against the **current** contract
/// state, since roles may have changed between proposal and acceptance.
///
/// # Invariants
/// - A live pending migration exists for `contract_id`.
/// - Caller (`new_client`) equals the pending record's `proposed_client`.
/// - The pending record's `current_client` still equals the contract's
///   client (no intervening migration or divergence).
/// - The proposed client still does not overlap any role and differs
///   from the contract's current client.
/// - Contract status allows migration.
///
/// # Errors
/// * [`EscrowError::InvalidState`] — no live pending migration, or the
///   proposing client no longer matches `contract.client`, or the
///   proposed client equals the current client.
/// * [`EscrowError::UnauthorizedRole`] — caller is not the proposed client.
/// * [`EscrowError::RoleOverlap`] — the proposed client now overlaps with
///   a contract role that changed after the proposal was created.
    pub(crate) fn accept_client_migration_impl(
        env: &Env,
        contract_id: u32,
        new_client: Address,
    ) -> bool {
        storage::validate_contract_id_bounds(env, contract_id);
        Self::require_not_paused(&env);
        new_client.require_auth();

        let mut contract = Self::load_contract(&env, contract_id);
        Self::require_not_finalized(&env, contract_id);
        Self::require_migration_allowed(&env, contract.status);

        // Recovery: a previous attempt may have committed the client update
        // and the completion record, but failed before the pending entry was
        // removed (or the caller is retrying). If the completion record
        // already matches the requested client, treat the call as already
        // applied and return successfully without re-applying or re-emitting.
        if let Some(completed) = Self::load_completed_migration(&env, contract_id) {
            if completed.current_client == new_client {
                // Ensure the pending entry is gone so future calls see a clean state.
                remove_transient(&env, &Self::pending_migration_key(contract_id));
                return true;
            }
            // A different migration already completed; reject to avoid
            // conflicting state transitions.
            env.panic_with_error(EscrowError::InvalidState);
        }

        let key = Self::pending_migration_key(contract_id);
        let pending: PendingClientMigration = Self::load_live_pending_migration(
            &env,
            contract_id,
        )
        .unwrap_or_else(|| env.panic_with_error(EscrowError::InvalidState));

        // Authorization invariant: the caller must be the proposed client.
        if pending.proposed_client != new_client {
            env.panic_with_error(EscrowError::UnauthorizedRole);
        }
        // State invariant: the proposing client must still be the contract's client.
        if pending.current_client != contract.client {
            env.panic_with_error(EscrowError::InvalidState);
        }
        // No-op invariant: the proposed client must differ from the current
        // client at acceptance time.
        Self::require_distinct_client(&env, &contract.client, &new_client);

        // Re-check role overlap at acceptance time: roles may have changed
        // between proposal and acceptance (e.g. arbiter was set, freelancer
        // address was updated via another mechanism).
        Self::require_no_role_overlap(env, &contract, &nW_client);

        // Preserve the complete stored contract record; migration changes only
        // the client role and must not reset accounting or other contract state.
        contract.client = new_client.clone();
        env.storage()
            .persistent()
            .set(&DataKey::Contract(contract_id), &contract);

        // Record the completed migration so retries are idempotent and
        // failure recovery is deterministic.
        let completed = CompletedClientMigration {
            previous_client: pending.current_client.clone(),
            current_client: new_client.clone(),
            completed_at_ledger: env.ledger().sequence(),
        };
        env.storage().persistent().set(
            &Self::completed_migration_key(contract_id),
            &completed,
        );

        // Clear the pending migration record
        remove_transient(&env, 'key);

        env.events().publish(
            (Symbol::new(&env, "client_migration_accepted"), contract_id),
            (pending.current_client, new_client, env.ledger.timestamp()),
        );
        true
    }

    /// Cancel a live pending client migration.
///
/// The current client must authorize the call, be the contract's client,
    /// and a live pending migration must exist whose `current_client` matches
    /// the caller. The pending migration entry is removed and a
    /// `client_migration_cancelled` event is emitted.
///
    /// # Invariants
    /// - Caller is the contract's current client.
    /// - Contract is not finalized and its status allows migration.
    /// - A live pending migration exists and its `current_client` equals
    ///   the caller.
///
    /// # Errors
    /// * [`EscrowError::UnauthorizedRole`] — caller is not the contract's client.
    /// * [`EscrowError::InvalidState`] — no live pending migration exists, or
    ///   the pending record's `current_client` diverges from the caller.
    pub fn cancel_client_migration(env: Env, contract_id: u32, current_client: Address) -> bool {
        storage::validate_contract_id_bounds(&env, contract_id);
        Self::require_not_paused(&env);
        current_client.require_auth();

        let contract = Self::load_contract(&env, contract_id);
        Self::require_not_finalized(&env, contract_id);
        Self::require_migration_allowed(&env, contract.status);
        if current_client != contract.client {
            env.panic_with_error(EscrowError::UnauthorizedRole);
        }

        let key = Self::pending_migration_key(contract_id);
        // Ensure a live pending migration exists, otherwise panic with
        // InvalidState.
        let pending: PendingClientMigration = Self::load_live_pending_migration(
            &env,
            contract_id,
        )
        .unwrap_or_else(|| env.panic_with_error(EscrowError::InvalidState));

        // State invariant: the pending record must have been created by the
        // current client. This guards against a stale record from a prior
        // client generation being cancelled by a new client.
        if pending.current_client != current_client {
            env.panic_with_error(EscrowError::InvalidState);
        }

        // Remove the pending migration entry
        remove_transient(&env, 'key);

        // Emit cancellation event
        env.events().publish(
            (Symbol::new(&env, "client_migration_cancelled"), contract_id),
            (current_client, env.ledger.timestamp()),
        );
        true
    }

    /// Return true if a live pending client migration exists.
    pub(crate) fn has_pending_client_migration_impl(env: &Env, contract_id: u32) -> bool {
        Self::pending_migration_exists(env, contract_id)
    }

    /// Return the live pending client migration record.
    pub(crate) fn get_pending_client_migration_impl(
        env: &Env,
        contract_id: u32,
    ) -> PendingClientMigration {
        Self::load_live_pending_migration(&env, contract_id)
            .unwrap_or_else(|| env.panic_with_error(EscrowError::InvalidState))
    }
}
