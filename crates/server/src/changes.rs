//! Long-poll (`GET /api/v1/changes`): one counter per person, bumped by
//! everything that gives their devices something new to fetch: a library
//! write, a session change, a fresh list for a channel they follow, durations.
//! An open app waits on it instead of polling every minute.
//!
//! In memory: the server is a single instance (AGENTS.md), so Postgres NOTIFY
//! would add nothing. Counters start at 0 when the server starts; a device
//! holding another number syncs at once, which is what a restart deserves.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use sqlx::PgPool;
use tokio::sync::watch;
use uuid::Uuid;

/// How long a wait is held before it answers "nothing new".
pub const WAIT: Duration = Duration::from_secs(25);

#[derive(Default)]
pub struct Changes {
    people: Mutex<HashMap<Uuid, watch::Sender<u64>>>,
}

impl Changes {
    fn sender(&self, user: Uuid) -> watch::Sender<u64> {
        self.people.lock().unwrap().entry(user).or_insert_with(|| watch::channel(0).0).clone()
    }

    /// Something changed for `user`.
    pub fn bump(&self, user: Uuid) {
        self.sender(user).send_modify(|g| *g = g.wrapping_add(1));
    }

    pub fn bump_all(&self, users: &[Uuid]) {
        for u in users {
            self.bump(*u);
        }
    }

    pub fn current(&self, user: Uuid) -> u64 {
        *self.sender(user).borrow()
    }

    /// The counter once it differs from `since`, or after [`WAIT`]:
    /// `(generation, changed)`.
    pub async fn wait(&self, user: Uuid, since: u64, wait: Duration) -> (u64, bool) {
        let mut rx = self.sender(user).subscribe();
        let now = *rx.borrow_and_update();
        if now != since {
            return (now, true);
        }
        match tokio::time::timeout(wait, rx.changed()).await {
            Ok(Ok(())) => (*rx.borrow(), true),
            _ => (*rx.borrow(), false),
        }
    }
}

/// Everyone a channel's list matters to: its followers for YouTube, the owner
/// for Floatplane and Sauce+.
pub async fn followers(pool: &PgPool, source: &str, channel_id: &str, owner: Uuid) -> Result<Vec<Uuid>, sqlx::Error> {
    if owner != crate::feeds::SHARED {
        return Ok(vec![owner]);
    }
    sqlx::query_scalar(
        "SELECT DISTINCT user_id FROM subscriptions WHERE source = $1 AND remote_id = $2 AND deleted_at IS NULL",
    )
    .bind(source)
    .bind(channel_id)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn waits_answer_at_once_when_behind_and_wake_on_a_bump() {
        let c = std::sync::Arc::new(Changes::default());
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(c.wait(a, 99, WAIT).await, (0, true), "a different number answers at once");
        assert_eq!(c.wait(a, 0, Duration::from_millis(20)).await, (0, false), "nothing new: times out");
        let waiter = {
            let c = c.clone();
            tokio::spawn(async move { c.wait(a, 0, WAIT).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        c.bump(b);
        c.bump(a);
        assert_eq!(waiter.await.unwrap(), (1, true));
        assert_eq!(c.current(b), 1, "per person");
    }
}
