# syntax=docker/dockerfile:1.7
ARG E2E_MARKETPULSE_PYTHON_BUILD_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/python:3.12-slim
FROM ${E2E_MARKETPULSE_PYTHON_BUILD_IMAGE}
ENV PIP_DISABLE_PIP_VERSION_CHECK=1 PIP_NO_CACHE_DIR=1
WORKDIR /build
COPY src/adapters/python ./src/adapters/python
COPY use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-python-service ./use-case
RUN python -m pip install ./src/adapters/python ./use-case \
    && useradd --system --uid 65532 --no-create-home agentvault \
    && install -d -o 65532 -g 65532 -m 0700 /var/lib/marketpulse
USER 65532:65532
ENTRYPOINT ["python", "-m", "app.main"]
