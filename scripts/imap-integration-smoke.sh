#!/usr/bin/env bash
set -euo pipefail

# Reproducible product-level lab for the selected migration engine. It
# exercises the packaged MailSwiftSync binary, its generated plan and secret
# delivery, real Dovecot servers, durable evidence, and the customer-proof
# verifier. The default remains the packaged imapsync path; CI invokes the
# native Dovecot path separately.

test_engine="${MAILSWIFTSYNC_TEST_ENGINE:-ImapSync}"
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
scale_messages_raw="${MAILSWIFTSYNC_SCALE_MESSAGES:-0}"
if [[ ! "$scale_messages_raw" =~ ^[0-9]{1,6}$ ]] || (( 10#$scale_messages_raw > 100000 )); then
  echo "FAIL: MAILSWIFTSYNC_SCALE_MESSAGES must be between 0 and 100000" >&2
  exit 1
fi
scale_messages=$((10#$scale_messages_raw))
scale_total_body_bytes_raw="${MAILSWIFTSYNC_SCALE_TOTAL_BODY_BYTES:-0}"
if [[ ! "$scale_total_body_bytes_raw" =~ ^[0-9]{1,10}$ ]] \
  || (( 10#$scale_total_body_bytes_raw > 2147483648 )); then
  echo "FAIL: MAILSWIFTSYNC_SCALE_TOTAL_BODY_BYTES must be between 0 and 2147483648" >&2
  exit 1
fi
scale_total_body_bytes=$((10#$scale_total_body_bytes_raw))
if (( scale_total_body_bytes > 0 && scale_messages == 0 )); then
  echo "FAIL: nonzero scale body bytes require MAILSWIFTSYNC_SCALE_MESSAGES" >&2
  exit 1
fi
disposable_scale="${MAILSWIFTSYNC_DISPOSABLE_SCALE:-0}"
if [[ "$disposable_scale" != 0 && "$disposable_scale" != 1 ]]; then
  echo "FAIL: MAILSWIFTSYNC_DISPOSABLE_SCALE must be 0 or 1" >&2
  exit 1
fi
if [[ "$disposable_scale" == 1 ]] && (( scale_messages == 0 )); then
  echo "FAIL: MAILSWIFTSYNC_DISPOSABLE_SCALE=1 requires a scale message fixture" >&2
  exit 1
fi
disable_dovecot_chroot="${MAILSWIFTSYNC_TEST_DISABLE_DOVECOT_CHROOT:-0}"
if [[ "$disable_dovecot_chroot" != 0 && "$disable_dovecot_chroot" != 1 ]]; then
  echo "FAIL: MAILSWIFTSYNC_TEST_DISABLE_DOVECOT_CHROOT must be 0 or 1" >&2
  exit 1
fi
if [[ "$test_engine" != "ImapSync" && "$test_engine" != "Dovecot" ]]; then
  echo "FAIL: MAILSWIFTSYNC_TEST_ENGINE must be ImapSync or Dovecot" >&2
  exit 1
fi

if [[ -n "${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-}" ]]; then
  mkdir -p -- "$MAILSWIFTSYNC_EVIDENCE_OUTPUT"
fi

required_tools=(dovecot doveadm doveconf mailswiftsync openssl ps timeout)
if [[ "$test_engine" == "ImapSync" ]]; then
  required_tools+=(imapsync)
fi
if (( scale_messages > 0 )); then
  required_tools+=(python3)
fi
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
imapsync_version="not-required"
if [[ "$test_engine" == "ImapSync" ]]; then
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
    echo "FAIL: imapsync product integration requires packaged imapsync 2.314; found ${imapsync_version:-unknown}" >&2
    exit 1
  fi
fi
echo "Using migration engine ${test_engine}, Dovecot ${dovecot_version}, imapsync ${imapsync_version}"
if [[ -n "${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-}" ]]; then
  {
    printf 'engine=%s\n' "$test_engine"
    printf 'dovecot_version=%s\n' "$dovecot_version"
    printf 'imapsync_version=%s\n' "$imapsync_version"
    printf 'dovecot_ssl_min_protocol=TLSv1.2\n'
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
        printf 'exit_status=%s\nengine=%s\ndovecot_version=%s\nimapsync_version=%s\ndovecot_ssl_min_protocol=TLSv1.2\n' \
          "$status" "${test_engine:-unknown}" "${dovecot_version:-unknown}" "${imapsync_version:-unknown}"
        echo '--- bounded process state (PID PPID state wait-channel command) ---'
        ps -eo pid,ppid,stat,wchan:32,comm | head -80 || true
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
  stop_server() {
    local pid="$1"
    local config="$2"
    local side="$3"
    local stop_status=0
    [[ -n "$pid" ]] || return 0
    # Dovecot owns child workers that may still create run-directory state
    # after the master receives SIGTERM. Ask it to shut down cleanly, then
    # wait for the foreground master before removing the fixture tree. The
    # admin command itself is bounded: a wedged control socket must not keep
    # the scale container alive until the workflow-level timeout.
    if [[ -f "$config" ]]; then
      if [[ -n "${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-}" ]]; then
        printf 'begin epoch=%s operation=dovecot-%s-stop\n' \
          "$(date +%s)" "$side" >> "$MAILSWIFTSYNC_EVIDENCE_OUTPUT/scale-progress.txt"
      fi
      timeout --kill-after=2s 10 doveadm -c "$config" stop >/dev/null 2>&1 || stop_status=$?
      if [[ -n "${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-}" ]]; then
        printf 'end epoch=%s operation=dovecot-%s-stop status=%s\n' \
          "$(date +%s)" "$side" "$stop_status" >> "$MAILSWIFTSYNC_EVIDENCE_OUTPUT/scale-progress.txt"
      fi
      if [[ "$stop_status" -ne 0 ]]; then
        echo "WARNING: graceful Dovecot $side shutdown did not complete; terminating the disposable fixture server" >&2
      fi
    fi
    for _ in {1..50}; do
      kill -0 "$pid" 2>/dev/null || break
      sleep 0.1
    done
    if kill -0 "$pid" 2>/dev/null; then
      kill -TERM "$pid" 2>/dev/null || true
      for _ in {1..50}; do
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.1
      done
    fi
    if kill -0 "$pid" 2>/dev/null; then
      kill -KILL "$pid" 2>/dev/null || true
    fi
    wait "$pid" 2>/dev/null || true
  }
  if [[ "$disposable_scale" == 1 ]]; then
    # A 100k-message Dovecot process can remain in uninterruptible filesystem
    # I/O while its Maildir is still mounted. `wait` after SIGKILL cannot be
    # bounded in that state. The caller runs this harness as the container's
    # main process with Docker --init, so let container teardown reap the
    # disposable servers rather than waiting on them here.
    if [[ -n "${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-}" ]]; then
      printf 'dovecot_shutdown=delegated_to_disposable_container_teardown\n' \
        >> "$MAILSWIFTSYNC_EVIDENCE_OUTPUT/scale-progress.txt"
    fi
  else
    stop_server "${destination_pid:-}" "${workspace:-}/destination.conf" destination
    stop_server "${source_pid:-}" "${workspace:-}/source.conf" source
  fi
  if [[ "${MAILSWIFTSYNC_KEEP_LAB:-0}" == "1" ]]; then
    echo "Keeping integration lab workspace: $workspace" >&2
  elif (( scale_messages > 0 )); then
    # The scale lab runs in a disposable container that the host removes after
    # collecting evidence. Recursively unlinking 100k message files inside
    # overlayfs can take longer than the migration itself; leave those files
    # for container teardown instead of blocking qualification completion.
    if [[ -n "${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-}" ]]; then
      printf 'workspace_cleanup=skipped_disposable_scale_container\n' \
        >> "$MAILSWIFTSYNC_EVIDENCE_OUTPUT/scale-progress.txt"
    fi
    echo "Keeping large scale workspace until disposable container teardown" >&2
  else
    local removed=0
    for _ in {1..10}; do
      if rm -rf -- "$workspace"; then
        removed=1
        break
      fi
      sleep 0.1
    done
    if [[ "$removed" != 1 ]]; then
      echo "FAIL: could not remove stopped integration workspace $workspace" >&2
      status=1
    fi
  fi
  if [[ -n "${memory_sampler_pid:-}" ]]; then
    kill "$memory_sampler_pid" 2>/dev/null || true
    wait "$memory_sampler_pid" 2>/dev/null || true
  fi
  if [[ -n "${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-}" ]]; then
    printf 'harness_cleanup_complete=true exit_status=%s\n' "$status" \
      >> "$MAILSWIFTSYNC_EVIDENCE_OUTPUT/scale-progress.txt"
  fi
  return "$status"
}
trap cleanup EXIT

uid="$(id -u)"
gid="$(id -g)"
login_user="$(id -un dovenull 2>/dev/null || id -un)"
internal_user="$(id -un dovecot 2>/dev/null || id -un)"
internal_group="$(id -gn dovecot 2>/dev/null || id -gn)"
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
  local login_chroot_setting=""
  if [[ "$disable_dovecot_chroot" == 1 ]]; then
    # Unprivileged sandboxes cannot chroot; use this only when the entire lab
    # is already isolated by an outer container or user namespace.
    login_chroot_setting="  chroot ="
  fi
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
ssl_min_protocol = TLSv1.2
base_dir = $root/run/
state_dir = $root/run/
protocols = imap
listen = 127.0.0.1
default_login_user = $login_user
default_internal_user = $internal_user
default_internal_group = $internal_group
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
${login_chroot_setting}
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

# imapsync's documented default deliberately skips duplicate source messages
# with identical Message-ID headers, and verification correctly reports that
# as missing mail. Keep this adversarial multiplicity fixture on the native
# Dovecot path; the default-imapsync happy path must use only engine-supported
# clean-transfer inputs.
if [[ "$test_engine" == "Dovecot" ]]; then
# Two distinct messages with the same Message-ID exercise multiplicity-aware
# reconciliation. Their Date headers differ, but Dovecot derives INTERNALDATE
# from the Maildir file time and both bodies have the same size, so the pair is
# identical on (Message-ID, folder, INTERNALDATE, RFC822.SIZE). The verifier
# must match them as a multiset and compare their flags as a multiset, without
# collapsing them into one identity or reporting the second as an extra.
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
fi

# Exercise UID gaps in the real server: create 100 messages in a separate
# mailbox, force Dovecot to assign UIDs, then expunge the first 90.  EXISTS is
# consequently 10 while the surviving UIDs are high (typically 91..100).
mkdir -p "$workspace/source/mail/$user/Maildir/.Sparse"/{cur,new,tmp}
for index in $(seq 1 100); do
  # Dovecot assigns UIDs in Maildir's lexical filename order. Zero-padding
  # keeps that order numeric so expunging UIDs 1..90 leaves fixture IDs 91..100.
  printf -v sparse_filename 'sparse-%03d.eml' "$index"
  cat > "$workspace/source/mail/$user/Maildir/.Sparse/new/$sparse_filename" <<EOF
From: migration-lab@example.test
To: lab@example.test
Subject: sparse UID fixture $index
Message-ID: <mailswiftsync-sparse-$index@example.test>
Date: Tue, 01 Jan 2030 01:00:00 +0000
Content-Type: text/plain; charset=utf-8

Sparse UID fixture message $index.
EOF
done

# Optional end-to-end scale qualification. Keep the ordinary smoke path quick;
# the dedicated scale workflow opts into 100k real RFC822 messages in one
# mailbox and then exercises the same packaged transfer and verifier path.
memory_sampler_pid=""
if (( scale_messages > 0 )) && [[ -n "${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-}" ]]; then
  # Attribute container memory to the controller, imapsync, and Dovecot, and
  # separate process memory from page cache, for the scale evidence.
  python3 "$script_dir/process-memory-sampler.py" \
    "$MAILSWIFTSYNC_EVIDENCE_OUTPUT/process-memory-peaks.txt" 1 &
  memory_sampler_pid=$!
fi

if (( scale_messages > 0 )); then
  echo "Generating $scale_messages scale messages in the source mailbox"
  if (( scale_total_body_bytes > 0 )); then
    python3 "$script_dir/generate-scale-maildir.py" \
      "$workspace/source/mail/$user/Maildir/new" \
      "$scale_messages" "$scale_total_body_bytes"
  else
    for ((index = 1; index <= scale_messages; index++)); do
      printf -v scale_filename 'scale-%06d.eml' "$index"
      printf 'From: scale-lab@example.test\nTo: lab@example.test\nSubject: Scale fixture %d\nMessage-ID: <mailswiftsync-scale-%06d@example.test>\nDate: Tue, 01 Jan 2030 02:00:00 +0000\nContent-Type: text/plain; charset=utf-8\n\nScale fixture message %d for durable transfer and verification.\n' \
        "$index" "$index" "$index" > "$workspace/source/mail/$user/Maildir/new/$scale_filename"
      if (( index % 10000 == 0 )); then
        echo "Generated $index / $scale_messages scale messages"
      fi
    done
  fi
fi

# Select/expunge through Dovecot so the Maildir fixture is tested with actual
# UID assignment rather than relying on filename order.
chown -R "$mail_uid:$mail_gid" "$workspace/source/mail/$user/Maildir"
doveadm -c "$workspace/source.conf" expunge -u "$user" mailbox Sparse uid 1:90
# Give the fixture read, flagged, answered, and custom-keyword state so the
# verifier's independent flag comparison runs against real servers.
# shellcheck disable=SC2016 # literal IMAP flag atoms, not shell expansions
doveadm -c "$workspace/source.conf" flags add -u "$user" \
  '\Seen \Flagged \Answered $MailSwiftSyncLab' \
  mailbox INBOX header Message-ID mailswiftsync-integration-fixture
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
imapsync_path=""
if [[ "$test_engine" == "ImapSync" ]]; then
  imapsync_path="$(command -v imapsync)"
fi
doveadm_path="$(command -v doveadm)"
dovecot_config=""
if [[ "$test_engine" == "Dovecot" ]]; then
  dovecot_config="$workspace/destination.conf"
fi
app_runtime="$XDG_RUNTIME_DIR"
state="$app_runtime/state.db"
diagnostic_dir="$app_runtime/diagnostics"
extra_options="--timeout=30"
if (( scale_messages > 0 )); then
  # Avoid per-message debug output contaminating the measured scale run.
  extra_options="--timeout=30"
fi
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
useuid = false
usecache = false
fastio1 = false
fastio2 = false
allowsizemismatch = false
delete2 = false
extra_options = "$extra_options"
EOF
source_secret="$app_runtime/source.secret"
destination_secret="$app_runtime/destination.secret"
printf '%s' "$password" > "$source_secret"
printf '%s' "$password" > "$destination_secret"
chmod 0600 "$source_secret" "$destination_secret"

run_product() {
  # A broken engine, fixture, or controller must produce a bounded release
  # failure rather than consuming an unattended CI runner indefinitely. Let
  # GNU timeout manage its own process group so engine descendants cannot keep
  # the output pipe open after the MailSwiftSync process times out. Force-kill
  # the group after a short grace period if a command ignores TERM. The explicit
  # large-mailbox path gets a larger, still bounded per-command cap.
  local command_timeout=180
  local operation="$1"
  if [[ "$operation" == "headless" ]]; then
    operation+="/${3:-unknown}"
  fi
  local progress_file=""
  if (( scale_messages > 0 )) && [[ -n "${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-}" ]]; then
    progress_file="$MAILSWIFTSYNC_EVIDENCE_OUTPUT/scale-progress.txt"
    printf 'begin epoch=%s operation=%s\n' "$(date +%s)" "$operation" >> "$progress_file"
  fi
  if (( scale_messages > 0 )); then
    command_timeout=2400
  fi
  # The command output is piped through awk for bounded logging. Bash reports
  # only the last process's status for a background pipeline, so waiting on
  # awk alone would turn a timed-out/failed MailSwiftSync process into a
  # successful harness step. Preserve the timeout command's status through a
  # private side channel while awk drains the output.
  local status_file="$workspace/run-product-status-${BASHPID}-${RANDOM}"
  (
    set +e
    MAILSWIFTSYNC_DEBUG_PROCESS_WAIT=1 timeout --kill-after=15s "$command_timeout" "$binary" "$@"
    command_status=$?
    printf '%s\n' "$command_status" > "$status_file"
    exit "$command_status"
  ) 2>&1 | LC_ALL=C awk -v path="$product_log" '
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
  ' &
  local pipeline_pid=$!
  (
    sleep 60 &
    local sleep_pid=$!
    trap 'kill "$sleep_pid" 2>/dev/null || true; wait "$sleep_pid" 2>/dev/null || true; exit 0' TERM
    wait "$sleep_pid" || exit 0
    if kill -0 "$pipeline_pid" 2>/dev/null; then
      {
        printf '\n--- live thread state after 60 seconds (PID TID PPID PGID SID state wait-channel command) ---\n'
        ps -eLo pid,tid,ppid,pgid,sess,stat,wchan:32,comm | head -160 || true
        while read -r app_pid; do
          [[ -n "$app_pid" ]] || continue
          printf 'MailSwiftSync PID %s child PIDs: ' "$app_pid"
          cat "/proc/$app_pid/task/$app_pid/children" 2>/dev/null || true
        done < <(ps -eo pid,comm | awk '$2 == "mailswiftsync" { print $1 }')
      } >> "$product_log"
      if [[ -n "$progress_file" ]]; then
        {
          printf 'process_snapshot epoch=%s operation=%s (PID TID PPID PGID SID state wait-channel command)\n' \
            "$(date +%s)" "$operation"
          ps -eLo pid,tid,ppid,pgid,sess,stat,wchan:32,comm | head -160 || true
          while read -r app_pid; do
            [[ -n "$app_pid" ]] || continue
            printf 'MailSwiftSync PID %s child PIDs: ' "$app_pid"
            cat "/proc/$app_pid/task/$app_pid/children" 2>/dev/null || true
          done < <(ps -eo pid,comm | awk '$2 == "mailswiftsync" { print $1 }')
        } >> "$progress_file"
      fi
    fi
  ) &
  local monitor_pid=$!
  # Missing or malformed command status is itself a harness failure (for
  # example, the evidence filesystem may have filled before the side-channel
  # write). Never fall back to awk's successful drain status.
  local pipeline_status=125
  wait "$pipeline_pid" || pipeline_status=$?
  if [[ -s "$status_file" ]] && read -r command_status < "$status_file" \
    && [[ "$command_status" =~ ^[0-9]+$ ]]; then
    pipeline_status="$command_status"
  fi
  rm -f -- "$status_file"
  kill "$monitor_pid" 2>/dev/null || true
  wait "$monitor_pid" 2>/dev/null || true
  if [[ -n "$progress_file" ]]; then
    printf 'end epoch=%s operation=%s status=%s\n' \
      "$(date +%s)" "$operation" "$pipeline_status" >> "$progress_file"
  fi
  return "$pipeline_status"
}

# A live command can return a documented nonzero terminal status even when the
# durable mailbox state is the expected reviewable result. Inspect that state
# below instead of letting `set -e` discard the useful diagnostics before the
# harness can distinguish verified_with_exceptions from a real failure.
run_product_live() {
  local status=0
  run_product "$@" || status=$?
  case "$status" in
    0)
      return 0
      ;;
    3|4|5)
      echo "INFO: live command returned documented terminal status $status; validating durable state" >&2
      return 0
      ;;
    *)
      return "$status"
      ;;
  esac
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

run_product_live headless "$state" live --acknowledge-destination-loss \
  --source-secret-file "$source_secret" --destination-secret-file "$destination_secret" \
  --diagnostic-log "$diagnostic_dir"
echo "PASS: packaged MailSwiftSync live migration completed"
if ! run_product status "$state" | grep -Eq '"state": "verified(_with_exceptions)?"'; then
  echo "FAIL: durable ledger did not record a verified terminal state" >&2
  exit 1
fi
echo "PASS: durable ledger records a verified terminal state"

# The ledger must say exactly what was asked of the engine in this pass and
# what verification observed, and the support bundle must expose that only
# as digests and classifications.
support_bundle="$app_runtime/support-bundle.json"
run_product support-bundle "$state" "$support_bundle" >/dev/null
if [[ "$test_engine" == Dovecot ]]; then
  expected_pass_kind='"pass_kind": "dovecot_'
  expected_method='"verification_method": "native_dovecot"'
else
  expected_pass_kind='"pass_kind": "imapsync_sync"'
  expected_method='"verification_method": "(metadata_reconciliation|body_hash)"'
fi
for pattern in "$expected_pass_kind" '"outcome": "completed"' '"command_sha256": "[0-9a-f]{64}"' "$expected_method" '"source_range": "(full_mailbox|resume_state:sha256=[0-9a-f]{64})"'; do
  if ! grep -Eq "$pattern" "$support_bundle"; then
    echo "FAIL: transfer-pass provenance is missing $pattern" >&2
    exit 1
  fi
done
if [[ "$test_engine" != Dovecot ]] && ! grep -Eq '"verified_folders": [1-9]' "$support_bundle"; then
  echo "FAIL: transfer-pass provenance recorded no verified folder snapshots" >&2
  exit 1
fi
if grep -qF -- "$(cat -- "$source_secret")" "$support_bundle" \
  || grep -qF -- "$(cat -- "$destination_secret")" "$support_bundle"; then
  echo "FAIL: support bundle contains a credential" >&2
  exit 1
fi
echo "PASS: durable transfer-pass provenance records the pass, its command digest, and its verification"

if [[ "$test_engine" == Dovecot ]]; then
  # Exercise the exact preservation command path (`doveadm sync -1`) against
  # an already-populated destination. Keep its plan/ledger separate from the
  # initial-mirror proof so the immutable plan snapshot remains meaningful.
  preservation_config="$app_runtime/preservation-config"
  preservation_profile_dir="$preservation_config/mailswiftsync"
  preservation_state="$app_runtime/preservation-state.db"
  mkdir -p "$preservation_profile_dir"
  chmod 0700 "$preservation_config" "$preservation_profile_dir"
  sed 's/^dovecot_strategy = "initial_mirror"$/dovecot_strategy = "destination_already_active"/' \
    "$XDG_CONFIG_HOME/mailswiftsync/profile.toml" \
    > "$preservation_profile_dir/profile.toml"
  chmod 0600 "$preservation_profile_dir/profile.toml"
  if ! grep -q '^dovecot_strategy = "destination_already_active"$' \
      "$preservation_profile_dir/profile.toml"; then
    echo "FAIL: preservation fixture did not select destination_already_active" >&2
    exit 1
  fi
  XDG_CONFIG_HOME="$preservation_config" run_product headless "$preservation_state" preflight \
    --source-secret-file "$source_secret" --destination-secret-file "$destination_secret" \
    --diagnostic-log "$diagnostic_dir"
  XDG_CONFIG_HOME="$preservation_config" run_product_live headless "$preservation_state" live \
    --source-secret-file "$source_secret" --destination-secret-file "$destination_secret" \
    --diagnostic-log "$diagnostic_dir"
  preservation_status="$(XDG_CONFIG_HOME="$preservation_config" run_product status "$preservation_state")"
  if ! grep -Eq '"state": "verified(_with_exceptions)?"' <<<"$preservation_status"; then
    echo "FAIL: Dovecot sync -1 preservation pass did not reach a verified terminal state" >&2
    printf '%s\n' "$preservation_status" >&2
    exit 1
  fi
  echo "PASS: Dovecot sync -1 preservation pass completed against the populated destination"
fi

proof="$app_runtime/customer-proof.json"
run_product customer-proof "$state" "$proof" \
  --source-provider generic_imap --destination-provider generic_imap \
  --source-auth password --destination-auth password --fixture-id packaged-generic-imap \
  --scenario-ids basic-small,idempotent-delta
if [[ "$test_engine" == Dovecot ]]; then
  expected_proof_engine='"engine": "Dovecot native'
else
  expected_proof_engine='"engine": "imapsync 2.314"'
fi
if ! grep -Fq -- "$expected_proof_engine" "$proof"; then
  echo "FAIL: customer proof did not identify the executed engine ($expected_proof_engine)" >&2
  exit 1
fi
run_product verify "$proof"
echo "PASS: packaged customer proof exported and verified"

if [[ "$test_engine" != Dovecot ]]; then
  # Verification-only reconciliation of the completed mailbox: no transfer
  # engine runs, mismatches stream through a private durable stage, and the
  # stage is removed once the evidence is committed.
  reverify_status="$(run_product status "$state")"
  reverify_ids="$(grep -o '"id": "[^"]*"' <<<"$reverify_status" | head -n 2 | sed 's/.*"id": "\([^"]*\)"/\1/')"
  reverify_project="$(sed -n 1p <<<"$reverify_ids")"
  reverify_job="$(sed -n 2p <<<"$reverify_ids")"
  if [[ -z "$reverify_project" || -z "$reverify_job" ]]; then
    echo "FAIL: could not read project and mailbox IDs for reverification" >&2
    exit 1
  fi
  reverify_differences="$app_runtime/reverify-differences.csv"
  reverify_result="$(run_product reverify "$state" "$reverify_project" "$reverify_job" \
    "$source_secret" "$destination_secret" --mode metadata --differences "$reverify_differences")"
  printf '%s\n' "$reverify_result"
  if ! grep -Eq '"missing_messages": ?0([^0-9]|$)' <<<"$reverify_result" \
    || ! grep -Eq '"extra_messages": ?0([^0-9]|$)' <<<"$reverify_result"; then
    echo "FAIL: reverification of the verified mailbox reported differences" >&2
    exit 1
  fi
  if [[ ! -s "$reverify_differences" ]]; then
    echo "FAIL: reverification did not write its differences export" >&2
    exit 1
  fi
  if compgen -G "$(dirname -- "$state")/verification-stages/*-reverify.sqlite*" >/dev/null; then
    echo "FAIL: reverification left its verification stage behind" >&2
    exit 1
  fi
  echo "PASS: verification-only reconciliation streamed through a durable stage and cleaned it up"
fi
if [[ "$test_engine" != Dovecot ]]; then
  if ! grep -Eq '"compared_messages": [1-9]' "$proof" \
    || ! grep -Eq '"mismatched_messages": 0([^0-9]|$)' "$proof"; then
    echo "FAIL: customer proof does not record clean independent flag verification" >&2
    exit 1
  fi
  echo "PASS: independent flag and keyword verification compared messages without differences"
fi
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

run_product_live headless "$state" live --acknowledge-destination-loss \
  --source-secret-file "$source_secret" --destination-secret-file "$destination_secret" \
  --reopen-reason "integration fixture incremental synchronization" \
  --diagnostic-log "$diagnostic_dir"
echo "PASS: packaged MailSwiftSync incremental live migration completed"
if ! run_product status "$state" | grep -Eq '"state": "verified(_with_exceptions)?"'; then
  echo "FAIL: incremental orchestration did not leave a verified terminal state" >&2
  exit 1
fi
echo "PASS: incremental orchestration preserved verified terminal state"

# The first proof certifies the initial mirror. Export a second proof from the
# same ledger after the incremental pass so retained evidence also covers the
# post-delta state rather than only the pre-delta transfer.
post_delta_proof="$app_runtime/customer-proof-post-delta.json"
run_product customer-proof "$state" "$post_delta_proof" \
  --source-provider generic_imap --destination-provider generic_imap \
  --source-auth password --destination-auth password --fixture-id packaged-generic-imap \
  --scenario-ids idempotent-delta
run_product verify "$post_delta_proof"
if ! grep -Fq -- "$expected_proof_engine" "$post_delta_proof"; then
  echo "FAIL: post-delta customer proof did not identify the transfer engine ($expected_proof_engine)" >&2
  exit 1
fi
echo "PASS: post-delta customer proof exported and verified"
if [[ -n "${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-}" ]]; then
  cp -- "$post_delta_proof" "$MAILSWIFTSYNC_EVIDENCE_OUTPUT/customer-proof-post-delta.json"
fi

destination_maildir="$workspace/destination/mail/$user/Maildir"
destination_message_ids=""
fixture_message_ids_file="$workspace/destination-fixture-message-ids.txt"
maildir_count_matching() {
  local pattern="$1"
  local matches
  if (( scale_messages > 0 )); then
    grep -F -c -- "$pattern" "$fixture_message_ids_file" || true
    return
  fi
  matches="$(find "$destination_maildir" -type f \
    \( -path '*/cur/*' -o -path '*/new/*' \) -print0 |
    xargs -0 -r grep -F -l -- "$pattern" || true)"
  if [[ -z "$matches" ]]; then
    printf '0'
  else
    printf '%s\n' "$matches" | wc -l | tr -d '[:space:]'
  fi
}
if [[ "$test_engine" == Dovecot ]]; then
  # The native-engine fixture uses mdbox to support exact backup semantics;
  # inspect stored messages through Dovecot rather than assuming Maildir files.
  destination_message_ids="$(doveadm -c "$workspace/destination.conf" fetch -u "$user" 'hdr.message-id' mailbox '*')"
  destination_messages="$(grep -c '^hdr.message-id:' <<<"$destination_message_ids" || true)"
  {
    printf 'Integration assertion: Dovecot destination message count=%s\n' "$destination_messages"
    printf '%s\n' "$destination_message_ids"
  } >> "$product_log"
else
  destination_messages="$(find "$destination_maildir" -type f \( -path '*/cur/*' -o -path '*/new/*' \) | wc -l)"
  if (( scale_messages > 0 )); then
    scan_status=0
    if [[ -n "${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-}" ]]; then
      printf 'begin epoch=%s operation=fixture-message-id-scan\n' "$(date +%s)" \
        >> "$MAILSWIFTSYNC_EVIDENCE_OUTPUT/scale-progress.txt"
    fi
    python3 "$script_dir/maildir-fixture-scan.py" "$destination_maildir" \
      > "$fixture_message_ids_file" || scan_status=$?
    if [[ -n "${MAILSWIFTSYNC_EVIDENCE_OUTPUT:-}" ]]; then
      printf 'end epoch=%s operation=fixture-message-id-scan status=%s\n' \
        "$(date +%s)" "$scan_status" >> "$MAILSWIFTSYNC_EVIDENCE_OUTPUT/scale-progress.txt"
    fi
    if [[ "$scan_status" -ne 0 ]]; then
      echo "FAIL: could not scan Maildir fixture Message-ID headers" >&2
      exit "$scan_status"
    fi
  fi
fi
destination_has_message_id() {
  local message_id="$1"
  if [[ "$test_engine" == Dovecot ]]; then
    grep -F -q -- "$message_id" <<<"$destination_message_ids"
  elif (( scale_messages > 0 )); then
    grep -F -q -- "$message_id" "$fixture_message_ids_file"
  else
    [[ "$(maildir_count_matching "Message-ID: <$message_id>")" -gt 0 ]]
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
expected_fixture_messages=15
if [[ "$test_engine" == "Dovecot" ]]; then
  expected_fixture_messages=17
fi
if [[ "$destination_messages" -lt "$expected_fixture_messages" ]] || ! destination_has_message_id "mailswiftsync-incremental-fixture@example.test"; then
  echo "FAIL: destination is missing the incremental fixture Message-ID" >&2
  exit 1
fi
if (( scale_messages > 0 && destination_messages < scale_messages + expected_fixture_messages )); then
  echo "FAIL: destination retained $destination_messages messages; expected at least $((scale_messages + expected_fixture_messages)) including scale fixtures" >&2
  exit 1
fi
if (( scale_messages > 0 )); then
  echo "PASS: destination retained at least $scale_messages scale messages in the single-mailbox run"
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
for index in 91 100; do
  if ! destination_has_message_id "mailswiftsync-sparse-$index@example.test"; then
    echo "FAIL: destination is missing sparse UID fixture message $index" >&2
    exit 1
  fi
done
if [[ "$test_engine" == Dovecot ]]; then
  sparse_count="$(grep -c '^hdr.message-id: <mailswiftsync-sparse-' <<<"$destination_message_ids" || true)"
else
  sparse_count="$(maildir_count_matching "Message-ID: <mailswiftsync-sparse-")"
fi
printf 'Integration assertion: sparse UID message count=%s\n' "$sparse_count" >> "$product_log"
if [[ "$sparse_count" -ne 10 ]]; then
  echo "FAIL: destination retained $sparse_count sparse UID messages; expected 10" >&2
  exit 1
fi
echo "PASS: destination retained sparse-UID fixture endpoints"
if [[ "$test_engine" == Dovecot ]]; then
  duplicate_count="$(grep -c '^hdr.message-id: <mailswiftsync-duplicate@example.test>' <<<"$destination_message_ids" || true)"
  printf 'Integration assertion: duplicate Message-ID count=%s\n' "$duplicate_count" >> "$product_log"
  if [[ "$duplicate_count" -ne 2 ]]; then
    echo "FAIL: destination retained $duplicate_count duplicate-ID messages; expected 2" >&2
    exit 1
  fi
fi
echo "PASS: destination retained literal, non-UTF-8, renamed special-use, and sparse-UID fixtures"
echo "PASS: MailSwiftSync product integration verified $destination_messages message(s) through ${test_engine}"

# Real-engine interruption drill: kill the controller mid-transfer, recover the
# ledger, and resume. With the default header-matching plan the resumed run
# must hold exactly the source messages, verified. With expert --useuid
# --usecache, imapsync trusts only its UID cache, so the message in flight when
# the controller died can be appended twice; that run must surface the
# difference rather than report a silently verified result.
run_interruption_drill() {
  local label="$1" useuid="$2" strict="$3" restore="${4:-}"
  local drill_messages="${5:-60}" payload_bytes="${6:-0}"
  local max_bytes_per_second="${7:-0}"
  local drill_user="drill-$label@example.test"
  local name index drill_filename
  if (( payload_bytes > 0 && max_bytes_per_second == 0 )); then
    echo "FAIL: interruption drill ($label) needs a byte throttle to observe a partial APPEND" >&2
    exit 1
  fi
  for name in source destination; do
    mkdir -p "$workspace/$name/mail/$drill_user/Maildir"/{cur,new,tmp}
    printf '%s:{PLAIN}%s:%s:%s::%s::\n' "$drill_user" "$password" "$mail_uid" "$mail_gid" \
      "$workspace/$name/mail/$drill_user" >> "$workspace/$name/passwd"
  done
  for ((index = 1; index <= drill_messages; index++)); do
    printf -v drill_filename 'drill-%03d.eml' "$index"
    local drill_path="$workspace/source/mail/$drill_user/Maildir/new/$drill_filename"
    printf 'From: drill-lab@example.test\nTo: %s\nSubject: Interruption drill %d\nMessage-ID: <mailswiftsync-drill-%s-%03d@example.test>\nDate: Tue, 01 Jan 2030 03:00:00 +0000\nContent-Type: text/plain; charset=utf-8\n\nInterruption drill message %d.\n' \
      "$drill_user" "$index" "$label" "$index" "$index" \
      > "$drill_path"
    if (( payload_bytes > 0 )); then
      head -c "$payload_bytes" /dev/zero | tr '\000' 'A' >> "$drill_path"
    fi
  done
  for name in source destination; do
    chown -R "$mail_uid:$mail_gid" "$workspace/$name/mail/$drill_user"
    if id dovecot >/dev/null 2>&1; then
      chmod 0750 "$workspace/$name/mail/$drill_user"
    else
      chmod 0755 "$workspace/$name/mail/$drill_user"
    fi
  done

  local drill_config="$app_runtime/drill-$label-config"
  local drill_state="$app_runtime/drill-$label-state.db"
  mkdir -p "$drill_config/mailswiftsync"
  chmod 0700 "$drill_config" "$drill_config/mailswiftsync"
  printf 'profile = "lab"\n' > "$drill_config/mailswiftsync/organization-policy.toml"
  chmod 0600 "$drill_config/mailswiftsync/organization-policy.toml"
  # Throttle the engine so the transfer is still in flight when it is killed.
  sed -e "s/^name = .*/name = \"Interruption drill $label\"/" \
    -e "s/^source_user = .*/source_user = \"$drill_user\"/" \
    -e "s/^destination_user = .*/destination_user = \"$drill_user\"/" \
    -e "s/^max_messages_per_second = .*/max_messages_per_second = 2/" \
    -e "s/^max_bytes_per_second = .*/max_bytes_per_second = $max_bytes_per_second/" \
    -e "s/^useuid = .*/useuid = $useuid/" \
    -e "s/^usecache = .*/usecache = $useuid/" \
    "$XDG_CONFIG_HOME/mailswiftsync/profile.toml" > "$drill_config/mailswiftsync/profile.toml"
  chmod 0600 "$drill_config/mailswiftsync/profile.toml"

  local drill_destination="$workspace/destination/mail/$drill_user/Maildir"
  drill_count() {
    find "$drill_destination" -type f \( -path '*/cur/*' -o -path '*/new/*' \) | wc -l | tr -d '[:space:]'
  }
  local drill_log="$app_runtime/drill-$label-live.log"
  XDG_CONFIG_HOME="$drill_config" "$binary" headless "$drill_state" live \
    --acknowledge-destination-loss \
    --source-secret-file "$source_secret" --destination-secret-file "$destination_secret" \
    > "$drill_log" 2>&1 &
  local drill_pid=$!
  local drill_deadline=$((SECONDS + 120))
  if (( payload_bytes > 0 )); then
    # Maildir APPEND writes to tmp until the full literal is accepted, then
    # atomically moves it to new/cur. The byte throttle keeps this partial
    # file observable long enough to kill the controller inside APPEND.
    local partial_append_bytes expected_message_bytes
    local message_path="$workspace/source/mail/$drill_user/Maildir/new/drill-001.eml"
    expected_message_bytes=$(stat -c '%s' "$message_path")
    partial_append_bytes=0
    while (( partial_append_bytes <= 1048576 || partial_append_bytes >= expected_message_bytes )); do
      if ! kill -0 "$drill_pid" 2>/dev/null; then
        echo "FAIL: interruption drill ($label) finished before a partial APPEND was observed" >&2
        tail -n 40 "$drill_log" >&2
        exit 1
      fi
      if (( SECONDS > drill_deadline )); then
        echo "FAIL: interruption drill ($label) never exposed a partial destination APPEND" >&2
        find "$drill_destination/tmp" -maxdepth 1 -type f -printf '%s bytes %p\n' >&2 || true
        tail -n 40 "$drill_log" >&2
        exit 1
      fi
      partial_append_bytes=0
      while IFS= read -r file_bytes; do
        if (( file_bytes > partial_append_bytes )); then
          partial_append_bytes=$file_bytes
        fi
      done < <(find "$drill_destination/tmp" -maxdepth 1 -type f -printf '%s\n')
      sleep 0.1
    done
    echo "Interruption drill ($label): killing during APPEND at $partial_append_bytes of $expected_message_bytes bytes"
  else
    while (( $(drill_count) < 5 )); do
      if ! kill -0 "$drill_pid" 2>/dev/null; then
        echo "FAIL: interruption drill ($label) finished before it could be interrupted" >&2
        tail -n 40 "$drill_log" >&2
        exit 1
      fi
      if (( SECONDS > drill_deadline )); then
        echo "FAIL: interruption drill ($label) transfer never started" >&2
        tail -n 40 "$drill_log" >&2
        exit 1
      fi
      sleep 0.2
    done
  fi
  kill -KILL "$drill_pid"
  wait "$drill_pid" 2>/dev/null || true
  local copied_before_kill
  copied_before_kill="$(drill_count)"
  echo "Interruption drill ($label): controller killed after $copied_before_kill of $drill_messages messages"
  # The engine must not outlive its controller and keep writing unsupervised.
  local engine_deadline=$((SECONDS + 20))
  while pgrep -f "imapsync.*$drill_user" >/dev/null; do
    if (( SECONDS > engine_deadline )); then
      echo "FAIL: imapsync kept running after its controller was killed ($label)" >&2
      pgrep -af "imapsync.*$drill_user" >&2 || true
      exit 1
    fi
    sleep 0.5
  done
  echo "PASS: the engine stopped with its killed controller ($label)"

  XDG_CONFIG_HOME="$drill_config" run_product recover "$drill_state"
  if [[ "$restore" == restore ]]; then
    # Resume from a ledger restored out of a post-crash backup rather than
    # the original file, as after losing the host's state volume.
    local drill_backup="$app_runtime/drill-$label-backup.db"
    XDG_CONFIG_HOME="$drill_config" run_product backup "$drill_state" "$drill_backup"
    drill_state="$app_runtime/drill-$label-restored.db"
    XDG_CONFIG_HOME="$drill_config" run_product restore "$drill_backup" "$drill_state"
    echo "PASS: interrupted drill ledger ($label) backed up and restored for resume"
  fi
  XDG_CONFIG_HOME="$drill_config" run_product_live headless "$drill_state" live \
    --acknowledge-destination-loss \
    --source-secret-file "$source_secret" --destination-secret-file "$destination_secret" \
    --diagnostic-log "$diagnostic_dir"
  local drill_status drill_final
  drill_status="$(XDG_CONFIG_HOME="$drill_config" run_product status "$drill_state")"
  drill_final="$(drill_count)"
  printf 'Integration assertion: interruption drill (%s) destination count=%s (killed at %s)\n' \
    "$label" "$drill_final" "$copied_before_kill" >> "$product_log"
  if [[ "$drill_final" -eq "$drill_messages" ]]; then
    if ! grep -q '"state": "verified"' <<<"$drill_status"; then
      echo "FAIL: resumed drill ($label) copied every message but did not verify" >&2
      printf '%s\n' "$drill_status" >&2
      exit 1
    fi
    echo "PASS: interrupted migration ($label) resumed to exactly $drill_messages verified messages"
  elif [[ "$strict" == strict ]] || grep -q '"state": "verified"' <<<"$drill_status"; then
    echo "FAIL: resumed drill ($label) holds $drill_final of $drill_messages messages (state below)" >&2
    printf '%s\n' "$drill_status" >&2
    exit 1
  else
    echo "PASS: resumed drill ($label) holds $drill_final of $drill_messages messages and verification flagged the difference"
  fi
}

if [[ "$test_engine" != Dovecot ]] && (( scale_messages == 0 )); then
  run_interruption_drill defaults false strict restore
  # The configured byte rate is an average run-level target, not in-flight
  # socket shaping. Kill only after observing a genuinely partial APPEND file.
  run_interruption_drill large-append false strict "" 1 67108864 1048576
  run_interruption_drill uid-cache true flagged
fi
