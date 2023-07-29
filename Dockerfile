FROM docker.io/clux/muslrust:stable AS build
WORKDIR /volume
COPY Cargo.toml Cargo.lock .
RUN cargo build --release || true
COPY logstash ./logstash/
COPY src ./src/
RUN cargo build --release

FROM docker.io/library/debian:bookworm-slim
COPY --from=build /volume/target/x86_64-unknown-linux-musl/release/lotus /bin/lotus
VOLUME ["/volume", "/docker.sock", "/.cache"]
WORKDIR /volume
ENV RUST_BACKTRACE=1
ENV DOCKER_HOST="unix://docker.sock"
ENTRYPOINT ["/bin/lotus"]
