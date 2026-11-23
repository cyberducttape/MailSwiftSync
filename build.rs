//! Embeds the Windows icon and version information in `mailswiftsync.exe`,
//! so Explorer, the Start menu, and file properties show the MailSwiftSync
//! icon and version instead of a generic program.
//!
//! Only MSVC targets are handled, with the Windows SDK resource compiler
//! (`rc.exe`, on PATH in a Visual Studio developer environment or named by
//! `RC`). Without it the build continues and prints a warning, unless
//! `MAILSWIFTSYNC_REQUIRE_WINDOWS_RESOURCES=1` (set by CI and the release
//! workflow) makes a missing resource an error. Nothing runs for other
//! targets.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icons/mailswiftsync.ico");
    println!("cargo:rerun-if-env-changed=RC");
    println!("cargo:rerun-if-env-changed=MAILSWIFTSYNC_REQUIRE_WINDOWS_RESOURCES");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "msvc" {
        return;
    }
    let required = std::env::var("MAILSWIFTSYNC_REQUIRE_WINDOWS_RESOURCES").as_deref() == Ok("1");
    match compile_resources() {
        Ok(resource) => {
            // link.exe accepts a compiled .res file as an input. Only the
            // binary gets it; tests and benches do not need an icon.
            println!("cargo:rustc-link-arg-bins={}", resource.display());
        }
        Err(error) if required => panic!("Windows resources are required: {error}"),
        Err(error) => println!(
            "cargo:warning=building without the Windows icon and version resource: {error}"
        ),
    }
}

fn compile_resources() -> Result<PathBuf, String> {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").map_err(|e| e.to_string())?);
    let out = PathBuf::from(std::env::var("OUT_DIR").map_err(|e| e.to_string())?);
    let icon = manifest.join("assets/icons/mailswiftsync.ico");
    if !icon.is_file() {
        return Err(format!("{} is missing", icon.display()));
    }
    let script = out.join("mailswiftsync.rc");
    std::fs::write(&script, resource_script(&icon)).map_err(|e| e.to_string())?;
    let resource = out.join("mailswiftsync.res");
    let compiler = resource_compiler();
    let status = Command::new(&compiler)
        .arg("/nologo")
        .arg("/fo")
        .arg(&resource)
        .arg(&script)
        .status()
        .map_err(|error| format!("could not run {} ({error})", compiler.display()))?;
    if !status.success() || !resource.is_file() {
        return Err(format!("{} failed ({status})", compiler.display()));
    }
    Ok(resource)
}

/// `RC` if set; else `rc.exe` from PATH (a developer prompt); else the newest
/// Windows 10/11 SDK's x64 `rc.exe`, which a plain runner shell lacks on PATH.
fn resource_compiler() -> PathBuf {
    if let Some(explicit) = std::env::var_os("RC") {
        return PathBuf::from(explicit);
    }
    let on_path = Command::new("rc.exe")
        .arg("/?")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok();
    if on_path {
        return PathBuf::from("rc.exe");
    }
    let kits = std::env::var_os("ProgramFiles(x86)")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)"))
        .join(r"Windows Kits\10\bin");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(&kits)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path().join(r"x64\rc.exe"))
        .filter(|candidate| candidate.is_file())
        .collect();
    // Directory names are SDK versions such as 10.0.22621.0; compare them
    // numerically so 10.0.100000 would sort after 10.0.22621.
    versions.sort_by_key(|candidate| {
        candidate
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .map(|name| {
                name.split('.')
                    .map(|part| part.parse::<u64>().unwrap_or(0))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });
    versions.pop().unwrap_or_else(|| PathBuf::from("rc.exe"))
}

fn resource_script(icon: &Path) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let numeric = [
        env!("CARGO_PKG_VERSION_MAJOR"),
        env!("CARGO_PKG_VERSION_MINOR"),
        env!("CARGO_PKG_VERSION_PATCH"),
        "0",
    ]
    .join(",");
    // Resource scripts take C-style string literals; double the backslashes.
    let icon = icon.display().to_string().replace('\\', "\\\\");
    format!(
        r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "FileDescription", "MailSwiftSync"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "mailswiftsync"
      VALUE "LegalCopyright", "MIT License"
      VALUE "OriginalFilename", "mailswiftsync.exe"
      VALUE "ProductName", "MailSwiftSync"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    )
}
