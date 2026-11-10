# Build: a fully static release binary (musl), with the templates baked in.
# cargo-auditable records the dependency list inside the binary so that image
# scanners such as Trivy can check the crates it was built from.
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev && cargo install cargo-auditable --locked
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src src
COPY templates templates
RUN cargo auditable build --release --locked && mkdir /data

# Run: an empty image. No operating system packages, shell or package manager;
# only the binary and its static files, as a non-root user.
FROM scratch
COPY --from=build /app/target/release/chirper-rs /app/chirper-rs
COPY --from=build --chown=65532:65532 /data /data
COPY static /app/static
WORKDIR /app
USER 65532:65532
ENV BIND_ADDR=0.0.0.0:8080 \
    DATABASE_PATH=/data/database.sqlite
EXPOSE 8080
ENTRYPOINT ["/app/chirper-rs"]
