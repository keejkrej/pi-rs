FROM rust:1-bookworm AS builder
WORKDIR /work
# Build against an ephemeral repository mount. Only outputs staged under
# /opt/evaluator persist; build artifacts outside /opt/evaluator are discarded.
RUN --mount=type=bind,source=.,target=/repo,rw \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/work/target \
    cargo build --manifest-path /repo/Cargo.toml --target-dir /work/target --locked --release -p pi-evals --bin pi-evals \
    && mkdir -p /opt/evaluator/root/bin /opt/evaluator/install \
    && cp /work/target/release/pi-evals /opt/evaluator/root/bin/pi-evals \
    && cp -R /repo/crates/pi-coding-agent/package /opt/evaluator/install/package

# Both variants use the same compiled runner. The control removes only the
# coding-agent documentation whose effect the experiment measures.
FROM builder AS without-docs-install
RUN rm -rf \
    /opt/evaluator/install/package/README.md \
    /opt/evaluator/install/package/CHANGELOG.md \
    /opt/evaluator/install/package/docs \
    /opt/evaluator/install/package/examples

FROM debian:bookworm-slim AS evaluator
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /opt/evaluator/root /repo
WORKDIR /repo
# The container starts as root so entrypoint.rs can validate the root-only binary
# and prepare secrets before permanently dropping to UID 65532 per task.
RUN chown -R 0:0 /repo \
    && chmod 0755 /repo /repo/bin \
    && chmod 0700 /repo/bin/pi-evals \
    && mkdir -p /repo/package /run/pi-eval-secrets \
    && chmod 0755 /repo/package \
    && chmod 0700 /run/pi-eval-secrets
ENV PI_PACKAGE_DIR=/repo/package
ENTRYPOINT ["/repo/bin/pi-evals", "--entrypoint"]

FROM evaluator AS without_docs
COPY --from=without-docs-install /opt/evaluator/install/package /repo/package
ENV PI_EVAL_VARIANT=without_docs

FROM evaluator AS with_docs
COPY --from=builder /opt/evaluator/install/package /repo/package
ENV PI_EVAL_VARIANT=with_docs
