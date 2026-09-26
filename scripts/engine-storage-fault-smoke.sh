#!/usr/bin/env bash
set -euo pipefail

# Engine-side storage-fault lab. release-readiness.md names this the
# remaining half of the disk-full/corruption chaos gap: the ledger-level
# half (a storage limit hit while state.db itself is written, and a
# corrupted/truncated ledger) is covered by controller-chaos-smoke.sh; this
# script covers the transfer engine itself running out of destination
# space mid-transfer, which that ledger-only fault injection cannot reach.
#
# The destination Dovecot's imap service is launched under a small
# RLIMIT_FSIZE (`ulimit -f`), through a wrapper substituted as
# `service imap { executable = ... }`. That confines the limit to the
# per-connection mail process, not the master/auth/login services, so it
# reproduces a destination write that fails partway through -- a full
# filesystem refuses to grow the file past its limit exactly the same way --
# without needing root, a real full disk, or a loopback-mounted filesystem.
# The oversized message's IMAP worker is killed by SIGXFSZ; a small message
# stays under the limit and transfers normally. This proves MailSwiftSync
# surfaces the failed message rather than reporting a false clean success.

if ! command -v dovecot >/dev/null 2>&1 || ! command -v imapsync >/dev/null 2>&1 || \
  ! command -v mailswiftsync >/dev/null 2>&1 || ! command -v openssl >/dev/null 2>&1 || \
  ! command -v timeout >/dev/null 2>&1; then
  echo "SKIP: install mailswiftsync, dovecot, imapsync, openssl, and timeout to run the engine storage-fault lab" >&2
  exit 77
fi

dovecot_version="$(dovecot --version 2>/dev/null || true)"
if [[ -z "$dovecot_version" ]]; then
  echo "FAIL: unable to determine the installed Dovecot version" >&2
  exit 1
fi
dovecot_semver="${dovecot_version%% *}"
imap_binary="$(find /usr/lib*/dovecot /usr/libexec/dovecot -maxdepth 1 -type f -name imap 2>/dev/null | head -1)"
if [[ -z "$imap_binary" ]]; then
  echo "FAIL: could not locate the dovecot imap service binary to wrap" >&2
  exit 1
fi
echo "Using Dovecot ${dovecot_version} and wrapping ${imap_binary}"

workspace="$(mktemp -d "${TMPDIR:-/tmp}/mailswiftsync-storage-fault-lab.XXXXXX")"
cleanup() {
  local status=$?
  if [[ "$status" -ne 0 ]]; then
    echo "--- MailSwiftSync storage-fault diagnostics (exit $status) ---" >&2
    if [[ -n "${binary:-}" && -f "${state:-}" ]]; then
      "$binary" status "$state" --summary >&2 || true
    fi
    for log in "${workspace:-}"/source/log/dovecot-info.log \
      "${workspace:-}"/source/log/dovecot.log \
      "${workspace:-}"/destination/log/dovecot-info.log \
      "${workspace:-}"/destination/log/dovecot.log; do
      if [[ -f "$log" ]]; then
        echo "--- $log ---" >&2
        tail -60 "$log" >&2 || true
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
    echo "Keeping storage-fault lab workspace: $workspace" >&2
  fi
  return "$status"
}
trap cleanup EXIT

uid="$(id -u)"
gid="$(id -g)"
mail_uid="$(id -u dovecot 2>/dev/null || printf '%s' "$uid")"
mail_gid="$(id -g dovecot 2>/dev/null || printf '%s' "$gid")"
user="lab@example.test"
password="lab-password"
if id dovecot >/dev/null 2>&1; then
  chown "$mail_uid:$mail_gid" "$workspace"
  chmod 0750 "$workspace"
else
  chmod 0755 "$workspace"
fi

start_server() {
  local name="$1" port="$2" fsize_limit="${3:-}"
  local root="$workspace/$name"
  local config="$workspace/$name.conf"
  mkdir -p "$root/mail/$user/Maildir"/{cur,new,tmp} "$root/run" "$root/log"
  printf '%s:{PLAIN}%s:%s:%s::%s::\n' "$user" "$password" "$mail_uid" "$mail_gid" "$root/mail/$user" > "$root/passwd"
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
  local extra_service=""
  if [[ -n "$fsize_limit" ]]; then
    # Wrapping only the "imap" service (the per-connection worker that
    # writes mail to disk) means an oversized write kills that one session,
    # not the master/auth/login services other connections depend on.
    cat > "$root/imap-fsize-wrapper" <<WRAP
#!/usr/bin/env bash
ulimit -f $fsize_limit
exec $imap_binary "\$@"
WRAP
    chmod 0755 "$root/imap-fsize-wrapper"
    extra_service="service imap {
  executable = $root/imap-fsize-wrapper
}"
  fi
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
$extra_service
EOF
  dovecot -F -c "$config" > "$root/dovecot.stdout" 2>&1 &
  echo "$!"
}

source_port="${MAILSWIFTSYNC_SOURCE_PORT:-19153}"
destination_port="${MAILSWIFTSYNC_DESTINATION_PORT:-19154}"
source_pid="$(start_server source "$source_port")"
# 4 blocks is comfortably under any real message here (the small fixture is
# under 200 bytes; the oversized fixture is 64 KiB) and comfortably above
# what Dovecot itself needs to write to start the per-connection worker, so
# only the oversized message's write trips it.
destination_pid="$(start_server destination "$destination_port" 4)"

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

small_message="$workspace/source/mail/$user/Maildir/new/small-fixture.eml"
cat > "$small_message" <<'EOF'
From: migration-lab@example.test
To: lab@example.test
Subject: MailSwiftSync storage-fault lab: small fixture
Message-ID: <mailswiftsync-storage-fault-small@example.test>
Date: Tue, 01 Jan 2030 00:00:00 +0000
Content-Type: text/plain; charset=utf-8

This message stays comfortably under the destination fsize limit.
EOF

large_message="$workspace/source/mail/$user/Maildir/new/large-fixture.eml"
{
  printf 'From: migration-lab@example.test\r\nTo: lab@example.test\r\nSubject: MailSwiftSync storage-fault lab: oversized fixture\r\nMessage-ID: <mailswiftsync-storage-fault-large@example.test>\r\nDate: Tue, 01 Jan 2030 00:01:00 +0000\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n'
  head -c 65536 /dev/zero | tr '\0' 'Z'
  printf '\r\n'
} > "$large_message"

binary="$(command -v mailswiftsync)"
imapsync_path="$(command -v imapsync)"
state="$workspace/state.db"
export XDG_CONFIG_HOME="$workspace/config"
mkdir -p "$XDG_CONFIG_HOME/mailswiftsync"
chmod 0700 "$XDG_CONFIG_HOME" "$XDG_CONFIG_HOME/mailswiftsync"
cat > "$XDG_CONFIG_HOME/mailswiftsync/profile.toml" <<EOF
name = "Storage fault lab"
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
dovecot_config = ""
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
source_secret="$workspace/source.secret"
destination_secret="$workspace/destination.secret"
printf '%s' "$password" > "$source_secret"
printf '%s' "$password" > "$destination_secret"
chmod 0600 "$source_secret" "$destination_secret"

run_product() {
  # A broken engine, fixture, or controller must produce a bounded release
  # failure rather than consuming an unattended CI runner indefinitely.
  timeout --foreground 300 "$binary" "$@"
}

run_product headless "$state" preflight \
  --source-secret-file "$source_secret" --destination-secret-file "$destination_secret"
echo "PASS: preflight completed against a destination whose imap worker has a byte-size write limit"

set +e
run_product headless "$state" live \
  --source-secret-file "$source_secret" --destination-secret-file "$destination_secret"
live_exit=$?
set -e

status_json="$(run_product status "$state")"
if grep -Eq '"state": "verified"' <<<"$status_json" && [[ "$live_exit" -eq 0 ]]; then
  echo "FAIL: live migration reported a clean verified success despite an oversized message failing to write on the destination" >&2
  printf '%s\n' "$status_json" >&2
  exit 1
fi
echo "PASS: the destination write fault was not reported as a clean verified success (live exit $live_exit)"

if ! grep -Eq '"state": "(verified_with_exceptions|failed|attention)"' <<<"$status_json"; then
  echo "FAIL: durable ledger did not record a recognizable outcome for the storage fault" >&2
  printf '%s\n' "$status_json" >&2
  exit 1
fi
echo "PASS: durable ledger recorded an explicit non-clean terminal or attention state"

if ! "$binary" status "$state" >/dev/null 2>&1; then
  echo "FAIL: the ledger was not readable after the destination storage fault" >&2
  exit 1
fi
echo "PASS: the ledger remained readable after the destination storage fault"

destination_small="$(find "$workspace/destination/mail/$user/Maildir" -type f \( -path '*/cur/*' -o -path '*/new/*' \) \
  -exec grep -F -l -- "Message-ID: <mailswiftsync-storage-fault-small@example.test>" {} + 2>/dev/null | head -1)"
if [[ -z "$destination_small" ]]; then
  echo "FAIL: the message under the destination fsize limit did not transfer" >&2
  exit 1
fi
echo "PASS: the message that stayed under the destination limit transferred correctly"

destination_large="$(find "$workspace/destination/mail/$user/Maildir" -type f \( -path '*/cur/*' -o -path '*/new/*' \) \
  -exec grep -F -l -- "Message-ID: <mailswiftsync-storage-fault-large@example.test>" {} + 2>/dev/null | head -1)"
if [[ -n "$destination_large" ]]; then
  echo "FAIL: the oversized message that should have exceeded the destination write limit was found on the destination intact" >&2
  exit 1
fi
echo "PASS: the message that exceeded the destination write limit was not silently marked delivered"
echo "PASS: MailSwiftSync surfaced a destination storage fault instead of reporting a false clean success"
