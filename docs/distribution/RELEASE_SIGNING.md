# Release signing setup

Preview tags (`vX.Y.Z-alpha.N`, `-beta.N`, `-rc.N`) publish unsigned while no
signing identity is configured. Stable tags (`vX.Y.Z`) refuse to publish
without one. This page lists what the owner has to provide, and the one-time
change that makes the one-line installers enforce it.

## 1. Repository secrets and variables

Set these under **Settings → Secrets and variables → Actions**. They are read
only by `.github/workflows/release.yml`.

| Platform | Name | Kind | What it is |
|---|---|---|---|
| Linux | `RELEASE_GPG_PRIVATE_KEY_BASE64` | secret | `gpg --armor --export-secret-keys <id> \| base64 -w0` of a signing-only key without a passphrase |
| Linux | `RELEASE_GPG_KEY_ID` | secret | The key's full fingerprint |
| Windows | `WINDOWS_SIGNING_CERT_BASE64` | secret | The Authenticode code-signing `.pfx`, base64-encoded |
| Windows | `WINDOWS_SIGNING_CERT_PASSWORD` | secret | The `.pfx` password |
| Windows | `WINDOWS_TIMESTAMP_URL` | variable | The certificate authority's RFC 3161 timestamp URL |
| macOS | `MACOS_SIGNING_CERT_BASE64` | secret | The Developer ID Application `.p12`, base64-encoded |
| macOS | `MACOS_SIGNING_CERT_PASSWORD` | secret | The `.p12` password |
| macOS | `MACOS_SIGNING_IDENTITY` | secret | `Developer ID Application: <Name> (<TEAMID>)` |
| macOS | `APPLE_ID`, `APPLE_APP_SPECIFIC_PASSWORD`, `APPLE_TEAM_ID` | secrets | For `notarytool` |

With these set, the release workflow:
- signs each Linux archive's, `.deb`'s and `.rpm`'s `.sha256` into a
  `.sha256.asc` file;
- Authenticode-signs and timestamps `mailswiftsync.exe`, then verifies it;
- Developer ID-signs the macOS binary with the hardened runtime, then
  notarizes the `.zip`.

## 2. Installer trust anchors

The installers check publisher signatures against values embedded in the
scripts themselves. Fetching a key from the same release channel would prove
nothing. The anchors ship empty, which turns the checks off. After the first
signed release, set them in one reviewed PR:

| File | Variable | Value |
|---|---|---|
| `install.sh` | `RELEASE_GPG_FINGERPRINT` | The GPG key's full fingerprint, uppercase, no spaces |
| `install.sh` | `release_public_key` | The output of `gpg --armor --export <fingerprint>`, pasted between the `KEY` markers |
| `install.sh` | `MACOS_TEAM_ID` | The 10-character Apple team ID |
| `install.ps1` | `$TrustedPublisher` | The signing certificate's exact subject, as `(Get-AuthenticodeSignature mailswiftsync.exe).SignerCertificate.Subject` prints it |

Once set:

| Situation | Stable `vX.Y.Z` | Preview |
|---|---|---|
| Valid signature from the embedded publisher | installed | installed |
| Signature present but invalid, or from another key, team or publisher | **refused** | **refused** |
| No signature | **refused** | installed, with a note that only the checksum was verified |
| `gpg` missing (Linux) | **refused** | installed, with a warning |

While the anchors are empty, a stable release installs with a warning that
the publisher signature was not checked. The SHA-256 checksum is always
required on every channel.

The macOS check runs `codesign --verify --strict` and compares the
`TeamIdentifier`. Gatekeeper's notarization assessment applies to quarantined
downloads, and a `curl` download is not quarantined, so the installer relies
on the code signature and team instead.

`tests/install-sh.sh` covers the GPG path with throwaway keys: a valid
signature, a tampered checksum, a foreign key, and missing signatures on both
channels. Run it after changing the anchors.
