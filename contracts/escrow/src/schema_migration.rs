//! Storage Schema Versioning and Migration Engine for Escrow.
//!
//! Provides a safe, versioned, admin-guarded upgrade path for escrow contract storage.
//!
//! ## Invariants
//! - Layout versions are monotonically increasing (1 -> 2 -> ...).
//! - Upgrades are in-place, atomic, and idempotent.
//! - Downgrades or jumps beyond known versions are strictly rejected with typed errors.
//! - Admin authentication is required for all schema mutations.
//! - Emits `escrow_schema_migrated` event on successful version transition.

use crate::ttl::{PERSISTENT_BUMP_THRESHOLD, PERSISTENT_TTL_LEDGERS};
use crate::types::{DataKey, Error};
use crate::Escrow;
use soroban_sdk::{Address, Env, Symbol};

/// Baseline storage schema version for fresh deployments.
pub const INITIAL_STORAGE_SCHEMA_VERSION: u32 = 1;

/// Highest supported storage schema version implemented by this WASM build.
pub const CURRENT_STORAGE_SCHEMA_VERSION: u32 = 2;

impl Escrow {
    /// Read the current on-ledger storage schema version.
    ///
    /// If no schema version is stored (legacy state), returns `INITIAL_STORAGE_SCHEMA_VERSION` (1).
    pub(crate) fn get_schema_version_impl(env: &Env) -> u32 {
        match env
            .storage()
            .persistent()
            .get::<_, u32>(&DataKey::SchemaVersion)
        {
            Some(version) => {
                env.storage().persistent().extend_ttl(
                    &DataKey::SchemaVersion,
                    PERSISTENT_BUMP_THRESHOLD,
                    PERSISTENT_TTL_LEDGERS,
                );
                version
            }
            // A legacy deployment has no marker to extend. Keep this read
            // side-effect free; the first successful migration creates it.
            None => INITIAL_STORAGE_SCHEMA_VERSION,
        }
    }

    /// Internal setter for the storage schema version with persistent TTL bump.
    pub(crate) fn set_schema_version_impl(env: &Env, version: u32) {
        env.storage()
            .persistent()
            .set(&DataKey::SchemaVersion, &version);

        env.storage().persistent().extend_ttl(
            &DataKey::SchemaVersion,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_TTL_LEDGERS,
        );
    }

    /// Execute storage schema upgrade from current version to `target_version`.
    ///
    /// # Access Control
    /// - Requires admin signature (`admin.require_auth()`).
    /// - Caller must match stored contract admin.
    ///
    /// # Error Semantics
    /// - `Error::InvalidMigrationVersion`: `target_version` is 0, exceeds current WASM support, or attempts a downgrade.
    pub(crate) fn migrate_escrow_storage_impl(
        env: &Env,
        admin: Address,
        target_version: u32,
    ) -> Result<u32, Error> {
        Self::require_initialized(env);
        let stored_admin: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| env.panic_with_error(Error::NotInitialized));

        admin.require_auth();
        if admin != stored_admin {
            return Err(Error::UnauthorizedRole);
        }

        let current_version = Self::get_schema_version_impl(env);

        // Validate both sides before the idempotent return. Otherwise a corrupt
        // marker (for example 0 or a future version) could be blessed forever
        // merely by retrying that same unsupported value. Soroban transactions
        // execute atomically; after a ledger conflict is retried, this check and
        // the equality path below make the winner's migration deterministic.
        let version_is_supported = |version: u32| {
            (INITIAL_STORAGE_SCHEMA_VERSION..=CURRENT_STORAGE_SCHEMA_VERSION).contains(&version)
        };
        if !version_is_supported(current_version) || !version_is_supported(target_version) {
            return Err(Error::InvalidMigrationVersion);
        }

        // Idempotency: if already at target_version, return Ok without error
        if current_version == target_version {
            return Ok(current_version);
        }

        // Reject downgrades
        if target_version < current_version {
            return Err(Error::InvalidMigrationVersion);
        }

        // Execute step-by-step sequential migrations
        let mut running_version = current_version;

        if running_version == 1 && target_version >= 2 {
            // v1 -> v2 migration logic: establish explicit schema version marker and bump persistent TTL
            running_version = 2;
        }

        // Every accepted target must be reached by an explicit migration step.
        // This fail-closed guard prevents a future constant bump from silently
        // persisting a partially migrated layout.
        if running_version != target_version {
            return Err(Error::InvalidMigrationVersion);
        }

        // Persist final version
        Self::set_schema_version_impl(env, running_version);

        // Emit migration event: topics = ("escrow_schema_migrated", current_version), data = (running_version, admin, timestamp)
        env.events().publish(
            (Symbol::new(env, "escrow_schema_migrated"), current_version),
            (running_version, admin, env.ledger().timestamp()),
        );

        Ok(running_version)
    }
}
