//! Project plans at MSP scale.
//!
//! A plan is the `mailswiftsync.worker-plan` document: a small header (the
//! ledger project) and one entry per mailbox (a label and the digest of its
//! local plan). The header is stored with the project and each mailbox as a
//! row, so a plan of a million mailboxes is never one JSON value.
//!
//! Small plans still arrive in one `PUT …/plan`. Large ones arrive through a
//! paged upload: start it against the current revision, send numbered pages
//! of entries (a resent identical page is accepted), and commit, which checks
//! the count and the revision and replaces the plan as one new revision in
//! one transaction. Approval and leases are unchanged; a lease carries only
//! the leased mailbox's entry.

use super::store::{Actor, Store, StoreError, StoreResult, new_id, now, validate_name};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::Deserialize;
use sha2::Digest as _;

/// Control-plane database version 6: plan mailboxes as rows, and paged
/// uploads. Existing plans move their `mailboxes` object into rows.
pub(super) const SCHEMA_V6: &str = "
CREATE TABLE plan_mailboxes(
    project_id TEXT NOT NULL REFERENCES projects(id),
    job_id TEXT NOT NULL,
    label TEXT NOT NULL,
    plan_sha256 TEXT NOT NULL,
    PRIMARY KEY(project_id, job_id)
) STRICT, WITHOUT ROWID;
CREATE TABLE plan_uploads(
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    base_revision INTEGER NOT NULL,
    header TEXT NOT NULL,
    expected_mailboxes INTEGER NOT NULL CHECK(expected_mailboxes >= 1),
    created_by TEXT NOT NULL,
    expires_at INTEGER NOT NULL
) STRICT;
CREATE TABLE plan_upload_pages(
    upload_id TEXT NOT NULL REFERENCES plan_uploads(id) ON DELETE CASCADE,
    page INTEGER NOT NULL,
    digest BLOB NOT NULL CHECK(length(digest) = 32),
    PRIMARY KEY(upload_id, page)
) STRICT;
CREATE TABLE plan_upload_mailboxes(
    upload_id TEXT NOT NULL REFERENCES plan_uploads(id) ON DELETE CASCADE,
    job_id TEXT NOT NULL,
    label TEXT NOT NULL,
    plan_sha256 TEXT NOT NULL,
    PRIMARY KEY(upload_id, job_id)
) STRICT, WITHOUT ROWID;
INSERT INTO plan_mailboxes(project_id,job_id,label,plan_sha256)
    SELECT p.id, j.key,
           COALESCE(json_extract(j.value,'$.label'),''),
           COALESCE(json_extract(j.value,'$.plan_sha256'),'')
    FROM projects p, json_each(p.plan,'$.mailboxes') j
    WHERE json_valid(p.plan) AND json_type(p.plan,'$.mailboxes')='object';
UPDATE projects SET plan=json_remove(plan,'$.mailboxes') WHERE json_valid(plan);
";

/// Mailboxes one plan may list.
pub(super) const MAX_PLAN_MAILBOXES: usize = 1_000_000;
/// Entries in one upload page; a page stays well inside the request limit.
pub(super) const MAX_PAGE_MAILBOXES: usize = 2_000;
/// Pages one upload may hold.
const MAX_UPLOAD_PAGES: i64 = (MAX_PLAN_MAILBOXES / MAX_PAGE_MAILBOXES) as i64 * 2;
/// How long an unfinished upload is kept.
const UPLOAD_SECONDS: i64 = 60 * 60;

/// The plan document's header: everything except the mailboxes.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    format: String,
    format_version: u64,
    ledger_project_id: String,
    ledger_project_name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    label: String,
    plan_sha256: String,
}

/// One mailbox of a plan.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct PlanMailbox {
    pub(super) job_id: String,
    pub(super) label: String,
    pub(super) plan_sha256: String,
}

/// A validated plan, split into its stored parts.
pub(super) struct ValidPlan {
    /// The header as compact JSON, as stored with the project.
    pub(super) header: String,
    pub(super) mailboxes: Vec<PlanMailbox>,
}

fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::Invalid(message.into())
}

/// The header of a plan document, validated and re-serialized compactly.
pub(super) fn validate_header(header: &serde_json::Value) -> StoreResult<String> {
    let parsed = Header::deserialize(header).map_err(|error| {
        invalid(format!(
            "the plan must be a {} v{} document exported by `control worker plan` ({error})",
            super::agent::PLAN_FORMAT,
            super::agent::PLAN_FORMAT_VERSION
        ))
    })?;
    if parsed.format != super::agent::PLAN_FORMAT
        || parsed.format_version != super::agent::PLAN_FORMAT_VERSION
    {
        return Err(invalid(format!(
            "the plan must be a {} v{} document exported by `control worker plan`",
            super::agent::PLAN_FORMAT,
            super::agent::PLAN_FORMAT_VERSION
        )));
    }
    validate_name(&parsed.ledger_project_id, "ledger project ID")?;
    validate_name(&parsed.ledger_project_name, "ledger project name")?;
    serde_json::to_string(header).map_err(|error| invalid(error.to_string()))
}

/// The entries of a `mailboxes` object, each validated, in job-ID order.
pub(super) fn validate_mailboxes(
    mailboxes: &serde_json::Value,
    max: usize,
) -> StoreResult<Vec<PlanMailbox>> {
    let object = mailboxes
        .as_object()
        .ok_or_else(|| invalid("the plan's mailboxes must be an object keyed by job ID"))?;
    if object.len() > max {
        return Err(invalid(format!(
            "{} mailboxes is more than the {max} allowed here",
            object.len()
        )));
    }
    let mut entries = Vec::with_capacity(object.len());
    for (job_id, entry) in object {
        let entry = Entry::deserialize(entry).map_err(|error| {
            invalid(format!(
                "the plan entry for mailbox {job_id:?} must hold only label and plan_sha256 ({error})"
            ))
        })?;
        validate_name(job_id, "mailbox job ID")?;
        validate_name(&entry.label, "mailbox label")?;
        let digest = &entry.plan_sha256;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(invalid(format!(
                "the plan digest for mailbox {job_id:?} must be 64 lowercase hex characters"
            )));
        }
        entries.push(PlanMailbox {
            job_id: job_id.clone(),
            label: entry.label,
            plan_sha256: entry.plan_sha256,
        });
    }
    entries.sort_by(|left, right| left.job_id.cmp(&right.job_id));
    Ok(entries)
}

/// A whole plan document sent in one request.
pub(super) fn validate_plan_document(
    plan: &serde_json::Value,
    max_bytes: usize,
) -> StoreResult<ValidPlan> {
    let object = plan
        .as_object()
        .ok_or_else(|| invalid("the plan must be a JSON object"))?;
    let mut header = object.clone();
    let mailboxes = header
        .remove("mailboxes")
        .ok_or_else(|| invalid("the plan lists no mailboxes"))?;
    let header = validate_header(&serde_json::Value::Object(header))?;
    let mailboxes = validate_mailboxes(&mailboxes, MAX_PLAN_MAILBOXES)?;
    if mailboxes.is_empty() {
        return Err(invalid("the plan lists no mailboxes"));
    }
    let bytes = serde_json::to_string(plan)
        .map_err(|error| invalid(error.to_string()))?
        .len();
    if bytes > max_bytes {
        return Err(invalid(format!(
            "the plan exceeds {max_bytes} bytes; upload it in pages (`control plan upload`)"
        )));
    }
    Ok(ValidPlan { header, mailboxes })
}

/// Replace a project's plan rows.
pub(super) fn replace_mailboxes(
    tx: &Transaction<'_>,
    project: &str,
    mailboxes: &[PlanMailbox],
) -> StoreResult<()> {
    tx.execute("DELETE FROM plan_mailboxes WHERE project_id=?1", [project])?;
    let mut insert = tx.prepare(
        "INSERT INTO plan_mailboxes(project_id,job_id,label,plan_sha256) VALUES(?1,?2,?3,?4)",
    )?;
    for mailbox in mailboxes {
        insert.execute(params![
            project,
            mailbox.job_id,
            mailbox.label,
            mailbox.plan_sha256
        ])?;
    }
    Ok(())
}

/// The plan a worker receives with a lease: the header and only the leased
/// mailbox's entry (none if the plan does not list it).
pub(super) fn leased_plan(
    tx: &Transaction<'_>,
    project: &str,
    header: &str,
    job_id: &str,
) -> rusqlite::Result<serde_json::Value> {
    let mut plan: serde_json::Value = serde_json::from_str(header).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let entry = tx
        .query_row(
            "SELECT label,plan_sha256 FROM plan_mailboxes WHERE project_id=?1 AND job_id=?2",
            params![project, job_id],
            |row| {
                Ok(serde_json::json!({
                    "label": row.get::<_, String>(0)?,
                    "plan_sha256": row.get::<_, String>(1)?,
                }))
            },
        )
        .optional()?;
    let mut mailboxes = serde_json::Map::new();
    if let Some(entry) = entry {
        mailboxes.insert(job_id.to_owned(), entry);
    }
    if let Some(object) = plan.as_object_mut() {
        object.insert("mailboxes".into(), serde_json::Value::Object(mailboxes));
    }
    Ok(plan)
}

/// An upload in progress, as returned when it starts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct PlanUpload {
    pub(super) upload_id: String,
    pub(super) base_revision: i64,
    pub(super) expected_mailboxes: i64,
    pub(super) expires_at: i64,
    pub(super) max_page_mailboxes: usize,
}

fn page_digest(entries: &[PlanMailbox]) -> [u8; 32] {
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"mailswiftsync-plan-page-v1");
    for entry in entries {
        for field in [&entry.job_id, &entry.label, &entry.plan_sha256] {
            hasher.update((field.len() as u64).to_be_bytes());
            hasher.update(field.as_bytes());
        }
    }
    hasher.finalize().into()
}

impl Store {
    /// A page of a project's plan mailboxes in job-ID order, after `after`.
    pub(super) fn plan_mailboxes(
        &self,
        tenant: &str,
        project: &str,
        after: Option<&str>,
        limit: usize,
    ) -> StoreResult<Vec<PlanMailbox>> {
        let connection = self.lock();
        let exists = connection
            .query_row(
                "SELECT 1 FROM projects WHERE tenant_id=?1 AND id=?2",
                params![tenant, project],
                |_| Ok(()),
            )
            .optional()?;
        if exists.is_none() {
            return Err(StoreError::NotFound);
        }
        let mut statement = connection.prepare(
            "SELECT job_id,label,plan_sha256 FROM plan_mailboxes
             WHERE project_id=?1 AND (?2 IS NULL OR job_id>?2) ORDER BY job_id LIMIT ?3",
        )?;
        let rows = statement
            .query_map(
                params![project, after, limit.min(MAX_PAGE_MAILBOXES) as i64],
                |row| {
                    Ok(PlanMailbox {
                        job_id: row.get(0)?,
                        label: row.get(1)?,
                        plan_sha256: row.get(2)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Start a paged upload that will replace the plan at `base_revision`.
    /// Any earlier unfinished upload for the project is discarded.
    pub(super) fn start_plan_upload(
        &self,
        tenant: &str,
        project: &str,
        base_revision: i64,
        header: &serde_json::Value,
        expected_mailboxes: usize,
        actor: &Actor<'_>,
    ) -> StoreResult<PlanUpload> {
        let header = validate_header(header)?;
        if expected_mailboxes == 0 || expected_mailboxes > MAX_PLAN_MAILBOXES {
            return Err(invalid(format!(
                "a plan lists 1 to {MAX_PLAN_MAILBOXES} mailboxes"
            )));
        }
        let id = new_id("pup")?;
        let at = now();
        let resource = format!("project:{project}");
        self.mutate(actor, Some(tenant), "project.plan_upload.start", &resource, &id, |tx| {
            require_revision(tx, tenant, project, base_revision)?;
            tx.execute(
                "DELETE FROM plan_uploads WHERE expires_at<=?1 OR (tenant_id=?2 AND project_id=?3)",
                params![at, tenant, project],
            )?;
            tx.execute(
                "INSERT INTO plan_uploads(id,tenant_id,project_id,base_revision,header,expected_mailboxes,created_by,expires_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    id,
                    tenant,
                    project,
                    base_revision,
                    header,
                    expected_mailboxes as i64,
                    actor.label,
                    at + UPLOAD_SECONDS
                ],
            )?;
            Ok(())
        })?;
        Ok(PlanUpload {
            upload_id: id,
            base_revision,
            expected_mailboxes: expected_mailboxes as i64,
            expires_at: at + UPLOAD_SECONDS,
            max_page_mailboxes: MAX_PAGE_MAILBOXES,
        })
    }

    /// Add numbered page `page` of mailbox entries. Sending the same page
    /// again with the same entries is accepted, so a client can retry.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn put_plan_upload_page(
        &self,
        tenant: &str,
        project: &str,
        upload: &str,
        page: i64,
        mailboxes: &serde_json::Value,
        actor: &Actor<'_>,
    ) -> StoreResult<i64> {
        if !(1..=MAX_UPLOAD_PAGES).contains(&page) {
            return Err(invalid(format!(
                "pages are numbered 1 to {MAX_UPLOAD_PAGES}"
            )));
        }
        let entries = validate_mailboxes(mailboxes, MAX_PAGE_MAILBOXES)?;
        if entries.is_empty() {
            return Err(invalid("a page lists at least one mailbox"));
        }
        let digest = page_digest(&entries);
        let resource = format!("project:{project}");
        let detail = format!("{upload} page {page}");
        self.mutate(actor, Some(tenant), "project.plan_upload.page", &resource, &detail, |tx| {
            let expected = open_upload(tx, tenant, project, upload)?;
            let stored: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT digest FROM plan_upload_pages WHERE upload_id=?1 AND page=?2",
                    params![upload, page],
                    |row| row.get(0),
                )
                .optional()?;
            match stored {
                Some(stored) if stored == digest => return received(tx, upload),
                Some(_) => {
                    return Err(StoreError::Conflict(format!(
                        "page {page} was already uploaded with different entries"
                    )));
                }
                None => {}
            }
            let mut insert = tx.prepare(
                "INSERT INTO plan_upload_mailboxes(upload_id,job_id,label,plan_sha256) VALUES(?1,?2,?3,?4)
                 ON CONFLICT(upload_id,job_id) DO NOTHING",
            )?;
            for entry in &entries {
                if insert.execute(params![upload, entry.job_id, entry.label, entry.plan_sha256])? == 0
                {
                    return Err(StoreError::Conflict(format!(
                        "mailbox {:?} is already on another page of this upload",
                        entry.job_id
                    )));
                }
            }
            drop(insert);
            tx.execute(
                "INSERT INTO plan_upload_pages(upload_id,page,digest) VALUES(?1,?2,?3)",
                params![upload, page, digest.as_slice()],
            )?;
            let count = received(tx, upload)?;
            if count > expected {
                return Err(invalid(format!(
                    "the upload now holds {count} mailboxes, more than the {expected} it announced"
                )));
            }
            Ok(count)
        })
    }

    /// Replace the plan with a complete upload, as one new revision.
    pub(super) fn commit_plan_upload(
        &self,
        tenant: &str,
        project: &str,
        upload: &str,
        actor: &Actor<'_>,
    ) -> StoreResult<super::store::Project> {
        let resource = format!("project:{project}");
        self.mutate(actor, Some(tenant), "project.plan_upload.commit", &resource, upload, |tx| {
            let expected = open_upload(tx, tenant, project, upload)?;
            let count = received(tx, upload)?;
            if count != expected {
                return Err(invalid(format!(
                    "the upload holds {count} of the {expected} mailboxes it announced"
                )));
            }
            let (base_revision, header): (i64, String) = tx.query_row(
                "SELECT base_revision,header FROM plan_uploads WHERE id=?1",
                [upload],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let changed = tx.execute(
                "UPDATE projects SET plan=?3,plan_revision=plan_revision+1,plan_changed_by=?4,approved_revision=NULL,approved_by=NULL
                 WHERE tenant_id=?1 AND id=?2 AND plan_revision=?5",
                params![tenant, project, header, actor.label, base_revision],
            )?;
            if changed == 0 {
                return Err(super::store::revision_conflict_or_missing(tx, tenant, project)?);
            }
            tx.execute("DELETE FROM plan_mailboxes WHERE project_id=?1", [project])?;
            tx.execute(
                "INSERT INTO plan_mailboxes(project_id,job_id,label,plan_sha256)
                 SELECT ?1,job_id,label,plan_sha256 FROM plan_upload_mailboxes WHERE upload_id=?2",
                params![project, upload],
            )?;
            tx.execute("DELETE FROM plan_uploads WHERE id=?1", [upload])?;
            Ok(())
        })?;
        self.project(tenant, project)
    }
}

/// The project must be at `revision`.
fn require_revision(
    tx: &Transaction<'_>,
    tenant: &str,
    project: &str,
    revision: i64,
) -> StoreResult<()> {
    let current: Option<i64> = tx
        .query_row(
            "SELECT plan_revision FROM projects WHERE tenant_id=?1 AND id=?2",
            params![tenant, project],
            |row| row.get(0),
        )
        .optional()?;
    match current {
        None => Err(StoreError::NotFound),
        Some(current) if current == revision => Ok(()),
        Some(current) => Err(StoreError::Conflict(format!(
            "the plan is at revision {current}, not {revision}"
        ))),
    }
}

/// An unexpired upload of this project, and the mailbox count it announced.
fn open_upload(
    tx: &Transaction<'_>,
    tenant: &str,
    project: &str,
    upload: &str,
) -> StoreResult<i64> {
    tx.query_row(
        "SELECT expected_mailboxes FROM plan_uploads
         WHERE id=?1 AND tenant_id=?2 AND project_id=?3 AND expires_at>?4",
        params![upload, tenant, project, now()],
        |row| row.get(0),
    )
    .optional()?
    .ok_or(StoreError::NotFound)
}

fn received(tx: &Transaction<'_>, upload: &str) -> StoreResult<i64> {
    Ok(tx.query_row(
        "SELECT COUNT(*) FROM plan_upload_mailboxes WHERE upload_id=?1",
        [upload],
        |row| row.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::control::authn;

    fn actor(label: &str) -> Actor<'static> {
        Actor {
            request_id: "test",
            label: label.to_owned(),
        }
    }

    fn entries(range: std::ops::Range<usize>) -> serde_json::Value {
        let mut mailboxes = serde_json::Map::new();
        for index in range {
            mailboxes.insert(
                format!("job-{index:07}"),
                serde_json::json!({ "label": format!("Mailbox {index}"), "plan_sha256": "c".repeat(64) }),
            );
        }
        serde_json::Value::Object(mailboxes)
    }

    fn header() -> serde_json::Value {
        let mut header = super::super::store::test_plan("ledger");
        header.as_object_mut().unwrap().remove("mailboxes");
        header
    }

    #[test]
    fn a_large_plan_arrives_in_pages_and_commits_as_one_revision() {
        let store = Store::in_memory();
        let password = authn::test_secret();
        let admin = store
            .bootstrap_administrator("admin@msp.example", "Admin", &password)
            .unwrap();
        let admin = actor(&format!("user:{admin}"));
        let editor = actor("user:usr_editor");
        let tenant = store.create_tenant("Acme", &admin).unwrap().id;
        let project = store
            .create_project(
                &tenant,
                "Move",
                &super::super::store::test_plan("ledger"),
                &editor,
            )
            .unwrap();
        store
            .approve_project(&tenant, &project.id, 1, "usr_admin", &admin)
            .unwrap();
        let total = 2 * MAX_PAGE_MAILBOXES + 500;

        let upload = store
            .start_plan_upload(&tenant, &project.id, 1, &header(), total, &editor)
            .unwrap();
        let id = upload.upload_id.as_str();
        let page = |number, range| {
            store.put_plan_upload_page(&tenant, &project.id, id, number, &entries(range), &editor)
        };
        assert_eq!(
            page(1, 0..MAX_PAGE_MAILBOXES).unwrap(),
            MAX_PAGE_MAILBOXES as i64
        );
        // A retried page is accepted; a different page under the same
        // number, or an entry already sent on another page, is not.
        assert_eq!(
            page(1, 0..MAX_PAGE_MAILBOXES).unwrap(),
            MAX_PAGE_MAILBOXES as i64
        );
        assert!(matches!(
            page(1, 1..MAX_PAGE_MAILBOXES + 1),
            Err(StoreError::Conflict(_))
        ));
        assert!(matches!(
            page(2, MAX_PAGE_MAILBOXES - 1..2 * MAX_PAGE_MAILBOXES - 1),
            Err(StoreError::Conflict(_))
        ));
        assert!(
            matches!(
                page(2, 0..MAX_PAGE_MAILBOXES + 1),
                Err(StoreError::Invalid(_)),
            ),
            "a page holds at most MAX_PAGE_MAILBOXES"
        );
        page(2, MAX_PAGE_MAILBOXES..2 * MAX_PAGE_MAILBOXES).unwrap();
        // Not every announced mailbox has arrived.
        assert!(matches!(
            store.commit_plan_upload(&tenant, &project.id, id, &editor),
            Err(StoreError::Invalid(_))
        ));
        page(3, 2 * MAX_PAGE_MAILBOXES..total).unwrap();
        let committed = store
            .commit_plan_upload(&tenant, &project.id, id, &editor)
            .unwrap();
        assert_eq!(committed.plan_revision, 2);
        assert_eq!(committed.plan_mailboxes, total as i64);
        assert!(!committed.approved, "a new plan withdraws approval");
        assert!(
            matches!(
                store.commit_plan_upload(&tenant, &project.id, id, &editor),
                Err(StoreError::NotFound)
            ),
            "a committed upload is gone"
        );

        // Reviewers page through it in job-ID order.
        let first = store.plan_mailboxes(&tenant, &project.id, None, 3).unwrap();
        assert_eq!(first[0].job_id, "job-0000000");
        let next = store
            .plan_mailboxes(&tenant, &project.id, Some(&first[2].job_id), 1)
            .unwrap();
        assert_eq!(next[0].job_id, "job-0000003");
        assert!(store.plan_mailboxes(&tenant, "prj_other", None, 1).is_err());
    }

    #[test]
    fn an_upload_is_bound_to_the_revision_it_replaces() {
        let store = Store::in_memory();
        let password = authn::test_secret();
        let admin = store
            .bootstrap_administrator("admin@msp.example", "Admin", &password)
            .unwrap();
        let admin = actor(&format!("user:{admin}"));
        let tenant = store.create_tenant("Acme", &admin).unwrap().id;
        let project = store
            .create_project(
                &tenant,
                "Move",
                &super::super::store::test_plan("ledger"),
                &admin,
            )
            .unwrap();
        assert!(matches!(
            store.start_plan_upload(&tenant, &project.id, 2, &header(), 1, &admin),
            Err(StoreError::Conflict(_))
        ));
        let upload = store
            .start_plan_upload(&tenant, &project.id, 1, &header(), 1, &admin)
            .unwrap();
        store
            .put_plan_upload_page(
                &tenant,
                &project.id,
                &upload.upload_id,
                1,
                &entries(0..1),
                &admin,
            )
            .unwrap();
        // Someone changes the plan meanwhile: the upload can no longer land.
        store
            .update_plan(
                &tenant,
                &project.id,
                &super::super::store::test_plan("other"),
                1,
                &admin,
            )
            .unwrap();
        assert!(matches!(
            store.commit_plan_upload(&tenant, &project.id, &upload.upload_id, &admin),
            Err(StoreError::Conflict(_))
        ));
        let mut bad = header();
        bad["format_version"] = 9.into();
        assert!(matches!(
            store.start_plan_upload(&tenant, &project.id, 2, &bad, 1, &admin),
            Err(StoreError::Invalid(_))
        ));
        let mut secret = entries(0..1);
        secret["job-0000000"]["pw"] = "hunter2".into();
        let upload = store
            .start_plan_upload(&tenant, &project.id, 2, &header(), 1, &admin)
            .unwrap();
        assert!(
            matches!(
                store.put_plan_upload_page(
                    &tenant,
                    &project.id,
                    &upload.upload_id,
                    1,
                    &secret,
                    &admin
                ),
                Err(StoreError::Invalid(_))
            ),
            "pages are allowlisted like whole plans"
        );
    }
}
