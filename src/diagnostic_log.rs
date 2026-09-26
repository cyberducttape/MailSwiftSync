//! Opt-in, local diagnostic transcripts for headless operators.
//!
//! These files are intentionally separate from the durable ledger.  Engine
//! output can contain mailbox and folder metadata, so the feature is disabled
//! by default and requires an operator-selected directory.

use std::{
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::credentials::{ensure_private_directory, restrict_file_permissions};

const RETAIN_FILES: usize = 20;
const MAX_LINE_BYTES: usize = 16 * 1024;
const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
const MAX_DIRECTORY_BYTES: u64 = 128 * 1024 * 1024;
const WRITER_CAPACITY: usize = 64 * 1024;
const FLUSH_BYTES: usize = 64 * 1024;
const FLUSH_LINES: usize = 64;

pub(crate) struct DiagnosticLogger {
    directory: PathBuf,
    file: Mutex<Option<FileState>>,
}

struct FileState {
    run_id: String,
    project_id: String,
    job_id: String,
    part: u32,
    file: BufWriter<File>,
    bytes_written: usize,
    pending_flush_bytes: usize,
    pending_flush_lines: usize,
}

impl DiagnosticLogger {
    pub(crate) fn create(directory: &Path) -> Result<Self, String> {
        ensure_private_directory(directory)
            .map_err(|error| format!("could not secure diagnostic log directory: {error}"))?;
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
            *state = Some(self.new_file(project_id, run_id, job_id, 0)?);
            self.prune()?;
        }
        let Some(state) = state.as_mut() else {
            return Err("diagnostic log state was not initialized".to_owned());
        };
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default();
        let bounded = truncate_line(line);
        let rendered = format!("{timestamp} [{stream}] {bounded}\n");
        if state.bytes_written > 0
            && state.bytes_written.saturating_add(rendered.len()) > MAX_FILE_BYTES
        {
            state
                .file
                .flush()
                .map_err(|error| format!("could not rotate diagnostic log: {error}"))?;
            let next_part = state.part.saturating_add(1);
            let replacement =
                self.new_file(&state.project_id, &state.run_id, &state.job_id, next_part)?;
            *state = replacement;
            self.prune()?;
        }
        state
            .file
            .write_all(rendered.as_bytes())
            .map_err(|error| format!("could not write diagnostic log: {error}"))?;
        state.bytes_written = state.bytes_written.saturating_add(rendered.len());
        state.pending_flush_bytes = state.pending_flush_bytes.saturating_add(rendered.len());
        state.pending_flush_lines = state.pending_flush_lines.saturating_add(1);
        if state.pending_flush_bytes >= FLUSH_BYTES || state.pending_flush_lines >= FLUSH_LINES {
            state
                .file
                .flush()
                .map_err(|error| format!("could not flush diagnostic log: {error}"))?;
            state.pending_flush_bytes = 0;
            state.pending_flush_lines = 0;
        }
        Ok(())
    }

    fn new_file(
        &self,
        project_id: &str,
        run_id: &str,
        job_id: &str,
        part: u32,
    ) -> Result<FileState, String> {
        let filename = if part == 0 {
            format!(
                "mailswiftsync-{project_id}-{run_id}.log",
                project_id = safe_component(project_id),
                run_id = safe_component(run_id),
            )
        } else {
            format!(
                "mailswiftsync-{project_id}-{run_id}-part-{part:04}.log",
                project_id = safe_component(project_id),
                run_id = safe_component(run_id),
            )
        };
        let path = self.directory.join(filename);
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|error| format!("could not create diagnostic log: {error}"))?;
        restrict_file_permissions(&path)
            .map_err(|error| format!("could not restrict diagnostic log permissions: {error}"))?;
        let mut file = BufWriter::with_capacity(WRITER_CAPACITY, file);
        let header = format!(
            "# MailSwiftSync diagnostic log; project_id={project_id} run_id={run_id} job_id={job_id}\n"
        );
        file.write_all(header.as_bytes())
            .map_err(|error| format!("could not initialize diagnostic log: {error}"))?;
        Ok(FileState {
            run_id: run_id.to_owned(),
            project_id: project_id.to_owned(),
            job_id: job_id.to_owned(),
            part,
            file,
            bytes_written: header.len(),
            pending_flush_bytes: header.len(),
            pending_flush_lines: 1,
        })
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
        let mut retained_bytes = 0_u64;
        for (index, (_, path)) in files.into_iter().enumerate() {
            let size = fs::metadata(&path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            if index < RETAIN_FILES && retained_bytes.saturating_add(size) <= MAX_DIRECTORY_BYTES {
                retained_bytes = retained_bytes.saturating_add(size);
                continue;
            }
            fs::remove_file(path)
                .map_err(|error| format!("could not prune diagnostic log: {error}"))?;
        }
        Ok(())
    }
}

impl Drop for DiagnosticLogger {
    fn drop(&mut self) {
        if let Ok(mut state) = self.file.lock()
            && let Some(state) = state.as_mut()
        {
            let _ = state.file.flush();
        }
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

    #[test]
    fn diagnostic_files_rotate_before_the_per_file_limit() {
        let directory = std::env::temp_dir().join(format!(
            "mailswiftsync-diagnostic-rotation-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let logger = DiagnosticLogger::create(&directory).unwrap();
        let line = "x".repeat(MAX_LINE_BYTES - 32);
        for _ in 0..((MAX_FILE_BYTES / line.len()) + 2) {
            logger
                .write_line("project", "run", "job", "stdout", &line)
                .unwrap();
        }
        drop(logger);
        let files = fs::read_dir(&directory)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(files.len() >= 2);
        for entry in files {
            assert!(entry.metadata().unwrap().len() <= MAX_FILE_BYTES as u64);
        }
        fs::remove_dir_all(directory).unwrap();
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
