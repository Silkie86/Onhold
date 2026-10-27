# One image, two roles: `onhold api` and `onhold worker` (plus `onhold migrate`).
# Build from the repository root: docker build -f infra/Dockerfile .

FROM rust:1.99.1-slim-bookworm AS builder
WORKDIR /src
ENV SQLX_OFFLINE=true
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --bin onhold \
    && cp target/release/onhold /usr/local/bin/onhold

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home onhold
COPY --from=builder /usr/local/bin/onhold /usr/local/bin/onhold
USER onhold
EXPOSE 8080 8081
ENTRYPOINT ["onhold"]
CMD ["api"]
