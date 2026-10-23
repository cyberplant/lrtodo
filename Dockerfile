# syntax=docker/dockerfile:1
# Imagen multi-arch de lrtodo (también la usa el add-on de Home Assistant).
#   docker buildx build --platform linux/amd64,linux/arm64 -t lrtodo .
# Todo se compila en la plataforma del builder y se cruza al target con zig.

ARG RUST_VERSION=1.99
ARG NODE_VERSION=24

# --- core en WASM para la PWA
FROM --platform=$BUILDPLATFORM rust:${RUST_VERSION}-bookworm AS wasm
RUN rustup target add wasm32-unknown-unknown \
    && cargo install wasm-bindgen-cli --version 0.2.128 --locked
WORKDIR /src
COPY . .
RUN cargo build -p lrtodo-wasm --target wasm32-unknown-unknown --release \
    && wasm-bindgen --target web --out-dir web/src/lib/wasm \
       target/wasm32-unknown-unknown/release/lrtodo_wasm.wasm

# --- PWA
FROM --platform=$BUILDPLATFORM node:${NODE_VERSION}-bookworm-slim AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
COPY --from=wasm /src/web/src/lib/wasm ./src/lib/wasm
RUN npx vite build

# --- binario estático (musl) para la arquitectura destino
FROM --platform=$BUILDPLATFORM rust:${RUST_VERSION}-bookworm AS build
ARG TARGETARCH
RUN apt-get update && apt-get install -y --no-install-recommends cmake python3-pip \
    && pip3 install --break-system-packages ziglang==0.15.2 \
    && cargo install cargo-zigbuild --version 0.23.4 --locked
WORKDIR /src
COPY . .
COPY --from=web /src/web/dist ./web/dist
# LTO completo necesita mucha RAM al linkear; thin anda en VMs chicas.
ENV CARGO_PROFILE_RELEASE_LTO=thin CARGO_PROFILE_RELEASE_CODEGEN_UNITS=4
RUN case "$TARGETARCH" in \
      amd64) T=x86_64-unknown-linux-musl ;; \
      arm64) T=aarch64-unknown-linux-musl ;; \
      *) echo "arquitectura no soportada: $TARGETARCH" && exit 1 ;; \
    esac \
    && rustup target add $T \
    && cargo zigbuild -p lrtodo-cli --release --target $T \
    && cp target/$T/release/lrtodo /lrtodo

# --- runtime
FROM alpine:3.22
RUN apk add --no-cache jq tzdata ca-certificates
COPY --from=build /lrtodo /usr/local/bin/lrtodo
COPY ha-addon/lrtodo/run.sh /run.sh
ENV LRTODO_DATA_DIR=/data/lrtodo LRTODO_BIND=0.0.0.0:8765
EXPOSE 8765
CMD ["/run.sh"]
