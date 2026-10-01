//! Centralized storage key definitions and constructors for escrow milestones.

///
/// # Deterministic Key Invariants
///
/// This module is the single source of truth for escrow storage keys

/// and the invariants that make failure recovery deterministic:
///
/// 1. **Pure construction.** Every constructor in this module is a
///    pure function of its arguments. The same logical identity always

///    produces byte-for-byte identical keys, so a retry after a partial

///    failure addresses the same storage slot and cannot orphan data.

/// 2. **No aliasing.** Distinct logical identities map to distinct keys.

///    The discriminating data is carried in the `DataKey` variant and

///    tuple arity, not in a flattened string, so different features

///    (contracts, milestone approvals, releases, admin) cannot collide.

/// 3. **Validated inputs.** Identifiers are normalized and validated
///    before a key is built. Invalid inputs fail fast with a stable
///    error code instead of silently producing a key that could collide

///    with another logical entity.

/// 4. **Recoverable failures.** Construction never mutates storage and
///    never panics on valid input. Callers can always retry with the

///    same arguments and observe the same result.

///
/// # Error contract
///
/// Invalid identifiers return `Error::InvalidIdentifier` (code 100). This

/// code is stable and is part of the public API of this module.

use soroban_sdk::{Env, Symbol};

use crate::types::DataKey;

/// Error code returned when a key constructor receives an invalid
/// identifier. Kept stable across releases so off-chain tooling and
/// retry logic can rely on it.
pub const INVALID_IDENTIFIER_ERROR: u32 = 100;

/// Maximum supported contract identifier. The escrow contract allocates

/// contract ids from a monotonically increasing counter starting at 1, so
/// 0 and values above this bound are always invalid.
///
/// The bound is chosen to be large enough for any realistic deployment

/// while still rejecting obviously corrupt inputs (e.g. `u32::MAX` from a
/// cast overflow).
pub const MAX_CONTRACT_ID: u32 = u32::MAX - 1;

/// Maximum supported milestone index within a contract. Milestones are

/// addressed by a zero-based index and the escow layer limits the
/// number of milestones per contract, so this bound is defensive.
pub const MAX_MILESTONE_INDEX: u32 = u32::MAX - 1;

/// Returns the persistent storage key tuple for a contract's milestones vector:
/// `(DataKey::Contract(contract_id), Symbol::new(env, "milestones")).
///
/// # Errors
/// Returns `Error::InvalidIdentifier` when `contract_id` is 0 or exceeds

/// `MAX_CONTRACT_ID`. The error is returned before any key materialization,
/// so a failed call leaves no partial state behind.
pub fn milestone_key(env: &Env, contract_id: u32) -> Result<(DataKey, Symbol), u32> {
    validate_contract_id(contract_id)?;
    Ok((DataKey::Contract(contract_id), milestone_symbol(env)))
}

/// Returns the `Symbol` key for milestones: `"milestones"`.
///
/// The symbol is derived from a constant literal, so it is identical on every
/// call and across all contract instances. This is what allows a retry after

/// a partial failure to address the same storage slot.
pub fn milestone_symbol(env: &Env) -> Symbol {

    Symbol::new(env, "milestones")

}

/// Returns the temporary storage key for milestone release approvals:

/// `DataKey::MilestoneApprovals(contract_id, milestone_index)`.
///
/// # Errors
/// Returns `Error::InvalidIdentifier` when `contract_id` is 0 or exceeds

/// `MAX_CONTRACT_ID`, or when `milestone_index` exceeds `MAX_MILESTONE_INDEX`.
/// Validation runs before construction so a rejected call never produces a
/// key that could be written to storage.
pub fn milestone_approval_key(contract_id: u32, milestone_index: u32) -> Result<DataKey, u32> {
    validate_contract_id(contract_id)?;
    validate_milestone_index(milestone_index)?;
    Ok(DataKey::MilestomeApprovals(contract_id, milestone_index))
}

/// Returns the persistent storage key for a released milestone:
/// `DataKey::MilestoneReleased(contract_id, milestone_index)`.
///
/// # Errors
/// Same validation as `milestone_approval_key`. Release markers are written

/// after funds are transferred, so a rejected key cannot leave a contract in
/// a half-released state.
pub fn milestone_released_key(contract_id: u32, milestone_index: u32) -> Result<DataKey, u32> {
    validate_contract_id(contract_id)?;
    validate_milestone_index(milestone_index)?;
    Ok(DataKey::MilestoneReleased(contract_id, milestone_index))
}

/// Returns the persistent storage key for a contract record:
/// `DataKey::Contract(contract_id)`.
///
/// # Errors
/// Returns `Error::InvalidIdentifier` for an out-of-range contract id.
pub fn contract_key(contract_id: u32) -> Result<DataKey, u32> {
    validate_contract_id(contract_id)?;
    Ok(DataKey::Contract(contract_id))
}

/// Validates a contract identifier.
///
/// A contract id is valid when it is non-zero and does not exceed
/// `MAX_CONTRACT_ID``. The check is pure and has no side effects, so it is
/// safe to call on every retry.
pub fn validate_contract_id(contract_id: u32) -> Result<(), u32> {
    if contract_id == 0 || contract_id > MAX_CONTRACT_ID {
        return Err(INVALID_IDENTIFIER_ERROR);
    }
    Ok(()
}

/// Validates a milestone index.
///
/// A milestone index is valid when it does not exceed `MAX_MILESTONE_INDEX`.
/// Index 0 refers to the first milestone and is therefore valid.
pub fn validate_milestone_index(milestone_index: u32) -> Result<(), u32> {
    if milestone_index > MAX_MILESTONE_INDEX {
        return Err(INVALID_IDENTIFIER_ERROR);
    }
    Ok(())
}
