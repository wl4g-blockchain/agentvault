ARG E2E_QUANTSTREAM_PYTHON_BUILD_IMAGE
ARG E2E_QUANTSTREAM_PYTHON_RUNTIME_IMAGE
FROM ${E2E_QUANTSTREAM_PYTHON_BUILD_IMAGE} AS builder
ENV PIP_DISABLE_PIP_VERSION_CHECK=1
WORKDIR /build
COPY src/sdk/python ./src/sdk/python
COPY use-cases/marketpulse-feed-subscription/deploy/mock-payer-quantstream-service-python/source ./use-case
RUN --mount=type=cache,target=/root/.cache/pip python -m pip install \
        --no-compile \
        --target=/runtime/usr/local/lib/python3.11/site-packages \
        ./src/sdk/python \
        ./use-case \
    && install -d -m 0700 /runtime/var/lib/quantstream

FROM ${E2E_QUANTSTREAM_PYTHON_RUNTIME_IMAGE}
ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1
COPY --from=builder --chown=65532:65532 /runtime/usr/local/lib/python3.11/site-packages /usr/local/lib/python3.11/site-packages
COPY --from=builder --chown=65532:65532 /runtime/var/lib/quantstream /var/lib/quantstream
USER 65532:65532
ENTRYPOINT ["python", "-m", "app.main"]
