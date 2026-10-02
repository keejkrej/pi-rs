//! Port of `diff/libesm/util/distance-iterator.js` (diff@8.0.4).

/// Iterator that traverses in the range of [min, max], stepping
/// by distance from a given start position. I.e. for [0, 4], with
/// start of 2, this will iterate 2, 3, 1, 4, 0.
pub fn distance_iterator(start: f64, min_line: f64, max_line: f64) -> DistanceIterator {
    DistanceIterator {
        start,
        min_line,
        max_line,
        want_forward: true,
        backward_exhausted: false,
        forward_exhausted: false,
        local_offset: 1.0,
    }
}

// PORT: State of the closure returned by [`distance_iterator`]; `next()` is one call of it.
#[derive(Clone, Debug)]
pub struct DistanceIterator {
    start: f64,
    min_line: f64,
    max_line: f64,
    want_forward: bool,
    backward_exhausted: bool,
    forward_exhausted: bool,
    local_offset: f64,
}

impl Iterator for DistanceIterator {
    type Item = f64;

    fn next(&mut self) -> Option<f64> {
        loop {
            if self.want_forward && !self.forward_exhausted {
                if self.backward_exhausted {
                    self.local_offset += 1.0;
                } else {
                    self.want_forward = false;
                }
                // Check if trying to fit beyond text length, and if not, check it fits
                // after offset location (or desired location on first iteration)
                if self.start + self.local_offset <= self.max_line {
                    return Some(self.start + self.local_offset);
                }
                self.forward_exhausted = true;
            }
            if !self.backward_exhausted {
                if !self.forward_exhausted {
                    self.want_forward = true;
                }
                // Check if trying to fit before text beginning, and if not, check it fits
                // before offset location
                if self.min_line <= self.start - self.local_offset {
                    let pos = self.start - self.local_offset;
                    self.local_offset += 1.0;
                    return Some(pos);
                }
                self.backward_exhausted = true;
                continue;
            }
            // We tried to fit hunk before text beginning and beyond text length, then
            // hunk can't fit on the text. Return undefined
            return None;
        }
    }
}
