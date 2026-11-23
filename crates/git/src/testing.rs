//! Helpers shared by the unit tests.

/// A small deterministic random number generator: the tests that use it check the same
/// cases on every run and need no dependency.
pub(crate) struct Lcg(pub u64);

impl Lcg {
    /// The next number, below `bound` (which must not be 0).
    pub fn next(&mut self, bound: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % bound
    }

    fn index(&mut self, len: usize) -> usize {
        self.next(len.max(1) as u64) as usize
    }

    /// A copy of `sample` with a few random edits: bytes changed, removed, inserted or
    /// repeated, a stretch moved elsewhere, or the end cut off. What a parser must
    /// survive, whatever it makes of it.
    pub fn mangle(&mut self, sample: &[u8]) -> Vec<u8> {
        let mut bytes = sample.to_vec();
        for _ in 0..1 + self.next(4) {
            if bytes.is_empty() {
                break;
            }
            let at = self.index(bytes.len());
            match self.next(7) {
                0 => bytes[at] = self.next(256) as u8,
                1 => {
                    bytes.remove(at);
                }
                2 => bytes.insert(at, b"\n\0 \t\"\\@+-:/0\xff"[self.index(13)]),
                3 => bytes.truncate(at),
                4 => {
                    let end = (at + self.index(40)).min(bytes.len());
                    let stretch = bytes[at..end].to_vec();
                    let to = self.index(bytes.len());
                    bytes.splice(to..to, stretch);
                }
                5 => {
                    let end = (at + self.index(40)).min(bytes.len());
                    bytes.drain(at..end);
                }
                _ => bytes[at] = b"\n\0 \t\"\\@+-:/0\xff"[self.index(13)],
            }
        }
        bytes
    }
}
