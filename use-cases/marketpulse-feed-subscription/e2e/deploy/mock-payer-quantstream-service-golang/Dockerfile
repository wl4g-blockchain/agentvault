# syntax=docker/dockerfile:1.7
ARG E2E_QUANTSTREAM_GOLANG_BUILD_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/golang:1.26-alpine
ARG E2E_QUANTSTREAM_GOLANG_RUNTIME_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/alpine:3.21
ARG E2E_QUANTSTREAM_GOLANG_PREBUILT_RUNTIME_IMAGE=localhost/quantstream-ingestor-golang-e2e:local

# Optional verified static artifact; inherit the nonroot Alpine runtime.
FROM ${E2E_QUANTSTREAM_GOLANG_PREBUILT_RUNTIME_IMAGE} AS prebuilt
ENV GODEBUG=netdns=go
COPY --chown=65532:65532 --chmod=0755 use-cases/marketpulse-feed-subscription/e2e/reports/.runtime/quantstream-ingestor-go /usr/local/bin/quantstream-ingestor-go

FROM ${E2E_QUANTSTREAM_GOLANG_BUILD_IMAGE} AS builder
RUN apk add --no-cache build-base
WORKDIR /build
COPY src/adapters/golang /build/src/adapters/golang
COPY use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-service-golang /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-service-golang
WORKDIR /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-service-golang
RUN --mount=type=cache,target=/go/pkg/mod \
    --mount=type=cache,target=/root/.cache/go-build \
    CGO_ENABLED=1 go build -trimpath -ldflags='-s -w -linkmode external -extldflags -static' -o /quantstream-ingestor-go .

FROM ${E2E_QUANTSTREAM_GOLANG_RUNTIME_IMAGE}
COPY --from=builder /quantstream-ingestor-go /usr/local/bin/quantstream-ingestor-go
RUN addgroup -g 65532 app \
    && adduser -D -H -u 65532 -G app app \
    && install -d -o app -g app -m 0700 /var/lib/quantstream
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/quantstream-ingestor-go"]
