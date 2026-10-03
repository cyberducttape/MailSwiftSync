#![no_main]

mod credentials {
    use std::{
        fs::{self, File, OpenOptions},
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    pub(crate) struct CleanupGuard(Vec<PathBuf>);

    impl CleanupGuard {
        pub(crate) fn new(paths: Vec<PathBuf>) -> Self {
            Self(paths)
        }
    }

    impl Drop for CleanupGuard {
        fn drop(&mut self) {
            for path in &self.0 {
                let _ = fs::remove_dir_all(path);
            }
        }
    }

    pub(crate) fn create_secret_directory() -> Result<PathBuf, String> {
        let path = std::env::temp_dir().join(format!(
            "mailswiftsync-migration-snapshot-fuzz-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&path)
            .map_err(|error| format!("could not create fuzz staging directory: {error}"))?;
        Ok(path)
    }

    pub(crate) fn open_secret_file(path: &std::path::Path) -> std::io::Result<File> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path)
    }

    pub(crate) fn verify_private_directory(path: &std::path::Path) -> std::io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                "fuzz staging path is not a real directory",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o022 != 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "fuzz staging directory is writable by other users",
                ));
            }
        }
        Ok(())
    }
}

mod atomic_artifact {
    use std::{io, path::Path};

    pub(crate) fn write_private_atomic(path: &Path, content: &str) -> io::Result<()> {
        std::fs::write(path, content)
    }
}

mod reports {
    pub(crate) mod integrity {
        pub(crate) fn with_proof_digest(
            mut value: serde_json::Value,
        ) -> Result<serde_json::Value, String> {
            value
                .as_object_mut()
                .ok_or("report must be an object")?
                .insert(
                    "proof_digest".into(),
                    serde_json::Value::String("fuzz".into()),
                );
            Ok(value)
        }
    }
}

#[path = "../../src/migrate_audit.rs"]
mod migrate_audit;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let Ok(base) = credentials::create_secret_directory() else {
        return;
    };
    let _cleanup = credentials::CleanupGuard::new(vec![base.clone()]);
    let source = base.join("source.json");
    let destination = base.join("destination.json");
    let report = base.join("report.json");
    let _ = std::fs::write(&source, data);
    let _ = std::fs::write(&destination, data);
    let _ = migrate_audit::compare_files(&source, &destination, &report);
});
