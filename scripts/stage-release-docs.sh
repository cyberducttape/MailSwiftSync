#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 1 ]]; then
  echo "usage: $0 BUNDLE_DIRECTORY" >&2
  exit 2
fi

bundle_dir="$1"
mkdir -p "$bundle_dir/docs"

root_documents=(
  README.md
  LICENSE
  PROVIDER_SETUP.md
  OAUTH_SETUP.md
  CAPABILITY_MANIFEST.md
  PRODUCTION_STATUS.md
  SECURITY.md
  CHANGELOG.md
)

for document in "${root_documents[@]}"; do
  if [[ ! -f "$document" ]]; then
    echo "FAIL: release document is missing from the repository: $document" >&2
    exit 1
  fi
  cp "$document" "$bundle_dir/"
done

# The automation contract links its golden field fixtures; integrators need
# them to validate status JSON against the published contract.
for fixture in tests/fixtures/automation/*.json; do
  mkdir -p "$bundle_dir/$(dirname "$fixture")"
  cp "$fixture" "$bundle_dir/$fixture"
done

# The bundled IBM Plex fonts ship under the SIL Open Font License, which
# requires its text to accompany every copy of the binary.
cp assets/fonts/OFL.txt "$bundle_dir/OFL-IBM-Plex.txt"

linked_artifact=capabilities.toml
{
  if [[ ! -f "$linked_artifact" ]]; then
    echo "FAIL: release-linked artifact is missing from the repository: $linked_artifact" >&2
    exit 1
  fi
  mkdir -p "$bundle_dir/$(dirname "$linked_artifact")"
  cp "$linked_artifact" "$bundle_dir/$linked_artifact"
}

distribution_documents=(
  docs/architecture.md
  docs/compatibility-matrix.md
  docs/container.md
  docs/provider-facts.md
  docs/provider-qualification-packs.md
  docs/release-0.1.0-alpha.md
  docs/release-readiness.md
  docs/verification-envelope.md
  docs/accessibility-evidence.md
  docs/bulk-migrations-template.csv
  docs/distribution/INSTALL.md
  docs/distribution/SERVICE.md
  docs/distribution/CREDENTIAL-LIFECYCLE.md
  docs/distribution/mailswiftsync-supervise.service
  docs/automation-contract.md
  docs/crash-recovery-validation.md
  docs/msp-operations-runbook.md
  docs/durability-and-concurrency-validation.md
  docs/release-upgrade-validation.md
  docs/adoption-qualification-plan.md
  docs/production-qualification-environment.md
  docs/msp-rbac-and-tenant-isolation.md
  docs/psa-event-contract.md
)
for document in "${distribution_documents[@]}"; do
  if [[ ! -f "$document" ]]; then
    echo "FAIL: distribution document is missing from the repository: $document" >&2
    exit 1
  fi
  destination="$bundle_dir/$document"
  mkdir -p "$(dirname "$destination")"
  cp "$document" "$destination"
done

if [[ ! -d docs/wiki ]]; then
  echo "FAIL: distribution wiki is missing from the repository: docs/wiki" >&2
  exit 1
fi
mkdir -p "$bundle_dir/docs/wiki"
cp -R docs/wiki/. "$bundle_dir/docs/wiki/"
