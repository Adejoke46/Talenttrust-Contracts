/// Minimum valid reputation rating (inclusive).
pub const MIN_RATING: u32 = 1;

/// Maximum valid reputation rating (inclusive).
pub const MAX_RATING: u32 = 5;

/// Max byte length of a reputation feedback comment.
pub const MAX_COMMENT_BYTES: u32 = 200;

/// Unit increment for pending reputation credits.
pub const REPUTATION_CREDIT_INCREMENT: i128 = 1;

/// Basis-point scaling factor for `get_average_rating` (×10_000 preserves four decimal places).
pub const SCALE: i128 = 10_000;

/// Upper bound on the `limit` parameter of paginated read views.
///
/// Keeps per-call storage reads bounded and prevents callers from requesting
/// unbounded scans in a single invocation.
pub const PAGE_CEILING: u32 = 50;

/// Normalize a pagination request without allowing an unbounded storage scan.
/// Zero remains valid and means "return an empty page", preserving the read
/// API's compatibility behavior; oversized requests are safely capped.
pub(crate) const fn normalize_page_limit(limit: u32) -> u32 {
    if limit > PAGE_CEILING {
        PAGE_CEILING
    } else {
        limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_limit_boundaries_are_deterministic() {
        assert_eq!(normalize_page_limit(0), 0);
        assert_eq!(normalize_page_limit(1), 1);
        assert_eq!(normalize_page_limit(PAGE_CEILING), PAGE_CEILING);
        assert_eq!(normalize_page_limit(PAGE_CEILING + 1), PAGE_CEILING);
        assert_eq!(normalize_page_limit(u32::MAX), PAGE_CEILING);
    }
}
