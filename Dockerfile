# Reproducible Linux headless image. The desktop controller remains available
# in the binary, but this image deliberately defaults to the CLI help instead
# of attempting to open a display server.
FROM rust:1.92-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
RUN cargo build --locked --release

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates imapsync dovecot-core \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 --shell /usr/sbin/nologin mailswiftsync \
    && install -d -o mailswiftsync -g mailswiftsync -m 0700 /var/lib/mailswiftsync /run/user/10001

COPY --from=builder /build/target/release/mailswiftsync /usr/local/bin/mailswiftsync
RUN chmod 0755 /usr/local/bin/mailswiftsync

ENV MAILSWIFTSYNC_STATE_PATH=/var/lib/mailswiftsync/state.db \
    XDG_RUNTIME_DIR=/run/user/10001

VOLUME ["/var/lib/mailswiftsync", "/run/user/10001"]
USER mailswiftsync
WORKDIR /var/lib/mailswiftsync
ENTRYPOINT ["/usr/local/bin/mailswiftsync"]
CMD ["--help"]
