#!/usr/bin/env bash
set -euo pipefail

# Exercise the exact IO::Socket::SSL shipped in the runtime image. A forced
# legacy client first proves each TLS 1.0/1.1 fixture is reachable; the product
# protocol floor must then reject it. A TLS 1.2-only fixture proves the same
# floor still permits the supported minimum. Certificate validation is
# intentionally disabled here: the packaged product's certificate policy is
# exercised separately by the real imapsync integration workflow.

required_tools=(openssl perl python3 timeout)
missing=()
for tool in "${required_tools[@]}"; do
  command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
done
if ((${#missing[@]} > 0)); then
  echo "FAIL: TLS floor smoke test is missing required tools: ${missing[*]}" >&2
  exit 1
fi

printf 'OpenSSL: '
openssl version
perl -MIO::Socket::SSL -e 'print "IO::Socket::SSL $IO::Socket::SSL::VERSION\n"'

test_dir="$(mktemp -d "${TMPDIR:-/tmp}/mss-tls-floor.XXXXXX")"
server_pid=""
server_port=""
cleanup() {
  if [[ -n "$server_pid" ]]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  for filename in key.pem cert.pem; do
    if [[ -f "$test_dir/$filename" ]]; then
      unlink "$test_dir/$filename"
    fi
  done
  rmdir "$test_dir"
}
trap cleanup EXIT

openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout "$test_dir/key.pem" -out "$test_dir/cert.pem" \
  -subj /CN=localhost -days 1 >/dev/null 2>&1

next_loopback_port() {
  python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()'
}

start_server() {
  local protocol="$1"
  if [[ -n "$server_pid" ]]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
    server_pid=""
  fi
  server_port="$(next_loopback_port)"
  openssl s_server -quiet -accept "127.0.0.1:$server_port" \
    -cert "$test_dir/cert.pem" -key "$test_dir/key.pem" \
    "-$protocol" -cipher 'DEFAULT:@SECLEVEL=0' \
    >/dev/null 2>&1 &
  server_pid=$!
  sleep 0.25
  if ! kill -0 "$server_pid" 2>/dev/null; then
    echo "FAIL: OpenSSL could not start the $protocol fixture server" >&2
    return 1
  fi
}

connect_with_version() {
  local port="$1"
  local version="$2"
  # Perl's single-quoted program intentionally leaves @ARGV for Perl.
  # shellcheck disable=SC2016
  timeout --kill-after=1s 5s perl -MIO::Socket::SSL -e '
    IO::Socket::SSL->new(
      PeerAddr => "127.0.0.1",
      PeerPort => $ARGV[0],
      SSL_verify_mode => 0,
      SSL_version => $ARGV[1],
      SSL_cipher_list => "DEFAULT:\@SECLEVEL=0",
      Timeout => 3,
    ) or die IO::Socket::SSL::errstr();
  ' "$port" "$version"
}

assert_legacy_rejected() {
  local protocol="$1"
  local forced_version="$2"
  start_server "$protocol"
  if ! connect_with_version "$server_port" "$forced_version"; then
    echo "FAIL: forced $protocol baseline could not reach its test server" >&2
    return 1
  fi
  if connect_with_version "$server_port" 'SSLv23:!TLSv1:!TLSv1_1:!SSLv3:!SSLv2' \
    >/dev/null 2>&1; then
    echo "FAIL: configured TLS 1.2+ floor accepted $protocol" >&2
    return 1
  fi
  echo "PASS: TLS 1.2+ floor rejected $protocol after a successful forced-version baseline"
}

assert_legacy_rejected tls1 TLSv1
assert_legacy_rejected tls1_1 TLSv1_1

start_server tls1_2
connect_with_version "$server_port" 'SSLv23:!TLSv1:!TLSv1_1:!SSLv3:!SSLv2'
echo 'PASS: TLS 1.2+ floor connected to a TLS 1.2-only server'
