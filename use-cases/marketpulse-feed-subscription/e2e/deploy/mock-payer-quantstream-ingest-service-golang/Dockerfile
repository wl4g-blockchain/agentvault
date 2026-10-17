# syntax=docker/dockerfile:1.7
ARG E2E_QUANTSTREAM_GOLANG_BUILD_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/golang:1.26-alpine
ARG E2E_QUANTSTREAM_GOLANG_RUNTIME_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/alpine:3.21
FROM ${E2E_QUANTSTREAM_GOLANG_BUILD_IMAGE} AS builder
RUN apk add --no-cache build-base
WORKDIR /build
COPY src/adapters/golang /build/src/adapters/golang
COPY use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-service-golang /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-service-golang
WORKDIR /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-service-golang
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
