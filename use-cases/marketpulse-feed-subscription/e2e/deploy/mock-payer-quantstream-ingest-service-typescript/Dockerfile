# syntax=docker/dockerfile:1.7
ARG E2E_QUANTSTREAM_TYPESCRIPT_BUILD_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/node:22-alpine
FROM ${E2E_QUANTSTREAM_TYPESCRIPT_BUILD_IMAGE} AS builder
WORKDIR /build
COPY src/adapters/typescript /build/src/adapters/typescript
RUN --mount=type=cache,target=/root/.npm cd src/adapters/typescript && npm ci && npm run build
COPY use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-service-typescript /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-service-typescript
WORKDIR /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-service-typescript
RUN --mount=type=cache,target=/root/.npm \
    npm ci \
    && npm run build \
    && npm prune --omit=dev

FROM ${E2E_QUANTSTREAM_TYPESCRIPT_BUILD_IMAGE}
WORKDIR /app
COPY --from=builder /build/src/adapters/typescript /src/adapters/typescript
COPY --from=builder /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-service-typescript/dist ./dist
COPY --from=builder /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-service-typescript/node_modules ./node_modules
COPY --from=builder /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-service-typescript/package.json ./package.json
RUN chown -R 65532:65532 /app /src/adapters/typescript \
    && install -d -o 65532 -g 65532 -m 0700 /var/lib/quantstream
USER 65532:65532
ENTRYPOINT ["node", "dist/main.js"]
