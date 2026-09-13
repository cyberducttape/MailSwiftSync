#!/usr/bin/env bash
set -euo pipefail

# Generate a CycloneDX 1.5 SBOM from the exact Cargo.lock dependency graph.
# The output contains package metadata only; it never reads profiles, keyring
# values, mailbox data, or the SQLite ledger.
output="${1:-dist/mailswiftsync-sbom.json}"
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
serial="$(uuidgen | tr '[:upper:]' '[:lower:]')"
timestamp="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

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
