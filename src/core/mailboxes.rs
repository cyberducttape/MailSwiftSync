use super::*;

impl StateStore {
    #[cfg(test)]
    pub fn add_mailbox(
        &self,
        project_id: &str,
        source: &str,
        destination: &str,
    ) -> rusqlite::Result<String> {
        let phase: String = self.connection.query_row(
            "SELECT phase FROM projects WHERE id=?1",
            [project_id],
            |row| row.get(0),
        )?;
        if phase == Phase::Complete.as_str() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let identity = normalized_destination_identity(destination, None);
        let duplicate: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM mailbox_jobs WHERE project_id=?1 AND (destination_identity=?2 OR (destination_identity='' AND lower(trim(destination_mailbox))=lower(trim(?3)))) )",
            params![project_id, identity, destination],
            |row| row.get(0),
        )?;
        if duplicate {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let id = Uuid::new_v4().to_string();
        let tx = self.connection.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO mailbox_jobs(id,project_id,source_mailbox,destination_mailbox,destination_identity,state) VALUES(?1,?2,?3,?4,?5,'queued')",
            params![id, project_id, source, destination, identity],
        )?;
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'mailbox_added',?2)",
            params![project_id, format!("Mailbox {destination} added")],
        )?;
        tx.commit()?;
        Ok(id)
    }
    #[cfg(test)]
    pub fn set_mailbox_state(&self, job_id: &str, state: &str) -> rusqlite::Result<()> {
        if !matches!(
            state,
            "queued"
                | "preflight"
                | "ready"
                | "running"
                | "delta_required"
                | "verification_difference"
                | "completed"
                | "failed"
                | "cancelled"
                | "attention"
        ) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        // A running mailbox must always have a durable execution owner. Only
        // begin_run() and the batch child-claim path establish that pairing;
        // a generic state update must not create an unowned running row.
        if state == "running" {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let (current, phase): (String, String) = self.connection.query_row(
            "SELECT j.state,p.phase FROM mailbox_jobs j JOIN projects p ON p.id=j.project_id WHERE j.id=?1",
            [job_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if phase == Phase::Complete.as_str() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        // Verification is owned by the evidence terminal methods below. A
        // generic state setter must not reuse evidence from an older run to
        // manufacture a verified result for a newer one.
        if state == "verified" {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if current != state && !valid_mailbox_transition(&current, state) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let changed = self.connection.execute(
            "UPDATE mailbox_jobs SET state=?, attention_reason=CASE WHEN ?='attention' THEN COALESCE(attention_reason,'unknown') ELSE NULL END, attempt=CASE WHEN ?='running' AND state<>'running' THEN attempt+1 ELSE attempt END WHERE id=?",
            params![state, state, state, job_id],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        Ok(())
    }
    pub fn mailbox_state(&self, job_id: &str) -> rusqlite::Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT state FROM mailbox_jobs WHERE id=?1",
                [job_id],
                |row| row.get(0),
            )
            .optional()
    }

    /// Return the durable reason and operator action for an attention row.
    /// Unknown legacy values are deliberately surfaced as `Unknown` rather
    /// than guessed from UI text.
    pub fn mailbox_attention_reason(
        &self,
        job_id: &str,
    ) -> rusqlite::Result<Option<AttentionReason>> {
        self.connection
            .query_row(
                "SELECT attention_reason FROM mailbox_jobs WHERE id=?1",
                [job_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(|value| {
                value.flatten().map(|reason| {
                    AttentionReason::parse(&reason).unwrap_or(AttentionReason::Unknown)
                })
            })
    }

    /// Load all durable attention reasons for a project in one read. Missing
    /// reasons are intentionally absent from the map; callers can distinguish
    /// a normal row from an explicitly classified operator-review row without
    /// issuing one query per mailbox.
    pub fn mailbox_attention_reasons(
        &self,
        project_id: &str,
    ) -> rusqlite::Result<HashMap<String, AttentionReason>> {
        let mut statement = self.connection.prepare(
            "SELECT id,attention_reason FROM mailbox_jobs WHERE project_id=?1 AND attention_reason IS NOT NULL",
        )?;
        let mut reasons = HashMap::new();
        for row in statement.query_map([project_id], |row| {
            let reason: String = row.get(1)?;
            Ok((
                row.get::<_, String>(0)?,
                AttentionReason::parse(&reason).unwrap_or(AttentionReason::Unknown),
            ))
        })? {
            let (job_id, reason) = row?;
            reasons.insert(job_id, reason);
        }
        Ok(reasons)
    }
    /// Return the last committed Dovecot stateful-sync checkpoint for a
    /// mailbox. The value is intentionally read separately from credentials;
    /// it contains engine state, not authentication material.
    pub fn mailbox_checkpoint(&self, job_id: &str) -> rusqlite::Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT checkpoint FROM mailbox_jobs WHERE id=?1",
                [job_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(|value| value.flatten())
    }

    /// Read all durable facts needed for batch admission in one consistent
    /// query. The returned vector always follows `job_ids` order and rejects
    /// an ID that is missing from the selected project.
    pub fn batch_admission_states(
        &self,
        project_id: &str,
        job_ids: &[String],
    ) -> rusqlite::Result<Vec<BatchAdmissionState>> {
        if job_ids.is_empty() {
            return Ok(Vec::new());
        }
        // Keep the number of host parameters comfortably below SQLite's
        // commonly configured limit. This matters for large MSP batches,
        // while retaining the single-query-per-chunk behavior that avoids an
        // N+1 admission scan.
        const MAX_IDS_PER_QUERY: usize = 500;
        let mut by_id = HashMap::with_capacity(job_ids.len());
        for chunk in job_ids.chunks(MAX_IDS_PER_QUERY) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT id,state,preflight_plan,checkpoint FROM mailbox_jobs WHERE project_id=?1 AND id IN ({placeholders})"
            );
            let mut values = Vec::with_capacity(chunk.len() + 1);
            values.push(project_id.to_owned());
            values.extend(chunk.iter().cloned());
            let mut statement = self.connection.prepare(&sql)?;
            let rows = statement
                .query_map(rusqlite::params_from_iter(values.iter()), |row| {
                    Ok(BatchAdmissionState {
                        job_id: row.get(0)?,
                        state: row.get(1)?,
                        preflight_plan: row.get(2)?,
                        checkpoint: row.get(3)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for row in rows {
                by_id.insert(row.job_id.clone(), row);
            }
        }
        job_ids
            .iter()
            .map(|job_id| {
                by_id
                    .remove(job_id)
                    .ok_or(rusqlite::Error::QueryReturnedNoRows)
            })
            .collect()
    }
    /// Persist the opaque digest of a preflighted plan. Callers should pass a
    /// canonical plan hash rather than generated command arguments; the
    /// stored value is used only for exact equality during live admission.
    pub fn set_preflight_plan(&self, job_id: &str, plan: &str) -> rusqlite::Result<()> {
        if plan.len() != 64 || !plan.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.connection.unchecked_transaction()?;
        let phase: String = tx.query_row(
            "SELECT p.phase FROM mailbox_jobs j JOIN projects p ON p.id=j.project_id WHERE j.id=?1",
            [job_id],
            |row| row.get(0),
        )?;
        if phase == Phase::Complete.as_str() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let changed = tx.execute(
            // A Dovecot state token is only meaningful for the plan that
            // produced it. Keep it for a repeated preflight of the same
            // plan, but invalidate it when a new plan is preflighted so a
            // later live run cannot resume across endpoint or policy drift.
            "UPDATE mailbox_jobs SET checkpoint=CASE WHEN preflight_plan=?1 THEN checkpoint ELSE NULL END, preflight_plan=?1 WHERE id=?2",
            params![plan, job_id],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        tx.commit()
    }
    pub fn preflight_plan(&self, job_id: &str) -> rusqlite::Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT preflight_plan FROM mailbox_jobs WHERE id=?1",
                [job_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(|value| value.flatten())
    }
}
