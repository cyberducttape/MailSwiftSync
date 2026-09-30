//! Opt-in, local diagnostic transcripts for headless operators.
//!
//! These files are intentionally separate from the durable ledger.  Engine
//! output can contain mailbox and folder metadata, so the feature is disabled
//! by default and requires an operator-selected directory.

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::credentials::{ensure_private_directory, restrict_file_permissions};
use sha2::{Digest, Sha256};

const RETAIN_FILES: usize = 20;
const MAX_LINE_BYTES: usize = 16 * 1024;
const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
const MAX_DIRECTORY_BYTES: u64 = 128 * 1024 * 1024;
const MAX_FILENAME_COMPONENT_BYTES: usize = 128;
const WRITER_CAPACITY: usize = 64 * 1024;
const FLUSH_BYTES: usize = 64 * 1024;
const FLUSH_LINES: usize = 64;

pub(crate) struct DiagnosticLogger {
    directory: PathBuf,
    state: Mutex<LoggerState>,
}

#[derive(Default)]
struct LoggerState {
    files: HashMap<String, FileState>,
    run_paths: HashMap<String, Vec<PathBuf>>,
    total_bytes: u64,
}

struct FileState {
    run_id: String,
    project_id: String,
    job_id: String,
    part: u32,
    path: PathBuf,
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
            state: Mutex::new(LoggerState::default()),
        };
        if let Ok(mut state) = logger.state.lock() {
            logger.prune(&mut state)?;
        } else {
            return Err("diagnostic log lock was poisoned".to_owned());
        }
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
            .state
            .lock()
            .map_err(|_| "diagnostic log lock was poisoned".to_owned())?;
        if !state.files.contains_key(run_id) {
            let file = self.new_file(project_id, run_id, job_id, 0)?;
            state
                .run_paths
                .entry(run_id.to_owned())
                .or_default()
                .push(file.path.clone());
            state.files.insert(run_id.to_owned(), file);
            self.prune(&mut state)?;
        }
        let file_state = state
            .files
            .get_mut(run_id)
            .ok_or_else(|| "diagnostic log state was not initialized".to_owned())?;
        if file_state.project_id != project_id || file_state.job_id != job_id {
            return Err("diagnostic run identifier was reused for another mailbox".to_owned());
        }
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default();
        let bounded = truncate_line(line);
        let rendered = format!("{timestamp} [{stream}] {bounded}\n");
        let rotation = if file_state.bytes_written > 0
            && file_state.bytes_written.saturating_add(rendered.len()) > MAX_FILE_BYTES
        {
            file_state
                .file
                .flush()
                .map_err(|error| format!("could not rotate diagnostic log: {error}"))?;
            Some((
                file_state.project_id.clone(),
                file_state.run_id.clone(),
                file_state.job_id.clone(),
                file_state.part.saturating_add(1),
            ))
        } else {
            None
        };
        if let Some((project_id, run_id, job_id, part)) = rotation {
            let replacement = self.new_file(&project_id, &run_id, &job_id, part)?;
            state
                .run_paths
                .entry(run_id.to_owned())
                .or_default()
                .push(replacement.path.clone());
            state.files.insert(run_id.to_owned(), replacement);
            self.prune(&mut state)?;
        }
        if state.total_bytes.saturating_add(rendered.len() as u64) > MAX_DIRECTORY_BYTES {
            return Err("diagnostic directory byte limit reached".to_owned());
        }
        let total_bytes = state.total_bytes.saturating_add(rendered.len() as u64);
        let file_state = state
            .files
            .get_mut(run_id)
            .ok_or_else(|| "diagnostic log state was not initialized".to_owned())?;
        file_state
            .file
            .write_all(rendered.as_bytes())
            .map_err(|error| format!("could not write diagnostic log: {error}"))?;
        file_state.bytes_written = file_state.bytes_written.saturating_add(rendered.len());
        file_state.pending_flush_bytes = file_state
            .pending_flush_bytes
            .saturating_add(rendered.len());
        file_state.pending_flush_lines = file_state.pending_flush_lines.saturating_add(1);
        if file_state.pending_flush_bytes >= FLUSH_BYTES
            || file_state.pending_flush_lines >= FLUSH_LINES
        {
            file_state
                .file
                .flush()
                .map_err(|error| format!("could not flush diagnostic log: {error}"))?;
            file_state.pending_flush_bytes = 0;
            file_state.pending_flush_lines = 0;
        }
        state.total_bytes = total_bytes;
        Ok(())
    }

    pub(crate) fn finish_run(&self, run_id: &str) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "diagnostic log lock was poisoned".to_owned())?;
        let flush_result = state.files.remove(run_id).map(|mut file| {
            file.file
                .flush()
                .map_err(|error| format!("could not flush completed diagnostic log: {error}"))
        });
        state.run_paths.remove(run_id);
        let prune_result = self.prune(&mut state);
        flush_result.unwrap_or(Ok(()))?;
        prune_result
    }

    fn new_file(
        &self,
        project_id: &str,
        run_id: &str,
        job_id: &str,
        part: u32,
    ) -> Result<FileState, String> {
        let mailbox_hash = pseudonym_hash(job_id);
        let filename = if part == 0 {
            format!(
                "mailswiftsync-{project_id}-{run_id}-mailbox-{mailbox_hash}.log",
                project_id = safe_component(project_id),
                run_id = safe_component(run_id),
            )
        } else {
            format!(
                "mailswiftsync-{project_id}-{run_id}-mailbox-{mailbox_hash}-part-{part:04}.log",
                project_id = safe_component(project_id),
                run_id = safe_component(run_id),
            )
        };
        let path = self.directory.join(filename);
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options
            .open(&path)
            .map_err(|error| format!("could not create diagnostic log: {error}"))?;
        restrict_file_permissions(&path)
            .map_err(|error| format!("could not restrict diagnostic log permissions: {error}"))?;
        let mut file = BufWriter::with_capacity(WRITER_CAPACITY, file);
        let header = format!(
            "# MailSwiftSync diagnostic log; project_id={project_id} run_id={run_id} mailbox_hash={mailbox_hash}\n"
        );
        file.write_all(header.as_bytes())
            .map_err(|error| format!("could not initialize diagnostic log: {error}"))?;
        file.flush()
            .map_err(|error| format!("could not flush diagnostic log header: {error}"))?;
        Ok(FileState {
            run_id: run_id.to_owned(),
            project_id: project_id.to_owned(),
            job_id: job_id.to_owned(),
            part,
            path,
            file,
            bytes_written: header.len(),
            pending_flush_bytes: header.len(),
            pending_flush_lines: 1,
        })
    }

    fn prune(&self, state: &mut LoggerState) -> Result<(), String> {
        let active_paths = state
            .run_paths
            .values()
            .flat_map(|paths| paths.iter().cloned())
            .collect::<HashSet<_>>();
        let mut files = fs::read_dir(&self.directory)
            .map_err(|error| format!("could not list diagnostic log directory: {error}"))?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                let name = path.file_name()?.to_str()?;
                (name.starts_with("mailswiftsync-") && entry.file_type().ok()?.is_file())
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
        let mut retained_files = 0_usize;
        for (_, path) in files {
            let size = fs::metadata(&path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            if active_paths.contains(&path)
                || (retained_files < RETAIN_FILES
                    && retained_bytes.saturating_add(size) <= MAX_DIRECTORY_BYTES)
            {
                retained_files = retained_files.saturating_add(1);
                retained_bytes = retained_bytes.saturating_add(size);
                continue;
            }
            fs::remove_file(path)
                .map_err(|error| format!("could not prune diagnostic log: {error}"))?;
        }
        state.total_bytes = fs::read_dir(&self.directory)
            .map_err(|error| format!("could not recount diagnostic logs: {error}"))?
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_type().is_ok_and(|kind| kind.is_file())
                    && entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.starts_with("mailswiftsync-"))
            })
            .filter_map(|entry| entry.metadata().ok())
            .filter(|metadata| metadata.is_file())
            .map(|metadata| metadata.len())
            .sum();
        Ok(())
    }
}

impl Drop for DiagnosticLogger {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            for file in state.files.values_mut() {
                let _ = file.file.flush();
            }
            state.files.clear();
            state.run_paths.clear();
            let _ = self.prune(&mut state);
        }
    }
}

fn safe_component(value: &str) -> String {
    value
        .chars()
        .take(MAX_FILENAME_COMPONENT_BYTES)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn pseudonym_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
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
        assert_eq!(
            safe_component(&"x".repeat(256)).len(),
            MAX_FILENAME_COMPONENT_BYTES
        );
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

    #[cfg(unix)]
    #[test]
    fn diagnostic_log_is_owner_only_at_creation_in_a_readable_directory() {
        use std::os::unix::fs::PermissionsExt;
        let directory =
            std::env::temp_dir().join(format!("mailswiftsync-diagnostic-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        let logger = DiagnosticLogger::create(&directory).unwrap();
        logger
            .write_line("project", "run", "job", "stdout", "mailbox metadata")
            .unwrap();
        drop(logger);
        let entry = std::fs::read_dir(&directory)
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(
            entry.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn concurrent_child_runs_write_separate_pseudonymous_transcripts() {
        let directory = std::env::temp_dir().join(format!(
            "mailswiftsync-diagnostic-concurrent-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let logger = std::sync::Arc::new(DiagnosticLogger::create(&directory).unwrap());
        let workers = (0..8)
            .map(|index| {
                let logger = std::sync::Arc::clone(&logger);
                std::thread::spawn(move || {
                    let job_id = format!("private-mailbox-id-{index}");
                    let run_id = format!("child-run-{index}");
                    for _ in 0..8 {
                        logger
                            .write_line(
                                "project",
                                &run_id,
                                &job_id,
                                "stdout",
                                &format!("marker-{index}"),
                            )
                            .unwrap();
                    }
                    logger.finish_run(&run_id).unwrap();
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap();
        }
        drop(logger);

        let files = fs::read_dir(&directory)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(files.len(), 8);
        for entry in &files {
            let content = fs::read_to_string(entry.path()).unwrap();
            let marker = (0..8)
                .map(|index| format!("marker-{index}"))
                .find(|marker| content.contains(marker))
                .expect("each child transcript contains its marker");
            let own_index = marker
                .trim_start_matches("marker-")
                .parse::<usize>()
                .unwrap();
            assert_eq!(content.matches(&marker).count(), 8);
            assert!(!content.contains("private-mailbox-id-"));
            assert!(
                !entry
                    .file_name()
                    .to_string_lossy()
                    .contains("private-mailbox-id-")
            );
            for other in 0..8 {
                if other != own_index {
                    assert!(!content.contains(&format!("marker-{other}")));
                }
            }
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn mailbox_pseudonym_is_stable_and_does_not_contain_the_identifier() {
        let identifier = "customer-mailbox-123";
        let hash = pseudonym_hash(identifier);
        assert_eq!(hash, pseudonym_hash(identifier));
        assert_eq!(hash.len(), 24);
        assert!(!hash.contains(identifier));
    }
}
