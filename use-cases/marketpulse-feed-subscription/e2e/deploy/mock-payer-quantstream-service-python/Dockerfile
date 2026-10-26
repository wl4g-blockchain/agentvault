# syntax=docker/dockerfile:1.7
ARG E2E_QUANTSTREAM_PYTHON_BUILD_IMAGE=docker.io/library/python:3.11-slim
ARG E2E_QUANTSTREAM_PYTHON_RUNTIME_IMAGE=gcr.io/distroless/python3-debian12:nonroot
FROM ${E2E_QUANTSTREAM_PYTHON_BUILD_IMAGE} AS builder
ENV PIP_DISABLE_PIP_VERSION_CHECK=1 PIP_NO_CACHE_DIR=1
WORKDIR /build
COPY src/adapters/python ./src/adapters/python
COPY use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-service-python ./use-case
RUN python -m pip install \
        --no-compile \
        --target=/runtime/usr/local/lib/python3.11/dist-packages \
        ./src/adapters/python \
        ./use-case \
    && install -d -m 0700 /runtime/var/lib/quantstream

FROM ${E2E_QUANTSTREAM_PYTHON_RUNTIME_IMAGE}
ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1
COPY --from=builder --chown=65532:65532 /runtime/usr/local/lib/python3.11/dist-packages /usr/local/lib/python3.11/dist-packages
COPY --from=builder --chown=65532:65532 /runtime/var/lib/quantstream /var/lib/quantstream
USER 65532:65532
ENTRYPOINT ["/usr/bin/python3.11", "-m", "app.main"]
