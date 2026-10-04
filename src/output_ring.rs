//! The output a session's program wrote lately, as it came, with when it
//! came: what `crystal read --since` lays out on a screen of its own, so it
//! shows what arrived since then and nothing from before. Adapted from
//! docket's scrollback ring. The screen itself keeps no times, and keeps
//! what the program drew, not the order it drew it in.
//!
//! It keeps the last [`MOST`] bytes, and a mark at most every
//! [`MARK_GAP_MS`] saying where the output had got to then. It knows when it
//! can't say what came since a time: when it has let go of output from
//! after it, or it began after it, as one does after a handover.

use std::collections::VecDeque;

/// The most output a ring keeps, in bytes.
pub const MOST: usize = 1024 * 1024;

/// How far apart two marks are at the least: `--since` is to the second,
/// and a program printing all day costs one mark a second rather than one a
/// read.
const MARK_GAP_MS: u64 = 1_000;

/// The most marks a ring keeps; past it the oldest go.
const MOST_MARKS: usize = 8_192;

pub struct OutputRing {
    bytes: VecDeque<u8>,
    most: usize,
    /// How many bytes came before the oldest kept: the place of that byte
    /// in all the output.
    start: u64,
    /// How many bytes have come in all.
    end: u64,
    /// When the output got where: the place of a read's first byte and when
    /// it came, in milliseconds since the Unix epoch, at most one every
    /// [`MARK_GAP_MS`], the oldest first. Everything from one mark's place
    /// to the next's came within [`MARK_GAP_MS`] of the mark's time.
    marks: VecDeque<(u64, u64)>,
    /// Output from before this time may have gone: the ring let go of it,
    /// or of the mark that dated it.
    lost_until: u64,
    /// When the ring began.
    began: u64,
    /// Whether it began with the program, so that nothing came before it.
    from_the_start: bool,
}

impl OutputRing {
    /// A ring that begins at `now`, in milliseconds since the Unix epoch:
    /// with the program when `from_the_start`, or after it had written what
    /// the ring will never hold, like a program a handover carried on.
    pub fn new(now: u64, from_the_start: bool) -> OutputRing {
        OutputRing::holding(MOST, now, from_the_start)
    }

    fn holding(most: usize, now: u64, from_the_start: bool) -> OutputRing {
        OutputRing {
            bytes: VecDeque::new(),
            most,
            start: 0,
            end: 0,
            marks: VecDeque::new(),
            lost_until: 0,
            began: now,
            from_the_start,
        }
    }

    /// Keeps `output`, which came at `now`, letting go of the oldest past
    /// [`MOST`].
    pub fn push(&mut self, output: &[u8], now: u64) {
        if output.is_empty() {
            return;
        }
        let due = self
            .marks
            .back()
            .is_none_or(|&(_, at)| now.saturating_sub(at) >= MARK_GAP_MS);
        if due {
            self.marks.push_back((self.end, now));
        }
        if output.len() >= self.most {
            self.bytes.clear();
            self.bytes.extend(&output[output.len() - self.most..]);
        } else {
            let over = (self.bytes.len() + output.len()).saturating_sub(self.most);
            self.bytes.drain(..over);
            self.bytes.extend(output);
        }
        self.end += output.len() as u64;
        self.start = self.end - self.bytes.len() as u64;
        // A mark whose output has all gone is no use; the one the oldest
        // byte kept falls after still dates it. What came by the end of a
        // mark's second is past knowing once it goes.
        while self.marks.len() > 1
            && (self.marks[1].0 <= self.start || self.marks.len() > MOST_MARKS)
        {
            if let Some((_, at)) = self.marks.pop_front() {
                self.lost_until = self.lost_until.max(at + MARK_GAP_MS);
            }
        }
    }

    /// The output that came from `since` on, in milliseconds since the Unix
    /// epoch, to the second: a byte that came at `since` or after is never
    /// left out, and a few from up to a second before may be let in. Empty
    /// when nothing has come since. `None` when the ring can't say, because
    /// it let go of output from after `since`, or began after it.
    pub fn since(&self, since: u64) -> Option<Vec<u8>> {
        if since < self.lost_until || (since < self.began && !self.from_the_start) {
            return None;
        }
        // The oldest mark's output may have gone in part.
        let cut_short = self
            .marks
            .front()
            .is_some_and(|&(place, at)| place < self.start && since < at + MARK_GAP_MS);
        if cut_short {
            return None;
        }
        let Some(&(place, _)) = self.marks.iter().find(|&&(_, at)| at + MARK_GAP_MS > since) else {
            return Some(Vec::new());
        };
        let skip = (place.max(self.start) - self.start) as usize;
        Some(self.bytes.iter().skip(skip).copied().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since_a_time_is_what_came_from_then_on() {
        let mut ring = OutputRing::new(0, true);
        ring.push(b"one\n", 1_000);
        ring.push(b"two\n", 1_500);
        ring.push(b"three\n", 5_000);
        ring.push(b"four\n", 9_000);
        assert_eq!(ring.since(0).unwrap(), b"one\ntwo\nthree\nfour\n");
        // Within a second of the mark, a little from before is let in.
        assert_eq!(ring.since(1_700).unwrap(), b"one\ntwo\nthree\nfour\n");
        assert_eq!(ring.since(4_000).unwrap(), b"three\nfour\n");
        assert_eq!(ring.since(5_500).unwrap(), b"three\nfour\n");
        assert_eq!(ring.since(6_000).unwrap(), b"four\n");
        assert_eq!(ring.since(20_000).unwrap(), b"");
    }

    #[test]
    fn a_ring_that_let_go_of_what_came_since_can_t_say() {
        let mut ring = OutputRing::holding(8, 0, true);
        ring.push(b"aaaa", 1_000);
        ring.push(b"bbbb", 3_000);
        ring.push(b"cccc", 5_000);
        // The a's have gone, and the b's came after 2s: the ring can't say
        // what came since then.
        assert_eq!(ring.since(500), None);
        assert_eq!(ring.since(2_500).unwrap(), b"bbbbcccc");
        assert_eq!(ring.since(4_500).unwrap(), b"cccc");
        // A read bigger than the ring keeps its end, and the ring can't say
        // what came since before it.
        ring.push(b"0123456789", 7_000);
        assert_eq!(ring.bytes.iter().copied().collect::<Vec<u8>>(), b"23456789");
        assert_eq!(ring.since(6_500), None);
        ring.push(b"z", 9_000);
        assert_eq!(ring.since(8_500).unwrap(), b"z");
    }

    #[test]
    fn a_ring_begun_after_the_program_can_t_say_what_came_before_it() {
        let mut ring = OutputRing::new(10_000, false);
        ring.push(b"after\n", 12_000);
        assert_eq!(ring.since(9_000), None);
        assert_eq!(ring.since(11_000).unwrap(), b"after\n");
        // One begun with its program had nothing before it.
        let mut fresh = OutputRing::new(10_000, true);
        fresh.push(b"first\n", 12_000);
        assert_eq!(fresh.since(9_000).unwrap(), b"first\n");
    }

    #[test]
    fn marks_come_a_second_apart_at_the_most() {
        let mut ring = OutputRing::new(0, true);
        for at in 0..100 {
            ring.push(b"x", 1_000 + at * 10);
        }
        assert_eq!(ring.marks.len(), 1);
        ring.push(b"y", 2_000);
        assert_eq!(ring.marks.len(), 2);
    }
}
