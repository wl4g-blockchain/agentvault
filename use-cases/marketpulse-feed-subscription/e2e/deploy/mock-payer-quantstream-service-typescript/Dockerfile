# syntax=docker/dockerfile:1.7
ARG E2E_QUANTSTREAM_TYPESCRIPT_BUILD_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/node:22-alpine
ARG E2E_QUANTSTREAM_TYPESCRIPT_RUNTIME_IMAGE=gcr.io/distroless/nodejs22-debian12:nonroot
FROM ${E2E_QUANTSTREAM_TYPESCRIPT_BUILD_IMAGE} AS builder
WORKDIR /build
COPY src/adapters/typescript /build/src/adapters/typescript
RUN --mount=type=cache,target=/root/.npm cd src/adapters/typescript && npm ci && npm run build
COPY use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-service-typescript /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-service-typescript
WORKDIR /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-service-typescript
RUN --mount=type=cache,target=/root/.npm \
    npm ci \
    && npm run build \
    && npm prune --omit=dev \
    && cd /build/src/adapters/typescript \
    && npm prune --omit=dev \
    && install -d -m 0700 /runtime/var/lib/quantstream

FROM ${E2E_QUANTSTREAM_TYPESCRIPT_RUNTIME_IMAGE}
WORKDIR /app
COPY --from=builder --chown=65532:65532 /build/src/adapters/typescript/dist /src/adapters/typescript/dist
COPY --from=builder --chown=65532:65532 /build/src/adapters/typescript/node_modules /src/adapters/typescript/node_modules
COPY --from=builder --chown=65532:65532 /build/src/adapters/typescript/package.json /src/adapters/typescript/package.json
COPY --from=builder --chown=65532:65532 /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-service-typescript/dist ./dist
COPY --from=builder --chown=65532:65532 /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-service-typescript/node_modules ./node_modules
COPY --from=builder --chown=65532:65532 /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-service-typescript/package.json ./package.json
COPY --from=builder --chown=65532:65532 /runtime/var/lib/quantstream /var/lib/quantstream
USER 65532:65532
ENTRYPOINT ["/nodejs/bin/node", "dist/main.js"]
