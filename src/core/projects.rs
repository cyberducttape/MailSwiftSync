use super::*;

impl StateStore {
    #[cfg(test)]
    pub fn create_project(
        &self,
        name: &str,
        source: &str,
        destination: &str,
    ) -> rusqlite::Result<Project> {
        let project = Project {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            source_endpoint: source.into(),
            destination_endpoint: destination.into(),
            phase: Phase::Discovery,
        };
        let tx = self.connection.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO projects(id,name,source_endpoint,destination_endpoint,phase) VALUES(?1,?2,?3,?4,?5)",
            params![project.id, project.name, project.source_endpoint, project.destination_endpoint, project.phase.as_str()],
        )?;
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'project_created','Project created without credentials')",
            [&project.id],
        )?;
        tx.commit()?;
        Ok(project)
    }
    pub fn create_project_with_mailbox(
        &self,
        name: &str,
        source: &str,
        destination: &str,
        source_mailbox: &str,
        destination_mailbox: &str,
    ) -> rusqlite::Result<(Project, String)> {
        let project = Project {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            source_endpoint: source.into(),
            destination_endpoint: destination.into(),
            phase: Phase::Discovery,
        };
        let job_id = Uuid::new_v4().to_string();
        let tx = self.connection.unchecked_transaction()?;
        tx.execute("INSERT INTO projects(id,name,source_endpoint,destination_endpoint,phase) VALUES(?1,?2,?3,?4,?5)", params![project.id, project.name, project.source_endpoint, project.destination_endpoint, project.phase.as_str()])?;
        tx.execute("INSERT INTO mailbox_jobs(id,project_id,source_mailbox,destination_mailbox,destination_identity,state) VALUES(?1,?2,?3,?4,?5,'queued')", params![job_id, project.id, source_mailbox, destination_mailbox, normalized_destination_identity(destination_mailbox, None)])?;
        tx.execute("INSERT INTO events(project_id,kind,detail) VALUES(?1,'project_created','Project created without credentials')", [&project.id])?;
        tx.commit()?;
        Ok((project, job_id))
    }
    #[cfg(test)]
    pub fn create_project_with_mailboxes(
        &self,
        name: &str,
        source: &str,
        destination: &str,
        mailboxes: &[(String, String)],
    ) -> rusqlite::Result<(Project, Vec<String>)> {
        if mailboxes.is_empty() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let mut destinations = BTreeSet::new();
        if mailboxes
            .iter()
            .any(|(_, destination)| !destinations.insert(destination.trim().to_ascii_lowercase()))
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
        let tx = self.connection.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO projects(id,name,source_endpoint,destination_endpoint,phase) VALUES(?1,?2,?3,?4,?5)",
            params![project.id, project.name, project.source_endpoint, project.destination_endpoint, project.phase.as_str()],
        )?;
        let mut ids = Vec::with_capacity(mailboxes.len());
        for (source_mailbox, destination_mailbox) in mailboxes {
            let id = Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO mailbox_jobs(id,project_id,source_mailbox,destination_mailbox,destination_identity,state) VALUES(?1,?2,?3,?4,?5,'queued')",
                params![id, project.id, source_mailbox, destination_mailbox, normalized_destination_identity(destination_mailbox, None)],
            )?;
            ids.push(id);
        }
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'project_created',?2)",
            params![
                project.id,
                format!("Batch created with {} mailbox jobs", ids.len())
            ],
        )?;
        tx.commit()?;
        Ok((project, ids))
    }
    pub fn create_project_with_mailbox_configs(
        &self,
        name: &str,
        source: &str,
        destination: &str,
        mailboxes: &[(String, String, String)],
    ) -> rusqlite::Result<(Project, Vec<String>)> {
        if mailboxes.is_empty() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let _total_profile_bytes = mailboxes.iter().try_fold(0usize, |total, (_, _, config)| {
            if config.len() > MAX_PERSISTED_PROFILE_BYTES {
                return Err(rusqlite::Error::InvalidQuery);
            }
            total
                .checked_add(config.len())
                .filter(|value| *value <= MAX_TOTAL_PERSISTED_PROFILE_BYTES)
                .ok_or(rusqlite::Error::InvalidQuery)
        })?;
        let mut destinations = BTreeSet::new();
        if mailboxes.iter().any(|(_, destination, config)| {
            !destinations.insert(normalized_destination_identity(destination, Some(config)))
        }) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let project = Project {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            source_endpoint: source.into(),
            destination_endpoint: destination.into(),
            phase: Phase::Discovery,
        };
        let tx = self.connection.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO projects(id,name,source_endpoint,destination_endpoint,phase) VALUES(?1,?2,?3,?4,?5)",
            params![project.id, project.name, project.source_endpoint, project.destination_endpoint, project.phase.as_str()],
        )?;
        let mut ids = Vec::with_capacity(mailboxes.len());
        for (source_mailbox, destination_mailbox, config) in mailboxes {
            let id = Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO mailbox_jobs(id,project_id,source_mailbox,destination_mailbox,destination_identity,state,config) VALUES(?1,?2,?3,?4,?5,'queued',?6)",
                params![
                    id,
                    project.id,
                    source_mailbox,
                    destination_mailbox,
                    normalized_destination_identity(destination_mailbox, Some(config)),
                    config
                ],
            )?;
            ids.push(id);
        }
        tx.execute(
            "INSERT INTO events(project_id,kind,detail) VALUES(?1,'project_created',?2)",
            params![
                project.id,
                format!("Batch created with {} mailbox jobs", ids.len())
            ],
        )?;
        tx.commit()?;
        Ok((project, ids))
    }
}
