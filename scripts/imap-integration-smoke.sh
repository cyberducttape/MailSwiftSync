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

if ! command -v dovecot >/dev/null 2>&1 || ! command -v imapsync >/dev/null 2>&1 || \
  ! command -v mailswiftsync >/dev/null 2>&1 || ! command -v openssl >/dev/null 2>&1 || \
  ! command -v timeout >/dev/null 2>&1; then
  echo "SKIP: install mailswiftsync, dovecot, imapsync, openssl, and timeout to run the IMAP integration lab" >&2
  exit 77
fi

dovecot_version="$(dovecot --version 2>/dev/null || true)"
if [[ -z "$dovecot_version" ]]; then
  echo "FAIL: unable to determine the installed Dovecot version" >&2
  exit 1
fi
dovecot_semver="${dovecot_version%% *}"
imapsync_version_output="$(imapsync --version 2>&1 || true)"
imapsync_version="$(sed -n -e 's/.*imapsync[[:space:]]\+\([0-9][0-9.]*\).*/\1/p' \
  -e 's/^[[:space:]]*v\?\([0-9][0-9.]*\)[[:space:]]*$/\1/p' \
  <<<"$imapsync_version_output" | head -1)"
if [[ "$imapsync_version" != "2.314" ]]; then
  echo "FAIL: product integration requires packaged imapsync 2.314; found ${imapsync_version:-unknown}" >&2
  exit 1
fi
echo "Using migration engine ${test_engine}, Dovecot ${dovecot_version}, and imapsync ${imapsync_version}"

workspace="$(mktemp -d "${TMPDIR:-/tmp}/mailswiftsync-imap-lab.XXXXXX")"
cleanup() {
  local status=$?
  if [[ "$status" -ne 0 ]]; then
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
      mail_settings="mail_driver = maildir
mail_path = ~/Maildir"
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
      mail_settings="mail_location = maildir:~/Maildir"
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
doveadm -c "$workspace/source.conf" expunge -u "$user" mailbox Sparse uid 1:90

binary="$(command -v mailswiftsync)"
imapsync_path="$(command -v imapsync)"
doveadm_path="$(command -v doveadm)"
dovecot_config=""
if [[ "$test_engine" == "Dovecot" ]]; then
  dovecot_config="$workspace/destination.conf"
fi
state="$workspace/state.db"
export XDG_CONFIG_HOME="$workspace/config"
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
doveadm_path = "$doveadm_path"
ssh_path = "ssh"
dovecot_execution = "local"
dovecot_ssh_user = ""
dovecot_config = "$dovecot_config"
batch_concurrency = 1
batch_retry_count = 0
max_messages_per_second = 0
max_bytes_per_second = 0
migration_timeout_hours = 1
allow_remote_password_in_argv = false
automap = true
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
source_secret="$workspace/source.secret"
destination_secret="$workspace/destination.secret"
printf '%s' "$password" > "$source_secret"
printf '%s' "$password" > "$destination_secret"
chmod 0600 "$source_secret" "$destination_secret"

run_product() {
  # A broken engine, fixture, or controller must produce a bounded release
  # failure rather than consuming an unattended CI runner indefinitely.
  timeout --foreground 180 "$binary" "$@"
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
  --source-secret-file "$source_secret" --destination-secret-file "$destination_secret"
echo "PASS: packaged MailSwiftSync preflight completed against real STARTTLS servers"
assert_mailbox_state ready

run_product headless "$state" live \
  --source-secret-file "$source_secret" --destination-secret-file "$destination_secret"
echo "PASS: packaged MailSwiftSync live migration completed"
if ! run_product status "$state" | grep -Eq '"state": "verified(_with_exceptions)?"'; then
  echo "FAIL: durable ledger did not record a verified terminal state" >&2
  exit 1
fi
echo "PASS: durable ledger records a verified terminal state"

proof="$workspace/customer-proof.json"
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
  --source-secret-file "$source_secret" --destination-secret-file "$destination_secret"
echo "PASS: packaged MailSwiftSync incremental live migration completed"
if ! run_product status "$state" | grep -Eq '"state": "verified(_with_exceptions)?"'; then
  echo "FAIL: incremental orchestration did not leave a verified terminal state" >&2
  exit 1
fi
echo "PASS: incremental orchestration preserved verified terminal state"

destination_messages="$(find "$workspace/destination/mail/$user/Maildir" -type f \( -path '*/cur/*' -o -path '*/new/*' \) | wc -l)"
if [[ "$destination_messages" -lt 1 ]]; then
  echo "FAIL: ${test_engine} reported success but destination Maildir has no fixture message" >&2
  exit 1
fi
if ! grep -R -F -l -- "Message-ID: <mailswiftsync-integration-fixture@example.test>" \
  "$workspace/destination/mail/$user/Maildir/cur" \
  "$workspace/destination/mail/$user/Maildir/new" >/dev/null 2>&1; then
  echo "FAIL: destination Maildir is missing the fixture Message-ID" >&2
  exit 1
fi
echo "PASS: destination retained the fixture Message-ID"
if [[ "$destination_messages" -lt 17 ]] || ! grep -R -F -l -- "Message-ID: <mailswiftsync-incremental-fixture@example.test>" \
  "$workspace/destination/mail/$user/Maildir/cur" \
  "$workspace/destination/mail/$user/Maildir/new" >/dev/null 2>&1; then
  echo "FAIL: destination is missing the incremental fixture Message-ID" >&2
  exit 1
fi
echo "PASS: destination retained both initial and incremental Message-IDs"
for message_id in \
  mailswiftsync-literal-framing@example.test \
  mailswiftsync-non-utf8@example.test \
  mailswiftsync-renamed-special-use@example.test; do
  if ! grep -R -F -l -- "Message-ID: <$message_id>" \
    "$workspace/destination/mail/$user/Maildir" >/dev/null 2>&1; then
    echo "FAIL: destination is missing edge-case fixture Message-ID <$message_id>" >&2
    exit 1
  fi
done
for index in 1 100; do
  if ! grep -R -F -l -- "Message-ID: <mailswiftsync-sparse-$index@example.test>" \
    "$workspace/destination/mail/$user/Maildir" >/dev/null 2>&1; then
    echo "FAIL: destination is missing sparse UID fixture message $index" >&2
    exit 1
  fi
done
sparse_count="$(grep -R -F -l -- "Message-ID: <mailswiftsync-sparse-" \
  "$workspace/destination/mail/$user/Maildir" | wc -l)"
if [[ "$sparse_count" -ne 10 ]]; then
  echo "FAIL: destination retained $sparse_count sparse UID messages; expected 10" >&2
  exit 1
fi
echo "PASS: destination retained sparse-UID fixture endpoints"
duplicate_count="$(grep -R -F -l -- "Message-ID: <mailswiftsync-duplicate@example.test>" \
  "$workspace/destination/mail/$user/Maildir" | wc -l)"
if [[ "$duplicate_count" -ne 2 ]]; then
  echo "FAIL: destination retained $duplicate_count duplicate-ID messages; expected 2" >&2
  exit 1
fi
echo "PASS: destination retained literal, non-UTF-8, renamed special-use, and duplicate-ID fixtures"
echo "PASS: MailSwiftSync product integration copied $destination_messages message(s) through ${test_engine}"
