# syntax=docker/dockerfile:1.7
ARG E2E_QUANTSTREAM_UI_BUILD_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/node:22-alpine
ARG E2E_QUANTSTREAM_UI_RUNTIME_IMAGE=gcr.io/distroless/nodejs22-debian12:nonroot
FROM ${E2E_QUANTSTREAM_UI_BUILD_IMAGE} AS builder
WORKDIR /build
COPY use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-ui/package.json ./package.json
COPY use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-ui/package-lock.json ./package-lock.json
RUN --mount=type=cache,target=/root/.npm npm ci --omit=dev --ignore-scripts
COPY use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-ui/public ./public
COPY use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-ui/src ./src

FROM ${E2E_QUANTSTREAM_UI_RUNTIME_IMAGE}
WORKDIR /app
COPY --from=builder --chown=65532:65532 /build/package.json /build/package-lock.json ./
COPY --from=builder --chown=65532:65532 /build/node_modules ./node_modules
COPY --from=builder --chown=65532:65532 /build/public ./public
COPY --from=builder --chown=65532:65532 /build/src ./src
USER 65532:65532
ENTRYPOINT ["/nodejs/bin/node", "src/server.mjs"]
