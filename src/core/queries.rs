use super::*;

impl StateStore {
    pub fn project(&self, id: &str) -> rusqlite::Result<Option<Project>> {
        self.connection.query_row("SELECT id,name,source_endpoint,destination_endpoint,phase FROM projects WHERE id=?1", [id], |r| Ok(Project { id:r.get(0)?, name:r.get(1)?, source_endpoint:r.get(2)?, destination_endpoint:r.get(3)?, phase: Phase::parse(&r.get::<_,String>(4)?)? })).optional()
    }
    pub fn latest_project(&self) -> rusqlite::Result<Option<Project>> {
        self.connection.query_row("SELECT id,name,source_endpoint,destination_endpoint,phase FROM projects ORDER BY created_at DESC, rowid DESC LIMIT 1", [], |r| Ok(Project { id:r.get(0)?, name:r.get(1)?, source_endpoint:r.get(2)?, destination_endpoint:r.get(3)?, phase: Phase::parse(&r.get::<_,String>(4)?)? })).optional()
    }
    pub fn recent_projects(&self, limit: usize) -> rusqlite::Result<Vec<ProjectListItem>> {
        let mut statement = self.connection.prepare(
            "SELECT id,name,source_endpoint,destination_endpoint,phase FROM projects ORDER BY created_at DESC, rowid DESC LIMIT ?1",
        )?;
        statement
            .query_map([limit as i64], |row| {
                Ok(ProjectListItem {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    source_endpoint: row.get(2)?,
                    destination_endpoint: row.get(3)?,
                    phase: Phase::parse(&row.get::<_, String>(4)?)?,
                })
            })?
            .collect()
    }

    pub fn project_count(&self) -> rusqlite::Result<usize> {
        self.connection
            .query_row("SELECT COUNT(*) FROM projects", [], |row| {
                row.get::<_, i64>(0)
            })
            .map(|count| count as usize)
    }

    /// Return the monotonic durable event position used by presentation
    /// caches. A single cheap query lets another controller's committed work
    /// invalidate the workspace read model without rebuilding every
    /// projection on every refresh interval.
    pub fn read_model_revision(&self) -> rusqlite::Result<i64> {
        self.connection
            .query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |row| {
                row.get(0)
            })
    }

    /// Return the durable event position for one project. This lets the
    /// workspace avoid rebuilding the selected project's projections when a
    /// different project changes.
    pub fn project_read_model_revision(&self, project_id: &str) -> rusqlite::Result<i64> {
        self.connection.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM events WHERE project_id=?1",
            [project_id],
            |row| row.get(0),
        )
    }

    /// Batch admission persists a secret-free mailbox configuration for each
    /// imported row. Use that durable marker instead of guessing from the
    /// human-facing project endpoints.
    pub fn project_has_mailbox_configs(&self, project_id: &str) -> rusqlite::Result<bool> {
        self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM mailbox_jobs WHERE project_id=?1 AND config IS NOT NULL AND length(trim(config)) > 0)",
            [project_id],
            |row| row.get(0),
        )
    }

    /// Identify a durable imported batch without loading every mailbox
    /// configuration into memory. A batch marker is valid only when the
    /// project has at least one mailbox and every mailbox has a non-empty
    /// secret-free configuration.
    pub fn project_has_complete_mailbox_configs(&self, project_id: &str) -> rusqlite::Result<bool> {
        self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM mailbox_jobs WHERE project_id=?1) AND NOT EXISTS(SELECT 1 FROM mailbox_jobs WHERE project_id=?1 AND (config IS NULL OR length(trim(config)) = 0))",
            [project_id],
            |row| row.get(0),
        )
    }
    pub fn first_mailbox(&self, project_id: &str) -> rusqlite::Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT id FROM mailbox_jobs WHERE project_id=?1 LIMIT 1",
                [project_id],
                |r| r.get(0),
            )
            .optional()
    }
    pub fn mailbox_identity(
        &self,
        job_id: &str,
    ) -> rusqlite::Result<Option<(String, String, String)>> {
        self.connection
            .query_row(
                "SELECT source_mailbox,destination_mailbox,state FROM mailbox_jobs WHERE id=?1",
                [job_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
    }
    pub fn mailboxes(&self, project_id: &str) -> rusqlite::Result<Vec<MailboxJob>> {
        let mut statement = self.connection.prepare(
            "SELECT id,source_mailbox,destination_mailbox,state,config FROM mailbox_jobs WHERE project_id=?1 ORDER BY rowid",
        )?;
        statement
            .query_map([project_id], |row| {
                Ok(MailboxJob {
                    id: row.get(0)?,
                    source_mailbox: row.get(1)?,
                    destination_mailbox: row.get(2)?,
                    state: row.get(3)?,
                    config: row.get(4)?,
                })
            })?
            .collect()
    }

    /// Load only one presentation page. Durable callers that need every row
    /// should continue using `mailboxes`; the workspace must not materialize a
    /// 100,000-row queue merely to render its historical view.
    pub fn mailbox_page(
        &self,
        project_id: &str,
        offset: u32,
        limit: u32,
    ) -> rusqlite::Result<Vec<MailboxJob>> {
        let mut statement = self.connection.prepare(
            "SELECT id,source_mailbox,destination_mailbox,state,config FROM mailbox_jobs WHERE project_id=?1 ORDER BY rowid LIMIT ?2 OFFSET ?3",
        )?;
        statement
            .query_map(params![project_id, limit, offset], |row| {
                Ok(MailboxJob {
                    id: row.get(0)?,
                    source_mailbox: row.get(1)?,
                    destination_mailbox: row.get(2)?,
                    state: row.get(3)?,
                    config: row.get(4)?,
                })
            })?
            .collect()
    }

    /// Load a bounded, secret-free mailbox status page with its attention
    /// classification in the same query. This is intended for summaries and
    /// support views; full mailbox configuration remains available through
    /// `mailbox_page` for callers that explicitly need it.
    pub fn mailbox_status_page(
        &self,
        project_id: &str,
        offset: u32,
        limit: u32,
    ) -> rusqlite::Result<Vec<(MailboxJob, Option<AttentionReason>)>> {
        let mut statement = self.connection.prepare(
            "SELECT id,source_mailbox,destination_mailbox,state,config,attention_reason FROM mailbox_jobs WHERE project_id=?1 ORDER BY rowid LIMIT ?2 OFFSET ?3",
        )?;
        statement
            .query_map(params![project_id, limit, offset], |row| {
                let reason = row.get::<_, Option<String>>(5)?.map(|value| {
                    AttentionReason::parse(&value).unwrap_or(AttentionReason::Unknown)
                });
                Ok((
                    MailboxJob {
                        id: row.get(0)?,
                        source_mailbox: row.get(1)?,
                        destination_mailbox: row.get(2)?,
                        state: row.get(3)?,
                        config: row.get(4)?,
                    },
                    reason,
                ))
            })?
            .collect()
    }

    pub fn mailbox_state_counts(&self, project_id: &str) -> rusqlite::Result<MailboxStateCounts> {
        self.connection.query_row(
            "SELECT COUNT(*), SUM(CASE WHEN state='ready' THEN 1 ELSE 0 END), SUM(CASE WHEN state IN ('running','claimed') THEN 1 ELSE 0 END), SUM(CASE WHEN state IN ('verified','verified_with_exceptions') THEN 1 ELSE 0 END), SUM(CASE WHEN state IN ('attention','failed','cancelled','verification_difference') THEN 1 ELSE 0 END) FROM mailbox_jobs WHERE project_id=?1",
            [project_id],
            |row| {
                Ok(MailboxStateCounts {
                    total: row.get::<_, i64>(0)? as usize,
                    ready: row.get::<_, Option<i64>>(1)?.unwrap_or(0) as usize,
                    running: row.get::<_, Option<i64>>(2)?.unwrap_or(0) as usize,
                    verified: row.get::<_, Option<i64>>(3)?.unwrap_or(0) as usize,
                    needs_review: row.get::<_, Option<i64>>(4)?.unwrap_or(0) as usize,
                })
            },
        )
    }
    pub fn all_mailboxes_verified(&self, project_id: &str) -> rusqlite::Result<bool> {
        let (total, verified): (i64, i64) = self.connection.query_row(
            "SELECT COUNT(*), SUM(CASE WHEN state IN ('verified','verified_with_exceptions') THEN 1 ELSE 0 END) FROM mailbox_jobs WHERE project_id=?1",
            [project_id],
            |row| Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0))),
        )?;
        Ok(total > 0 && total == verified)
    }
}
