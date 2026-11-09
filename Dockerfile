# syntax=docker/dockerfile:1.7
# Boba Agent: one image with the server, the web UI, git, the GitHub CLI and Claude Code.

# ---- web UI
FROM node:22-bookworm-slim AS web
WORKDIR /src/web
RUN corepack enable
COPY web/package.json web/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY web/ ./
RUN pnpm build

# ---- server (embeds web/dist)
FROM rust:1-bookworm AS server
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY crates crates
COPY --from=web /src/web/dist web/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p boba-server && cp target/release/boba-agent /usr/local/bin/boba-agent

# ---- runtime
FROM debian:bookworm-slim
ARG UID=1000
ARG GID=1000
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl git gnupg openssh-client tini \
    && mkdir -p /etc/apt/keyrings \
    && curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg -o /etc/apt/keyrings/githubcli.gpg \
    && echo "deb [signed-by=/etc/apt/keyrings/githubcli.gpg] https://cli.github.com/packages stable main" > /etc/apt/sources.list.d/github-cli.list \
    && curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends gh nodejs \
    && npm install -g @anthropic-ai/claude-code \
    && npm cache clean --force && rm -rf /var/lib/apt/lists/*
RUN groupadd -g ${GID} boba && useradd -m -u ${UID} -g ${GID} -s /bin/bash boba
COPY --from=server /usr/local/bin/boba-agent /usr/local/bin/boba-agent
USER boba
WORKDIR /home/boba
ENV BOBA_DATA_DIR=/data BOBA_PORT=7777
EXPOSE 7777
HEALTHCHECK --interval=30s --timeout=5s CMD curl -fsS http://127.0.0.1:7777/api/health || exit 1
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["boba-agent"]
