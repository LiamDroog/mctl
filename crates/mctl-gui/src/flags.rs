//! Short flags that mark a value or panel as something other than a plain measurement, instead of a
//! sentence of caveat after it. The legend lives in the help overlay.

/// Estimated, assumed, inferred or configured rather than measured or taken from a source.
pub const EST: &str = "[EST]";
/// A schedule or plan: what is intended, not what is happening.
pub const PLAN: &str = "[PLAN]";
/// Derived from TLEs, so not valid inside about 5 km.
pub const TLE: &str = "[TLE]";
/// A sky-survey cutout of the target field, not an image from the observatory.
pub const SURVEY: &str = "[SURVEY]";
/// A current value shown while the clock is not at the present.
pub const NOW: &str = "[NOW]";

/// Flag and what it means, for the help overlay.
pub const LEGEND: [(&str, &str); 5] = [
    (
        EST,
        "estimated, assumed, inferred or configured: not measured or sourced",
    ),
    (PLAN, "scheduled or planned: not what is happening"),
    (TLE, "TLE-derived: not valid inside about 5 km"),
    (
        SURVEY,
        "sky-survey cutout of the field: not an observatory image",
    ),
    (
        NOW,
        "the current value, shown while the clock is not at the present",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_flag_is_bracketed_upper_case_and_in_the_legend_once() {
        for f in [EST, PLAN, TLE, SURVEY, NOW] {
            assert!(
                f.starts_with('[') && f.ends_with(']') && f == f.to_uppercase(),
                "{f}"
            );
            assert_eq!(LEGEND.iter().filter(|(l, _)| *l == f).count(), 1, "{f}");
        }
    }
}
