#!/usr/bin/env bash
set -euo pipefail

# Generate an SPDX 2.3 package SBOM from the final Debian runtime image.
# This deliberately inventories the image's dpkg database rather than the
# Cargo graph; generate-sbom.sh covers the Rust dependency graph separately.
image="${1:?usage: generate-image-sbom.sh IMAGE [OUTPUT]}"
output="${2:-dist/mailswiftsync-image-sbom.spdx.json}"
project_root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
packages="$(mktemp "${TMPDIR:-/tmp}/mailswiftsync-image-packages.XXXXXX")"
cleanup() { rm -f -- "$packages"; }
trap cleanup EXIT

command -v docker >/dev/null 2>&1 || {
  echo "docker is required to generate an image SBOM" >&2
  exit 1
}
command -v jq >/dev/null 2>&1 || {
  echo "jq is required to generate an image SBOM" >&2
  exit 1
}

image_id="$(docker image inspect --format '{{.Id}}' "$image")"
docker run --rm --entrypoint /usr/bin/dpkg-query "$image" \
  -W -f='${Package}\t${Version}\t${Architecture}\n' > "$packages"

mkdir -p -- "$(dirname -- "$output")"
source_date_epoch="${SOURCE_DATE_EPOCH:-$(git -C "$project_root" log -1 --format=%ct HEAD)}"
if timestamp="$(date -u -d "@${source_date_epoch}" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null)"; then
  :
else
  timestamp="$(date -u -r "$source_date_epoch" +%Y-%m-%dT%H:%M:%SZ)"
fi

jq -Rn \
  --arg image "$image" \
  --arg image_id "$image_id" \
  --arg timestamp "$timestamp" \
  --rawfile package_lines "$packages" '
  ($package_lines
    | split("\n")
    | map(select(length > 0) | split("\t")
      | {
          name: .[0],
          versionInfo: .[1],
          architecture: .[2],
          SPDXID: ("SPDXRef-Package-" + ((.[0] + "-" + .[2]) | gsub("[^A-Za-z0-9.-]"; "-"))),
          downloadLocation: "NOASSERTION",
          filesAnalyzed: false,
          licenseConcluded: "NOASSERTION",
          licenseDeclared: "NOASSERTION",
          supplier: "NOASSERTION",
          externalRefs: []
        })) as $packages
  | {
      SPDXID: "SPDXRef-DOCUMENT",
      spdxVersion: "SPDX-2.3",
      creationInfo: {
        created: $timestamp,
        creators: ["Tool: MailSwiftSync generate-image-sbom.sh"]
      },
      name: ($image + " image package inventory"),
      documentNamespace: ("https://mailswiftsync.dev/spdx/image/" + $image_id),
      dataLicense: "CC0-1.0",
      comment: "Package inventory from the final runtime image dpkg database; file-level and source provenance fields are NOASSERTION.",
      packages: $packages,
      relationships: ($packages | map({spdxElementId: "SPDXRef-DOCUMENT", relationshipType: "CONTAINS", relatedSpdxElement: .SPDXID}))
    }
' > "$output"

jq -e '.spdxVersion == "SPDX-2.3" and (.packages | length > 0)' "$output" >/dev/null
chmod 0644 "$output"
echo "Generated SPDX image SBOM: $output"
