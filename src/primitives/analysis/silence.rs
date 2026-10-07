//! Where a signal is quiet: stretches under a threshold, and the span that
//! is not.
//!
//! "Is this microphone live?" and "cut the dead air" are the same
//! question asked of a recording and of a block.

use core::ops::Range;

/// The stretches of `samples` where every sample stays under `threshold`
/// (a linear level: `db_to_linear(-60.0)` for −60 dBFS) for at least
/// `min_length` samples, in order. A stretch at either end counts, so a
/// recording that starts late begins with one.
pub fn stretches(samples: &[f32], threshold: f32, min_length: usize) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    let mut start = None;
    for (i, &s) in samples.iter().enumerate() {
        match (s.abs() < threshold, start) {
            (true, None) => start = Some(i),
            (false, Some(from)) => {
                if i - from >= min_length.max(1) {
                    found.push(from..i);
                }
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        if samples.len() - from >= min_length.max(1) {
            found.push(from..samples.len());
        }
    }
    found
}

/// The span from the first sample at or above `threshold` to just past
/// the last: what is left after trimming the silence off both ends. Empty
/// (`0..0`) when nothing reaches the threshold.
pub fn trimmed(samples: &[f32], threshold: f32) -> Range<usize> {
    let first = samples.iter().position(|s| s.abs() >= threshold);
    let last = samples.iter().rposition(|s| s.abs() >= threshold);
    match (first, last) {
        (Some(a), Some(b)) => a..b + 1,
        _ => 0..0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stretches_are_found_with_their_edges() {
        //            0    1    2    3    4    5    6    7    8    9
        let s = [0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.4, 0.0, 0.0, 0.0];
        assert_eq!(stretches(&s, 0.1, 2), vec![0..2, 3..6, 7..10]);
        assert_eq!(stretches(&s, 0.1, 3), vec![3..6, 7..10]);
        assert_eq!(stretches(&s, 0.1, 4), Vec::<Range<usize>>::new());
        assert_eq!(
            stretches(&s, 0.1, 0),
            vec![0..2, 3..6, 7..10],
            "zero means one"
        );
        assert_eq!(stretches(&[], 0.1, 1), Vec::<Range<usize>>::new());
        assert_eq!(stretches(&[0.0; 4], 0.1, 1), vec![0..4], "all quiet");
        assert_eq!(stretches(&[0.9; 4], 0.1, 1), Vec::<Range<usize>>::new());
    }

    #[test]
    fn trimming_keeps_the_span_that_sounds() {
        let s = [0.0, 0.01, 0.5, 0.0, 0.4, 0.01, 0.0];
        assert_eq!(trimmed(&s, 0.1), 2..5);
        assert_eq!(trimmed(&s, 0.001), 1..6);
        assert_eq!(trimmed(&s, 0.9), 0..0);
        assert_eq!(trimmed(&[], 0.1), 0..0);
    }
}
