#!/usr/bin/env bash
set -euo pipefail

# Reproducible product-level lab for the selected migration engine. It
# exercises the packaged MailSwiftSync binary, its generated plan and secret
# delivery, real Dovecot servers, durable evidence, and the customer-proof
# verifier. The default remains the packaged imapsync path; CI invokes the
# native Dovecot path separately.

test_engine="${MAILSWIFTSYNC_TEST_ENGINE:-ImapSync}"
if [[ "$test_engine" != "ImapSync" && "$test_engine" != "Dovecot" ]]; then
  echo "FAIL: MAILSWIFTSYNC_TEST_ENGINE must be ImapSync or Dovecot" >&2
  exit 1
fi

if [[ -n "${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-}" ]]; then
  mkdir -p -- "$MAILSWIFTSYNC_EVIDENCE_OUTPUT"
fi

required_tools=(dovecot doveadm doveconf imapsync mailswiftsync openssl ps timeout)
missing_tools=()
for tool in "${required_tools[@]}"; do
  command -v "$tool" >/dev/null 2>&1 || missing_tools+=("$tool")
done
if ((${#missing_tools[@]} > 0)); then
  if [[ -n "${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-}" ]]; then
    {
      printf 'engine=%s\nuid=%s\nPATH=%s\nmissing_tools=%s\n' \
        "$test_engine" "$(id -u)" "$PATH" "${missing_tools[*]}"
      for tool in "${required_tools[@]}"; do
        printf '%s=' "$tool"
        command -v "$tool" 2>&1 || true
      done
      if command -v dpkg-query >/dev/null 2>&1; then
        dpkg-query -W -f='${binary:Package} ${Version}\n' \
          dovecot-core dovecot-imapd imapsync 2>&1 || true
      fi
    } > "$MAILSWIFTSYNC_EVIDENCE_OUTPUT/integration-environment.txt"
    chmod 0600 "$MAILSWIFTSYNC_EVIDENCE_OUTPUT/integration-environment.txt" 2>/dev/null || true
  fi
  echo "FAIL: required IMAP integration tools are missing: ${missing_tools[*]}" >&2
  exit 1
fi

dovecot_version="$(dovecot --version 2>/dev/null || true)"
if [[ -z "$dovecot_version" ]]; then
  if [[ -n "${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-}" ]]; then
    {
      printf 'engine=%s\nuid=%s\nPATH=%s\ndovecot=' "$test_engine" "$(id -u)" "$PATH"
      command -v dovecot 2>&1 || true
      dpkg-query -W -f='${binary:Package} ${Version}\n' dovecot-core dovecot-imapd 2>&1 || true
    } > "$MAILSWIFTSYNC_EVIDENCE_OUTPUT/integration-environment.txt"
    chmod 0600 "$MAILSWIFTSYNC_EVIDENCE_OUTPUT/integration-environment.txt" 2>/dev/null || true
  fi
  echo "FAIL: unable to determine the installed Dovecot version" >&2
  exit 1
fi
dovecot_semver="${dovecot_version%% *}"
imapsync_version_output="$(imapsync --version 2>&1 || true)"
imapsync_version="$(sed -n -e 's/.*imapsync[[:space:]]\+\([0-9][0-9.]*\).*/\1/p' \
  -e 's/^[[:space:]]*v\?\([0-9][0-9.]*\)[[:space:]]*$/\1/p' \
  <<<"$imapsync_version_output" | head -1)"
if [[ "$imapsync_version" != "2.314" ]]; then
  if [[ -n "${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-}" ]]; then
    {
      printf 'engine=%s\n' "$test_engine"
      printf 'dovecot_version=%s\n' "$dovecot_version"
      printf 'imapsync_parsed_version=%s\n' "${imapsync_version:-unknown}"
      printf 'imapsync_version_output=%s\n' "$imapsync_version_output"
    } > "$MAILSWIFTSYNC_EVIDENCE_OUTPUT/integration-environment.txt"
    chmod 0600 "$MAILSWIFTSYNC_EVIDENCE_OUTPUT/integration-environment.txt" 2>/dev/null || true
  fi
  echo "FAIL: product integration requires packaged imapsync 2.314; found ${imapsync_version:-unknown}" >&2
  exit 1
fi
echo "Using migration engine ${test_engine}, Dovecot ${dovecot_version}, and imapsync ${imapsync_version}"
if [[ -n "${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-}" ]]; then
  {
    printf 'engine=%s\n' "$test_engine"
    printf 'dovecot_version=%s\n' "$dovecot_version"
    printf 'imapsync_version=%s\n' "$imapsync_version"
  } > "$MAILSWIFTSYNC_EVIDENCE_OUTPUT/integration-environment.txt"
  chmod 0600 "$MAILSWIFTSYNC_EVIDENCE_OUTPUT/integration-environment.txt" 2>/dev/null || true
fi

workspace="$(mktemp -d "${TMPDIR:-/tmp}/mailswiftsync-imap-lab.XXXXXX")"
product_log="$workspace/mailswiftsync.log"
# The disposable Dovecot fixture and the product process run under the same
# identity selected by the integration container. Give the product a private
# runtime directory owned by that identity rather than inheriting the image's
# default /run/user/10001 when the privileged fixture setup runs as root.
export XDG_RUNTIME_DIR="$workspace/runtime"
mkdir -m 0700 -- "$XDG_RUNTIME_DIR"
if [[ -n "${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-}" ]]; then
  mkdir -p -- "$MAILSWIFTSYNC_EVIDENCE_OUTPUT"
fi
cleanup() {
  local status=$?
  if [[ "$status" -ne 0 ]]; then
    if [[ -n "${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-}" ]]; then
      {
        printf 'exit_status=%s\nengine=%s\ndovecot_version=%s\nimapsync_version=%s\n' \
          "$status" "${test_engine:-unknown}" "${dovecot_version:-unknown}" "${imapsync_version:-unknown}"
        if [[ -n "${binary:-}" && -f "${state:-}" ]]; then
          echo '--- bounded durable status summary ---'
          "$binary" status "$state" --summary 2>&1 || true
        fi
        for log in "${workspace:-}"/source/log/dovecot-info.log \
          "${workspace:-}"/source/log/dovecot.log \
          "${workspace:-}"/destination/log/dovecot-info.log \
          "${workspace:-}"/destination/log/dovecot.log \
          "${workspace:-}"/source/dovecot.stdout \
          "${workspace:-}"/destination/dovecot.stdout; do
          if [[ -f "$log" ]]; then
            echo "--- $log (last 32 KiB) ---"
            tail -c 32768 "$log" || true
          fi
        done
        if [[ -n "${product_log:-}" && -f "${product_log:-}" ]]; then
          echo '--- MailSwiftSync command output (last 64 KiB) ---'
          tail -c 65536 "$product_log" || true
        fi
        if [[ -n "${diagnostic_dir:-}" && -d "${diagnostic_dir:-}" ]]; then
          for log in "$diagnostic_dir"/mailswiftsync-*.log; do
            if [[ -f "$log" ]]; then
              echo "--- $log (last 64 KiB) ---"
              tail -c 65536 "$log" || true
            fi
          done
        fi
      } | sed 's/lab-password/[REDACTED]/g' | head -c 262144 \
        > "$MAILSWIFTSYNC_EVIDENCE_OUTPUT/integration-diagnostics.txt" || true
      chmod 0600 "$MAILSWIFTSYNC_EVIDENCE_OUTPUT/integration-diagnostics.txt" 2>/dev/null || true
    fi
    echo "--- MailSwiftSync integration diagnostics (exit $status) ---" >&2
    if [[ -n "${binary:-}" && -f "${state:-}" ]]; then
      "$binary" status "$state" --summary >&2 || true
    fi
    for log in "${workspace:-}"/source/log/dovecot-info.log \
      "${workspace:-}"/source/log/dovecot.log \
      "${workspace:-}"/destination/log/dovecot-info.log \
      "${workspace:-}"/destination/log/dovecot.log \
      "${workspace:-}"/source/dovecot.stdout \
      "${workspace:-}"/destination/dovecot.stdout; do
      if [[ -f "$log" ]]; then
        echo "--- $log ---" >&2
        tail -120 "$log" >&2 || true
      fi
    done
    if [[ -n "${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-}" && -f "${product_log:-}" ]]; then
      sed 's/lab-password/[REDACTED]/g' "$product_log" \
        > "$MAILSWIFTSYNC_EVIDENCE_OUTPUT/integration-product.log" || true
      chmod 0600 "$MAILSWIFTSYNC_EVIDENCE_OUTPUT/integration-product.log" 2>/dev/null || true
    fi
  fi
  if [[ -n "${source_pid:-}" ]]; then kill "$source_pid" 2>/dev/null || true; fi
  if [[ -n "${destination_pid:-}" ]]; then kill "$destination_pid" 2>/dev/null || true; fi
  wait "${source_pid:-}" 2>/dev/null || true
  wait "${destination_pid:-}" 2>/dev/null || true
  if [[ "${MAILSWIFTSYNC_KEEP_LAB:-0}" != "1" ]]; then
    rm -rf -- "$workspace"
  else
    echo "Keeping integration lab workspace: $workspace" >&2
  fi
  return "$status"
}
trap cleanup EXIT

uid="$(id -u)"
gid="$(id -g)"
# Dovecot 2.4 rejects passwd-file entries that resolve to uid 0. Use the
# service account for the disposable fixture when it exists; this also keeps
# the test representative of an unprivileged mailbox service.
mail_uid="$(id -u dovecot 2>/dev/null || printf '%s' "$uid")"
mail_gid="$(id -g dovecot 2>/dev/null || printf '%s' "$gid")"
user="lab@example.test"
password="lab-password"
# The Dovecot auth worker must traverse every parent of its passwd-file. Give
# the disposable lab root to the fixture service account so Dovecot cannot
# harden it back to mode 0700 while starting its auth service.
if id dovecot >/dev/null 2>&1; then
  chown "$mail_uid:$mail_gid" "$workspace"
  chmod 0750 "$workspace"
else
  chmod 0755 "$workspace"
fi

start_server() {
  local name="$1" port="$2"
  local root="$workspace/$name"
  local config="$workspace/$name.conf"
  mkdir -p "$root/mail/$user/Maildir"/{cur,new,tmp} "$root/run" "$root/log"
  printf '%s:{PLAIN}%s:%s:%s::%s::\n' "$user" "$password" "$mail_uid" "$mail_gid" "$root/mail/$user" > "$root/passwd"
  # The distributed Bookworm image pins Dovecot 2.3.19.1, while some local
  # validation hosts already run Dovecot 2.4. Select the matching dialect so
  # the release fixture tests the packaged runtime rather than accidentally
  # testing a different configuration generation.
  openssl req -x509 -newkey rsa:2048 -nodes -days 2 \
    -keyout "$root/ca.key" -out "$root/ca.crt" \
    -subj "/CN=MailSwiftSync integration CA" \
    -addext "basicConstraints=critical,CA:TRUE" \
    -addext "keyUsage=critical,keyCertSign,cRLSign" \
    >/dev/null 2>&1
  openssl req -newkey rsa:2048 -nodes -keyout "$root/tls.key" \
    -out "$root/tls.csr" -subj "/CN=127.0.0.1" >/dev/null 2>&1
  cat > "$root/tls.ext" <<'EOF'
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
subjectAltName=IP:127.0.0.1
EOF
  openssl x509 -req -days 2 -in "$root/tls.csr" \
    -CA "$root/ca.crt" -CAkey "$root/ca.key" -CAcreateserial \
    -out "$root/tls.crt" -extfile "$root/tls.ext" >/dev/null 2>&1
  chmod 0600 "$root/ca.key" "$root/tls.key"
  local version_setting mail_settings auth_settings auth_cleartext_setting tls_settings
  case "$dovecot_version" in
    2.4.*)
      version_setting="dovecot_config_version = 2.4.0
dovecot_storage_version = $dovecot_semver"
      if [[ "$name" == destination && "$test_engine" == Dovecot ]]; then
        # The native backup strategy must reconcile a previously-created
        # destination INBOX. Maildir cannot delete/recreate INBOX, so use
        # mdbox for the native target fixture while keeping Maildir as source.
        mail_settings="mail_driver = mdbox
mail_path = ~/mdbox"
      else
        mail_settings="mail_driver = maildir
mail_path = ~/Maildir"
      fi
      auth_settings="passdb passwd-file {
  passwd_file_path = $root/passwd
}
userdb passwd-file {
  passwd_file_path = $root/passwd
}"
      auth_cleartext_setting="auth_allow_cleartext = yes"
      tls_settings="ssl_server {
  cert_file = $root/tls.crt
  key_file = $root/tls.key
}"
      ;;
    2.3.*)
      version_setting=""
      if [[ "$name" == destination && "$test_engine" == Dovecot ]]; then
        mail_settings="mail_location = mdbox:~/mdbox"
      else
        mail_settings="mail_location = maildir:~/Maildir"
      fi
      auth_settings="passdb {
  driver = passwd-file
  args = $root/passwd
}
userdb {
  driver = passwd-file
  args = $root/passwd
}"
      auth_cleartext_setting="disable_plaintext_auth = no"
      tls_settings="ssl_cert = <$root/tls.crt
ssl_key = <$root/tls.key"
      ;;
    *)
      echo "FAIL: unsupported Dovecot configuration generation: $dovecot_version" >&2
      return 1
      ;;
  esac
  # Dovecot's unprivileged auth worker must traverse the temporary path to
  # read the owner-only passwd file. Mail data remains owner-only.
  chmod 0711 "$workspace" "$root"
  if id dovecot >/dev/null 2>&1 && chgrp -R dovecot "$root" 2>/dev/null; then
    chown -R "$mail_uid:$mail_gid" "$root/mail/$user"
    chmod 0750 "$root/mail" "$root/mail/$user"
    chmod 0640 "$root/passwd"
  else
    chmod 0755 "$root/mail" "$root/mail/$user"
    chmod 0644 "$root/passwd"
  fi
  cat > "$config" <<EOF
$version_setting
base_dir = $root/run/
state_dir = $root/run/
protocols = imap
listen = 127.0.0.1
log_path = $root/log/dovecot.log
info_log_path = $root/log/dovecot-info.log
ssl = yes
$tls_settings
$auth_cleartext_setting
auth_mechanisms = plain login
auth_verbose = yes
first_valid_uid = 1
mail_home = $root/mail/%u
$mail_settings
namespace inbox {
  inbox = yes
  prefix =
  separator = /
}
$auth_settings
service imap-login {
  inet_listener imap {
    port = $port
  }
  inet_listener imaps {
    port = 0
  }
}
EOF
  dovecot -F -c "$config" > "$root/dovecot.stdout" 2>&1 &
  echo "$!"
}

source_port="${MAILSWIFTSYNC_SOURCE_PORT:-19143}"
destination_port="${MAILSWIFTSYNC_DESTINATION_PORT:-19144}"
source_pid="$(start_server source "$source_port")"
destination_pid="$(start_server destination "$destination_port")"

for port in "$source_port" "$destination_port"; do
  ready=0
  for _ in {1..50}; do
    if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
      exec 3>&-
      ready=1
      break
    fi
    sleep 0.1
  done
  if [[ "$ready" != 1 ]]; then
    echo "FAIL: Dovecot did not listen on 127.0.0.1:$port" >&2
    exit 1
  fi
done

message="$workspace/source/mail/$user/Maildir/new/fixture.eml"
cat > "$message" <<'EOF'
From: migration-lab@example.test
To: lab@example.test
Subject: MailSwiftSync integration fixture
Message-ID: <mailswiftsync-integration-fixture@example.test>
Date: Tue, 01 Jan 2030 00:00:00 +0000
Content-Type: text/plain; charset=utf-8

This message is a disposable integration fixture.
EOF

# Keep these fixtures deliberately awkward.  They are Maildir files rather
# than generated UTF-8 strings so Dovecot exposes the same RFC822 bytes that
# an IMAP FETCH literal would expose to the verifier.
literal_message="$workspace/source/mail/$user/Maildir/new/literal-framing.eml"
printf 'From: migration-lab@example.test\r\nTo: lab@example.test\r\nSubject: literal framing\r\nMessage-ID: <mailswiftsync-literal-framing@example.test>\r\nDate: Tue, 01 Jan 2030 00:02:00 +0000\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: 34\r\n\r\nfirst line\r\nFrom not an mbox separator\r\n' > "$literal_message"

latin1_message="$workspace/source/mail/$user/Maildir/new/non-utf8.eml"
printf 'From: migration-lab@example.test\r\nTo: lab@example.test\r\nSubject: non UTF-8 body\r\nMessage-ID: <mailswiftsync-non-utf8@example.test>\r\nDate: Tue, 01 Jan 2030 00:03:00 +0000\r\nContent-Type: text/plain; charset=iso-8859-1\r\n\r\ncaf\351 et cr\350me\r\n' > "$latin1_message"

# The source provider calls this folder "Sent Items".  Its special-use role
# is intentionally represented by the name, not by assuming the provider's
# canonical "Sent" spelling.
mkdir -p "$workspace/source/mail/$user/Maildir/.Sent Items"/{cur,new,tmp}
special_message="$workspace/source/mail/$user/Maildir/.Sent Items/new/renamed-special-use.eml"
cat > "$special_message" <<'EOF'
From: migration-lab@example.test
To: lab@example.test
Subject: renamed special-use folder
Message-ID: <mailswiftsync-renamed-special-use@example.test>
Date: Tue, 01 Jan 2030 00:04:00 +0000
Content-Type: text/plain; charset=utf-8

This message must remain attributable after folder renaming.
EOF

# Two distinct messages with the same Message-ID exercise multiplicity-aware
# reconciliation.  Their sizes/dates differ, so a verifier must not collapse
# them into one identity or report the second as an unrelated extra.
for suffix in one two; do
  duplicate_message="$workspace/source/mail/$user/Maildir/new/duplicate-$suffix.eml"
  if [[ "$suffix" == one ]]; then
    duplicate_subject="duplicate one"
    duplicate_date="00:05:00"
  else
    duplicate_subject="duplicate two"
    duplicate_date="00:06:00"
  fi
  cat > "$duplicate_message" <<EOF
From: migration-lab@example.test
To: lab@example.test
Subject: $duplicate_subject
Message-ID: <mailswiftsync-duplicate@example.test>
Date: Tue, 01 Jan 2030 $duplicate_date +0000
Content-Type: text/plain; charset=utf-8

Duplicate fixture occurrence: $suffix.
EOF
done

# Exercise UID gaps in the real server: create 100 messages in a separate
# mailbox, force Dovecot to assign UIDs, then expunge the first 90.  EXISTS is
# consequently 10 while the surviving UIDs are high (typically 91..100).
mkdir -p "$workspace/source/mail/$user/Maildir/.Sparse"/{cur,new,tmp}
for index in $(seq 1 100); do
  cat > "$workspace/source/mail/$user/Maildir/.Sparse/new/sparse-$index.eml" <<EOF
From: migration-lab@example.test
To: lab@example.test
Subject: sparse UID fixture $index
Message-ID: <mailswiftsync-sparse-$index@example.test>
Date: Tue, 01 Jan 2030 01:00:00 +0000
Content-Type: text/plain; charset=utf-8

Sparse UID fixture message $index.
EOF
done

# Select/expunge through Dovecot so the Maildir fixture is tested with actual
# UID assignment rather than relying on filename order.
chown -R "$mail_uid:$mail_gid" "$workspace/source/mail/$user/Maildir"
doveadm -c "$workspace/source.conf" expunge -u "$user" mailbox Sparse uid 1:90
# doveadm may create or rewrite mailbox index/UID files as root while
# preparing the sparse-UID fixture. Restore service-account ownership before
# the migration process accesses the mailbox over IMAP.
chown -R "$mail_uid:$mail_gid" "$workspace/source/mail/$user/Maildir"
sparse_mailbox="$workspace/source/mail/$user/Maildir/.Sparse"
sparse_owner="$(stat -c '%u' "$sparse_mailbox")"
sparse_mode="$(stat -c '%a' "$sparse_mailbox")"
if [[ "$sparse_owner" != "$mail_uid" ]] || (( (8#$sparse_mode & 0200) == 0 )); then
  echo "FAIL: sparse mailbox must be owned by uid $mail_uid and owner-writable; got uid $sparse_owner mode $sparse_mode" >&2
  exit 1
fi

binary="$(command -v mailswiftsync)"
imapsync_path="$(command -v imapsync)"
doveadm_path="$(command -v doveadm)"
dovecot_config=""
if [[ "$test_engine" == "Dovecot" ]]; then
  dovecot_config="$workspace/destination.conf"
fi
app_runtime="$XDG_RUNTIME_DIR"
state="$app_runtime/state.db"
diagnostic_dir="$app_runtime/diagnostics"
export XDG_CONFIG_HOME="$app_runtime/config"
mkdir -p "$XDG_CONFIG_HOME/mailswiftsync"
chmod 0700 "$XDG_CONFIG_HOME" "$XDG_CONFIG_HOME/mailswiftsync"
cat > "$XDG_CONFIG_HOME/mailswiftsync/profile.toml" <<EOF
name = "Packaged integration"
source_host = "127.0.0.1"
source_port = "$source_port"
source_tls = "starttls"
source_user = "$user"
source_auth = "password"
source_credential_id = ""
source_ca_bundle = "$workspace/source/ca.crt"
source_certificate_pin_sha256 = ""
allow_insecure_source_transport = false
destination_host = "127.0.0.1"
destination_user = "$user"
destination_auth = "password"
destination_credential_id = ""
destination_port = "$destination_port"
destination_tls = "starttls"
destination_ca_bundle = "$workspace/destination/ca.crt"
destination_certificate_pin_sha256 = ""
imapsync_path = "$imapsync_path"
engine = "$test_engine"
dovecot_strategy = "initial_mirror"
doveadm_path = "$doveadm_path"
dovecot_config = "$dovecot_config"
batch_concurrency = 1
batch_retry_count = 0
max_messages_per_second = 0
max_bytes_per_second = 0
migration_timeout_hours = 1
automap = false
addheader = false
justfolders = false
sync_internaldates = true
useuid = true
usecache = true
fastio1 = false
fastio2 = false
allowsizemismatch = false
delete2 = false
extra_options = ""
EOF
source_secret="$app_runtime/source.secret"
destination_secret="$app_runtime/destination.secret"
printf '%s' "$password" > "$source_secret"
printf '%s' "$password" > "$destination_secret"
chmod 0600 "$source_secret" "$destination_secret"

run_product() {
  # A broken engine, fixture, or controller must produce a bounded release
  # failure rather than consuming an unattended CI runner indefinitely.
  timeout --foreground 180 "$binary" "$@" 2>&1 | LC_ALL=C awk -v path="$product_log" '
    {
      print
      fflush()
      if (saved < 4 * 1024 * 1024) {
        line = $0 ORS
        remaining = 4 * 1024 * 1024 - saved
        if (length(line) > remaining) line = substr(line, 1, remaining)
        printf "%s", line >> path
        saved += length(line)
      }
    }
    END { close(path) }
  '
}

assert_mailbox_state() {
  local expected="$1"
  local status_json
  status_json="$(run_product status "$state")"
  if ! grep -q "\"state\": \"$expected\"" <<<"$status_json"; then
    echo "FAIL: durable status did not contain mailbox state '$expected'" >&2
    printf '%s\n' "$status_json" >&2
    exit 1
  fi
  echo "PASS: durable ledger records mailbox state '$expected'"
}

run_product headless "$state" preflight \
  --source-secret-file "$source_secret" --destination-secret-file "$destination_secret" \
  --diagnostic-log "$diagnostic_dir"
echo "PASS: packaged MailSwiftSync preflight completed against real STARTTLS servers"
assert_mailbox_state ready

run_product headless "$state" live \
  --source-secret-file "$source_secret" --destination-secret-file "$destination_secret" \
  --diagnostic-log "$diagnostic_dir"
echo "PASS: packaged MailSwiftSync live migration completed"
if ! run_product status "$state" | grep -Eq '"state": "verified(_with_exceptions)?"'; then
  echo "FAIL: durable ledger did not record a verified terminal state" >&2
  exit 1
fi
echo "PASS: durable ledger records a verified terminal state"

proof="$app_runtime/customer-proof.json"
run_product customer-proof "$state" "$proof" \
  --source-provider generic_imap --destination-provider generic_imap \
  --source-auth password --destination-auth password --fixture-id packaged-generic-imap \
  --scenario-ids basic-small,idempotent-delta
run_product verify "$proof"
echo "PASS: packaged customer proof exported and verified"
if [[ -n "${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-}" ]]; then
  mkdir -p "$MAILSWIFTSYNC_EVIDENCE_OUTPUT"
  cp -- "$proof" "$MAILSWIFTSYNC_EVIDENCE_OUTPUT/customer-proof.json"
fi

second_message="$workspace/source/mail/$user/Maildir/new/delta-fixture.eml"
cat > "$second_message" <<'EOF'
From: migration-lab@example.test
To: lab@example.test
Subject: MailSwiftSync incremental fixture
Message-ID: <mailswiftsync-incremental-fixture@example.test>
Date: Tue, 01 Jan 2030 00:01:00 +0000
Content-Type: text/plain; charset=utf-8

This message proves that a subsequent incremental pass is exercised.
EOF

run_product headless "$state" live \
  --source-secret-file "$source_secret" --destination-secret-file "$destination_secret" \
  --reopen-reason "integration fixture incremental synchronization" \
  --diagnostic-log "$diagnostic_dir"
echo "PASS: packaged MailSwiftSync incremental live migration completed"
if ! run_product status "$state" | grep -Eq '"state": "verified(_with_exceptions)?"'; then
  echo "FAIL: incremental orchestration did not leave a verified terminal state" >&2
  exit 1
fi
echo "PASS: incremental orchestration preserved verified terminal state"

destination_maildir="$workspace/destination/mail/$user/Maildir"
destination_message_ids=""
if [[ "$test_engine" == Dovecot ]]; then
  # The native-engine fixture uses mdbox to support exact backup semantics;
  # inspect stored messages through Dovecot rather than assuming Maildir files.
  destination_message_ids="$(doveadm -c "$workspace/destination.conf" fetch -u "$user" 'hdr.message-id' mailbox '*')"
  destination_messages="$(grep -c '^hdr.message-id:' <<<"$destination_message_ids" || true)"
else
  destination_messages="$(find "$destination_maildir" -type f \( -path '*/cur/*' -o -path '*/new/*' \) | wc -l)"
fi
destination_has_message_id() {
  local message_id="$1"
  if [[ "$test_engine" == Dovecot ]]; then
    grep -F -q -- "$message_id" <<<"$destination_message_ids"
  else
    grep -R -F -q -- "Message-ID: <$message_id>" "$destination_maildir"
  fi
}
if [[ "$destination_messages" -lt 1 ]]; then
  echo "FAIL: ${test_engine} reported success but destination has no fixture message" >&2
  exit 1
fi
if ! destination_has_message_id "mailswiftsync-integration-fixture@example.test"; then
  echo "FAIL: destination is missing the fixture Message-ID" >&2
  exit 1
fi
echo "PASS: destination retained the fixture Message-ID"
if [[ "$destination_messages" -lt 17 ]] || ! destination_has_message_id "mailswiftsync-incremental-fixture@example.test"; then
  echo "FAIL: destination is missing the incremental fixture Message-ID" >&2
  exit 1
fi
echo "PASS: destination retained both initial and incremental Message-IDs"
for message_id in \
  mailswiftsync-literal-framing@example.test \
  mailswiftsync-non-utf8@example.test \
  mailswiftsync-renamed-special-use@example.test; do
  if ! destination_has_message_id "$message_id"; then
    echo "FAIL: destination is missing edge-case fixture Message-ID <$message_id>" >&2
    exit 1
  fi
done
for index in 1 100; do
  if ! destination_has_message_id "mailswiftsync-sparse-$index@example.test"; then
    echo "FAIL: destination is missing sparse UID fixture message $index" >&2
    exit 1
  fi
done
if [[ "$test_engine" == Dovecot ]]; then
  sparse_count="$(grep -c '^hdr.message-id: <mailswiftsync-sparse-' <<<"$destination_message_ids" || true)"
else
  sparse_count="$(grep -R -F -l -- "Message-ID: <mailswiftsync-sparse-" "$destination_maildir" | wc -l)"
fi
if [[ "$sparse_count" -ne 10 ]]; then
  echo "FAIL: destination retained $sparse_count sparse UID messages; expected 10" >&2
  exit 1
fi
echo "PASS: destination retained sparse-UID fixture endpoints"
if [[ "$test_engine" == Dovecot ]]; then
  duplicate_count="$(grep -c '^hdr.message-id: <mailswiftsync-duplicate@example.test>' <<<"$destination_message_ids" || true)"
else
  duplicate_count="$(grep -R -F -l -- "Message-ID: <mailswiftsync-duplicate@example.test>" "$destination_maildir" | wc -l)"
fi
if [[ "$duplicate_count" -ne 2 ]]; then
  echo "FAIL: destination retained $duplicate_count duplicate-ID messages; expected 2" >&2
  exit 1
fi
echo "PASS: destination retained literal, non-UTF-8, renamed special-use, and duplicate-ID fixtures"
echo "PASS: MailSwiftSync product integration verified $destination_messages message(s) through ${test_engine}"
