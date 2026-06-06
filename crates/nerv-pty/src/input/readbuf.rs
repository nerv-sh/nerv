use memmem::{Searcher, TwoWaySearcher};

/// This is a simple, small, read buffer that always has the buffer
/// contents available as a contiguous slice.
#[derive(Debug)]
pub struct ReadBuffer {
    storage: Vec<u8>,
}

impl Default for ReadBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadBuffer {
    pub fn new() -> Self {
        Self {
            storage: Vec::with_capacity(16),
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        self.storage.as_slice()
    }

    pub fn is_empty(&self) -> bool {
        self.storage.is_empty()
    }

    pub fn len(&self) -> usize {
        self.storage.len()
    }

    /// Mark `len` bytes as consumed, discarding them and shunting
    /// the contents of the buffer such that the remainder of the
    /// bytes are available at the front of the buffer.
    pub fn advance(&mut self, len: usize) {
        let remain = self.storage.len() - len;
        self.storage.rotate_left(len);
        self.storage.truncate(remain);
    }

    /// Append the contents of the slice to the read buffer
    pub fn extend_with(&mut self, slice: &[u8]) {
        self.storage.extend_from_slice(slice);
    }

    /// Search for `needle` starting at `offset`.  Returns its offset
    /// into the buffer if found, else None.
    pub fn find_subsequence(&self, offset: usize, needle: &[u8]) -> Option<usize> {
        let needle = TwoWaySearcher::new(needle);
        let haystack = &self.storage[offset..];
        needle.search_in(haystack).map(|x| x + offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_buffer_is_empty() {
        let b = ReadBuffer::new();
        assert!(b.is_empty());
        assert_eq!(b.len(), 0);
        assert_eq!(b.as_slice(), b"");
    }

    #[test]
    fn extend_and_inspect() {
        let mut b = ReadBuffer::new();
        b.extend_with(b"hello");
        assert!(!b.is_empty());
        assert_eq!(b.len(), 5);
        assert_eq!(b.as_slice(), b"hello");
        b.extend_with(b" world");
        assert_eq!(b.as_slice(), b"hello world");
    }

    /// `advance` discards the leading bytes and shunts the tail to
    /// the front. Locks the contract `read_loop` depends on for
    /// parsing partial escape sequences.
    #[test]
    fn advance_shifts_tail_to_front() {
        let mut b = ReadBuffer::new();
        b.extend_with(b"abcdef");
        b.advance(3);
        assert_eq!(b.as_slice(), b"def");
        assert_eq!(b.len(), 3);
    }

    #[test]
    fn advance_all_clears_buffer() {
        let mut b = ReadBuffer::new();
        b.extend_with(b"abc");
        b.advance(3);
        assert!(b.is_empty());
    }

    /// `find_subsequence(0, needle)` returns the absolute offset of
    /// the first match.
    #[test]
    fn find_subsequence_from_start() {
        let mut b = ReadBuffer::new();
        b.extend_with(b"the quick brown fox");
        assert_eq!(b.find_subsequence(0, b"quick"), Some(4));
        assert_eq!(b.find_subsequence(0, b"fox"), Some(16));
        assert_eq!(b.find_subsequence(0, b"cat"), None);
    }

    /// `find_subsequence(offset, needle)` searches only past
    /// `offset` and returns the absolute index. Catches a regression
    /// where the offset is silently ignored.
    #[test]
    fn find_subsequence_respects_offset() {
        let mut b = ReadBuffer::new();
        b.extend_with(b"abc abc abc");
        // First `abc` skipped when offset >= 1.
        assert_eq!(b.find_subsequence(1, b"abc"), Some(4));
        assert_eq!(b.find_subsequence(5, b"abc"), Some(8));
        assert_eq!(b.find_subsequence(9, b"abc"), None);
    }

    #[test]
    fn find_subsequence_empty_needle_matches_at_offset() {
        let mut b = ReadBuffer::new();
        b.extend_with(b"abc");
        assert_eq!(b.find_subsequence(0, b""), Some(0));
        assert_eq!(b.find_subsequence(2, b""), Some(2));
    }
}
