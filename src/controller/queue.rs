//! Controller operations on the durable batch queue: deriving row facts,
//! persisting an import, rebuilding a row's executable plan, and bulk plan
//! edits. Presentation reads live in `ui::queue_model`.

use super::batch_admission::{
    batch_project_identity, decode_persisted_batch_profile, duplicate_destination,
    durable_batch_profile_config,
};
use crate::{Form, Profile, SecretString, bulk_import::BulkJob, core};
use std::collections::HashMap;

/// Session-only passwords from an explicitly acknowledged plaintext import,
/// keyed by durable job ID. They are never written to the ledger.
pub(crate) type SessionSecrets = HashMap<String, (SecretString, SecretString)>;

/// Secret-free presentation facts derived from a row's plan.
pub(crate) fn queue_row_facts(label: &str, profile: &Profile) -> core::QueueRowFacts {
    core::QueueRowFacts {
        label: label.to_owned(),
        source_host: profile.source_host.clone(),
        destination_host: profile.destination_host.clone(),
        search_key: crate::ui::fold_search_text(
            &[
                label,
                profile.source_host.as_str(),
                profile.source_user.as_str(),
                profile.destination_host.as_str(),
                profile.destination_user.as_str(),
            ]
            .join(" "),
        ),
        destructive: profile
            .destination_mutation_policy()
            .may_remove_destination_state(),
        policy: profile.destination_mutation_policy().as_str().to_owned(),
    }
}

pub(crate) struct ImportedQueue {
    pub(crate) project_id: String,
    pub(crate) len: usize,
    pub(crate) session_secrets: SessionSecrets,
}

/// Write an imported mailbox file to the ledger as a new batch queue in one
/// transaction. Plaintext session passwords stay in memory only.
pub(crate) fn persist_imported_queue(
    store: &core::StateStore,
    jobs: Vec<BulkJob>,
    fallback_profile: &Profile,
) -> Result<ImportedQueue, String> {
    if jobs.is_empty() {
        return Err("The mailbox file contains no migration rows.".into());
    }
    if let Some(error) = duplicate_destination(&jobs)? {
        return Err(error);
    }
    let identity = batch_project_identity(&jobs, fallback_profile);
    let rows = jobs
        .iter()
        .map(|job| {
            let profile = job.profile();
            Ok(core::QueueInsert {
                source_mailbox: profile.source_user.clone(),
                destination_mailbox: profile.destination_user.clone(),
                destination_identity: core::destination_identity_from_parts(
                    &profile.destination_user,
                    &profile.destination_user,
                    &profile.destination_host,
                    &profile.destination_tls,
                    &profile.destination_port,
                ),
                config: durable_batch_profile_config(&profile)?,
                facts: queue_row_facts(&job.label, &profile),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let (project, ids) = store
        .create_batch_queue(
            &identity.name,
            &identity.source_endpoint,
            &identity.destination_endpoint,
            &rows,
        )
        .map_err(|error| format!("Could not record the imported queue durably: {error}"))?;
    drop(rows);
    let session_secrets = ids
        .iter()
        .zip(jobs)
        .filter(|(_, job)| !job.source_password.is_empty() || !job.destination_password.is_empty())
        .map(|(id, job)| (id.clone(), (job.source_password, job.destination_password)))
        .collect();
    Ok(ImportedQueue {
        project_id: project.id,
        len: ids.len(),
        session_secrets,
    })
}

/// Derive presentation facts for rows written before they were stored.
/// Runs once per legacy ledger; returns how many rows were filled.
pub(crate) fn backfill_queue_facts(store: &core::StateStore) -> Result<usize, String> {
    let mut filled = 0;
    loop {
        let rows = store
            .queue_rows_missing_facts(1_000)
            .map_err(|error| format!("Could not read queue rows to index: {error}"))?;
        if rows.is_empty() {
            return Ok(filled);
        }
        let facts = rows
            .iter()
            .map(|row| {
                let label = format!("{} → {}", row.source_mailbox, row.destination_mailbox);
                let facts = decode_persisted_batch_profile(row.config.as_deref(), &row.id)
                    .map(|profile| queue_row_facts(&label, &profile))
                    // An undecodable plan still gets a searchable label; its
                    // admission fails closed on the same decode.
                    .unwrap_or_else(|_| core::QueueRowFacts {
                        search_key: crate::ui::fold_search_text(&label),
                        label: label.clone(),
                        ..core::QueueRowFacts::default()
                    });
                (row.id.clone(), facts)
            })
            .collect::<Vec<_>>();
        store
            .set_queue_facts(&facts)
            .map_err(|error| format!("Could not index queue rows: {error}"))?;
        filled += facts.len();
    }
}

/// Rebuild the executable plan of one durable row. Expert options are not
/// persisted; they come from the operator's current profile, exactly as the
/// run snapshot records their digest.
pub(crate) fn job_from_plan(
    row: &core::QueuePlanRow,
    extra_options: &str,
    secrets: Option<&(SecretString, SecretString)>,
    dry_run: bool,
) -> Result<BulkJob, String> {
    let mut profile = decode_persisted_batch_profile(row.config.as_deref(), &row.id)?;
    if profile.destination_tls.is_empty() {
        profile.destination_tls = crate::default_destination_tls();
    }
    profile.extra_options = extra_options.to_owned();
    let (source_password, destination_password) = secrets.cloned().unwrap_or_default();
    let form = Form {
        profile,
        source_password,
        destination_password,
        dry_run,
    };
    Ok(BulkJob::from_form(
        row.label.clone(),
        form,
        row.state.clone(),
    ))
}

/// Rebuild a durable row with the operator's current batch scheduling policy.
/// Mailbox identity remains durable, while concurrency and throughput are
/// run-level settings and must not be taken from the imported row.
pub(crate) fn job_from_plan_with_batch_policy(
    row: &core::QueuePlanRow,
    batch_profile: &Profile,
    secrets: Option<&(SecretString, SecretString)>,
    dry_run: bool,
) -> Result<BulkJob, String> {
    let mut form = job_from_plan(row, &batch_profile.extra_options, secrets, dry_run)?.form();
    form.profile.batch_concurrency = batch_profile.batch_concurrency.clamp(1, 16);
    form.profile.max_messages_per_second = batch_profile.max_messages_per_second;
    form.profile.max_bytes_per_second = batch_profile.max_bytes_per_second;
    Ok(BulkJob::from_form(
        row.label.clone(),
        form,
        row.state.clone(),
    ))
}

/// Give every row that has neither a session password nor a credential
/// reference on one side the keyring credential `credential_id`. Changing a
/// plan clears its preflight, so affected rows must be validated again.
pub(crate) fn apply_keyring_to_queue(
    store: &core::StateStore,
    project_id: &str,
    credential_id: &str,
    source: bool,
    session_secrets: &SessionSecrets,
) -> Result<usize, String> {
    let mut updates = Vec::new();
    store.queue_plan_scan(project_id, |row| {
        let side_has_password = session_secrets.get(&row.id).is_some_and(|(src, dst)| {
            if source {
                !src.is_empty()
            } else {
                !dst.is_empty()
            }
        });
        let mut profile = decode_persisted_batch_profile(row.config.as_deref(), &row.id)?;
        let reference = if source {
            &mut profile.source_credential_id
        } else {
            &mut profile.destination_credential_id
        };
        if side_has_password || !reference.trim().is_empty() {
            return Ok(());
        }
        *reference = credential_id.to_owned();
        updates.push((
            row.id.clone(),
            durable_batch_profile_config(&profile)?,
            queue_row_facts(&row.label, &profile),
        ));
        Ok(())
    })?;
    if updates.is_empty() {
        return Ok(0);
    }
    store
        .update_queue_plans(project_id, &updates)
        .map_err(|error| format!("Could not update the queue's credential references: {error}"))
}

/// A scheduler job source over admitted rows of `project_id`. With a ledger
/// file it reads through its own read-only connection on the scheduler
/// thread; a session without one (an in-memory store) prefetches the
/// project's plans up front. Build it before durable admission so a failure
/// here can never leave an admitted run without a worker. Dry runs load each job's configured credentials
/// as the job is read; live workers refresh credentials themselves just
/// before authenticating.
pub(crate) fn ledger_job_loader(
    store: &core::StateStore,
    state_path: Option<std::path::PathBuf>,
    project_id: &str,
    batch_profile: Profile,
    session_secrets: SessionSecrets,
    dry_run: bool,
) -> Result<super::batch_scheduler::JobLoader, String> {
    let prepare = move |row: &core::QueuePlanRow| -> Result<BulkJob, String> {
        let job = job_from_plan_with_batch_policy(
            row,
            &batch_profile,
            session_secrets.get(&row.id),
            dry_run,
        )?;
        if !dry_run {
            return Ok(job);
        }
        let mut form = job.form();
        form.load_configured_keyring_credentials()
            .map_err(|error| {
                format!(
                    "Could not load credentials for mailbox {}: {error}",
                    row.label
                )
            })?;
        Ok(BulkJob::from_form(
            job.label.clone(),
            form,
            job.state.clone(),
        ))
    };
    let resolve = move |plans: Vec<core::QueuePlanRow>, ids: &[String]| {
        let mut by_id = plans
            .into_iter()
            .map(|row| (row.id.clone(), row))
            .collect::<HashMap<_, _>>();
        ids.iter()
            .map(|id| {
                by_id
                    .remove(id)
                    .ok_or_else(|| format!("Mailbox {id} is no longer in the durable queue."))
                    .and_then(|row| prepare(&row))
            })
            .collect::<Vec<_>>()
    };
    let project_id = project_id.to_owned();
    match state_path {
        Some(path) => {
            let mut reader: Option<core::StateStore> = None;
            Ok(Box::new(move |ids: &[String]| {
                if reader.is_none() {
                    reader = Some(core::StateStore::open_readonly(&path).map_err(|error| {
                        format!("Could not open the ledger to read admitted jobs: {error}")
                    })?);
                }
                let store = reader.as_ref().expect("reader opened above");
                let plans = store
                    .queue_plans(&project_id, ids)
                    .map_err(|error| format!("Could not read admitted jobs: {error}"))?;
                Ok(resolve(plans, ids))
            }))
        }
        None => {
            let mut plans = HashMap::new();
            store.queue_plan_scan(&project_id, |row| {
                plans.insert(row.id.clone(), row);
                Ok(())
            })?;
            Ok(Box::new(move |ids: &[String]| {
                let page = ids
                    .iter()
                    .filter_map(|id| plans.remove(id))
                    .collect::<Vec<_>>();
                Ok(resolve(page, ids))
            }))
        }
    }
}

/// Selected rows in queue order, at most `limit` of them.
pub(crate) fn selected_rows(
    store: &core::StateStore,
    project_id: &str,
    is_selected: impl Fn(&str) -> bool,
    limit: usize,
) -> Result<Vec<core::QueueRow>, String> {
    let mut rowids = Vec::new();
    store
        .queue_scan(project_id, |row| {
            if rowids.len() < limit && is_selected(&row.id) {
                rowids.push(row.rowid);
            }
        })
        .map_err(|error| format!("Could not read the mailbox queue: {error}"))?;
    store
        .queue_rows(project_id, &rowids)
        .map_err(|error| format!("Could not read the mailbox queue: {error}"))
}

/// Whether selected destinations differ only by case on an endpoint whose
/// account-name case semantics are unknown.
pub(crate) fn selected_case_collision(
    store: &core::StateStore,
    project_id: &str,
    is_selected: impl Fn(&str) -> bool,
) -> Result<bool, String> {
    let mut detector = super::batch_admission::CaseCollisionDetector::default();
    let mut collision = false;
    store.queue_plan_scan(project_id, |row| {
        if collision || !is_selected(&row.id) {
            return Ok(());
        }
        let profile = decode_persisted_batch_profile(row.config.as_deref(), &row.id)?;
        collision = detector.add(&profile)?;
        Ok(())
    })?;
    Ok(collision)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(index: usize, credential: &str) -> BulkJob {
        let mut form = Form::default();
        form.profile.source_host = "old.example".into();
        form.profile.source_user = format!("user{index}@old.example");
        form.profile.destination_host = "new.example".into();
        form.profile.destination_user = format!("user{index}@new.example");
        form.profile.source_credential_id = credential.into();
        form.profile.extra_options = "--nofoldersizes".into();
        BulkJob::from_form(format!("Row {index}"), form, "imported".into())
    }

    #[test]
    fn import_persists_rows_with_facts_and_keeps_extra_options_out() {
        let store = core::StateStore::in_memory().unwrap();
        let imported =
            persist_imported_queue(&store, vec![job(0, ""), job(1, "kr")], &Profile::default())
                .unwrap();
        assert_eq!(imported.len, 2);
        assert!(imported.session_secrets.is_empty());
        let rowids = store
            .queue_rowids(&imported.project_id, "user1@new", None)
            .unwrap();
        let row = &store.queue_rows(&imported.project_id, &rowids).unwrap()[0];
        assert_eq!(row.label, "Row 1");
        assert_eq!(row.destination_host, "new.example");
        let ids = store.mailbox_ids(&imported.project_id).unwrap();
        let plan = &store.queue_plans(&imported.project_id, &ids[1..]).unwrap()[0];
        assert!(!plan.config.as_deref().unwrap().contains("nofoldersizes"));
        let rebuilt = job_from_plan(plan, "--nofoldersizes", None, true).unwrap();
        assert_eq!(rebuilt.profile().extra_options, "--nofoldersizes");
        assert_eq!(rebuilt.profile().source_credential_id, "kr");
        assert!(rebuilt.form().dry_run);
    }

    #[test]
    fn batch_policy_overrides_imported_throttle_settings() {
        let store = core::StateStore::in_memory().unwrap();
        let mut imported_form = job(0, "").form();
        imported_form.profile.max_messages_per_second = 1_000;
        imported_form.profile.max_bytes_per_second = 10_000;
        imported_form.profile.batch_concurrency = 4;
        let imported_job = BulkJob::from_form(
            imported_form.profile.source_user.clone(),
            imported_form,
            "imported".into(),
        );
        let imported =
            persist_imported_queue(&store, vec![imported_job], &Profile::default()).unwrap();
        let id = store.mailbox_ids(&imported.project_id).unwrap().remove(0);
        let row = store
            .queue_plans(&imported.project_id, std::slice::from_ref(&id))
            .unwrap()
            .remove(0);

        let mut current_profile = Profile::default();
        current_profile.batch_concurrency = 2;
        current_profile.max_messages_per_second = 200;
        current_profile.max_bytes_per_second = 2_000;
        current_profile.extra_options = "--nofoldersizes".into();
        let rebuilt = job_from_plan_with_batch_policy(&row, &current_profile, None, true).unwrap();
        assert_eq!(rebuilt.profile().batch_concurrency, 2);
        assert_eq!(rebuilt.profile().max_messages_per_second, 200);
        assert_eq!(rebuilt.profile().max_bytes_per_second, 2_000);
        assert_eq!(rebuilt.profile().extra_options, "--nofoldersizes");
    }

    #[test]
    fn bulk_destination_identity_matches_the_parsed_plan_identity() {
        let store = core::StateStore::in_memory().unwrap();
        let mut variants = vec![job(0, ""), job(1, "")];
        let mut tls = variants[1].profile();
        tls.destination_tls = "starttls".into();
        tls.destination_port = " 1143 ".into();
        tls.destination_host = " IMAP.New.Example ".into();
        variants[1] = BulkJob::from_form(
            "tls".into(),
            Form {
                profile: tls,
                ..Form::default()
            },
            "imported".into(),
        );
        let imported = persist_imported_queue(&store, variants, &Profile::default()).unwrap();
        let ids = store.mailbox_ids(&imported.project_id).unwrap();
        for plan in store.queue_plans(&imported.project_id, &ids).unwrap() {
            let profile = job_from_plan(&plan, "", None, true).unwrap().profile();
            assert_eq!(
                store.mailbox_destination_identity(&plan.id).unwrap(),
                core::normalized_destination_identity_for_test(
                    &profile.destination_user,
                    plan.config.as_deref()
                )
            );
        }
    }

    #[test]
    fn duplicate_destinations_are_rejected_before_anything_is_written() {
        let store = core::StateStore::in_memory().unwrap();
        let error =
            persist_imported_queue(&store, vec![job(0, ""), job(0, "")], &Profile::default())
                .err()
                .unwrap();
        assert!(error.contains("destination"), "{error}");
        assert!(store.recent_projects(10).unwrap().is_empty());
    }

    #[test]
    fn keyring_reference_fills_only_rows_without_credentials_and_clears_preflight() {
        let store = core::StateStore::in_memory().unwrap();
        let imported = persist_imported_queue(
            &store,
            vec![job(0, ""), job(1, "existing")],
            &Profile::default(),
        )
        .unwrap();
        let applied = apply_keyring_to_queue(
            &store,
            &imported.project_id,
            "shared",
            true,
            &SessionSecrets::new(),
        )
        .unwrap();
        assert_eq!(applied, 1);
        let ids = store.mailbox_ids(&imported.project_id).unwrap();
        let plans = store.queue_plans(&imported.project_id, &ids).unwrap();
        let credential = |index: usize| {
            job_from_plan(&plans[index], "", None, true)
                .unwrap()
                .profile()
                .source_credential_id
        };
        assert_eq!(
            (credential(0).as_str(), credential(1).as_str()),
            ("shared", "existing")
        );
    }

    #[test]
    fn legacy_rows_get_facts_from_their_plans() {
        let store = core::StateStore::in_memory().unwrap();
        let profile = job(3, "").profile();
        let config = durable_batch_profile_config(&profile).unwrap();
        let (project, _) = store
            .create_project_with_mailbox_configs(
                "legacy",
                "old.example",
                "new.example",
                &[(
                    profile.source_user.clone(),
                    profile.destination_user.clone(),
                    config,
                )],
            )
            .unwrap();
        assert!(
            store
                .queue_rowids(&project.id, "user3", None)
                .unwrap()
                .is_empty()
        );
        assert_eq!(backfill_queue_facts(&store).unwrap(), 1);
        let rowids = store.queue_rowids(&project.id, "user3", None).unwrap();
        assert_eq!(
            store.queue_rows(&project.id, &rowids).unwrap()[0].label,
            "user3@old.example → user3@new.example"
        );
        assert_eq!(backfill_queue_facts(&store).unwrap(), 0);
    }
}
