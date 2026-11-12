ARG E2E_MARKETPULSE_GOLANG_BUILD_IMAGE
ARG E2E_MARKETPULSE_GOLANG_RUNTIME_IMAGE
ARG E2E_MARKETPULSE_GOLANG_PREBUILT_RUNTIME_IMAGE

# Optional verified static artifact; inherit the nonroot Alpine runtime.
FROM ${E2E_MARKETPULSE_GOLANG_PREBUILT_RUNTIME_IMAGE} AS prebuilt
ENV GODEBUG=netdns=go
COPY --chown=65532:65532 --chmod=0755 use-cases/marketpulse-feed-subscription/reports/.runtime/marketpulse-golang /usr/local/bin/marketpulse-golang

FROM ${E2E_MARKETPULSE_GOLANG_BUILD_IMAGE} AS builder
RUN apk add --no-cache build-base
WORKDIR /build
COPY src/sdk/golang /build/src/sdk/golang
COPY use-cases/marketpulse-feed-subscription/deploy/mock-payee-marketpulse-golang-service/source /build/use-cases/marketpulse-feed-subscription/deploy/mock-payee-marketpulse-golang-service/source
WORKDIR /build/use-cases/marketpulse-feed-subscription/deploy/mock-payee-marketpulse-golang-service/source
RUN --mount=type=cache,target=/go/pkg/mod --mount=type=cache,target=/root/.cache/go-build \
    CGO_ENABLED=1 go build -trimpath -ldflags='-s -w -linkmode external -extldflags -static' -o /marketpulse-golang .

FROM ${E2E_MARKETPULSE_GOLANG_RUNTIME_IMAGE}
COPY --from=builder /marketpulse-golang /usr/local/bin/marketpulse-golang
RUN addgroup -g 65532 app && adduser -D -H -u 65532 -G app app \
    && install -d -o app -g app -m 0700 /var/lib/marketpulse
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/marketpulse-golang"]
