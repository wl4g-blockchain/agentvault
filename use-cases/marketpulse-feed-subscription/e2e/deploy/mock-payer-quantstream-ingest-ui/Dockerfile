# syntax=docker/dockerfile:1.7
ARG E2E_QUANTSTREAM_UI_BUILD_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/node:22-alpine
FROM ${E2E_QUANTSTREAM_UI_BUILD_IMAGE}
WORKDIR /app
COPY use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-ui/package.json ./package.json
COPY use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-ui/package-lock.json ./package-lock.json
RUN --mount=type=cache,target=/root/.npm npm ci --omit=dev --ignore-scripts
COPY use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-ui/public ./public
COPY use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-ui/src ./src
USER node
ENTRYPOINT ["node", "src/server.mjs"]
