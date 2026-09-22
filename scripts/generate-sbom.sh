#!/usr/bin/env bash
set -euo pipefail

# Generate a CycloneDX 1.5 SBOM from the exact Cargo.lock dependency graph.
# The output contains package metadata only; it never reads profiles, keyring
# values, mailbox data, or the SQLite ledger.
output="${1:-dist/mailswiftsync-rust-sbom.cdx.json}"
project_root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
metadata="$(mktemp "${TMPDIR:-/tmp}/mailswiftsync-cargo-metadata.XXXXXX.json")"
cleanup() { rm -f -- "$metadata"; }
trap cleanup EXIT

command -v cargo >/dev/null 2>&1 || {
  echo "cargo is required to generate the SBOM" >&2
  exit 1
}
command -v jq >/dev/null 2>&1 || {
  echo "jq is required to generate the SBOM" >&2
  exit 1
}

mkdir -p -- "$(dirname -- "$output")"
(cd "$project_root" && cargo metadata --locked --format-version 1 > "$metadata")
if command -v sha256sum >/dev/null 2>&1; then
  lock_digest="$(sha256sum "$project_root/Cargo.lock" | awk '{print $1}')"
else
  lock_digest="$(shasum -a 256 "$project_root/Cargo.lock" | awk '{print $1}')"
fi
# CycloneDX requires a UUID-shaped serial number. Derive it from the locked
# dependency graph instead of uuidgen so identical source inputs produce the
# same SBOM. SOURCE_DATE_EPOCH is the release timestamp contract; fall back to
# the repository's latest commit for local invocations.
serial="${lock_digest:0:8}-${lock_digest:8:4}-${lock_digest:12:4}-${lock_digest:16:4}-${lock_digest:20:12}"
source_date_epoch="${SOURCE_DATE_EPOCH:-$(git -C "$project_root" log -1 --format=%ct HEAD)}"
if timestamp="$(date -u -d "@${source_date_epoch}" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null)"; then
  :
else
  timestamp="$(date -u -r "$source_date_epoch" +%Y-%m-%dT%H:%M:%SZ)"
fi

jq --arg serial "$serial" --arg timestamp "$timestamp" '
  . as $metadata
  | ($metadata.packages | map(select(.name == "mailswiftsync")) | .[0]) as $root
  | ($metadata.packages
      | map({key: .id, value: ("pkg:cargo/" + .name + "@" + .version)})
      | from_entries) as $purl
  | ($metadata.packages
      | map(select(.name != "mailswiftsync")
          | {
              type: "library",
              "bom-ref": $purl[.id],
              name: .name,
              version: .version,
              scope: "required",
              licenses: (
                if .license then
                  if (.license | test("[[:space:]](OR|AND)[[:space:]]"))
                  then [{expression: .license}]
                  else [{license: {id: .license}}]
                  end
                else []
                end
              )
            })) as $components
  | {
      bomFormat: "CycloneDX",
      specVersion: "1.5",
      serialNumber: ("urn:uuid:" + $serial),
      version: 1,
      metadata: {
        timestamp: $timestamp,
        tools: [{vendor: "MailSwiftSync", name: "generate-sbom.sh"}],
        component: {
          type: "application",
          "bom-ref": $purl[$root.id],
          name: "mailswiftsync",
          version: $root.version
        }
      },
      components: $components,
      dependencies: (
        ($metadata.resolve.nodes // [])
        | map({
            ref: $purl[.id],
            dependsOn: ([.deps[]?.pkg | $purl[.]] | map(select(. != null)))
          })
      )
    }
' "$metadata" > "$output"

jq -e '.bomFormat == "CycloneDX" and .specVersion == "1.5" and (.components | length > 0)' "$output" >/dev/null
chmod 0644 "$output"
echo "Generated CycloneDX SBOM: $output"
