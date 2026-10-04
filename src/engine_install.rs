//! One-command installation of the qualified imapsync engine.
//!
//! MailSwiftSync's trusted verification contract is qualified for exactly
//! imapsync 2.314, so the installer only ever fetches that release, from the
//! official distribution, over HTTPS, and refuses any artifact whose SHA-256
//! differs from the digest pinned here (the Debian digest is the same one the
//! container image pins). After installing, the engine's own `--version`
//! output must report 2.314 before the path is handed back.
//!
//! - Debian/Ubuntu: the verified `.deb` is installed with the system package
//!   manager so its Perl dependencies resolve; elevation uses `sudo` from a
//!   terminal or `pkexec` from the desktop.
//! - Windows: the verified portable zip is unpacked into a private per-user
//!   engines directory; no elevation is needed.
//! - Elsewhere the installer explains the supported manual route instead of
//!   guessing at a Perl module installation.

use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub(crate) const QUALIFIED_IMAPSYNC_VERSION: &str = "2.314";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Artifact {
    pub(crate) url: &'static str,
    pub(crate) file_name: &'static str,
    pub(crate) sha256: &'static str,
    pub(crate) max_bytes: u64,
}

pub(crate) const DEBIAN_PACKAGE: Artifact = Artifact {
    url: "https://imapsync.lamiral.info/dist2/imapsync-2.314.deb",
    file_name: "imapsync-2.314.deb",
    sha256: "e8b9b410ac763749cd05568e51ab7a158c041b1ef78ca559f47dc1fa377d0870",
    max_bytes: 64 * 1024 * 1024,
};

pub(crate) const WINDOWS_PORTABLE: Artifact = Artifact {
    url: "https://imapsync.lamiral.info/dist2/imapsync_2.314.zip",
    file_name: "imapsync_2.314.zip",
    sha256: "61972bf94532bf186dd6d6ee54a4c70bb7ab3bdb79f324321cd742fd50953a0c",
    max_bytes: 64 * 1024 * 1024,
};

/// Directory inside the Windows zip that holds the engine.
const WINDOWS_ARCHIVE_ROOT: &str = "imapsync_2.314/";
const WINDOWS_EXECUTABLE: &str = "imapsync.exe";
const DEBIAN_EXECUTABLE: &str = "/usr/bin/imapsync";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstallMethod {
    DebianPackage,
    WindowsPortable,
}

/// How this host can obtain the qualified engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstallPlan {
    Automatic(InstallMethod),
    /// No automatic route; the text names the supported manual one.
    Manual(&'static str),
}

/// How the Debian route obtains root for the package manager.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Elevation {
    /// `sudo`, prompting on the controlling terminal.
    Sudo,
    /// `pkexec`, prompting with the desktop's polkit agent.
    Pkexec,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InstallProgress {
    Downloading { received: u64, total: Option<u64> },
    Verified,
    Installing,
    Checking,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstalledEngine {
    pub(crate) executable: PathBuf,
    pub(crate) version: String,
}

const MACOS_GUIDANCE: &str = "Automatic installation is not available on macOS because imapsync needs Perl modules from CPAN. Install imapsync 2.314 from the official tarball (https://imapsync.lamiral.info/dist2/imapsync-2.314.tgz) following its INSTALL.d/INSTALL.OnMac.txt, or run the pinned MailSwiftSync container image, then enter the imapsync path in MailSwiftSync.";
const OTHER_GUIDANCE: &str = "Automatic installation supports Debian/Ubuntu (apt) and Windows. Install imapsync 2.314 from the official distribution (https://imapsync.lamiral.info/dist2/) using your platform's INSTALL.d instructions, or run the pinned MailSwiftSync container image, then enter the imapsync path in MailSwiftSync.";

pub(crate) fn plan_for_host() -> InstallPlan {
    plan_for(
        std::env::consts::OS,
        Path::new("/etc/debian_version").exists(),
        Path::new("/usr/bin/apt-get").exists(),
    )
}

/// Whether the desktop can request elevation for the package manager.
pub(crate) fn desktop_elevation_available() -> bool {
    Path::new("/usr/bin/pkexec").exists()
}

fn plan_for(os: &str, debian_release_file: bool, apt_available: bool) -> InstallPlan {
    match os {
        "windows" => InstallPlan::Automatic(InstallMethod::WindowsPortable),
        "linux" if debian_release_file && apt_available => {
            InstallPlan::Automatic(InstallMethod::DebianPackage)
        }
        "macos" => InstallPlan::Manual(MACOS_GUIDANCE),
        _ => InstallPlan::Manual(OTHER_GUIDANCE),
    }
}

/// Private per-user directory for downloaded and unpacked engines, beside
/// the durable state so `MAILSWIFTSYNC_STATE_PATH` isolation also applies.
pub(crate) fn engines_directory() -> Result<PathBuf, String> {
    let state = crate::storage_paths::persistent_state_path()?;
    let parent = state
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or("the state path has no parent directory for installed engines")?;
    let directory = parent.join("engines");
    crate::credentials::ensure_private_directory(&directory)
        .map_err(|error| format!("could not prepare {}: {error}", directory.display()))?;
    Ok(directory)
}

#[cfg(test)]
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn file_sha256(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Copy `reader` into `destination` while hashing, refusing anything larger
/// than the artifact's bound, and accept it only when the digest matches.
/// The file is written under a temporary name and renamed into place only
/// after verification, so a partial or substituted download is never used.
pub(crate) fn store_verified<R: Read>(
    artifact: &Artifact,
    mut reader: R,
    total: Option<u64>,
    directory: &Path,
    mut progress: impl FnMut(InstallProgress),
) -> Result<PathBuf, String> {
    if total.is_some_and(|total| total > artifact.max_bytes) {
        return Err(format!(
            "{} is larger than the {} MB limit for this artifact",
            artifact.file_name,
            artifact.max_bytes / 1_000_000
        ));
    }
    let final_path = directory.join(artifact.file_name);
    let partial_path = directory.join(format!(".{}.partial", artifact.file_name));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&partial_path)
        .map_err(|error| format!("could not write {}: {error}", partial_path.display()))?;
    let mut hasher = Sha256::new();
    let mut received = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    let result = loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break Ok(()),
            Ok(read) => read,
            Err(error) => break Err(format!("download interrupted: {error}")),
        };
        received += read as u64;
        if received > artifact.max_bytes {
            break Err(format!(
                "{} exceeded the {} MB limit",
                artifact.file_name,
                artifact.max_bytes / 1_000_000
            ));
        }
        hasher.update(&buffer[..read]);
        if let Err(error) = file.write_all(&buffer[..read]) {
            break Err(format!("could not write the download: {error}"));
        }
        progress(InstallProgress::Downloading { received, total });
    };
    let result = result.and_then(|()| {
        file.sync_all()
            .map_err(|error| format!("could not flush the download: {error}"))?;
        let digest: String = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if digest != artifact.sha256 {
            return Err(format!(
                "{} failed SHA-256 verification (expected {}, received {digest}); nothing was installed",
                artifact.file_name, artifact.sha256
            ));
        }
        std::fs::rename(&partial_path, &final_path)
            .map_err(|error| format!("could not store the verified download: {error}"))
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&partial_path);
    }
    result?;
    progress(InstallProgress::Verified);
    Ok(final_path)
}

/// Download `artifact` into `directory` unless a copy with the pinned digest
/// is already there.
pub(crate) fn fetch_verified(
    artifact: &Artifact,
    directory: &Path,
    mut progress: impl FnMut(InstallProgress),
) -> Result<PathBuf, String> {
    let cached = directory.join(artifact.file_name);
    if file_sha256(&cached).is_ok_and(|digest| digest == artifact.sha256) {
        progress(InstallProgress::Verified);
        return Ok(cached);
    }
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(15 * 60))
        .user_agent(concat!("MailSwiftSync/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| format!("could not prepare the download client: {error}"))?;
    let response = client
        .get(artifact.url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| format!("could not download {}: {error}", artifact.url))?;
    let total = response.content_length();
    store_verified(artifact, response, total, directory, progress)
}

/// Unpack only the engine directory of the verified Windows zip, rejecting
/// any entry whose path would escape the destination.
pub(crate) fn unpack_windows_engine(archive: &Path, destination: &Path) -> Result<PathBuf, String> {
    let file = std::fs::File::open(archive)
        .map_err(|error| format!("could not open {}: {error}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|error| format!("the engine archive is not a valid zip: {error}"))?;
    let mut executable = None;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|error| format!("could not read the engine archive: {error}"))?;
        let Some(relative) = entry.enclosed_name() else {
            return Err(format!("unsafe path in engine archive: {}", entry.name()));
        };
        let Ok(inside) = relative.strip_prefix(WINDOWS_ARCHIVE_ROOT.trim_end_matches('/')) else {
            continue;
        };
        if inside.as_os_str().is_empty() {
            continue;
        }
        let target = destination.join(inside);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)
                .map_err(|error| format!("could not create {}: {error}", target.display()))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
        }
        let mut output = std::fs::File::create(&target)
            .map_err(|error| format!("could not write {}: {error}", target.display()))?;
        std::io::copy(&mut entry, &mut output)
            .map_err(|error| format!("could not unpack {}: {error}", target.display()))?;
        if inside == Path::new(WINDOWS_EXECUTABLE) {
            executable = Some(target);
        }
    }
    executable.ok_or_else(|| format!("{WINDOWS_EXECUTABLE} is missing from the engine archive"))
}

fn package_install_command(elevation: Elevation, package: &Path) -> Command {
    let mut command = Command::new(match elevation {
        Elevation::Sudo => "sudo",
        Elevation::Pkexec => "pkexec",
    });
    command
        .arg("/usr/bin/apt-get")
        .arg("install")
        .arg("--no-install-recommends")
        .arg("--yes")
        .arg(package);
    command
}

/// The exact privileged command, for operators who prefer to run it.
pub(crate) fn manual_package_command(package: &Path) -> String {
    let package = package.display().to_string();
    let shell_quoted_package = format!("'{}'", package.replace('\'', "'\\''"));
    format!(
        "sudo apt-get install --no-install-recommends --yes {}",
        shell_quoted_package
    )
}

fn confirm_qualified(executable: &Path) -> Result<InstalledEngine, String> {
    let identity = crate::runner::resolve_imapsync_identity(&executable.to_string_lossy());
    if identity.output_profile != crate::verification::ImapsyncOutputProfile::Packaged2314 {
        return Err(format!(
            "{} reports \"{}\", not the qualified imapsync {QUALIFIED_IMAPSYNC_VERSION}",
            executable.display(),
            identity.version
        ));
    }
    Ok(InstalledEngine {
        executable: executable.to_path_buf(),
        version: identity.version,
    })
}

/// Download, verify, install, and confirm the qualified engine.
pub(crate) fn install(
    method: InstallMethod,
    elevation: Elevation,
    mut progress: impl FnMut(InstallProgress),
) -> Result<InstalledEngine, String> {
    let directory = engines_directory()?;
    match method {
        InstallMethod::DebianPackage => {
            let package = fetch_verified(&DEBIAN_PACKAGE, &directory, &mut progress)?;
            progress(InstallProgress::Installing);
            let status = package_install_command(elevation, &package)
                .status()
                .map_err(|error| {
                    format!(
                        "could not start the package manager ({error}); run it yourself: {}",
                        manual_package_command(&package)
                    )
                })?;
            if !status.success() {
                return Err(format!(
                    "package installation did not complete successfully ({status}). The package manager may have changed system state. Review apt/dpkg output before retrying. To retry manually run: {}",
                    manual_package_command(&package)
                ));
            }
            progress(InstallProgress::Checking);
            confirm_qualified(Path::new(DEBIAN_EXECUTABLE))
        }
        InstallMethod::WindowsPortable => {
            let archive = fetch_verified(&WINDOWS_PORTABLE, &directory, &mut progress)?;
            progress(InstallProgress::Installing);
            let destination = directory.join(format!("imapsync-{QUALIFIED_IMAPSYNC_VERSION}"));
            let executable = unpack_windows_engine(&archive, &destination)?;
            progress(InstallProgress::Checking);
            confirm_qualified(&executable)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Artifact, DEBIAN_PACKAGE, InstallMethod, InstallPlan, InstallProgress,
        manual_package_command, plan_for, sha256_hex, store_verified, unpack_windows_engine,
    };
    use std::io::Write;

    fn private_directory(name: &str) -> std::path::PathBuf {
        let directory = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!("mailswiftsync-{name}-{}", uuid::Uuid::new_v4()));
        crate::credentials::ensure_private_directory(&directory).unwrap();
        directory
    }

    #[test]
    fn only_debian_with_apt_and_windows_are_automatic() {
        assert_eq!(
            plan_for("linux", true, true),
            InstallPlan::Automatic(InstallMethod::DebianPackage)
        );
        assert!(matches!(
            plan_for("linux", false, true),
            InstallPlan::Manual(_)
        ));
        assert!(matches!(
            plan_for("linux", true, false),
            InstallPlan::Manual(_)
        ));
        assert_eq!(
            plan_for("windows", false, false),
            InstallPlan::Automatic(InstallMethod::WindowsPortable)
        );
        assert!(
            matches!(plan_for("macos", false, false), InstallPlan::Manual(text) if text.contains("2.314"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn manual_package_command_shell_quotes_package_path() {
        let package = std::path::Path::new("/tmp/dir with 'quote'/$() ;/imapsync.deb");
        assert_eq!(
            manual_package_command(package),
            r#"sudo apt-get install --no-install-recommends --yes '/tmp/dir with '\''quote'\''/$() ;/imapsync.deb'"#
        );
    }

    #[test]
    fn pinned_debian_digest_matches_the_container_image_pin() {
        let dockerfile = include_str!("../Dockerfile");
        assert!(dockerfile.contains(&format!("IMAPSYNC_SHA256={}", DEBIAN_PACKAGE.sha256)));
        assert!(dockerfile.contains("IMAPSYNC_VERSION=2.314"));
    }

    #[test]
    fn verified_download_is_stored_only_when_the_digest_matches() {
        let directory = private_directory("engine-download");
        let payload = b"qualified engine bytes".to_vec();
        let artifact = Artifact {
            url: "https://example.invalid/engine",
            file_name: "engine.bin",
            sha256: Box::leak(sha256_hex(&payload).into_boxed_str()),
            max_bytes: 1024,
        };
        let mut events = Vec::new();
        let stored = store_verified(
            &artifact,
            payload.as_slice(),
            Some(payload.len() as u64),
            &directory,
            |event| events.push(event),
        )
        .unwrap();
        assert_eq!(std::fs::read(&stored).unwrap(), payload);
        assert_eq!(events.last(), Some(&InstallProgress::Verified));

        let tampered = Artifact {
            file_name: "tampered.bin",
            ..artifact
        };
        let error =
            store_verified(&tampered, &b"substituted"[..], None, &directory, |_| {}).unwrap_err();
        assert!(error.contains("SHA-256"), "{error}");
        assert!(!directory.join("tampered.bin").exists());
        assert!(!directory.join(".tampered.bin.partial").exists());

        let bounded = Artifact {
            file_name: "large.bin",
            max_bytes: 4,
            ..artifact
        };
        let error =
            store_verified(&bounded, payload.as_slice(), None, &directory, |_| {}).unwrap_err();
        assert!(error.contains("limit"), "{error}");
        assert!(!directory.join("large.bin").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// Opt-in check against the real upstream zip when re-pinning:
    /// `MAILSWIFTSYNC_TEST_ENGINE_ZIP=<path> cargo test -- --ignored real_windows`.
    #[test]
    #[ignore]
    fn real_windows_archive_matches_the_pin_and_unpacks() {
        let Some(archive) = std::env::var_os("MAILSWIFTSYNC_TEST_ENGINE_ZIP") else {
            return;
        };
        let archive = std::path::PathBuf::from(archive);
        assert_eq!(
            super::file_sha256(&archive).unwrap(),
            super::WINDOWS_PORTABLE.sha256
        );
        let directory = private_directory("engine-real-unpack");
        let executable = unpack_windows_engine(&archive, &directory).unwrap();
        assert!(std::fs::metadata(&executable).unwrap().len() > 1_000_000);
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// Opt-in network check of the real download path (no installation):
    /// `MAILSWIFTSYNC_TEST_ENGINE_DOWNLOAD=1 cargo test -- --ignored real_download`.
    #[test]
    #[ignore]
    fn real_download_is_verified_against_the_pin() {
        if std::env::var_os("MAILSWIFTSYNC_TEST_ENGINE_DOWNLOAD").is_none() {
            return;
        }
        let directory = private_directory("engine-real-download");
        let mut verified = false;
        let path = super::fetch_verified(&DEBIAN_PACKAGE, &directory, |event| {
            verified |= event == InstallProgress::Verified;
        })
        .unwrap();
        assert!(verified);
        assert_eq!(super::file_sha256(&path).unwrap(), DEBIAN_PACKAGE.sha256);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn windows_unpack_keeps_only_the_engine_directory_and_rejects_traversal() {
        let directory = private_directory("engine-unpack");
        let archive = directory.join("engine.zip");
        {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("imapsync_2.314/imapsync.exe", options)
                .unwrap();
            zip.write_all(b"MZ").unwrap();
            zip.start_file("imapsync_2.314/FAQ.d/FAQ.txt", options)
                .unwrap();
            zip.write_all(b"faq").unwrap();
            zip.start_file("unrelated/readme.txt", options).unwrap();
            zip.write_all(b"skip").unwrap();
            zip.finish().unwrap();
        }
        let destination = directory.join("installed");
        let executable = unpack_windows_engine(&archive, &destination).unwrap();
        assert_eq!(executable, destination.join("imapsync.exe"));
        assert!(destination.join("FAQ.d/FAQ.txt").exists());
        assert!(!destination.join("unrelated").exists());

        let hostile = directory.join("hostile.zip");
        {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&hostile).unwrap());
            zip.start_file("../escape.exe", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"MZ").unwrap();
            zip.finish().unwrap();
        }
        assert!(unpack_windows_engine(&hostile, &directory.join("hostile")).is_err());
        assert!(!directory.join("escape.exe").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
