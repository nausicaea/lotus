FROM docker.io/clux/muslrust:stable AS build
WORKDIR /volume
COPY Cargo.toml Cargo.lock .
RUN cargo build --release || true
COPY logstash ./logstash/
COPY src ./src/
RUN cargo build --release

FROM scratch

COPY --from=build /volume/target/x86_64-unknown-linux-musl/release/lotus /bin/lotus
VOLUME ["/volume", "/var/run/docker.sock", "/.cache/lotus"]
WORKDIR /volume
ENV RUST_BACKTRACE=1
ENTRYPOINT ["/bin/lotus"]
