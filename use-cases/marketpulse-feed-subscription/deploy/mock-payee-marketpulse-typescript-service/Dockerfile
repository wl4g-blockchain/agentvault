ARG E2E_MARKETPULSE_TYPESCRIPT_BUILD_IMAGE
ARG E2E_MARKETPULSE_TYPESCRIPT_RUNTIME_IMAGE
FROM ${E2E_MARKETPULSE_TYPESCRIPT_BUILD_IMAGE} AS builder
WORKDIR /build
COPY src/sdk/typescript /build/src/sdk/typescript
RUN --mount=type=cache,target=/root/.npm cd src/sdk/typescript && npm ci && npm run build
COPY use-cases/marketpulse-feed-subscription/deploy/mock-payee-marketpulse-typescript-service/source /build/use-cases/marketpulse-feed-subscription/deploy/mock-payee-marketpulse-typescript-service/source
WORKDIR /build/use-cases/marketpulse-feed-subscription/deploy/mock-payee-marketpulse-typescript-service/source
RUN --mount=type=cache,target=/root/.npm \
    npm ci \
    && npm run build \
    && npm prune --omit=dev \
    && cd /build/src/sdk/typescript \
    && npm prune --omit=dev \
    && install -d -m 0700 /runtime/var/lib/marketpulse

FROM ${E2E_MARKETPULSE_TYPESCRIPT_RUNTIME_IMAGE}
WORKDIR /app
COPY --from=builder --chown=65532:65532 /build/src/sdk/typescript/dist /src/sdk/typescript/dist
COPY --from=builder --chown=65532:65532 /build/src/sdk/typescript/node_modules /src/sdk/typescript/node_modules
COPY --from=builder --chown=65532:65532 /build/src/sdk/typescript/package.json /src/sdk/typescript/package.json
COPY --from=builder --chown=65532:65532 /build/use-cases/marketpulse-feed-subscription/deploy/mock-payee-marketpulse-typescript-service/source/dist ./dist
COPY --from=builder --chown=65532:65532 /build/use-cases/marketpulse-feed-subscription/deploy/mock-payee-marketpulse-typescript-service/source/node_modules ./node_modules
COPY --from=builder --chown=65532:65532 /build/use-cases/marketpulse-feed-subscription/deploy/mock-payee-marketpulse-typescript-service/source/package.json ./package.json
COPY --from=builder --chown=65532:65532 /runtime/var/lib/marketpulse /var/lib/marketpulse
USER 65532:65532
ENTRYPOINT ["node", "dist/src/main.js"]
