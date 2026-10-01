//! Centralized storage key definitions and constructors for escrow milestones.
//!
//! ## Invariants
//!
//! Every function in this module enforces the following at the keys layer,
//! providing defence-in-depth even when callers have already validated inputs:
//!
//! * `contract_id` must be non-zero. Contract IDs are assigned starting at 1
//!   by `create_contract`; 0 is a sentinel that indicates "no contract" and
//!   must never reach persistent storage.
//!
//! * `milestone_index` must be strictly less than [`MAX_MILESTONES`]. Indices
//!   at or above the protocol hard cap cannot correspond to a stored milestone,
//!   so building a key for them would silently produce a dead storage entry
//!   that could never be read back through the normal milestone vector.
//!
//! Validation panic codes:
//! * `contract_id == 0`              → [`Error::InvalidContractId`]  (code 4)
//! * `milestone_index >= MAX_MILESTONES` → [`Error::IndexOutOfBounds`]    (code 3)

use crate::milestones_consts::MAX_MILESTONES;
use crate::types::{DataKey, Error};
use soroban_sdk::{Env, Symbol};

// ── Internal validation helpers ──────────────────────────────────────────────

/// Assert that `contract_id` is non-zero.
///
/// # Panics
/// Panics with [`Error::InvalidContractId`] when `contract_id == 0`.
#[inline]
fn require_valid_contract_id(env: &Env, contract_id: u32) {
    if contract_id == 0 {
        env.panic_with_error(Error::InvalidContractId);
    }
}

/// Assert that `milestone_index` is within the protocol hard cap.
///
/// # Panics
/// Panics with [`Error::IndexOutOfBounds`] when
/// `milestone_index >= MAX_MILESTONES`.
#[inline]
fn require_valid_milestone_index(env: &Env, milestone_index: u32) {
    if milestone_index >= MAX_MILESTONES {
        env.panic_with_error(Error::IndexOutOfBounds);
    }
}

// ── Public key constructors ───────────────────────────────────────────────────

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
/// `(DataKey::Contract(contract_id), Symbol::new(env, "milestones"))`.
///
/// # Invariants
/// * `contract_id` must be non-zero (IDs start at 1).
///
/// # Panics
/// * [`Error::InvalidContractId`] if `contract_id == 0`.
pub fn milestone_key(env: &Env, contract_id: u32) -> (DataKey, Symbol) {
    require_valid_contract_id(env, contract_id);
    (DataKey::Contract(contract_id), milestone_symbol(env))
}

/// Returns the `Symbol` key for milestones: `"milestones"`.
///
/// This is a pure function with no inputs to validate.
pub fn milestone_symbol(env: &Env) -> Symbol {

    Symbol::new(env, "milestones")

}

/// Returns the temporary storage key for milestone release approvals:

/// `DataKey::MilestoneApprovals(contract_id, milestone_index)`.
///
/// # Invariants
/// * `contract_id` must be non-zero (IDs start at 1).
/// * `milestone_index` must be less than [`MAX_MILESTONES`] (currently 10).
///
/// # Panics
/// * [`Error::InvalidContractId`] if `contract_id == 0`.
/// * [`Error::IndexOutOfBounds`] if `milestone_index >= MAX_MILESTONES`.
pub fn milestone_approval_key(env: &Env, contract_id: u32, milestone_index: u32) -> DataKey {
    require_valid_contract_id(env, contract_id);
    require_valid_milestone_index(env, milestone_index);
    DataKey::MilestoneApprovals(contract_id, milestone_index)
}
