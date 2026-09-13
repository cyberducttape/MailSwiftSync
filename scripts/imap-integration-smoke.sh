#!/usr/bin/env bash
set -euo pipefail

# Reproducible local engine lab for the supported generic-IMAP path. This
# intentionally exercises real Dovecot servers and the real imapsync binary;
# it does not replace controller crash/restart tests, which require a
# headless supervisor and remain a release blocker.

if ! command -v dovecot >/dev/null 2>&1 || ! command -v imapsync >/dev/null 2>&1; then
  echo "SKIP: install dovecot and imapsync to run the IMAP integration lab" >&2
  exit 77
fi

dovecot_version="$(dovecot --version 2>/dev/null || true)"
if [[ "$dovecot_version" != 2.4.* ]]; then
  echo "SKIP: this fixture currently targets Dovecot 2.4.x (found: ${dovecot_version:-unknown})" >&2
  exit 77
fi

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

start_server() {
  local name="$1" port="$2"
  local root="$workspace/$name"
  local config="$workspace/$name.conf"
  mkdir -p "$root/mail/$user/Maildir"/{cur,new,tmp} "$root/run" "$root/log"
  printf '%s:{PLAIN}%s:%s:%s::%s::\n' "$user" "$password" "$mail_uid" "$mail_gid" "$root/mail/$user" > "$root/passwd"
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
dovecot_config_version = 2.4.5
dovecot_storage_version = 2.4.5
base_dir = $root/run/
state_dir = $root/run/
protocols = imap
listen = 127.0.0.1
log_path = $root/log/dovecot.log
info_log_path = $root/log/dovecot-info.log
ssl = no
auth_allow_cleartext = yes
auth_mechanisms = plain login
auth_verbose = yes
first_valid_uid = 1
mail_home = $root/mail/%u
mail_driver = maildir
mail_path = ~/Maildir
passdb passwd-file {
  passwd_file_path = $root/passwd
}
userdb passwd-file {
  passwd_file_path = $root/passwd
}
service imap-login {
  inet_listener imap {
    port = $port
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

imapsync \
  --host1 127.0.0.1 --port1 "$source_port" --user1 "$user" --password1 "$password" --notls1 \
  --host2 127.0.0.1 --port2 "$destination_port" --user2 "$user" --password2 "$password" --notls2 \
  --automap --nolog

destination_messages="$(find "$workspace/destination/mail/$user/Maildir" -type f \( -path '*/cur/*' -o -path '*/new/*' \) | wc -l)"
if [[ "$destination_messages" -lt 1 ]]; then
  echo "FAIL: imapsync reported success but destination Maildir has no fixture message" >&2
  exit 1
fi
echo "PASS: real Dovecot-to-Dovecot IMAP transfer copied $destination_messages message(s)"
