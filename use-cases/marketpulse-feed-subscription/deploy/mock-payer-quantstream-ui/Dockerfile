ARG E2E_QUANTSTREAM_UI_BUILD_IMAGE
ARG E2E_QUANTSTREAM_UI_RUNTIME_IMAGE
FROM ${E2E_QUANTSTREAM_UI_BUILD_IMAGE} AS builder
WORKDIR /build
COPY use-cases/marketpulse-feed-subscription/deploy/mock-payer-quantstream-ui/source/package.json ./package.json
COPY use-cases/marketpulse-feed-subscription/deploy/mock-payer-quantstream-ui/source/package-lock.json ./package-lock.json
RUN --mount=type=cache,target=/root/.npm npm ci --omit=dev --ignore-scripts
COPY use-cases/marketpulse-feed-subscription/deploy/mock-payer-quantstream-ui/source/public ./public
COPY use-cases/marketpulse-feed-subscription/deploy/mock-payer-quantstream-ui/source/src ./src

FROM ${E2E_QUANTSTREAM_UI_RUNTIME_IMAGE}
WORKDIR /app
COPY --from=builder --chown=65532:65532 /build/package.json /build/package-lock.json ./
COPY --from=builder --chown=65532:65532 /build/public ./public
COPY --from=builder --chown=65532:65532 /build/src ./src
USER 65532:65532
ENTRYPOINT ["node", "src/server.mjs"]
