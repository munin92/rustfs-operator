FROM ghcr.io/openprojectx/dockerhub/library/rust:1.97.0-trixie AS builder
WORKDIR /build

# Dependency layer: build all dependencies against dummy sources so the
# layer is keyed on Cargo.toml only and survives source-code changes
# (persisted across CI runs by the buildx GHA cache).
COPY Cargo.toml ./
RUN mkdir src \
    && echo 'fn main() {}' > src/main.rs \
    && touch src/lib.rs \
    && cargo build --release --bin rustfs-operator \
    && rm -rf src

COPY src ./src
# touch so cargo rebuilds the crate itself against the real sources
RUN touch src/main.rs src/lib.rs \
    && cargo build --release --bin rustfs-operator

FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 65532 nonroot
COPY --from=builder /build/target/release/rustfs-operator /usr/local/bin/rustfs-operator
USER 65532
ENTRYPOINT ["/usr/local/bin/rustfs-operator"]
CMD ["run"]
