//! Sequence number helpers and general numeric utilities.
//!
//! ## Half-Range Comparison
//!
//! UDP sequence numbers are 16-bit unsigned integers that wrap around at
//! 65535 -> 0. A naive `a > b` comparison breaks at the wrap point. The
//! standard fix is the *half-range* rule:
//!
//! ```text
//! s1 is "greater than" s2 when:
//!   ( s1 > s2  AND  s1 - s2 <= 32768 )  -- s1 is ahead, no wrap
//!   OR
//!   ( s1 < s2  AND  s2 - s1 > 32768  )  -- s1 wrapped past s2
//! ```
//!
//! This correctly handles all wrap-around cases as long as the two sequences
//! are never more than 32767 apart. The library enforces this by keeping
//! buffer sizes well within that bound.
//!
//! All arithmetic on sequence numbers uses `wrapping_add` / `wrapping_sub`
//! to stay correct at the wrap boundary. Direct `a > b` or `a - b` on
//! raw sequence values is forbidden on the hot path.
//!
//! ## Exponential Moving Average
//!
//! [`smooth_value`] implements a simple EMA:
//!
//! ```text
//! new_estimate = current + (sample - current) * factor
//! ```
//!
//! Used for RTT, packet loss, and bandwidth smoothing. If the sample and
//! current value are within 0.00001 the sample is returned directly to
//! avoid floating-point drift accumulation.

/// Check if sequence s1 is greater than s2, handling wrap-around
///
/// # Examples
///
/// ```
/// use reliable_rs::sequence_greater_than;
///
/// assert!(sequence_greater_than(1, 0));
/// assert!(sequence_greater_than(0, 65535)); // Wrap-around
/// assert!(!sequence_greater_than(65535, 0));
/// ```
#[inline]
pub fn sequence_greater_than(s1: u16, s2: u16) -> bool {
    ((s1 > s2) && (s1.wrapping_sub(s2) <= 32768)) || ((s1 < s2) && (s2.wrapping_sub(s1) > 32768))
}

/// Check if sequence s1 is less than s2, handling wrap-around
///
/// # Examples
///
/// ```
/// use reliable_rs::sequence_less_than;
///
/// assert!(sequence_less_than(0, 1));
/// assert!(sequence_less_than(65535, 0)); // Wrap-around
/// ```
#[inline]
pub fn sequence_less_than(s1: u16, s2: u16) -> bool {
    sequence_greater_than(s2, s1)
}

/// Calculate the number of bits required to represent a value
#[allow(dead_code)]
#[inline]
pub(crate) fn bits_required(min: u32, max: u32) -> u32 {
    if min == max {
        0
    } else {
        let range = max - min;
        32 - range.leading_zeros()
    }
}

/// Smooth a value using exponential moving average
#[inline]
pub(crate) fn smooth_value(current: f32, new: f32, factor: f32) -> f32 {
    if (current - new).abs() < 0.00001 {
        new
    } else {
        current + (new - current) * factor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sequence_greater_than() {
        // Normal cases
        assert!(sequence_greater_than(1, 0));
        assert!(sequence_greater_than(100, 50));
        assert!(!sequence_greater_than(0, 1));
        assert!(!sequence_greater_than(50, 100));

        // Equal
        assert!(!sequence_greater_than(5, 5));

        // Wrap-around cases
        assert!(sequence_greater_than(0, 65535));
        assert!(sequence_greater_than(1, 65534));
        assert!(!sequence_greater_than(65535, 0));
        assert!(!sequence_greater_than(65534, 1));

        // Near wrap-around boundary
        assert!(sequence_greater_than(32768, 0));
        assert!(!sequence_greater_than(32769, 0));
    }

    #[test]
    fn test_sequence_less_than() {
        assert!(sequence_less_than(0, 1));
        assert!(sequence_less_than(65535, 0));
        assert!(!sequence_less_than(1, 0));
        assert!(!sequence_less_than(0, 65535));
    }

    #[test]
    fn test_bits_required() {
        assert_eq!(bits_required(0, 0), 0);
        assert_eq!(bits_required(0, 1), 1);
        assert_eq!(bits_required(0, 2), 2);
        assert_eq!(bits_required(0, 3), 2);
        assert_eq!(bits_required(0, 4), 3);
        assert_eq!(bits_required(0, 255), 8);
        assert_eq!(bits_required(0, 256), 9);
    }

    #[test]
    fn test_smooth_value() {
        let current = 100.0;
        let new = 200.0;
        let factor = 0.1;

        let smoothed = smooth_value(current, new, factor);
        assert!((smoothed - 110.0).abs() < 0.001);

        // Same values should return new
        let same = smooth_value(100.0, 100.0, factor);
        assert!((same - 100.0).abs() < 0.001);
    }
}
