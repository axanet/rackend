//! Background maintenance (SPEC §5.4 step 9): trash purge, audit
//! retention and session garbage collection. Each unit of work is its own
//! audited transaction.

use time::Duration;

use super::App;
use crate::Result;
use crate::domain::Actor;
use crate::ports::StoreExt;
use crate::time::format_timestamp;
use crate::usecases::collections::purge_in_tx;

/// Audit rows deleted per transaction.
const AUDIT_BATCH: u32 = 10_000;

impl App {
    /// Purges trash entries past `purge_after`. Returns how many.
    pub async fn job_purge_trash(&self) -> Result<usize> {
        let now = self.clock.now();
        let due = self
            .store
            .write(move |tx| tx.trash_due(&format_timestamp(now), 100))
            .await?;
        let mut n = 0;
        for t in due {
            let actor = Actor::system();
            self.store
                .write(move |tx| purge_in_tx(tx, &t, &actor, now))
                .await?;
            n += 1;
        }
        if n > 0 {
            self.reload_schema().await?;
            self.after_file_changes();
        }
        Ok(n)
    }

    /// Deletes audit entries older than `audit.retention_days` (0 = keep forever).
    pub async fn job_audit_retention(&self) -> Result<u64> {
        let days = self.settings().int("audit", "retention_days", 365);
        if days <= 0 {
            return Ok(0);
        }
        let before = format_timestamp(self.clock.now() - Duration::days(days));
        let mut total = 0;
        loop {
            let b = before.clone();
            let n = self
                .store
                .write(move |tx| tx.audit_delete_before(&b, AUDIT_BATCH))
                .await?;
            total += n;
            if n < u64::from(AUDIT_BATCH) {
                return Ok(total);
            }
        }
    }

    /// Deletes sessions revoked or expired more than 30 days ago, and
    /// one-time tokens and codes that expired more than a day ago.
    pub async fn job_session_gc(&self) -> Result<u64> {
        let before = format_timestamp(self.clock.now() - Duration::days(30));
        let tokens_before = format_timestamp(self.clock.now() - Duration::days(1));
        self.store
            .write(
                move |tx| Ok(tx.sessions_delete_before(&before)? + tx.tokens_gc(&tokens_before)?),
            )
            .await
    }
}
