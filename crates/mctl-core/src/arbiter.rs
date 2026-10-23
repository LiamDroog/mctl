//! Source arbitration (DESIGN.md §3.2): the first source, in configured priority order, that is
//! healthy and valid at the requested time is active. Everything else keeps propagating in the
//! background so residuals can be published.

/// A source's eligibility at the requested time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    /// Has data, not stale beyond its limit, and propagates without error.
    pub healthy: bool,
    /// The requested time lies within the source's validity span.
    pub valid: bool,
}

/// Index of the active source among `candidates` (given in priority order), if any qualifies.
pub fn select(candidates: &[Candidate]) -> Option<usize> {
    candidates.iter().position(|c| c.healthy && c.valid)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: Candidate = Candidate {
        healthy: true,
        valid: true,
    };

    #[test]
    fn highest_priority_eligible_source_wins() {
        assert_eq!(select(&[OK, OK]), Some(0));
    }

    #[test]
    fn unhealthy_or_out_of_span_sources_are_skipped() {
        let stale = Candidate {
            healthy: false,
            valid: true,
        };
        let outside = Candidate {
            healthy: true,
            valid: false,
        };
        assert_eq!(select(&[stale, OK]), Some(1));
        assert_eq!(select(&[outside, OK]), Some(1));
        assert_eq!(
            select(&[stale, outside]),
            None,
            "no eligible source means no solution, not a guess"
        );
        assert_eq!(select(&[]), None);
    }
}
