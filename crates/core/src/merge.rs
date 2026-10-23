//! Whether a device's change replaces the server's row. The server leads:
//! a device wins a conflict only when it is clearly newer.

/// How much newer a conflicting change must be to win. Below this the two
/// writes are "at the same time" and the server's stays.
pub const DOUBT_MS: i64 = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// The server has no such row.
    Insert,
    /// The change replaces the server's row.
    Apply,
    /// The server's row stays; the device gets it back.
    Keep,
}

/// `server`: the server row's `(rev, updatedAt)`. `base_rev`: the rev the
/// device last saw for this row. `updated_at`: the change's, already moved
/// onto the server's clock ([`Clock::adjust`]).
pub fn decide(server: Option<(i64, i64)>, base_rev: i64, updated_at: i64) -> Decision {
    match server {
        None => Decision::Insert,
        // The device saw the latest version and changed it: a plain update.
        Some((rev, _)) if base_rev >= rev => Decision::Apply,
        // Someone else wrote the row since the device last saw it.
        Some((_, server_updated)) if updated_at > server_updated + DOUBT_MS => Decision::Apply,
        Some(_) => Decision::Keep,
    }
}

/// Moves a device's timestamps onto the server's clock: shifted by how far
/// the device's clock is off (measured per request), never later than now.
/// A device whose clock runs ahead can then not win every conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    offset: i64,
    now: i64,
}

impl Clock {
    pub fn new(client_now: i64, server_now: i64) -> Self {
        Clock { offset: server_now.saturating_sub(client_now), now: server_now }
    }

    pub fn adjust(&self, t: i64) -> i64 {
        self.cap(t.saturating_add(self.offset))
    }

    /// Never later than now, never negative; idempotent, so a date that
    /// round-trips through devices does not move.
    pub fn cap(&self, t: i64) -> i64 {
        t.clamp(0, self.now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_rows_are_inserted() {
        assert_eq!(decide(None, 0, 10), Decision::Insert);
    }

    #[test]
    fn a_device_that_saw_the_latest_version_updates_it() {
        // Even with an older timestamp: nobody else wrote in between.
        assert_eq!(decide(Some((7, 50_000)), 7, 10), Decision::Apply);
    }

    #[test]
    fn a_stale_device_loses_unless_clearly_newer() {
        let server = Some((9, 100_000));
        assert_eq!(decide(server, 7, 99_000), Decision::Keep, "older");
        assert_eq!(decide(server, 7, 100_000 + DOUBT_MS), Decision::Keep, "in doubt: server wins");
        assert_eq!(decide(server, 7, 100_000 + DOUBT_MS + 1), Decision::Apply, "clearly newer");
        assert_eq!(decide(server, 0, 50), Decision::Keep, "a device that never saw the row");
    }

    #[test]
    fn clocks_are_shifted_and_capped() {
        // The device runs 10 s slow: its stamps move forward by 10 s.
        let slow = Clock::new(990_000, 1_000_000);
        assert_eq!(slow.adjust(980_000), 990_000);
        // The device runs an hour fast: shifted back, so it does not win.
        let fast = Clock::new(1_000_000 + 3_600_000, 1_000_000);
        assert_eq!(fast.adjust(1_000_000 + 3_600_000), 1_000_000);
        // A stamp after the device's own "now" is still capped at server now.
        let right = Clock::new(1_000_000, 1_000_000);
        assert_eq!(right.adjust(5_000_000), 1_000_000);
        assert_eq!(Clock::new(5, 0).adjust(1), 0, "never negative");
        assert_eq!(right.cap(right.cap(5_000_000)), 1_000_000);
        assert_eq!(slow.cap(980_000), 980_000, "capping does not shift");
    }
}
