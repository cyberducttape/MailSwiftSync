//! The batch queue as durable state.
//!
//! A batch project's `mailbox_jobs` rows are the queue. Presentation reads
//! filtered row IDs and pages of rows from SQLite instead of holding every
//! row's plan in memory, and admission and the scheduler read rows from here
//! as they need them. Each row's derived, secret-free presentation facts
//! (label, hosts, a case-folded search key, and its destination-mutation
//! policy) live in the narrow `mailbox_queue_facts` table keyed by the row's
//! rowid, so filtering and counting neither decode plans nor read past them.

use super::{
    AttentionReason, MAX_DURABLE_MAILBOX_ROWS, MAX_PERSISTED_PROFILE_BYTES,
    MAX_TOTAL_PERSISTED_PROFILE_BYTES, Phase, Project, StateStore,
};
use rusqlite::{OptionalExtension, params, params_from_iter};
use std::collections::BTreeSet;
use uuid::Uuid;

/// The presented state, derived from durable facts only: a row whose child
/// run is admitted but not yet claimed is `queued`; a `queued` row that no
/// run has ever admitted is `imported`; otherwise the mailbox state. The
/// run sets are uncorrelated subqueries SQLite materializes once per query,
/// so presenting 100,000 rows costs two set lookups each, not two index
/// probes into `runs`. `?1` must be the project ID in every query using it.
const EFFECTIVE_STATE: &str = "CASE WHEN m.id IN (SELECT job_id FROM runs WHERE project_id=?1 AND status='queued' AND job_id IS NOT NULL) THEN 'queued' WHEN m.state='queued' AND m.id NOT IN (SELECT job_id FROM runs WHERE project_id=?1 AND job_id IS NOT NULL) THEN 'imported' ELSE m.state END";
const MAX_IDS_PER_QUERY: usize = 500;

/// Secret-free facts the controller derives from a row's plan.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QueueRowFacts {
    pub label: String,
    pub source_host: String,
    pub destination_host: String,
    /// Case-folded text matched by queue search.
    pub search_key: String,
    pub destructive: bool,
    /// The plan's destination-mutation policy identifier.
    pub policy: String,
}

/// One row to insert into a new batch queue.
#[derive(Clone, Debug)]
pub struct QueueInsert {
    pub source_mailbox: String,
    pub destination_mailbox: String,
    /// The plan's destination identity, as `normalized_destination_identity`
    /// would derive it from `config` (see `destination_identity_from_parts`).
    pub destination_identity: String,
    /// Secret-free serialized plan.
    pub config: String,
    /// Shared normalized policy for new queue rows.
    pub batch_plan_config: Option<String>,
    /// Compact mailbox identity/credential delta applied to the shared plan.
    pub row_overrides: Option<String>,
    pub facts: QueueRowFacts,
}

/// One queue row as presented.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueRow {
    pub rowid: i64,
    pub id: String,
    pub label: String,
    pub source_host: String,
    pub source_user: String,
    pub destination_host: String,
    pub destination_user: String,
    /// Durable state, or `imported` for a row no run has admitted yet.
    pub state: String,
    pub destructive: bool,
    pub policy: String,
    pub attention_reason: Option<AttentionReason>,
}

/// The fields selection accounting needs, for streaming every row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueScanRow {
    pub rowid: i64,
    pub id: String,
    /// Presentation state (`imported` for a never-admitted row).
    pub state: String,
    /// The durable state the admission policy evaluates.
    pub durable_state: String,
    pub destructive: bool,
}

/// A row's plan, for admission and execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuePlanRow {
    pub rowid: i64,
    pub id: String,
    pub state: String,
    pub label: String,
    pub config: Option<String>,
    pub row_overrides: Option<String>,
    pub destructive: bool,
}

/// A row whose presentation facts must be (re)derived from its plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueFactsRow {
    pub id: String,
    pub source_mailbox: String,
    pub destination_mailbox: String,
    pub config: Option<String>,
    pub row_overrides: Option<String>,
}

/// Aggregate selection projection used by the batch UI. The database counts
/// selected rows without materializing or visiting the rest of a large queue.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QueueSelectionSummary {
    pub selected_loaded: usize,
    pub live_eligible: usize,
    pub delta_eligible: usize,
    pub ready: usize,
    pub review: usize,
    pub visible: usize,
    pub selected_rowids: Vec<i64>,
}

/// Columns of a presented `QueueRow` from `mailbox_jobs m` joined to
/// `mailbox_queue_facts f`.
const ROW_COLUMNS: &str = "m.rowid,m.id,COALESCE(f.label,''),COALESCE(f.source_host,''),m.source_mailbox,COALESCE(f.destination_host,''),m.destination_mailbox,CASE WHEN m.id IN (SELECT job_id FROM runs WHERE project_id=?1 AND status='queued' AND job_id IS NOT NULL) THEN 'queued' WHEN m.state='queued' AND m.id NOT IN (SELECT job_id FROM runs WHERE project_id=?1 AND job_id IS NOT NULL) THEN 'imported' ELSE m.state END,f.destructive,m.attention_reason,COALESCE(f.policy,'')";

/// Insert or replace one row's facts; the job's own rowid keys them.
const FACTS_UPSERT: &str = "INSERT OR REPLACE INTO mailbox_queue_facts(job_rowid,job_id,project_id,label,source_host,destination_host,search_key,destructive,policy,state) SELECT rowid,id,project_id,?2,?3,?4,?5,?6,?7,state FROM mailbox_jobs WHERE id=?1";

fn upsert_facts(
    statement: &mut rusqlite::Statement<'_>,
    job_id: &str,
    facts: &QueueRowFacts,
) -> rusqlite::Result<()> {
    let inserted = statement.execute(params![
        job_id,
        facts.label,
        facts.source_host,
        facts.destination_host,
        facts.search_key,
        facts.destructive,
        facts.policy,
    ])?;
    if inserted != 1 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

/// `?2,?3,…` for `count` values following the project ID in `?1`.
fn numbered_placeholders(count: usize) -> String {
    (2..count + 2)
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn sql_bool(value: Option<i64>) -> bool {
    value.is_some_and(|value| value != 0)
}

impl StateStore {
    /// Create a batch project whose rows are the queue, in one transaction.
    pub fn create_batch_queue(
        &self,
        name: &str,
        source: &str,
        destination: &str,
        rows: &[QueueInsert],
    ) -> rusqlite::Result<(Project, Vec<String>)> {
        if rows.is_empty() || rows.len() > MAX_DURABLE_MAILBOX_ROWS {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let mut shared_configs = std::collections::HashSet::new();
        rows.iter().try_fold(0usize, |total, row| {
            let config = row.batch_plan_config.as_deref().unwrap_or(&row.config);
            if config.len() > MAX_PERSISTED_PROFILE_BYTES {
                return Err(rusqlite::Error::InvalidQuery);
            }
            if let Some(config) = row.batch_plan_config.as_ref() {
                shared_configs.insert(config.clone());
            }
            total
                .checked_add(if row.batch_plan_config.is_some() {
                    0
                } else {
                    row.config.len()
                })
                .filter(|value| *value <= MAX_TOTAL_PERSISTED_PROFILE_BYTES)
                .ok_or(rusqlite::Error::InvalidQuery)
        })?;
        let shared_bytes = shared_configs.iter().try_fold(0usize, |total, config| {
            total
                .checked_add(config.len())
                .filter(|value| *value <= MAX_TOTAL_PERSISTED_PROFILE_BYTES)
                .ok_or(rusqlite::Error::InvalidQuery)
        })?;
        if shared_bytes > MAX_TOTAL_PERSISTED_PROFILE_BYTES {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let mut destinations = BTreeSet::new();
        if rows
            .iter()
            .any(|row| !destinations.insert(row.destination_identity.as_str()))
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let project = Project {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            source_endpoint: source.into(),
            destination_endpoint: destination.into(),
            phase: Phase::Discovery,
        };
        // A large import inserts into several random-key B-trees (UUID
        // primary key and indexes). Give this one transaction a bigger page
        // cache, then restore the connection's previous size.
        let previous_cache: i64 = self
            .connection
            .query_row("PRAGMA cache_size", [], |row| row.get(0))?;
        self.connection
            .pragma_update(None, "cache_size", -(64 * 1024))?;
        let result = self.insert_batch_queue(&project, rows);
        self.connection
            .pragma_update(None, "cache_size", previous_cache)?;
        let ids = result?;
        Ok((project, ids))
    }

    fn insert_batch_queue(
        &self,
        project: &Project,
        rows: &[QueueInsert],
    ) -> rusqlite::Result<Vec<String>> {
        let tx = self.connection.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO projects(id,name,source_endpoint,destination_endpoint,phase) VALUES(?1,?2,?3,?4,?5)",
            params![project.id, project.name, project.source_endpoint, project.destination_endpoint, project.phase.as_str()],
        )?;
        let mut ids = Vec::with_capacity(rows.len());
        let mut plan_ids = std::collections::HashMap::<String, String>::new();
        for config in rows.iter().filter_map(|row| row.batch_plan_config.as_ref()) {
            if plan_ids.contains_key(config) {
                continue;
            }
            let id = Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO batch_plans(id,project_id,config) VALUES(?1,?2,?3)",
                params![id, project.id, config],
            )?;
            plan_ids.insert(config.clone(), id);
        }
        {
            let mut insert = tx.prepare(
                "INSERT INTO mailbox_jobs(id,project_id,source_mailbox,destination_mailbox,destination_identity,state,config,batch_plan_id,row_overrides) VALUES(?1,?2,?3,?4,?5,'queued',?6,?7,?8)",
            )?;
            let mut facts = tx.prepare(
                "INSERT INTO mailbox_queue_facts(job_rowid,job_id,project_id,label,source_host,destination_host,search_key,destructive,policy,state) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'queued')",
            )?;
            for row in rows {
                let id = Uuid::new_v4().to_string();
                insert.execute(params![
                    id,
                    project.id,
                    row.source_mailbox,
                    row.destination_mailbox,
                    row.destination_identity,
                    if row.batch_plan_config.is_some() {
                        None::<String>
                    } else {
                        Some(row.config.clone())
                    },
                    row.batch_plan_config
                        .as_ref()
                        .and_then(|config| plan_ids.get(config)),
                    row.row_overrides,
                ])?;
                facts.execute(params![
                    tx.last_insert_rowid(),
                    id,
                    project.id,
                    row.facts.label,
                    row.facts.source_host,
                    row.facts.destination_host,
                    row.facts.search_key,
                    row.facts.destructive,
                    row.facts.policy,
                ])?;
                ids.push(id);
            }
        }
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'project_created',?2)",
            params![
                project.id,
                format!("Batch created with {} mailbox jobs", ids.len())
            ],
        )?;
        tx.commit()?;
        Ok(ids)
    }

    pub fn queue_len(&self, project_id: &str) -> rusqlite::Result<usize> {
        self.connection.query_row(
            "SELECT COUNT(*) FROM mailbox_queue_facts WHERE project_id=?1",
            [project_id],
            |row| Ok(row.get::<_, i64>(0)? as usize),
        )
    }

    /// Row IDs matching a folded search and an optional effective state, in
    /// queue order. This is the queue's virtual-row index: eight bytes per
    /// matching row, with row contents fetched only for what is on screen.
    pub fn queue_rowids(
        &self,
        project_id: &str,
        folded_search: &str,
        state: Option<&str>,
    ) -> rusqlite::Result<Vec<i64>> {
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT f.job_rowid FROM mailbox_queue_facts f WHERE f.project_id=?1 AND (?2='' OR instr(f.search_key,?2)>0) AND (?3 IS NULL OR f.job_rowid IN (SELECT m.rowid FROM mailbox_jobs m WHERE m.project_id=?1 AND {EFFECTIVE_STATE}=?3)) ORDER BY f.job_rowid"
        ))?;
        statement
            .query_map(params![project_id, folded_search, state], |row| row.get(0))?
            .collect()
    }

    /// Whether one row matches a folded search.
    pub fn queue_row_matches(
        &self,
        project_id: &str,
        rowid: i64,
        folded_search: &str,
    ) -> rusqlite::Result<bool> {
        self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM mailbox_queue_facts WHERE project_id=?1 AND job_rowid=?2 AND (?3='' OR instr(search_key,?3)>0))",
            params![project_id, rowid, folded_search],
            |row| row.get(0),
        )
    }

    /// Rows for the given row IDs, in the order requested.
    pub fn queue_rows(&self, project_id: &str, rowids: &[i64]) -> rusqlite::Result<Vec<QueueRow>> {
        let mut by_rowid = std::collections::HashMap::with_capacity(rowids.len());
        for chunk in rowids.chunks(MAX_IDS_PER_QUERY) {
            let placeholders = numbered_placeholders(chunk.len());
            let mut statement = self.connection.prepare(&format!(
                "SELECT {ROW_COLUMNS} FROM mailbox_jobs m LEFT JOIN mailbox_queue_facts f ON f.job_rowid=m.rowid WHERE m.project_id=?1 AND m.rowid IN ({placeholders})"
            ))?;
            let values = std::iter::once(rusqlite::types::Value::Text(project_id.to_owned()))
                .chain(
                    chunk
                        .iter()
                        .map(|rowid| rusqlite::types::Value::Integer(*rowid)),
                );
            let rows = statement
                .query_map(params_from_iter(values), queue_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for row in rows {
                by_rowid.insert(row.rowid, row);
            }
        }
        Ok(rowids
            .iter()
            .filter_map(|rowid| by_rowid.remove(rowid))
            .collect())
    }

    pub fn queue_row(&self, project_id: &str, job_id: &str) -> rusqlite::Result<Option<QueueRow>> {
        self.connection
            .query_row(
                &format!(
                    "SELECT {ROW_COLUMNS} FROM mailbox_jobs m LEFT JOIN mailbox_queue_facts f ON f.job_rowid=m.rowid WHERE m.project_id=?1 AND m.id=?2"
                ),
                params![project_id, job_id],
                queue_row,
            )
            .optional()
    }

    /// Visit every row's selection-relevant fields in queue order without
    /// materializing the queue, for presentation. It reads only the narrow
    /// facts table, whose state copy a trigger keeps current; execution
    /// decisions use `queue_durable_scan` instead.
    pub fn queue_scan(
        &self,
        project_id: &str,
        mut visit: impl FnMut(QueueScanRow),
    ) -> rusqlite::Result<()> {
        let mut statement = self.connection.prepare_cached(
            "SELECT f.job_rowid,f.job_id,CASE WHEN f.job_id IN (SELECT job_id FROM runs WHERE project_id=?1 AND status='queued' AND job_id IS NOT NULL) THEN 'queued' WHEN f.state='queued' AND f.job_id NOT IN (SELECT job_id FROM runs WHERE project_id=?1 AND job_id IS NOT NULL) THEN 'imported' ELSE f.state END,f.destructive,f.state FROM mailbox_queue_facts f WHERE f.project_id=?1 ORDER BY f.job_rowid"
        )?;
        let mut rows = statement.query([project_id])?;
        while let Some(row) = rows.next()? {
            visit(QueueScanRow {
                rowid: row.get(0)?,
                id: row.get(1)?,
                state: row.get(2)?,
                durable_state: row.get(4)?,
                destructive: sql_bool(row.get(3)?),
            });
        }
        Ok(())
    }

    /// As `queue_scan`, reading states from `mailbox_jobs` itself. Admission
    /// and automation use this so execution never depends on a derived copy.
    pub fn queue_durable_scan(
        &self,
        project_id: &str,
        mut visit: impl FnMut(QueueScanRow),
    ) -> rusqlite::Result<()> {
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT m.rowid,m.id,{EFFECTIVE_STATE},f.destructive,m.state FROM mailbox_jobs m LEFT JOIN mailbox_queue_facts f ON f.job_rowid=m.rowid WHERE m.project_id=?1 ORDER BY m.rowid"
        ))?;
        let mut rows = statement.query([project_id])?;
        while let Some(row) = rows.next()? {
            visit(QueueScanRow {
                rowid: row.get(0)?,
                id: row.get(1)?,
                state: row.get(2)?,
                durable_state: row.get(4)?,
                destructive: sql_bool(row.get(3)?),
            });
        }
        Ok(())
    }

    /// Count of rows per presented state, for queue health.
    pub fn queue_state_counts(&self, project_id: &str) -> rusqlite::Result<Vec<(String, usize)>> {
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT {EFFECTIVE_STATE} AS effective,COUNT(*) FROM mailbox_jobs m WHERE m.project_id=?1 GROUP BY effective"
        ))?;
        statement
            .query_map([project_id], |row| {
                Ok((row.get(0)?, row.get::<_, i64>(1)? as usize))
            })?
            .collect()
    }

    /// Return queue IDs whose effective state is one of the requested values.
    /// The effective-state expression keeps imported and admitted queued rows
    /// consistent with the presentation projection used by the UI, while the
    /// database performs the filtering instead of materializing every queue
    /// row in Rust.
    pub fn queue_ids_by_effective_states(
        &self,
        project_id: &str,
        states: &[&str],
    ) -> rusqlite::Result<Vec<String>> {
        if states.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = numbered_placeholders(states.len());
        let sql = format!(
            "SELECT m.id FROM mailbox_jobs m WHERE m.project_id=?1 AND {EFFECTIVE_STATE} IN ({placeholders}) ORDER BY m.rowid"
        );
        let params = std::iter::once(project_id.to_owned())
            .chain(states.iter().map(|state| (*state).to_owned()));
        self.connection
            .prepare_cached(&sql)?
            .query_map(params_from_iter(params), |row| row.get(0))?
            .collect()
    }

    /// Return the first queue row not excluded by a compact select-all set.
    pub fn first_queue_job_excluding(
        &self,
        project_id: &str,
        excluded_ids: &std::collections::HashSet<String>,
    ) -> rusqlite::Result<Option<String>> {
        let excluded = excluded_ids.iter().cloned().collect::<Vec<_>>();
        let predicate = if excluded.is_empty() {
            String::new()
        } else {
            format!(" AND id NOT IN ({})", numbered_placeholders(excluded.len()))
        };
        let sql = format!(
            "SELECT id FROM mailbox_jobs WHERE project_id=?1{predicate} ORDER BY rowid LIMIT 1"
        );
        let params = std::iter::once(project_id.to_owned()).chain(excluded);
        self.connection
            .query_row(&sql, params_from_iter(params), |row| row.get(0))
            .optional()
    }

    /// Return the first existing queue row from an explicit selection.
    pub fn first_queue_job_in(
        &self,
        project_id: &str,
        selected_ids: &std::collections::HashSet<String>,
    ) -> rusqlite::Result<Option<String>> {
        let selected = selected_ids.iter().cloned().collect::<Vec<_>>();
        if selected.is_empty() {
            return Ok(None);
        }
        let mut first: Option<(i64, String)> = None;
        for chunk in selected.chunks(MAX_IDS_PER_QUERY) {
            let sql = format!(
                "SELECT rowid,id FROM mailbox_jobs WHERE project_id=?1 AND id IN ({}) ORDER BY rowid LIMIT 1",
                numbered_placeholders(chunk.len())
            );
            let params = std::iter::once(project_id.to_owned()).chain(chunk.iter().cloned());
            let candidate = self
                .connection
                .query_row(&sql, params_from_iter(params), |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .optional()?;
            if candidate
                .as_ref()
                .is_some_and(|(rowid, _)| first.as_ref().is_none_or(|current| *rowid < current.0))
            {
                first = candidate;
            }
        }
        Ok(first.map(|(_, id)| id))
    }

    /// Aggregate the selected-row projection in SQL. Explicit selections are
    /// queried in SQLite parameter-sized chunks; select-all uses one aggregate
    /// query and only pays for its compact exclusion set.
    pub fn queue_selection_summary(
        &self,
        project_id: &str,
        selected_ids: &[String],
        all_selected: bool,
        visible_rowids: &[i64],
    ) -> rusqlite::Result<QueueSelectionSummary> {
        let ids = if all_selected {
            Vec::new()
        } else {
            selected_ids.to_vec()
        };
        let mut summary = QueueSelectionSummary::default();
        let mut selected_rowids = Vec::new();
        let chunks = if all_selected {
            vec![selected_ids]
        } else {
            ids.chunks(MAX_IDS_PER_QUERY).collect::<Vec<_>>()
        };
        for chunk in chunks {
            let (predicate, values): (String, Vec<String>) = if all_selected {
                let predicate = if chunk.is_empty() {
                    String::new()
                } else {
                    format!(" AND m.id NOT IN ({})", numbered_placeholders(chunk.len()))
                };
                (predicate, chunk.to_vec())
            } else {
                if chunk.is_empty() {
                    continue;
                }
                (
                    format!(" AND m.id IN ({})", numbered_placeholders(chunk.len())),
                    chunk.to_vec(),
                )
            };
            let sql = format!(
                "SELECT COUNT(*), COALESCE(SUM(effective='ready'),0), COALESCE(SUM(effective IN ('attention','failed','cancelled','verification_difference')),0), COALESCE(SUM(durable_state IN ('queued','preflight','ready','running','completed','verified','verified_with_exceptions','failed','cancelled','attention','delta_required','verification_difference')),0), COALESCE(SUM(durable_state='delta_required'),0) FROM (SELECT m.state AS durable_state, {EFFECTIVE_STATE} AS effective FROM mailbox_jobs m WHERE m.project_id=?1{predicate})"
            );
            let params = std::iter::once(project_id.to_owned()).chain(values.iter().cloned());
            let row = self
                .connection
                .query_row(&sql, params_from_iter(params), |row| {
                    Ok((
                        row.get::<_, i64>(0)? as usize,
                        row.get::<_, i64>(1)? as usize,
                        row.get::<_, i64>(2)? as usize,
                        row.get::<_, i64>(3)? as usize,
                        row.get::<_, i64>(4)? as usize,
                    ))
                })?;
            summary.selected_loaded += row.0;
            summary.ready += row.1;
            summary.review += row.2;
            summary.live_eligible += row.3;
            summary.delta_eligible += row.4;
            if !all_selected {
                let rowid_sql = format!(
                    "SELECT m.rowid FROM mailbox_jobs m WHERE m.project_id=?1{predicate} ORDER BY m.rowid"
                );
                let params = std::iter::once(project_id.to_owned()).chain(values.iter().cloned());
                selected_rowids.extend(
                    self.connection
                        .prepare(&rowid_sql)?
                        .query_map(params_from_iter(params), |row| row.get(0))?
                        .collect::<rusqlite::Result<Vec<i64>>>()?,
                );
            }
        }
        summary.visible = if all_selected {
            let excluded = if selected_ids.is_empty() {
                0
            } else {
                let excluded = self.queue_rowids_for_ids(project_id, selected_ids)?;
                visible_rowids
                    .iter()
                    .filter(|rowid| excluded.contains(rowid))
                    .count()
            };
            visible_rowids.len().saturating_sub(excluded)
        } else {
            let visible = visible_rowids
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>();
            selected_rowids
                .iter()
                .filter(|rowid| visible.contains(rowid))
                .count()
        };
        summary.selected_rowids = selected_rowids;
        Ok(summary)
    }

    fn queue_rowids_for_ids(
        &self,
        project_id: &str,
        ids: &[String],
    ) -> rusqlite::Result<std::collections::HashSet<i64>> {
        let mut rowids = std::collections::HashSet::with_capacity(ids.len());
        for chunk in ids.chunks(MAX_IDS_PER_QUERY) {
            if chunk.is_empty() {
                continue;
            }
            let sql = format!(
                "SELECT rowid FROM mailbox_jobs WHERE project_id=?1 AND id IN ({})",
                numbered_placeholders(chunk.len())
            );
            let params = std::iter::once(project_id.to_owned()).chain(chunk.iter().cloned());
            rowids.extend(
                self.connection
                    .prepare(&sql)?
                    .query_map(params_from_iter(params), |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<i64>>>()?,
            );
        }
        Ok(rowids)
    }

    /// Plans of the given jobs, in the order requested. Missing IDs are
    /// omitted; callers compare lengths when every ID must resolve.
    pub fn queue_plans(
        &self,
        project_id: &str,
        job_ids: &[String],
    ) -> rusqlite::Result<Vec<QueuePlanRow>> {
        let mut by_id = std::collections::HashMap::with_capacity(job_ids.len());
        for chunk in job_ids.chunks(MAX_IDS_PER_QUERY) {
            let placeholders = numbered_placeholders(chunk.len());
            let mut statement = self.connection.prepare(&format!(
                "SELECT m.rowid,m.id,{EFFECTIVE_STATE},COALESCE(f.label,''),COALESCE(m.config,p.config),m.row_overrides,COALESCE(f.destructive,0) FROM mailbox_jobs m LEFT JOIN batch_plans p ON p.id=m.batch_plan_id LEFT JOIN mailbox_queue_facts f ON f.job_rowid=m.rowid WHERE m.project_id=?1 AND m.id IN ({placeholders})"
            ))?;
            let values = std::iter::once(project_id.to_owned()).chain(chunk.iter().cloned());
            let rows = statement
                .query_map(params_from_iter(values), queue_plan_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for row in rows {
                by_id.insert(row.id.clone(), row);
            }
        }
        Ok(job_ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }

    /// Visit every row's plan in queue order.
    pub fn queue_plan_scan(
        &self,
        project_id: &str,
        mut visit: impl FnMut(QueuePlanRow) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut statement = self
            .connection
            .prepare_cached(&format!(
                "SELECT m.rowid,m.id,{EFFECTIVE_STATE},COALESCE(f.label,''),COALESCE(m.config,p.config),m.row_overrides,COALESCE(f.destructive,0) FROM mailbox_jobs m LEFT JOIN batch_plans p ON p.id=m.batch_plan_id LEFT JOIN mailbox_queue_facts f ON f.job_rowid=m.rowid WHERE m.project_id=?1 ORDER BY m.rowid"
            ))
            .map_err(|error| error.to_string())?;
        let mut rows = statement
            .query([project_id])
            .map_err(|error| error.to_string())?;
        while let Some(row) = rows.next().map_err(|error| error.to_string())? {
            visit(queue_plan_row(row).map_err(|error| error.to_string())?)?;
        }
        Ok(())
    }

    /// Up to `limit` rows of batch projects whose presentation facts were
    /// never derived (rows written before queue facts were introduced).
    pub fn queue_rows_missing_facts(&self, limit: u32) -> rusqlite::Result<Vec<QueueFactsRow>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT m.id,m.source_mailbox,m.destination_mailbox,COALESCE(m.config,p.config),m.row_overrides FROM mailbox_jobs m LEFT JOIN batch_plans p ON p.id=m.batch_plan_id WHERE COALESCE(m.config,p.config) IS NOT NULL AND NOT EXISTS(SELECT 1 FROM mailbox_queue_facts f WHERE f.job_rowid=m.rowid) ORDER BY m.rowid LIMIT ?1",
        )?;
        statement
            .query_map([limit], |row| {
                Ok(QueueFactsRow {
                    id: row.get(0)?,
                    source_mailbox: row.get(1)?,
                    destination_mailbox: row.get(2)?,
                    config: row.get(3)?,
                    row_overrides: row.get(4)?,
                })
            })?
            .collect()
    }

    /// Store derived facts for existing rows in one transaction.
    pub fn set_queue_facts(&self, facts: &[(String, QueueRowFacts)]) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        {
            let mut upsert = tx.prepare_cached(FACTS_UPSERT)?;
            for (id, facts) in facts {
                upsert_facts(&mut upsert, id, facts)?;
            }
        }
        tx.commit()
    }

    /// Replace the plans (and derived facts) of rows that are not running,
    /// in one transaction. A changed plan no longer matches its preflight,
    /// so its recorded preflight plan is cleared.
    pub fn update_queue_plans(
        &self,
        project_id: &str,
        updates: &[(String, String, QueueRowFacts)],
    ) -> rusqlite::Result<usize> {
        if updates
            .iter()
            .any(|(_, config, _)| config.len() > MAX_PERSISTED_PROFILE_BYTES)
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.connection.unchecked_transaction()?;
        let active: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE project_id=?1 AND status IN ('queued','running'))",
            [project_id],
            |row| row.get(0),
        )?;
        if active {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let mut changed = 0;
        {
            let mut update = tx.prepare_cached(
                "UPDATE mailbox_jobs SET config=?3,row_overrides=NULL,preflight_plan=NULL WHERE project_id=?1 AND id=?2",
            )?;
            let mut upsert = tx.prepare_cached(FACTS_UPSERT)?;
            for (id, config, facts) in updates {
                let updated = update.execute(params![project_id, id, config])?;
                if updated == 1 {
                    upsert_facts(&mut upsert, id, facts)?;
                }
                changed += updated;
            }
        }
        if changed != updates.len() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.commit()?;
        Ok(changed)
    }
}

impl StateStore {
    #[cfg(test)]
    pub fn mailbox_destination_identity(&self, job_id: &str) -> rusqlite::Result<String> {
        self.connection.query_row(
            "SELECT destination_identity FROM mailbox_jobs WHERE id=?1",
            [job_id],
            |row| row.get(0),
        )
    }

    /// Record a durable attention reason directly, for tests and the debug
    /// demo scene.
    #[cfg(any(test, debug_assertions))]
    pub fn force_attention_reason(&self, job_id: &str, reason: &str) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE mailbox_jobs SET attention_reason=?2 WHERE id=?1",
            params![job_id, reason],
        )?;
        Ok(())
    }

    /// Write a mailbox's durable state without the transition rules, for
    /// tests and the debug demo scene that need rows in a given state.
    #[cfg(any(test, debug_assertions))]
    pub fn force_mailbox_state(&self, job_id: &str, state: &str) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE mailbox_jobs SET state=?2 WHERE id=?1",
            params![job_id, state],
        )?;
        Ok(())
    }
}

fn queue_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<QueueRow> {
    Ok(QueueRow {
        rowid: row.get(0)?,
        id: row.get(1)?,
        label: row.get(2)?,
        source_host: row.get(3)?,
        source_user: row.get(4)?,
        destination_host: row.get(5)?,
        destination_user: row.get(6)?,
        state: row.get(7)?,
        destructive: sql_bool(row.get(8)?),
        attention_reason: row
            .get::<_, Option<String>>(9)?
            .map(|value| AttentionReason::parse(&value).unwrap_or(AttentionReason::Unknown)),
        policy: row.get(10)?,
    })
}

fn queue_plan_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<QueuePlanRow> {
    Ok(QueuePlanRow {
        rowid: row.get(0)?,
        id: row.get(1)?,
        state: row.get(2)?,
        label: row.get(3)?,
        config: row.get(4)?,
        row_overrides: row.get(5)?,
        destructive: sql_bool(row.get(6)?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert(index: usize, destructive: bool) -> QueueInsert {
        let user = format!("user{index}@example.test");
        QueueInsert {
            source_mailbox: user.clone(),
            destination_mailbox: user.clone(),
            destination_identity: super::super::policy::normalized_destination_identity(
                &user,
                Some(&format!(
                    "destination_host = \"new.example\"\ndestination_user = \"{user}\"\n"
                )),
            ),
            config: format!("destination_host = \"new.example\"\ndestination_user = \"{user}\"\n"),
            batch_plan_config: None,
            row_overrides: None,
            facts: QueueRowFacts {
                label: format!("Row {index}"),
                source_host: "old.example".into(),
                destination_host: "new.example".into(),
                search_key: format!("row {index} old.example {user} new.example {user}"),
                destructive,
                policy: "additive".into(),
            },
        }
    }

    fn queue(store: &StateStore, count: usize) -> (String, Vec<String>) {
        let rows = (0..count)
            .map(|index| insert(index, index == 1))
            .collect::<Vec<_>>();
        let (project, ids) = store
            .create_batch_queue("queue", "old.example", "new.example", &rows)
            .unwrap();
        (project.id, ids)
    }

    #[test]
    fn queue_reads_filter_page_and_count_without_materializing_plans() {
        let store = StateStore::in_memory().unwrap();
        let (project, ids) = queue(&store, 12);
        assert_eq!(store.queue_len(&project).unwrap(), 12);
        let all = store.queue_rowids(&project, "", None).unwrap();
        assert_eq!(all.len(), 12);
        assert!(all.windows(2).all(|pair| pair[0] < pair[1]));
        // "user1" matches user1, user10, user11.
        let matching = store.queue_rowids(&project, "user1", None).unwrap();
        assert_eq!(matching.len(), 3);
        // A never-admitted row is presented as imported.
        assert_eq!(
            store
                .queue_rowids(&project, "", Some("imported"))
                .unwrap()
                .len(),
            12
        );
        assert!(
            store
                .queue_rowids(&project, "", Some("ready"))
                .unwrap()
                .is_empty()
        );

        let page = store.queue_rows(&project, &[all[3], all[1]]).unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].id, ids[3]);
        assert_eq!(page[1].label, "Row 1");
        assert!(page[1].destructive);
        assert_eq!(page[1].state, "imported");
        assert_eq!(page[1].source_host, "old.example");

        assert_eq!(
            store.queue_state_counts(&project).unwrap(),
            vec![("imported".to_owned(), 12)]
        );
        let mut scanned = Vec::new();
        store
            .queue_scan(&project, |row| scanned.push((row.id, row.destructive)))
            .unwrap();
        assert_eq!(scanned.len(), 12);
        assert_eq!(scanned[1], (ids[1].clone(), true));

        let plans = store
            .queue_plans(
                &project,
                &[ids[5].clone(), "missing".into(), ids[0].clone()],
            )
            .unwrap();
        assert_eq!(
            plans.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            [ids[5].as_str(), ids[0].as_str()]
        );
        assert!(plans[0].config.as_deref().unwrap().contains("user5"));
    }

    #[test]
    fn plan_updates_clear_preflight_and_refuse_while_a_run_is_active() {
        let store = StateStore::in_memory().unwrap();
        let (project, ids) = queue(&store, 2);
        store
            .connection
            .execute(
                "UPDATE mailbox_jobs SET preflight_plan='digest' WHERE id=?1",
                [&ids[0]],
            )
            .unwrap();
        let mut facts = insert(0, false).facts;
        facts.label = "Renamed".into();
        store
            .update_queue_plans(
                &project,
                &[(ids[0].clone(), "config = 1\n".into(), facts.clone())],
            )
            .unwrap();
        let (preflight, label): (Option<String>, String) = store
            .connection
            .query_row(
                "SELECT m.preflight_plan,f.label FROM mailbox_jobs m JOIN mailbox_queue_facts f ON f.job_rowid=m.rowid WHERE m.id=?1",
                [&ids[0]],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((preflight, label.as_str()), (None, "Renamed"));
        assert!(
            store
                .update_queue_plans(&project, &[("missing".into(), "x".into(), facts.clone())])
                .is_err()
        );
        store
            .insert_run_for_test(&project, Some(&ids[1]), "active-run", "imapsync")
            .unwrap();
        assert!(
            store
                .update_queue_plans(&project, &[(ids[0].clone(), "y".into(), facts)])
                .is_err()
        );
        // Once a run admitted it, the queued row is no longer "imported".
        assert_eq!(
            store.queue_row(&project, &ids[1]).unwrap().unwrap().state,
            "queued"
        );
    }

    #[test]
    fn presentation_state_copy_follows_every_durable_state_write() {
        let store = StateStore::in_memory().unwrap();
        let (project, ids) = queue(&store, 3);
        store.set_mailbox_state(&ids[1], "ready").unwrap();
        store
            .connection
            .execute(
                "UPDATE mailbox_jobs SET state='failed' WHERE id=?1",
                [&ids[2]],
            )
            .unwrap();
        let mut presented = Vec::new();
        store
            .queue_scan(&project, |row| presented.push(row.state))
            .unwrap();
        let mut durable = Vec::new();
        store
            .queue_durable_scan(&project, |row| durable.push(row.state))
            .unwrap();
        assert_eq!(presented, ["imported", "ready", "failed"]);
        assert_eq!(presented, durable);
        let counts = store
            .queue_state_counts(&project)
            .unwrap()
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            (counts["ready"], counts["failed"], counts["imported"]),
            (1, 1, 1)
        );
    }

    #[test]
    fn legacy_rows_are_listed_for_fact_backfill_until_set() {
        let store = StateStore::in_memory().unwrap();
        let (project, ids) = queue(&store, 3);
        store
            .connection
            .execute(
                "DELETE FROM mailbox_queue_facts WHERE project_id=?1",
                [&project],
            )
            .unwrap();
        let missing = store.queue_rows_missing_facts(2).unwrap();
        assert_eq!(missing.len(), 2);
        assert_eq!(missing[0].id, ids[0]);
        store
            .set_queue_facts(
                &missing
                    .iter()
                    .map(|row| (row.id.clone(), insert(9, false).facts))
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        assert_eq!(store.queue_rows_missing_facts(10).unwrap().len(), 1);
    }
}
