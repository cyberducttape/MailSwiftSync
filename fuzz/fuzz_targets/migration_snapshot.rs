#![no_main]

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
                .insert("proof_digest".into(), serde_json::Value::String("fuzz".into()));
            Ok(value)
        }
    }
}

#[path = "../../src/migrate_audit.rs"]
mod migrate_audit;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let base = std::env::temp_dir().join(format!("mailswiftsync-fuzz-{}", std::process::id()));
    let source = base.with_extension("source.json");
    let destination = base.with_extension("destination.json");
    let report = base.with_extension("report.json");
    let _ = std::fs::write(&source, data);
    let _ = std::fs::write(&destination, data);
    let _ = migrate_audit::compare_files(&source, &destination, &report);
    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_file(destination);
    let _ = std::fs::remove_file(report);
});
