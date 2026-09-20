use super::*;

impl StateStore {
    /// Atomically records a run and moves its mailbox into `running`.
    /// Keeping these writes together prevents restart recovery from seeing a
    /// running mailbox without the run record needed to explain it.
    #[cfg(test)]
    pub fn begin_run(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
        engine: &str,
    ) -> rusqlite::Result<()> {
        self.begin_run_with_snapshot(project_id, job_id, run_id, engine, "")
    }
    /// Atomically starts a mailbox run and persists its immutable, secret-free
    /// execution-plan snapshot (with session passwords excluded).
    pub fn begin_run_with_snapshot(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
        engine: &str,
        plan_snapshot: &str,
    ) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let (current, phase_at_start): (String, String) = tx.query_row(
            "SELECT j.state,p.phase FROM mailbox_jobs j JOIN projects p ON p.id=j.project_id WHERE j.id=?1 AND j.project_id=?2",
            params![job_id, project_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if phase_at_start == Phase::Complete.as_str() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        // A mailbox may never have two active runs. Recovery must first move
        // the previous run to an operator-review state before retrying it.
        if current == "running" || !valid_mailbox_transition(&current, "running") {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let active_run_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE project_id=?1 AND job_id=?2 AND status IN ('queued','running'))",
            params![project_id, job_id],
            |row| row.get(0),
        )?;
        if active_run_exists {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute(
            "INSERT INTO runs(id,project_id,job_id,engine,phase_at_start,plan_snapshot,status) VALUES(?1,?2,?3,?4,?5,?6,'running')",
            params![run_id, project_id, job_id, engine, phase_at_start, plan_snapshot],
        )?;
        tx.execute(
            "UPDATE mailbox_jobs SET state='running',attention_reason=NULL,attempt=CASE WHEN state<>'running' THEN attempt+1 ELSE attempt END WHERE id=?1",
            [job_id],
        )?;
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'run_started',?3)",
            params![project_id, run_id, format!("{engine} ({run_id})")],
        )?;
        tx.commit()
    }
    /// Start a batch as one durable boundary. Child mailboxes remain queued
    /// until an individual worker claims them, so durable state reflects work
    /// that has actually reached the execution layer.
    #[cfg(test)]
    pub fn begin_batch_run(
        &self,
        project_id: &str,
        job_ids: &[String],
        run_id: &str,
        engine: &str,
        expected_plans: &[String],
    ) -> rusqlite::Result<()> {
        self.begin_batch_run_with_snapshot(project_id, job_ids, run_id, engine, expected_plans, "")
    }
    /// Atomically starts a batch and stores its immutable, secret-free plan
    /// snapshot alongside the parent run (with session passwords excluded).
    #[cfg(test)]
    pub fn begin_batch_run_with_snapshot(
        &self,
        project_id: &str,
        job_ids: &[String],
        run_id: &str,
        engine: &str,
        expected_plans: &[String],
        plan_snapshot: &str,
    ) -> rusqlite::Result<()> {
        self.begin_batch_run_with_children(
            project_id,
            job_ids,
            run_id,
            engine,
            expected_plans,
            plan_snapshot,
            &[],
        )
        .map(|_| ())
    }

    /// Atomically starts a batch parent and one durable child run per
    /// mailbox. Child runs and mailbox rows remain queued until workers claim
    /// them, so durable state reflects actual worker ownership.
    #[allow(clippy::too_many_arguments)]
    pub fn begin_batch_run_with_children(
        &self,
        project_id: &str,
        job_ids: &[String],
        run_id: &str,
        engine: &str,
        expected_plans: &[String],
        plan_snapshot: &str,
        child_plans: &[BatchChildPlan],
    ) -> rusqlite::Result<Vec<String>> {
        let mut unique_job_ids = BTreeSet::new();
        let duplicate_job_id = job_ids.iter().any(|job_id| !unique_job_ids.insert(job_id));
        if job_ids.is_empty()
            || duplicate_job_id
            || (!expected_plans.is_empty() && expected_plans.len() != job_ids.len())
            || (!child_plans.is_empty() && child_plans.len() != job_ids.len())
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.connection.unchecked_transaction()?;
        let phase_at_start: String = tx.query_row(
            "SELECT phase FROM projects WHERE id=?1",
            [project_id],
            |row| row.get(0),
        )?;
        if phase_at_start == Phase::Complete.as_str() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let mut destinations = BTreeSet::new();
        for (index, job_id) in job_ids.iter().enumerate() {
            let (
                current,
                destination,
                destination_identity,
                actual_plan,
                config,
                active_run_exists,
            ): (
                String,
                String,
                String,
                Option<String>,
                Option<String>,
                bool,
            ) = tx.query_row(
                "SELECT j.state,j.destination_mailbox,j.destination_identity,j.preflight_plan,j.config,
                        EXISTS(SELECT 1 FROM runs r WHERE r.project_id=j.project_id AND r.job_id=j.id AND r.status IN ('queued','running'))
                 FROM mailbox_jobs j WHERE j.id=?1 AND j.project_id=?2",
                params![job_id, project_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )?;
            let identity = if destination_identity.is_empty() {
                normalized_destination_identity(&destination, config.as_deref())
            } else {
                destination_identity
            };
            if !destinations.insert(identity) {
                return Err(rusqlite::Error::InvalidQuery);
            }
            // Reject an already-running child rather than treating it as a
            // harmless retry. This keeps one durable execution owner per
            // mailbox even when two callers race.
            if current == "running" || !valid_mailbox_transition(&current, "running") {
                return Err(rusqlite::Error::InvalidQuery);
            }
            if active_run_exists {
                return Err(rusqlite::Error::InvalidQuery);
            }
            if let Some(expected_plan) = expected_plans.get(index)
                && actual_plan.as_deref() != Some(expected_plan.as_str())
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        tx.execute(
            "INSERT INTO runs(id,project_id,job_id,engine,phase_at_start,plan_snapshot,status) VALUES(?1,?2,NULL,?3,?4,?5,'running')",
            params![run_id, project_id, engine, phase_at_start, plan_snapshot],
        )?;
        let mut child_run_ids = Vec::with_capacity(job_ids.len());
        for (index, job_id) in job_ids.iter().enumerate() {
            let child_run_id = Uuid::new_v4().to_string();
            let child_plan = child_plans.get(index);
            let child_engine = child_plan
                .map(|plan| plan.engine.as_str())
                .unwrap_or(engine);
            let child_snapshot = child_plan
                .map(|plan| plan.plan_snapshot.as_str())
                .unwrap_or("");
            tx.execute(
                "INSERT INTO runs(id,project_id,job_id,parent_run_id,engine,phase_at_start,plan_snapshot,status) VALUES(?1,?2,?3,?4,?5,?6,?7,'queued')",
                params![
                    child_run_id,
                    project_id,
                    job_id,
                    run_id,
                    child_engine,
                    phase_at_start,
                    child_snapshot
                ],
            )?;
            if let Some(version) = child_plan.and_then(|plan| plan.engine_version.as_deref()) {
                tx.execute(
                    "INSERT INTO engine_versions(run_id,version) VALUES(?1,?2)",
                    params![child_run_id, version],
                )?;
            }
            child_run_ids.push(child_run_id);
        }
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'run_started',?3)",
            params![
                project_id,
                run_id,
                format!("{engine} ({run_id}); {} child jobs", job_ids.len())
            ],
        )?;
        tx.commit().map(|()| child_run_ids)
    }
    /// Legacy test fixture for the pre-child batch model. Production callers
    /// must use `claim_batch_mailbox_for_child`, which binds the mailbox,
    /// parent wave, and child run in one transaction. Keeping this helper
    /// test-only prevents an external caller from accidentally bypassing the
    /// mailbox-specific child ownership invariant.
    #[cfg(test)]
    pub fn claim_batch_mailbox(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
    ) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let parent_is_running: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND project_id=?2 AND job_id IS NULL AND status='running')",
            params![run_id, project_id],
            |row| row.get(0),
        )?;
        if !parent_is_running {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let (current, phase): (String, String) = tx.query_row(
            "SELECT j.state,p.phase FROM mailbox_jobs j JOIN projects p ON p.id=j.project_id WHERE j.id=?1 AND j.project_id=?2",
            params![job_id, project_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if phase == Phase::Complete.as_str() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if current == "running" {
            return tx.commit();
        }
        if !valid_mailbox_transition(&current, "running") {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute(
            "UPDATE mailbox_jobs SET state='running',attention_reason=NULL,attempt=attempt+1 WHERE id=?1 AND project_id=?2",
            params![job_id, project_id],
        )?;
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'mailbox_claimed',?3)",
            params![
                project_id,
                run_id,
                format!("{job_id} claimed by batch run {run_id}")
            ],
        )?;
        tx.commit()
    }
    /// Claim a mailbox for its durable child run. The parent run proves that
    /// this belongs to the active batch, while the child relation prevents a
    /// worker from attaching a process to another row in the same project.
    pub fn claim_batch_mailbox_for_child(
        &self,
        project_id: &str,
        job_id: &str,
        parent_run_id: &str,
        child_run_id: &str,
    ) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let parent_is_running: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND project_id=?2 AND job_id IS NULL AND status='running')",
            params![parent_run_id, project_id],
            |row| row.get(0),
        )?;
        let child_status: String = tx.query_row(
            "SELECT status FROM runs WHERE id=?1 AND project_id=?2 AND job_id=?3 AND parent_run_id=?4",
            params![child_run_id, project_id, job_id, parent_run_id],
            |row| row.get(0),
        )?;
        if !parent_is_running || !matches!(child_status.as_str(), "queued" | "running") {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let (current, phase): (String, String) = tx.query_row(
            "SELECT j.state,p.phase FROM mailbox_jobs j JOIN projects p ON p.id=j.project_id WHERE j.id=?1 AND j.project_id=?2",
            params![job_id, project_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if phase == Phase::Complete.as_str() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if current == "running" {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if !valid_mailbox_transition(&current, "running") {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute(
            "UPDATE mailbox_jobs SET state='running',attention_reason=NULL,attempt=attempt+1 WHERE id=?1 AND project_id=?2",
            params![job_id, project_id],
        )?;
        tx.execute(
            "UPDATE runs SET status='running' WHERE id=?1 AND project_id=?2 AND job_id=?3 AND parent_run_id=?4 AND status='queued'",
            params![child_run_id, project_id, job_id, parent_run_id],
        )?;
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'mailbox_claimed',?3)",
            params![
                project_id,
                child_run_id,
                format!("{job_id} claimed by child run {child_run_id}")
            ],
        )?;
        tx.commit()
    }

    /// Test-only legacy helper. Production retries keep the original child
    /// claim and run ownership across internal attempts; releasing ownership
    /// between attempts reintroduces UI timing races.
    #[cfg(test)]
    pub fn release_batch_mailbox_for_retry(
        &self,
        project_id: &str,
        job_id: &str,
        parent_run_id: &str,
        child_run_id: &str,
    ) -> rusqlite::Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let parent_is_running: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND project_id=?2 AND job_id IS NULL AND status='running')",
            params![parent_run_id, project_id],
            |row| row.get(0),
        )?;
        let child_is_running: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND project_id=?2 AND job_id=?3 AND parent_run_id=?4 AND status='running')",
            params![child_run_id, project_id, job_id, parent_run_id],
            |row| row.get(0),
        )?;
        let mailbox_is_running: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM mailbox_jobs WHERE id=?1 AND project_id=?2 AND state='running')",
            params![job_id, project_id],
            |row| row.get(0),
        )?;
        if !parent_is_running || !child_is_running || !mailbox_is_running {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute(
            "UPDATE runs SET status='queued',detail='released for transient retry' WHERE id=?1 AND project_id=?2 AND job_id=?3 AND parent_run_id=?4 AND status='running'",
            params![child_run_id, project_id, job_id, parent_run_id],
        )?;
        tx.execute(
            "UPDATE mailbox_jobs SET state='ready',attention_reason=NULL WHERE id=?1 AND project_id=?2 AND state='running'",
            params![job_id, project_id],
        )?;
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'mailbox_retry_released',?3)",
            params![project_id, child_run_id, format!("{job_id} released for transient retry")],
        )?;
        tx.commit()
    }
    pub fn finish_run(&self, run_id: &str, status: &str, detail: &str) -> rusqlite::Result<()> {
        if !matches!(
            status,
            "completed" | "failed" | "cancelled" | "abandoned" | "verification_failed"
        ) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.connection.unchecked_transaction()?;
        let project_id: String = tx.query_row(
            "SELECT project_id FROM runs WHERE id=?1 AND status='running'",
            [run_id],
            |row| row.get(0),
        )?;
        // A parent batch is not complete while any child is still queued or
        // running.  Keep this invariant in the store so a controller bug or
        // partial event stream cannot produce a deceptively terminal batch.
        let has_unfinished_children: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE parent_run_id=?1 AND status IN ('queued','running'))",
            [run_id],
            |row| row.get(0),
        )?;
        if has_unfinished_children {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let has_unsuccessful_children: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE parent_run_id=?1 AND status<>'completed')",
            [run_id],
            |row| row.get(0),
        )?;
        if status == "completed" && has_unsuccessful_children {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let changed = tx.execute(
            "UPDATE runs SET status=?1,finished_at=CURRENT_TIMESTAMP,detail=?2 WHERE id=?3 AND status='running'",
            params![status, detail, run_id],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        tx.execute("DELETE FROM active_processes WHERE run_id=?1", [run_id])?;
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'run_finished',?3)",
            params![project_id, run_id, detail],
        )?;
        tx.commit()
    }
    /// Atomically completes a single-mailbox run and records the durable
    /// mailbox state.  Completion is deliberately one transaction: a run
    /// must never be marked finished while its mailbox remains `running` (or
    /// vice versa) after a database failure or process interruption.
    #[cfg(test)]
    pub fn finish_run_for_mailbox(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
        run_status: &str,
        mailbox_state: &str,
        detail: &str,
    ) -> rusqlite::Result<()> {
        self.finish_run_for_mailbox_with_checkpoint(
            project_id,
            job_id,
            run_id,
            run_status,
            mailbox_state,
            detail,
            None,
        )
    }

    /// Atomically completes a mailbox run and optionally advances its
    /// Dovecot stateful-sync checkpoint. A checkpoint is written only as part
    /// of the same terminal transaction, so a process result and its resume
    /// state can never diverge in the ledger.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_run_for_mailbox_with_checkpoint(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
        run_status: &str,
        mailbox_state: &str,
        detail: &str,
        checkpoint: Option<&str>,
    ) -> rusqlite::Result<()> {
        self.finish_run_for_mailbox_with_preflight_plan_and_checkpoint(
            project_id,
            job_id,
            run_id,
            run_status,
            mailbox_state,
            detail,
            None,
            checkpoint,
        )
    }

    /// Atomically completes a mailbox run, optionally records its successful
    /// dry-preflight digest, and optionally advances its Dovecot checkpoint.
    /// The two digests are deliberately committed with the terminal state so
    /// a live batch cannot observe a ready child without its preflight proof.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_run_for_mailbox_with_preflight_plan_and_checkpoint(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
        run_status: &str,
        mailbox_state: &str,
        detail: &str,
        preflight_plan: Option<&str>,
        checkpoint: Option<&str>,
    ) -> rusqlite::Result<()> {
        if preflight_plan.is_some_and(|value| {
            value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if checkpoint.is_some_and(|value| !valid_dovecot_checkpoint(value)) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if checkpoint.is_some() && run_status != "completed" {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if !matches!(
            run_status,
            "completed" | "failed" | "cancelled" | "verification_failed"
        ) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if !matches!(
            mailbox_state,
            "ready"
                | "completed"
                | "verified"
                | "delta_required"
                | "verification_difference"
                | "failed"
                | "cancelled"
                | "attention"
        ) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let attention_reason =
            attention_reason_for(mailbox_state, detail).map(AttentionReason::as_str);
        let tx = self.connection.unchecked_transaction()?;
        let current: String = tx.query_row(
            "SELECT state FROM mailbox_jobs WHERE id=?1 AND project_id=?2",
            params![job_id, project_id],
            |row| row.get(0),
        )?;
        let terminal_pair_is_valid = match run_status {
            "completed" => matches!(
                mailbox_state,
                "ready" | "completed" | "delta_required" | "verification_difference" | "attention"
            ),
            "failed" => matches!(mailbox_state, "failed" | "attention"),
            "cancelled" => matches!(mailbox_state, "cancelled" | "attention"),
            "verification_failed" => {
                matches!(mailbox_state, "attention" | "verification_difference")
            }
            _ => false,
        };
        if !terminal_pair_is_valid {
            return Err(rusqlite::Error::InvalidQuery);
        }
        // Verification must be committed together with the evidence that
        // proves this run.  Merely finding an older evidence row is not
        // sufficient: otherwise a later run could reuse stale evidence and
        // manufacture `verified` through this generic terminal path.
        if mailbox_state == "verified" {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if current != mailbox_state && !valid_mailbox_transition(&current, mailbox_state) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let run_changed = tx.execute(
            "UPDATE runs SET status=?1,finished_at=CURRENT_TIMESTAMP,detail=?2 WHERE id=?3 AND project_id=?4 AND job_id=?5 AND (status='running' OR (status='queued' AND ?1 IN ('cancelled','failed','verification_failed'))) ",
            params![run_status, detail, run_id, project_id, job_id],
        )?;
        if run_changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        tx.execute(
            "UPDATE mailbox_jobs SET state=?1,attention_reason=?2,preflight_plan=COALESCE(?3,preflight_plan),checkpoint=COALESCE(?4,checkpoint) WHERE id=?5 AND project_id=?6",
            params![mailbox_state, attention_reason, preflight_plan, checkpoint, job_id, project_id],
        )?;
        tx.execute("DELETE FROM active_processes WHERE run_id=?1", [run_id])?;
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'run_finished',?3)",
            params![
                project_id,
                run_id,
                if run_status == "completed" {
                    "success"
                } else {
                    "failure"
                }
            ],
        )?;
        tx.commit()
    }
    /// Atomically records terminal verification evidence and completes the
    /// active run. A verified result is allowed to move directly from
    /// `running` here because the evidence and terminal transition share one
    /// transaction; ordinary mailbox-state updates still reject that jump.
    #[allow(clippy::too_many_arguments)]
    #[cfg(test)]
    pub fn finish_run_for_mailbox_with_evidence(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
        run_status: &str,
        mailbox_state: &str,
        detail: &str,
        value: &MailboxEvidence,
    ) -> rusqlite::Result<()> {
        self.finish_run_for_mailbox_with_evidence_and_checkpoint(
            project_id,
            job_id,
            run_id,
            run_status,
            mailbox_state,
            detail,
            value,
            None,
        )
    }

    /// Atomically records verification evidence, completes the run, updates
    /// the mailbox state, and optionally stores the new Dovecot checkpoint.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_run_for_mailbox_with_evidence_and_checkpoint(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
        run_status: &str,
        mailbox_state: &str,
        detail: &str,
        value: &MailboxEvidence,
        checkpoint: Option<&str>,
    ) -> rusqlite::Result<()> {
        self.finish_run_for_mailbox_with_evidence_and_preflight_plan_and_checkpoint(
            project_id,
            job_id,
            run_id,
            run_status,
            mailbox_state,
            detail,
            value,
            None,
            checkpoint,
        )
    }

    /// Atomically records evidence, completes a mailbox run, and optionally
    /// persists both the dry-preflight digest and Dovecot checkpoint.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_run_for_mailbox_with_evidence_and_preflight_plan_and_checkpoint(
        &self,
        project_id: &str,
        job_id: &str,
        run_id: &str,
        run_status: &str,
        mailbox_state: &str,
        detail: &str,
        value: &MailboxEvidence,
        preflight_plan: Option<&str>,
        checkpoint: Option<&str>,
    ) -> rusqlite::Result<()> {
        if preflight_plan.is_some_and(|value| {
            value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if checkpoint.is_some_and(|value| !valid_dovecot_checkpoint(value)) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if run_status != "completed"
            || !matches!(
                mailbox_state,
                "verified" | "delta_required" | "verification_difference"
            )
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        // `verified` is a claim about the evidence, not merely a requested
        // mailbox state. Keep this invariant in the store so a future caller
        // cannot accidentally promote mismatching aggregate evidence by
        // bypassing the current UI decision logic.
        if mailbox_state == "verified" && !value.is_exact_match() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let attention_reason =
            attention_reason_for(mailbox_state, detail).map(AttentionReason::as_str);
        let tx = self.connection.unchecked_transaction()?;
        let current: String = tx.query_row(
            "SELECT state FROM mailbox_jobs WHERE id=?1 AND project_id=?2",
            params![job_id, project_id],
            |row| row.get(0),
        )?;
        let evidence_terminal_jump = current == "running"
            && matches!(
                mailbox_state,
                "verified" | "delta_required" | "verification_difference"
            );
        if current != mailbox_state
            && !evidence_terminal_jump
            && !valid_mailbox_transition(&current, mailbox_state)
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let run_changed = tx.execute(
            "UPDATE runs SET status='completed',finished_at=CURRENT_TIMESTAMP,detail=?1 WHERE id=?2 AND project_id=?3 AND job_id=?4 AND status='running'",
            params![detail, run_id, project_id, job_id],
        )?;
        if run_changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        tx.execute("INSERT INTO evidence_history(job_id,run_id,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative,missing_messages,extra_messages,modified_messages) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)", params![job_id, run_id, value.source_messages, value.destination_messages, value.source_bytes, value.destination_bytes, value.unmatched_messages, value.failed_messages, value.source_folders, value.destination_folders, value.authoritative, value.missing_messages, value.extra_messages, value.modified_messages])?;
        tx.execute("INSERT INTO evidence(job_id,source_messages,destination_messages,source_bytes,destination_bytes,unmatched_messages,failed_messages,source_folders,destination_folders,authoritative,missing_messages,extra_messages,modified_messages) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13) ON CONFLICT(job_id) DO UPDATE SET source_messages=excluded.source_messages,destination_messages=excluded.destination_messages,source_bytes=excluded.source_bytes,destination_bytes=excluded.destination_bytes,unmatched_messages=excluded.unmatched_messages,failed_messages=excluded.failed_messages,source_folders=excluded.source_folders,destination_folders=excluded.destination_folders,authoritative=excluded.authoritative,missing_messages=excluded.missing_messages,extra_messages=excluded.extra_messages,modified_messages=excluded.modified_messages,captured_at=CURRENT_TIMESTAMP", params![job_id, value.source_messages, value.destination_messages, value.source_bytes, value.destination_bytes, value.unmatched_messages, value.failed_messages, value.source_folders, value.destination_folders, value.authoritative, value.missing_messages, value.extra_messages, value.modified_messages])?;
        tx.execute(
            "UPDATE mailbox_jobs SET state=?1,attention_reason=?2,preflight_plan=COALESCE(?3,preflight_plan),checkpoint=COALESCE(?4,checkpoint) WHERE id=?5 AND project_id=?6",
            params![mailbox_state, attention_reason, preflight_plan, checkpoint, job_id, project_id],
        )?;
        tx.execute("DELETE FROM active_processes WHERE run_id=?1", [run_id])?;
        tx.execute(
            "INSERT INTO events(project_id,run_id,kind,detail) VALUES(?1,?2,'run_finished',?3)",
            params![project_id, run_id, "success with verification evidence"],
        )?;
        tx.commit()
    }
}
