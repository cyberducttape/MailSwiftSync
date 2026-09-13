#!/usr/bin/env bash
set -euo pipefail

# Reproducible product-level lab for the supported generic-IMAP path. It
# exercises the packaged MailSwiftSync binary, its generated plan and secret
# delivery, the real Dovecot servers/imapsync binary, durable evidence, and
# the customer-proof verifier. Direct engine checks remain supplemental.

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
echo "Using Dovecot ${dovecot_version} and imapsync $(imapsync --version 2>/dev/null || true)"

workspace="$(mktemp -d "${TMPDIR:-/tmp}/mailswiftsync-imap-lab.XXXXXX")"
cleanup() {
  if [[ -n "${source_pid:-}" ]]; then kill "$source_pid" 2>/dev/null || true; fi
  if [[ -n "${destination_pid:-}" ]]; then kill "$destination_pid" 2>/dev/null || true; fi
  wait "${source_pid:-}" 2>/dev/null || true
  wait "${destination_pid:-}" 2>/dev/null || true
  if [[ "${MAILSWIFTSYNC_KEEP_LAB:-0}" != "1" ]]; then
    rm -rf -- "$workspace"
  else
    echo "Keeping integration lab workspace: $workspace" >&2
  fi
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

binary="$(command -v mailswiftsync)"
imapsync_path="$(command -v imapsync)"
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
engine = "ImapSync"
doveadm_path = "doveadm"
ssh_path = "ssh"
dovecot_execution = "automatic"
dovecot_ssh_user = ""
dovecot_config = ""
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
run_product customer-proof "$state" "$proof"
run_product verify "$proof"
echo "PASS: packaged customer proof exported and verified"

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
  echo "FAIL: imapsync reported success but destination Maildir has no fixture message" >&2
  exit 1
fi
if ! grep -R -F -l -- "Message-ID: <mailswiftsync-integration-fixture@example.test>" \
  "$workspace/destination/mail/$user/Maildir/cur" \
  "$workspace/destination/mail/$user/Maildir/new" >/dev/null 2>&1; then
  echo "FAIL: destination Maildir is missing the fixture Message-ID" >&2
  exit 1
fi
echo "PASS: destination retained the fixture Message-ID"
if [[ "$destination_messages" -lt 2 ]] || ! grep -R -F -l -- "Message-ID: <mailswiftsync-incremental-fixture@example.test>" \
  "$workspace/destination/mail/$user/Maildir/cur" \
  "$workspace/destination/mail/$user/Maildir/new" >/dev/null 2>&1; then
  echo "FAIL: destination is missing the incremental fixture Message-ID" >&2
  exit 1
fi
echo "PASS: destination retained both initial and incremental Message-IDs"
echo "PASS: MailSwiftSync product integration copied $destination_messages message(s) through the packaged engine"
