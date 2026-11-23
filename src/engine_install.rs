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
//! - Fedora/RHEL (dnf) and Arch (pacman): the distribution's own Perl
//!   packages supply imapsync's modules (dnf resolves them by `perl(Module)`
//!   name), and the `imapsync` script itself is taken from the verified
//!   official 2.314 tarball into the private engines directory. The distro
//!   imapsync package is never installed, so its version cannot drift.
//! - Windows: the verified portable zip is unpacked into a private per-user
//!   engines directory; no elevation is needed.
//! - macOS: Homebrew's `imapsync` formula, which builds from the same
//!   official tarball; the version check below still decides.
//! - Elsewhere the installer explains the supported manual route instead of
//!   guessing at a Perl module installation.

use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use uuid::Uuid;

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

pub(crate) const SOURCE_TARBALL: Artifact = Artifact {
    url: "https://imapsync.lamiral.info/dist2/imapsync-2.314.tgz",
    file_name: "imapsync-2.314.tgz",
    sha256: "34e1715c6584889ff3bd9c0acc2fab254446282beda90c8e5a71a83b3a59dbca",
    max_bytes: 64 * 1024 * 1024,
};

/// The engine script inside the official tarball.
const TARBALL_SCRIPT: &str = "imapsync-2.314/imapsync";
const MAX_SCRIPT_BYTES: u64 = 8 * 1024 * 1024;

/// Perl modules the 2.314 script loads, by the `perl(Module)` names Fedora
/// and RHEL packages provide. `procps-ng` supplies the `ps` it calls.
const DNF_REQUIREMENTS: &[&str] = &[
    "perl(Authen::NTLM)",
    "perl(Compress::Zlib)",
    "perl(Data::Dumper)",
    "perl(Data::Uniqid)",
    "perl(Digest::HMAC_MD5)",
    "perl(Digest::HMAC_SHA1)",
    "perl(Digest::MD5)",
    "perl(Digest::SHA)",
    "perl(Encode)",
    "perl(Encode::IMAPUTF7)",
    "perl(English)",
    "perl(File::Copy::Recursive)",
    "perl(File::Glob)",
    "perl(File::Path)",
    "perl(File::Spec)",
    "perl(File::Tail)",
    "perl(Getopt::Long)",
    "perl(IO::Socket::SSL)",
    "perl(IO::Tee)",
    "perl(IPC::Open3)",
    "perl(JSON::WebToken)",
    "perl(Mail::IMAPClient)",
    "perl(MIME::Base64)",
    "perl(Pod::Usage)",
    "perl(Proc::ProcessTable)",
    "perl(Readonly)",
    "perl(Regexp::Common)",
    "perl(Sys::Hostname)",
    "perl(Sys::MemInfo)",
    "perl(Term::ReadKey)",
    "perl(Test::More)",
    "perl(Text::ParseWords)",
    "perl(Time::HiRes)",
    "perl(Time::Local)",
    "perl(Unicode::String)",
    "perl(URI::Escape)",
    "procps-ng",
];

/// The Arch `imapsync` package's runtime dependencies (extra, 2.314).
const PACMAN_REQUIREMENTS: &[&str] = &[
    "perl",
    "perl-cgi",
    "perl-crypt-openssl-rsa",
    "perl-data-uniqid",
    "perl-date-manip",
    "perl-digest-sha1",
    "perl-email-address",
    "perl-encode-imaputf7",
    "perl-file-copy-recursive",
    "perl-file-tail",
    "perl-html-parser",
    "perl-http-daemon-ssl",
    "perl-io-socket-inet6",
    "perl-io-socket-ssl",
    "perl-io-tee",
    "perl-json-webtoken",
    "perl-libwww",
    "perl-mail-imapclient",
    "perl-module-runtime",
    "perl-module-scandeps",
    "perl-net-dns",
    "perl-net-server",
    "perl-ntlm",
    "perl-package-stash",
    "perl-proc-processtable",
    "perl-readonly",
    "perl-regexp-common",
    "perl-sys-meminfo",
    "perl-term-readkey",
    "perl-unicode-string",
    "procps-ng",
];

/// Directory inside the Windows zip that holds the engine.
const WINDOWS_ARCHIVE_ROOT: &str = "imapsync_2.314/";
const WINDOWS_EXECUTABLE: &str = "imapsync.exe";
const DEBIAN_EXECUTABLE: &str = "/usr/bin/imapsync";
const MAX_ENGINE_ARCHIVE_FILES: usize = 4_096;
const MAX_ENGINE_UNPACKED_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstallMethod {
    DebianPackage,
    WindowsPortable,
    /// Fedora/RHEL Perl packages plus the verified tarball script.
    DnfPerlScript,
    /// Arch Perl packages plus the verified tarball script.
    PacmanPerlScript,
    /// Homebrew's imapsync formula on macOS.
    Homebrew,
}

impl InstallMethod {
    /// Whether the system package manager needs root (sudo or pkexec).
    pub(crate) fn needs_elevation(self) -> bool {
        matches!(
            self,
            Self::DebianPackage | Self::DnfPerlScript | Self::PacmanPerlScript
        )
    }

    /// Whether the engine lands outside the standard `imapsync` on PATH, so
    /// the saved profile must name its path.
    pub(crate) fn saves_engine_path(self) -> bool {
        self != Self::DebianPackage
    }

    /// What the installer will do, for the confirmation prompt.
    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::DebianPackage => "install it with apt-get (sudo will ask for your password)",
            Self::WindowsPortable => "unpack it into MailSwiftSync's private engines directory",
            Self::DnfPerlScript => {
                "install its Perl modules with dnf (sudo will ask for your password) and place the verified script in MailSwiftSync's private engines directory"
            }
            Self::PacmanPerlScript => {
                "install its Perl modules with pacman (sudo will ask for your password) and place the verified script in MailSwiftSync's private engines directory"
            }
            Self::Homebrew => {
                "install Homebrew's imapsync formula, which is built from that same release (no password needed)"
            }
        }
    }

    /// The artifact MailSwiftSync downloads and verifies itself, if any.
    pub(crate) fn artifact(self) -> Option<&'static Artifact> {
        match self {
            Self::DebianPackage => Some(&DEBIAN_PACKAGE),
            Self::WindowsPortable => Some(&WINDOWS_PORTABLE),
            Self::DnfPerlScript | Self::PacmanPerlScript => Some(&SOURCE_TARBALL),
            Self::Homebrew => None,
        }
    }
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

const MACOS_GUIDANCE: &str = "Automatic installation on macOS uses Homebrew, which is not installed. Install Homebrew from https://brew.sh and run `mailswiftsync install-engine` again, or install imapsync 2.314 from the official tarball (https://imapsync.lamiral.info/dist2/imapsync-2.314.tgz) following its INSTALL.d/INSTALL.Darwin.txt, then enter the imapsync path in MailSwiftSync.";
const OTHER_GUIDANCE: &str = "Automatic installation supports Debian/Ubuntu (apt), Fedora/RHEL (dnf), Arch (pacman), macOS (Homebrew), and Windows. Install imapsync 2.314 from the official distribution (https://imapsync.lamiral.info/dist2/) using your platform's INSTALL.d instructions, or run the pinned MailSwiftSync container image, then enter the imapsync path in MailSwiftSync.";

const APT_GET: &str = "/usr/bin/apt-get";
const DNF: &str = "/usr/bin/dnf";
const PACMAN: &str = "/usr/bin/pacman";
/// Homebrew's default prefixes on Apple silicon and Intel Macs.
const BREW_CANDIDATES: [&str; 2] = ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"];

/// What the installer can see about the host's package managers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct HostFacts {
    debian_release_file: bool,
    apt: bool,
    dnf: bool,
    pacman: bool,
    brew: bool,
}

pub(crate) fn plan_for_host() -> InstallPlan {
    plan_for(
        std::env::consts::OS,
        HostFacts {
            debian_release_file: Path::new("/etc/debian_version").exists(),
            apt: Path::new(APT_GET).exists(),
            dnf: Path::new(DNF).exists(),
            pacman: Path::new(PACMAN).exists(),
            brew: homebrew().is_some(),
        },
    )
}

fn homebrew() -> Option<PathBuf> {
    BREW_CANDIDATES
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

/// Whether the desktop can request elevation for the package manager.
pub(crate) fn desktop_elevation_available() -> bool {
    Path::new("/usr/bin/pkexec").exists()
}

fn plan_for(os: &str, host: HostFacts) -> InstallPlan {
    match os {
        "windows" => InstallPlan::Automatic(InstallMethod::WindowsPortable),
        "linux" if host.debian_release_file && host.apt => {
            InstallPlan::Automatic(InstallMethod::DebianPackage)
        }
        "linux" if host.dnf => InstallPlan::Automatic(InstallMethod::DnfPerlScript),
        "linux" if host.pacman => InstallPlan::Automatic(InstallMethod::PacmanPerlScript),
        "macos" if host.brew => InstallPlan::Automatic(InstallMethod::Homebrew),
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
    // A UUID prevents concurrent installer processes from sharing/truncating
    // one predictable partial file. The verified artifact is still published
    // only by the final atomic rename below.
    let partial_path = directory.join(format!(
        ".{}.{}.partial",
        artifact.file_name,
        Uuid::new_v4()
    ));
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
    cleanup_partial_download(result, &partial_path, |path| std::fs::remove_file(path))?;
    progress(InstallProgress::Verified);
    Ok(final_path)
}

fn cleanup_partial_download(
    result: Result<(), String>,
    partial_path: &Path,
    remove: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<(), String> {
    let Err(primary_error) = result else {
        return result;
    };
    match remove(partial_path) {
        Ok(()) => Err(primary_error),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(primary_error),
        Err(cleanup_error) => Err(format!(
            "{primary_error}; could not remove unverified partial engine artifact {}: {cleanup_error}",
            partial_path.display()
        )),
    }
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
    unpack_windows_engine_with_limits(
        archive,
        destination,
        MAX_ENGINE_ARCHIVE_FILES,
        MAX_ENGINE_UNPACKED_BYTES,
    )
}

fn unpack_windows_engine_with_limits(
    archive: &Path,
    destination: &Path,
    max_files: usize,
    max_unpacked_bytes: u64,
) -> Result<PathBuf, String> {
    let file = std::fs::File::open(archive)
        .map_err(|error| format!("could not open {}: {error}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|error| format!("the engine archive is not a valid zip: {error}"))?;
    if zip.len() > max_files {
        return Err(format!(
            "the engine archive contains {} entries; the limit is {max_files}",
            zip.len()
        ));
    }

    // Inspect every entry before creating anything in the destination. This
    // keeps a rejected archive from leaving a partially extracted engine and
    // bounds both metadata amplification and decompression output.
    let mut unpacked_bytes = 0_u64;
    for index in 0..zip.len() {
        let entry = zip
            .by_index(index)
            .map_err(|error| format!("could not inspect the engine archive: {error}"))?;
        let entry_name = entry
            .name()
            .map_err(|error| format!("could not read an engine archive entry name: {error}"))?;
        let Some(relative) = entry.enclosed_name() else {
            return Err(format!("unsafe path in engine archive: {entry_name}"));
        };
        let Ok(inside) = relative.strip_prefix(WINDOWS_ARCHIVE_ROOT.trim_end_matches('/')) else {
            continue;
        };
        if inside.as_os_str().is_empty() || entry.is_dir() {
            continue;
        }
        unpacked_bytes = unpacked_bytes
            .checked_add(entry.size())
            .ok_or_else(|| "the engine archive unpacked-size budget overflowed".to_owned())?;
        if unpacked_bytes > max_unpacked_bytes {
            return Err(format!(
                "the engine archive expands to {unpacked_bytes} bytes; the limit is {max_unpacked_bytes} bytes"
            ));
        }
    }

    let mut executable = None;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|error| format!("could not read the engine archive: {error}"))?;
        let entry_name = entry
            .name()
            .map_err(|error| format!("could not read an engine archive entry name: {error}"))?;
        let Some(relative) = entry.enclosed_name() else {
            return Err(format!("unsafe path in engine archive: {entry_name}"));
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

/// The package-manager invocation (program first, absolute path) for a
/// method that installs system packages. `package` is the verified `.deb`.
fn package_manager_argv(method: InstallMethod, package: &Path) -> Vec<std::ffi::OsString> {
    let mut argv: Vec<std::ffi::OsString> = match method {
        InstallMethod::DebianPackage => vec![
            APT_GET.into(),
            "install".into(),
            "--no-install-recommends".into(),
            "--yes".into(),
            package.into(),
        ],
        InstallMethod::DnfPerlScript => vec![
            DNF.into(),
            "install".into(),
            "--assumeyes".into(),
            "--setopt=install_weak_deps=False".into(),
        ],
        InstallMethod::PacmanPerlScript => {
            vec![
                PACMAN.into(),
                "-S".into(),
                "--needed".into(),
                "--noconfirm".into(),
            ]
        }
        InstallMethod::WindowsPortable | InstallMethod::Homebrew => Vec::new(),
    };
    match method {
        InstallMethod::DnfPerlScript => argv.extend(DNF_REQUIREMENTS.iter().map(Into::into)),
        InstallMethod::PacmanPerlScript => argv.extend(PACMAN_REQUIREMENTS.iter().map(Into::into)),
        _ => {}
    }
    argv
}

fn package_install_command(elevation: Elevation, argv: &[std::ffi::OsString]) -> Command {
    let mut command = Command::new(match elevation {
        Elevation::Sudo => "sudo",
        Elevation::Pkexec => "pkexec",
    });
    command.args(argv);
    command
}

fn shell_quote(word: &str) -> String {
    if !word.is_empty()
        && word
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_./=:+@%".contains(&byte))
    {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// The exact privileged command, for operators who prefer to run it.
fn manual_command(argv: &[std::ffi::OsString]) -> String {
    let mut words = vec!["sudo".to_owned()];
    for (index, word) in argv.iter().enumerate() {
        let word = word.to_string_lossy();
        let word = if index == 0 {
            Path::new(word.as_ref())
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| word.into_owned())
        } else {
            word.into_owned()
        };
        words.push(shell_quote(&word));
    }
    words.join(" ")
}

/// The Debian route's privileged command for the verified package.
#[cfg(all(test, unix))]
pub(crate) fn manual_package_command(package: &Path) -> String {
    manual_command(&package_manager_argv(InstallMethod::DebianPackage, package))
}

/// Run the elevated package manager, explaining how to retry by hand.
fn run_package_manager(
    method: InstallMethod,
    elevation: Elevation,
    package: &Path,
) -> Result<(), String> {
    let argv = package_manager_argv(method, package);
    let manual = manual_command(&argv);
    let status = package_install_command(elevation, &argv)
        .status()
        .map_err(|error| {
            format!("could not start the package manager ({error}); run it yourself: {manual}")
        })?;
    if status.success() {
        return Ok(());
    }
    let hint = match method {
        InstallMethod::DnfPerlScript => {
            " On RHEL, CentOS Stream, Rocky or AlmaLinux, enable EPEL and CRB first."
        }
        InstallMethod::PacmanPerlScript => {
            " If packages were not found, update the system with `sudo pacman -Syu` first."
        }
        _ => " Review apt/dpkg output before retrying.",
    };
    Err(format!(
        "package installation did not complete successfully ({status}). The package manager may have changed system state.{hint} To retry manually run: {manual}"
    ))
}

/// Copy the engine script out of the verified official tarball into
/// `destination/imapsync`, executable by the owner. Only the one regular
/// file at [`TARBALL_SCRIPT`] is read; every other entry is skipped.
pub(crate) fn extract_tarball_script(
    archive: &Path,
    destination: &Path,
) -> Result<PathBuf, String> {
    let file = std::fs::File::open(archive)
        .map_err(|error| format!("could not open {}: {error}", archive.display()))?;
    let mut tar = flate2::read::GzDecoder::new(std::io::BufReader::new(file));
    let script = read_tar_member(&mut tar, TARBALL_SCRIPT, MAX_SCRIPT_BYTES)?
        .ok_or_else(|| format!("{TARBALL_SCRIPT} is missing from the engine tarball"))?;
    crate::credentials::ensure_private_directory(destination)
        .map_err(|error| format!("could not prepare {}: {error}", destination.display()))?;
    let executable = destination.join("imapsync");
    let partial = destination.join(format!(".imapsync.{}.partial", Uuid::new_v4()));
    let written = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o700);
        let mut out = options.open(&partial)?;
        out.write_all(&script)?;
        out.sync_all()?;
        std::fs::rename(&partial, &executable)
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&partial);
        return Err(format!("could not write {}: {error}", executable.display()));
    }
    Ok(executable)
}

/// Read one regular file from a ustar/GNU tar stream by exact name.
fn read_tar_member(
    tar: &mut impl Read,
    wanted: &str,
    max_bytes: u64,
) -> Result<Option<Vec<u8>>, String> {
    let corrupt = |what: &str| format!("the engine tarball is corrupt ({what})");
    let mut header = [0_u8; 512];
    loop {
        if let Err(error) = tar.read_exact(&mut header) {
            return if error.kind() == std::io::ErrorKind::UnexpectedEof {
                Ok(None)
            } else {
                Err(corrupt(&error.to_string()))
            };
        }
        if header.iter().all(|byte| *byte == 0) {
            return Ok(None);
        }
        let field = |range: std::ops::Range<usize>| {
            let bytes = &header[range];
            let end = bytes
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(bytes.len());
            String::from_utf8_lossy(&bytes[..end]).into_owned()
        };
        let size_text = field(124..136);
        let size = u64::from_str_radix(size_text.trim(), 8).map_err(|_| corrupt("entry size"))?;
        let prefix = if &header[257..262] == b"ustar" {
            field(345..500)
        } else {
            String::new()
        };
        let name = match (prefix.is_empty(), field(0..100)) {
            (true, name) => name,
            (false, name) => format!("{prefix}/{name}"),
        };
        let regular = matches!(header[156], b'0' | 0);
        let padded = size.div_ceil(512) * 512;
        if regular && name == wanted {
            if size > max_bytes {
                return Err(format!("{wanted} is larger than expected ({size} bytes)"));
            }
            let mut contents = vec![0_u8; size as usize];
            tar.read_exact(&mut contents)
                .map_err(|_| corrupt("truncated entry"))?;
            return Ok(Some(contents));
        }
        let skipped = std::io::copy(&mut tar.by_ref().take(padded), &mut std::io::sink())
            .map_err(|error| corrupt(&error.to_string()))?;
        if skipped != padded {
            return Err(corrupt("truncated entry"));
        }
    }
}

/// The first run after installation can be the slowest the engine ever has:
/// the packaged Windows build unpacks its bundled Perl and is virus-scanned.
const FIRST_RUN_VERSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

fn confirm_qualified(executable: &Path) -> Result<InstalledEngine, String> {
    let version = crate::runner::probe_engine_version_within(
        &executable.to_string_lossy(),
        FIRST_RUN_VERSION_TIMEOUT,
    )
    .map_err(|reason| {
        format!(
            "{} did not report its version ({reason}); it should be the qualified imapsync {QUALIFIED_IMAPSYNC_VERSION}",
            executable.display()
        )
    })?;
    if crate::verification::imapsync_output_profile(Some(&version))
        != crate::verification::ImapsyncOutputProfile::Packaged2314
    {
        return Err(format!(
            "{} reports \"{version}\", not the qualified imapsync {QUALIFIED_IMAPSYNC_VERSION}",
            executable.display()
        ));
    }
    Ok(InstalledEngine {
        executable: executable.to_path_buf(),
        version,
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
            run_package_manager(method, elevation, &package)?;
            progress(InstallProgress::Checking);
            confirm_qualified(Path::new(DEBIAN_EXECUTABLE))
        }
        InstallMethod::DnfPerlScript | InstallMethod::PacmanPerlScript => {
            let archive = fetch_verified(&SOURCE_TARBALL, &directory, &mut progress)?;
            progress(InstallProgress::Installing);
            run_package_manager(method, elevation, &archive)?;
            let destination = directory.join(format!("imapsync-{QUALIFIED_IMAPSYNC_VERSION}"));
            let executable = extract_tarball_script(&archive, &destination)?;
            progress(InstallProgress::Checking);
            confirm_qualified(&executable)
        }
        InstallMethod::Homebrew => {
            let brew = homebrew().ok_or("Homebrew is no longer installed")?;
            progress(InstallProgress::Installing);
            let status = Command::new(&brew)
                .args(["install", "imapsync"])
                .status()
                .map_err(|error| {
                    format!(
                        "could not start Homebrew ({error}); run it yourself: brew install imapsync"
                    )
                })?;
            if !status.success() {
                return Err(format!(
                    "`brew install imapsync` did not complete successfully ({status}); review Homebrew's output and retry"
                ));
            }
            progress(InstallProgress::Checking);
            let executable = brew.with_file_name("imapsync");
            confirm_qualified(&executable).map_err(|error| {
                format!("{error}. Homebrew may ship a different imapsync release; run `brew upgrade imapsync` or install 2.314 from the official tarball")
            })
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
        Artifact, DEBIAN_PACKAGE, HostFacts, InstallMethod, InstallPlan, InstallProgress, plan_for,
        sha256_hex, store_verified, unpack_windows_engine,
    };
    use std::io::Write;

    #[test]
    fn failed_download_reports_partial_artifact_cleanup_failure() {
        let partial_path = std::path::Path::new("unverified-engine.partial");
        let error = super::cleanup_partial_download(
            Err("download failed digest verification".to_owned()),
            partial_path,
            |_| Err(std::io::Error::other("injected cleanup failure")),
        )
        .unwrap_err();

        assert!(error.contains("download failed digest verification"));
        assert!(error.contains("could not remove unverified partial engine artifact"));
        assert!(error.contains("unverified-engine.partial"));
        assert!(error.contains("injected cleanup failure"));
    }

    fn private_directory(name: &str) -> std::path::PathBuf {
        let directory = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!("mailswiftsync-{name}-{}", uuid::Uuid::new_v4()));
        crate::credentials::ensure_private_directory(&directory).unwrap();
        directory
    }

    #[test]
    fn each_supported_package_manager_gets_an_automatic_route() {
        let host = |configure: fn(&mut HostFacts)| {
            let mut facts = HostFacts::default();
            configure(&mut facts);
            facts
        };
        let debian = host(|facts| {
            facts.debian_release_file = true;
            facts.apt = true;
        });
        assert_eq!(
            plan_for("linux", debian),
            InstallPlan::Automatic(InstallMethod::DebianPackage)
        );
        // apt without Debian's release file, or the file without apt, is not
        // Debian; with nothing else available the route is manual.
        assert!(matches!(
            plan_for("linux", host(|facts| facts.apt = true)),
            InstallPlan::Manual(_)
        ));
        assert!(matches!(
            plan_for("linux", host(|facts| facts.debian_release_file = true)),
            InstallPlan::Manual(_)
        ));
        assert_eq!(
            plan_for("linux", host(|facts| facts.dnf = true)),
            InstallPlan::Automatic(InstallMethod::DnfPerlScript)
        );
        assert_eq!(
            plan_for("linux", host(|facts| facts.pacman = true)),
            InstallPlan::Automatic(InstallMethod::PacmanPerlScript)
        );
        assert_eq!(
            plan_for("macos", host(|facts| facts.brew = true)),
            InstallPlan::Automatic(InstallMethod::Homebrew)
        );
        assert!(
            matches!(plan_for("macos", HostFacts::default()), InstallPlan::Manual(text) if text.contains("2.314") && text.contains("brew.sh"))
        );
        assert_eq!(
            plan_for("windows", HostFacts::default()),
            InstallPlan::Automatic(InstallMethod::WindowsPortable)
        );
        assert!(matches!(
            plan_for("freebsd", host(|facts| facts.brew = true)),
            InstallPlan::Manual(_)
        ));
        for method in [
            InstallMethod::DebianPackage,
            InstallMethod::DnfPerlScript,
            InstallMethod::PacmanPerlScript,
        ] {
            assert!(method.needs_elevation());
        }
        assert!(!InstallMethod::Homebrew.needs_elevation());
        assert!(!InstallMethod::WindowsPortable.needs_elevation());
        assert!(!InstallMethod::DebianPackage.saves_engine_path());
        assert!(InstallMethod::DnfPerlScript.saves_engine_path());
        assert!(InstallMethod::Homebrew.artifact().is_none());
    }

    #[test]
    fn script_routes_install_perl_modules_but_never_the_distro_imapsync() {
        use super::{manual_command, package_manager_argv};
        let tarball = std::path::Path::new("/unused/imapsync-2.314.tgz");
        let dnf = manual_command(&package_manager_argv(InstallMethod::DnfPerlScript, tarball));
        assert!(dnf.starts_with(
            "sudo dnf install --assumeyes --setopt=install_weak_deps=False 'perl(Authen::NTLM)'"
        ));
        assert!(dnf.contains("'perl(Mail::IMAPClient)'") && dnf.ends_with(" procps-ng"));
        let pacman = manual_command(&package_manager_argv(
            InstallMethod::PacmanPerlScript,
            tarball,
        ));
        assert!(pacman.starts_with("sudo pacman -S --needed --noconfirm perl perl-cgi "));
        assert!(pacman.contains(" perl-mail-imapclient "));
        for command in [dnf, pacman] {
            assert!(!command.contains("tgz"), "{command}");
            assert!(
                !command.split(' ').any(|word| word == "imapsync"),
                "{command}"
            );
        }
    }

    fn tar_entry(name: &str, kind: u8, contents: &[u8]) -> Vec<u8> {
        let mut header = [0_u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..107].copy_from_slice(b"0000644");
        header[124..135].copy_from_slice(format!("{:011o}", contents.len()).as_bytes());
        header[156] = kind;
        header[257..263].copy_from_slice(b"ustar\0");
        let mut entry = header.to_vec();
        entry.extend_from_slice(contents);
        entry.resize(entry.len().div_ceil(512) * 512, 0);
        entry
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn tarball_extraction_takes_only_the_engine_script() {
        let directory = private_directory("engine-tarball");
        let mut tar = Vec::new();
        tar.extend(tar_entry("imapsync-2.314/README", b'0', b"readme"));
        // A symlink or directory with the script's name is never used.
        tar.extend(tar_entry("imapsync-2.314/imapsync", b'2', b""));
        tar.extend(tar_entry(
            "imapsync-2.314/imapsync",
            b'0',
            b"#!/usr/bin/env perl\nprint \"2.314\\n\";\n",
        ));
        tar.extend(tar_entry("imapsync-2.314/X/imapsync", b'0', b"other"));
        tar.extend([0_u8; 1024]);
        let archive = directory.join("imapsync-2.314.tgz");
        std::fs::write(&archive, gzip(&tar)).unwrap();

        let destination = directory.join("imapsync-2.314");
        let executable = super::extract_tarball_script(&archive, &destination).unwrap();
        assert_eq!(executable, destination.join("imapsync"));
        assert_eq!(
            std::fs::read(&executable).unwrap(),
            b"#!/usr/bin/env perl\nprint \"2.314\\n\";\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&executable).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 1);

        // A tarball without the script, or a truncated one, is refused.
        let missing = directory.join("missing.tgz");
        std::fs::write(
            &missing,
            gzip(&tar_entry("imapsync-2.314/README", b'0', b"x")),
        )
        .unwrap();
        assert!(
            super::extract_tarball_script(&missing, &directory.join("m"))
                .unwrap_err()
                .contains("missing")
        );
        let truncated = directory.join("truncated.tgz");
        let entry = tar_entry("imapsync-2.314/imapsync", b'0', &[b'x'; 900]);
        std::fs::write(&truncated, gzip(&entry[..700])).unwrap();
        assert!(
            super::extract_tarball_script(&truncated, &directory.join("t"))
                .unwrap_err()
                .contains("corrupt")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// Opt-in network check that the pinned official tarball still contains
    /// the 2.314 script: `MAILSWIFTSYNC_TEST_ENGINE_DOWNLOAD=1 cargo test --
    /// --ignored real_tarball`.
    #[test]
    #[ignore]
    fn real_tarball_matches_the_pin_and_holds_the_script() {
        if std::env::var_os("MAILSWIFTSYNC_TEST_ENGINE_DOWNLOAD").is_none() {
            return;
        }
        let directory = private_directory("engine-real-tarball");
        let archive = super::fetch_verified(&super::SOURCE_TARBALL, &directory, |_| {}).unwrap();
        let script =
            super::extract_tarball_script(&archive, &directory.join("imapsync-2.314")).unwrap();
        let text = std::fs::read_to_string(script).unwrap();
        assert!(text.starts_with("#!/usr/bin/env perl"));
        assert!(text.contains("2.314"));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn manual_package_command_shell_quotes_package_path() {
        use super::manual_package_command;
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
        assert!(
            !std::fs::read_dir(&directory)
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".tampered.bin.")
                    && entry.file_name().to_string_lossy().ends_with(".partial"))
        );

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
            // Deflated like the published imapsync zip; Stored would hide a
            // build without deflate support.
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
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

        assert!(
            super::unpack_windows_engine_with_limits(
                &archive,
                &directory.join("too-many"),
                1,
                super::MAX_ENGINE_UNPACKED_BYTES,
            )
            .unwrap_err()
            .contains("entries")
        );
        assert!(
            super::unpack_windows_engine_with_limits(
                &archive,
                &directory.join("too-large"),
                super::MAX_ENGINE_ARCHIVE_FILES,
                4,
            )
            .unwrap_err()
            .contains("expands")
        );

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
