# Reproducible Linux headless image. The desktop controller remains available
# in the binary, but this image deliberately defaults to the CLI help instead
# of attempting to open a display server.

FROM debian:bookworm-slim AS imapsync-package

# Debian Bookworm does not ship imapsync in its configured repositories. Pin
# the upstream Debian artifact and verify it before it enters the runtime
# image; update both values deliberately when changing the engine version.
ARG IMAPSYNC_VERSION=2.314
ARG IMAPSYNC_SHA256=e8b9b410ac763749cd05568e51ab7a158c041b1ef78ca559f47dc1fa377d0870
RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && curl --fail --location --proto '=https' --tlsv1.2 \
        "https://imapsync.lamiral.info/dist2/imapsync-${IMAPSYNC_VERSION}.deb" \
        --output /tmp/imapsync.deb \
    && echo "${IMAPSYNC_SHA256}  /tmp/imapsync.deb" | sha256sum --check

FROM rust:1.92-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
RUN cargo build --locked --release

FROM debian:bookworm-slim

ARG DOVECOT_VERSION=1:2.3.19.1+dfsg1-2.1+deb12u6

RUN apt-get update \
    && apt-get install --no-install-recommends -y \
        ca-certificates \
        openssl \
        "dovecot-core=${DOVECOT_VERSION}" \
        "dovecot-imapd=${DOVECOT_VERSION}" \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 --shell /usr/sbin/nologin mailswiftsync \
    && install -d -o mailswiftsync -g mailswiftsync -m 0700 /var/lib/mailswiftsync /run/user/10001 \
    && install -d -m 0755 /usr/local/lib/mailswiftsync

COPY --from=imapsync-package /tmp/imapsync.deb /tmp/imapsync.deb
RUN apt-get update \
    && apt-get install --no-install-recommends -y /tmp/imapsync.deb \
    && rm -f /tmp/imapsync.deb \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/mailswiftsync /usr/local/bin/mailswiftsync
COPY scripts/imap-integration-smoke.sh /usr/local/lib/mailswiftsync/imap-integration-smoke.sh
COPY scripts/controller-recovery-smoke.sh /usr/local/lib/mailswiftsync/controller-recovery-smoke.sh
COPY scripts/controller-chaos-smoke.sh /usr/local/lib/mailswiftsync/controller-chaos-smoke.sh
COPY scripts/engine-storage-fault-smoke.sh /usr/local/lib/mailswiftsync/engine-storage-fault-smoke.sh
RUN chmod 0755 /usr/local/bin/mailswiftsync
RUN chmod 0755 /usr/local/lib/mailswiftsync/imap-integration-smoke.sh
RUN chmod 0755 /usr/local/lib/mailswiftsync/controller-recovery-smoke.sh
RUN chmod 0755 /usr/local/lib/mailswiftsync/controller-chaos-smoke.sh
RUN chmod 0755 /usr/local/lib/mailswiftsync/engine-storage-fault-smoke.sh

ENV MAILSWIFTSYNC_STATE_PATH=/var/lib/mailswiftsync/state.db \
    XDG_RUNTIME_DIR=/run/user/10001

VOLUME ["/var/lib/mailswiftsync", "/run/user/10001"]
USER mailswiftsync
WORKDIR /var/lib/mailswiftsync
ENTRYPOINT ["/usr/local/bin/mailswiftsync"]
CMD ["--help"]
