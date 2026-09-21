//! Opt-in, local diagnostic transcripts for headless operators.
//!
//! These files are intentionally separate from the durable ledger.  Engine
//! output can contain mailbox and folder metadata, so the feature is disabled
//! by default and requires an operator-selected directory.

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::credentials::{restrict_directory_permissions, restrict_file_permissions};

const RETAIN_FILES: usize = 20;
const MAX_LINE_BYTES: usize = 16 * 1024;

pub(crate) struct DiagnosticLogger {
    directory: PathBuf,
    file: Mutex<Option<FileState>>,
}

struct FileState {
    run_id: String,
    file: File,
}

impl DiagnosticLogger {
    pub(crate) fn create(directory: &Path) -> Result<Self, String> {
        let existed = match fs::symlink_metadata(directory) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err("diagnostic log directory must not be a symlink".into());
                }
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(format!(
                    "could not inspect diagnostic log directory: {error}"
                ));
            }
        };
        fs::create_dir_all(directory)
            .map_err(|error| format!("could not create diagnostic log directory: {error}"))?;
        let metadata = fs::symlink_metadata(directory)
            .map_err(|error| format!("could not inspect diagnostic log directory: {error}"))?;
        if !metadata.is_dir() {
            return Err("diagnostic log path is not a directory".into());
        }
        if !existed {
            restrict_directory_permissions(directory)
                .map_err(|error| format!("could not restrict diagnostic log directory: {error}"))?;
        }
        let logger = Self {
            directory: directory.to_owned(),
            file: Mutex::new(None),
        };
        logger.prune()?;
        Ok(logger)
    }

    pub(crate) fn write_line(
        &self,
        project_id: &str,
        run_id: &str,
        job_id: &str,
        stream: &str,
        line: &str,
    ) -> Result<(), String> {
        let mut state = self
            .file
            .lock()
            .map_err(|_| "diagnostic log lock was poisoned".to_owned())?;
        if state
            .as_ref()
            .is_none_or(|current| current.run_id != run_id)
        {
            let filename = format!(
                "mailswiftsync-{project_id}-{run_id}.log",
                project_id = safe_component(project_id),
                run_id = safe_component(run_id),
            );
            let path = self.directory.join(filename);
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
                .map_err(|error| format!("could not create diagnostic log: {error}"))?;
            restrict_file_permissions(&path).map_err(|error| {
                format!("could not restrict diagnostic log permissions: {error}")
            })?;
            writeln!(
                file,
                "# MailSwiftSync diagnostic log; project_id={project_id} run_id={run_id} job_id={job_id}"
            )
            .map_err(|error| format!("could not initialize diagnostic log: {error}"))?;
            *state = Some(FileState {
                run_id: run_id.to_owned(),
                file,
            });
            self.prune()?;
        }
        let state = state
            .as_mut()
            .expect("diagnostic log state was just initialized");
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default();
        let bounded = truncate_line(line);
        writeln!(state.file, "{timestamp} [{stream}] {bounded}")
            .map_err(|error| format!("could not write diagnostic log: {error}"))?;
        state
            .file
            .flush()
            .map_err(|error| format!("could not flush diagnostic log: {error}"))
    }

    fn prune(&self) -> Result<(), String> {
        let mut files = fs::read_dir(&self.directory)
            .map_err(|error| format!("could not list diagnostic log directory: {error}"))?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                let name = path.file_name()?.to_str()?;
                name.starts_with("mailswiftsync-")
                    .then(|| {
                        entry
                            .metadata()
                            .ok()
                            .and_then(|m| m.modified().ok())
                            .map(|t| (t, path))
                    })
                    .flatten()
            })
            .collect::<Vec<_>>();
        files.sort_by(|left, right| right.0.cmp(&left.0));
        for (_, path) in files.into_iter().skip(RETAIN_FILES) {
            fs::remove_file(path)
                .map_err(|error| format!("could not prune diagnostic log: {error}"))?;
        }
        Ok(())
    }
}

fn safe_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn truncate_line(value: &str) -> String {
    if value.len() <= MAX_LINE_BYTES {
        return value.replace('\n', "\\n").replace('\r', "\\r");
    }
    let mut end = MAX_LINE_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}…[truncated]",
        value[..end].replace('\n', "\\n").replace('\r', "\\r")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn components_are_safe_for_filenames() {
        assert_eq!(safe_component("a/b c"), "a_b_c");
    }

    #[test]
    fn lines_are_bounded() {
        assert!(truncate_line(&"x".repeat(MAX_LINE_BYTES + 100)).len() < MAX_LINE_BYTES + 32);
    }

    #[cfg(unix)]
    #[test]
    fn existing_directory_permissions_are_not_changed() {
        use std::os::unix::fs::PermissionsExt;
        let directory =
            std::env::temp_dir().join(format!("mailswiftsync-diagnostic-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("shared");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _logger = DiagnosticLogger::create(&path).unwrap();
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o755
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
