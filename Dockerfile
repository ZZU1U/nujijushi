FROM node:24-bookworm-slim AS web
WORKDIR /app/webui
COPY webui/package.json webui/package-lock.json ./
RUN npm ci
COPY webui/index.html webui/tsconfig.json webui/vite.config.ts ./
COPY webui/src ./src
RUN npm run build

FROM rust:1.95.0-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock rust-toolchain.toml build.rs ./
COPY src ./src
COPY crates ./crates
COPY migrations ./migrations
COPY templates ./templates
COPY --from=web /app/webui/dist ./webui/dist
RUN NUJIJUSHI_REQUIRE_WEBUI=1 cargo build --release --locked

FROM debian:bookworm-slim
ARG GALLERY_DL_VERSION=1.32.21
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates ffmpeg python3-venv tini tzdata \
    && rm -rf /var/lib/apt/lists/* \
    && python3 -m venv /opt/gallery-dl \
    && /opt/gallery-dl/bin/pip install --no-cache-dir "gallery-dl==${GALLERY_DL_VERSION}" PySocks==1.7.1 \
    && groupadd --gid 10001 nujijushi \
    && useradd --uid 10001 --gid 10001 --home-dir /data --no-create-home nujijushi \
    && install -d -m 0700 -o nujijushi -g nujijushi /data

COPY --from=build /app/target/release/nujijushi /usr/local/bin/nujijushi
COPY --from=build /app/crates/nujijushi-sources/src/twitter/LICENSE-signing /usr/share/doc/nujijushi/LICENSE-signing
ENV HOME=/data \
    NUJIJUSHI_HOME=/data \
    NUJIJUSHI_BIND=0.0.0.0:8788 \
    PATH="/opt/gallery-dl/bin:${PATH}"
WORKDIR /data
USER 10001:10001
EXPOSE 8788
STOPSIGNAL SIGINT
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/nujijushi"]
CMD ["run"]
