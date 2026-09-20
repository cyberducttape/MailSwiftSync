#!/bin/bash
# Evidence-based provider compatibility gate
# Only allows release when each tested provider has structured evidence

set -euo pipefail

EVIDENCE_DIR="tests/provider-evidence"
MATRIX_FILE="docs/compatibility-matrix.md"
SCHEMA_FILE="$EVIDENCE_DIR/schema.json"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

if [[ "${1:-}" == "--release" ]]; then
    MODE="release"
else
    MODE="preview"
fi

echo "Provider Compatibility Evidence Gate (Mode: $MODE)"
echo "=================================================="

# Check if schema exists
if [[ ! -f "$SCHEMA_FILE" ]]; then
    echo -e "${RED}✗ FAIL: Schema not found at $SCHEMA_FILE${NC}"
    exit 1
fi

# Parse matrix for provider rows (skip header and fixture row)
# Look for "Generic IMAP" rows that claim testing has occurred
declare -a TESTED_PROVIDERS
declare -a MISSING_EVIDENCE

mapfile -t MATRIX_ROWS < <(grep "^| Generic IMAP" "$MATRIX_FILE")

if [[ ${#MATRIX_ROWS[@]} -eq 0 ]]; then
    echo -e "${YELLOW}ℹ No hosted providers in matrix${NC}"
    exit 0
fi

for row in "${MATRIX_ROWS[@]}"; do
    # Extract provider name (second column)
    provider=$(echo "$row" | cut -d'|' -f3 | xargs)

    # Normalize provider name for evidence filename
    provider_id=$(echo "$provider" | tr '()' '_' | tr '/' '-' | tr ' ' '_' | tr '[:upper:]' '[:lower:]')

    # Check if row claims any testing (not "Code path verified" only)
    if echo "$row" | grep -q "Code path verified"; then
        # Check if there's actual evidence
        dry_pilot=$(echo "$row" | cut -d'|' -f8 | xargs)
        live_pilot=$(echo "$row" | cut -d'|' -f9 | xargs)
        recovery=$(echo "$row" | cut -d'|' -f10 | xargs)

        # If any column suggests testing beyond code path, require evidence
        if [[ "$dry_pilot" == *"integration test"* ]] || \
           [[ "$live_pilot" == *"integration test"* ]] || \
           [[ "$recovery" == *"test"* ]]; then

            # Look for evidence files
            evidence_files=$(find "$EVIDENCE_DIR" -name "*${provider_id}*.json" 2>/dev/null || true)

            if [[ -z "$evidence_files" ]]; then
                MISSING_EVIDENCE+=("$provider")
            else
                # Validate evidence files against schema
                for evidence_file in $evidence_files; do
                    if ! jsonschema -i "$evidence_file" "$SCHEMA_FILE" 2>/dev/null; then
                        echo -e "${RED}✗ FAIL: $evidence_file does not match schema${NC}"
                        exit 1
                    fi
                done
                echo -e "${GREEN}✓ $provider: Evidence found and validated${NC}"
            fi
        fi
    fi
done

# Report missing evidence
if [[ ${#MISSING_EVIDENCE[@]} -gt 0 ]]; then
    echo ""
    echo -e "${RED}Missing Evidence for Tested Providers:${NC}"
    for provider in "${MISSING_EVIDENCE[@]}"; do
        echo "  - $provider"
    done

    if [[ "$MODE" == "release" ]]; then
        echo ""
        echo -e "${RED}✗ FAIL: Release blocked${NC}"
        echo "Evidence required for live-tested providers before release."
        echo ""
        echo "To add evidence, create a JSON file in $EVIDENCE_DIR following tests/provider-evidence/schema.json"
        exit 1
    else
        echo ""
        echo -e "${YELLOW}⚠ Preview mode: Missing evidence not blocking${NC}"
    fi
else
    echo ""
    if [[ "$MODE" == "release" ]]; then
        echo -e "${GREEN}✓ All tested providers have evidence${NC}"
        echo -e "${GREEN}✓ Release gate PASSED${NC}"
    else
        echo -e "${GREEN}✓ Preview gate PASSED${NC}"
    fi
fi
