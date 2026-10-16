# syntax=docker/dockerfile:1.7
ARG E2E_MARKETPULSE_TYPESCRIPT_BUILD_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/node:22-alpine
FROM ${E2E_MARKETPULSE_TYPESCRIPT_BUILD_IMAGE} AS builder
WORKDIR /build
COPY src/adapters/typescript /build/src/adapters/typescript
RUN --mount=type=cache,target=/root/.npm cd src/adapters/typescript && npm ci && npm run build
COPY use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-typescript-service /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-typescript-service
WORKDIR /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-typescript-service
RUN --mount=type=cache,target=/root/.npm \
    npm ci \
    && npm run build \
    && npm prune --omit=dev

FROM ${E2E_MARKETPULSE_TYPESCRIPT_BUILD_IMAGE}
WORKDIR /app
COPY --from=builder /build/src/adapters/typescript /src/adapters/typescript
COPY --from=builder /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-typescript-service/dist ./dist
COPY --from=builder /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-typescript-service/node_modules ./node_modules
COPY --from=builder /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-typescript-service/package.json ./package.json
RUN chown -R 65532:65532 /app /src/adapters/typescript \
    && mkdir -p /var/lib/marketpulse && chown 65532:65532 /var/lib/marketpulse
USER 65532:65532
ENTRYPOINT ["node", "dist/src/main.js"]
