use super::*;

const MAX_PROJECT_LIST_ROWS: usize = 10_000;
const MAX_DURABLE_MAILBOX_ROWS: usize = 100_000;
const MAX_MAILBOX_PAGE_ROWS: u32 = 1_000;
const MAX_MAILBOX_STATUS_ROWS: u32 = 100_000;

fn durable_mailbox_limit_error() -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("durable mailbox queue exceeded the {MAX_DURABLE_MAILBOX_ROWS}-row safety limit"),
    )))
}

impl StateStore {
    pub fn project(&self, id: &str) -> rusqlite::Result<Option<Project>> {
        self.connection.query_row("SELECT id,name,source_endpoint,destination_endpoint,phase FROM projects WHERE id=?1", [id], |r| Ok(Project { id:r.get(0)?, name:r.get(1)?, source_endpoint:r.get(2)?, destination_endpoint:r.get(3)?, phase: Phase::parse(&r.get::<_,String>(4)?)? })).optional()
    }
    pub fn latest_project(&self) -> rusqlite::Result<Option<Project>> {
        self.connection.query_row("SELECT id,name,source_endpoint,destination_endpoint,phase FROM projects ORDER BY created_at DESC, rowid DESC LIMIT 1", [], |r| Ok(Project { id:r.get(0)?, name:r.get(1)?, source_endpoint:r.get(2)?, destination_endpoint:r.get(3)?, phase: Phase::parse(&r.get::<_,String>(4)?)? })).optional()
    }
    pub fn recent_projects(&self, limit: usize) -> rusqlite::Result<Vec<ProjectListItem>> {
        let limit = limit.min(MAX_PROJECT_LIST_ROWS);
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
            .and_then(sqlite_usize)
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
            "SELECT id,source_mailbox,destination_mailbox,state,config FROM mailbox_jobs WHERE project_id=?1 ORDER BY rowid LIMIT ?2",
        )?;
        let rows = statement
            .query_map(
                params![project_id, MAX_DURABLE_MAILBOX_ROWS as i64 + 1],
                |row| {
                    Ok(MailboxJob {
                        id: row.get(0)?,
                        source_mailbox: row.get(1)?,
                        destination_mailbox: row.get(2)?,
                        state: row.get(3)?,
                        config: row.get(4)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if rows.len() > MAX_DURABLE_MAILBOX_ROWS {
            return Err(durable_mailbox_limit_error());
        }
        Ok(rows)
    }

    /// Compare a complete imported queue without materializing its durable
    /// profiles. Queue order is part of the batch identity, so stream the
    /// rows in rowid order and compare each one with the admitted input.
    pub fn mailbox_queue_matches(
        &self,
        project_id: &str,
        desired: &[(String, String, String)],
    ) -> rusqlite::Result<bool> {
        let mut statement = self.connection.prepare(
            "SELECT source_mailbox,destination_mailbox,config FROM mailbox_jobs WHERE project_id=?1 ORDER BY rowid",
        )?;
        let mut rows = statement.query([project_id])?;
        for (source_mailbox, destination_mailbox, config) in desired {
            let Some(row) = rows.next()? else {
                return Ok(false);
            };
            let stored_source: String = row.get(0)?;
            let stored_destination: String = row.get(1)?;
            let stored_config: Option<String> = row.get(2)?;
            if stored_source != *source_mailbox
                || stored_destination != *destination_mailbox
                || stored_config.as_deref() != Some(config.as_str())
            {
                return Ok(false);
            }
        }
        Ok(rows.next()?.is_none())
    }

    pub fn mailbox_ids(&self, project_id: &str) -> rusqlite::Result<Vec<String>> {
        let mut statement = self
            .connection
            .prepare("SELECT id FROM mailbox_jobs WHERE project_id=?1 ORDER BY rowid LIMIT ?2")?;
        let rows = statement
            .query_map(
                params![project_id, MAX_DURABLE_MAILBOX_ROWS as i64 + 1],
                |row| row.get(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if rows.len() > MAX_DURABLE_MAILBOX_ROWS {
            return Err(durable_mailbox_limit_error());
        }
        Ok(rows)
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
        let limit = limit.min(MAX_MAILBOX_PAGE_ROWS);
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
        let limit = limit.min(MAX_MAILBOX_STATUS_ROWS);
        let mut statement = self.connection.prepare(
            "SELECT id,source_mailbox,destination_mailbox,state,attention_reason FROM mailbox_jobs WHERE project_id=?1 ORDER BY rowid LIMIT ?2 OFFSET ?3",
        )?;
        statement
            .query_map(params![project_id, limit, offset], |row| {
                let reason = row.get::<_, Option<String>>(4)?.map(|value| {
                    AttentionReason::parse(&value).unwrap_or(AttentionReason::Unknown)
                });
                Ok((
                    MailboxJob {
                        id: row.get(0)?,
                        source_mailbox: row.get(1)?,
                        destination_mailbox: row.get(2)?,
                        state: row.get(3)?,
                        config: None,
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
                    total: sqlite_usize(row.get(0)?)?,
                    ready: sqlite_usize(row.get::<_, Option<i64>>(1)?.unwrap_or(0))?,
                    running: sqlite_usize(row.get::<_, Option<i64>>(2)?.unwrap_or(0))?,
                    verified: sqlite_usize(row.get::<_, Option<i64>>(3)?.unwrap_or(0))?,
                    needs_review: sqlite_usize(row.get::<_, Option<i64>>(4)?.unwrap_or(0))?,
                })
            },
        )
    }

    pub fn all_mailbox_state_counts(&self) -> rusqlite::Result<MailboxStateCounts> {
        self.connection.query_row(
            "SELECT COUNT(*), SUM(CASE WHEN state='ready' THEN 1 ELSE 0 END), SUM(CASE WHEN state IN ('running','claimed') THEN 1 ELSE 0 END), SUM(CASE WHEN state IN ('verified','verified_with_exceptions') THEN 1 ELSE 0 END), SUM(CASE WHEN state IN ('attention','failed','cancelled','verification_difference') THEN 1 ELSE 0 END) FROM mailbox_jobs",
            [],
            |row| {
                Ok(MailboxStateCounts {
                    total: sqlite_usize(row.get(0)?)?,
                    ready: sqlite_usize(row.get::<_, Option<i64>>(1)?.unwrap_or(0))?,
                    running: sqlite_usize(row.get::<_, Option<i64>>(2)?.unwrap_or(0))?,
                    verified: sqlite_usize(row.get::<_, Option<i64>>(3)?.unwrap_or(0))?,
                    needs_review: sqlite_usize(row.get::<_, Option<i64>>(4)?.unwrap_or(0))?,
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
