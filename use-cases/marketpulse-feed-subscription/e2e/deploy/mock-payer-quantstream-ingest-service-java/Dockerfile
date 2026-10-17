# syntax=docker/dockerfile:1.7
ARG E2E_QUANTSTREAM_JAVA_BUILD_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/maven:3.9.9-eclipse-temurin-17
ARG E2E_QUANTSTREAM_JAVA_RUNTIME_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/eclipse-temurin:17-jre
FROM ${E2E_QUANTSTREAM_JAVA_BUILD_IMAGE} AS builder
WORKDIR /build
COPY src/adapters/java ./src/adapters/java
RUN --mount=type=cache,target=/root/.m2 mvn -q -f src/adapters/java/pom.xml -DskipTests install
COPY use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-service-java ./use-case
RUN --mount=type=cache,target=/root/.m2 \
    mvn -q -f use-case/pom.xml -DskipTests package dependency:copy-dependencies \
    && mkdir -p /application \
    && cp -R use-case/target/classes /application/classes \
    && cp -R use-case/target/dependency /application/lib

FROM ${E2E_QUANTSTREAM_JAVA_RUNTIME_IMAGE}
WORKDIR /app
COPY --from=builder /application/classes ./classes
COPY --from=builder /application/lib ./lib
RUN mkdir -p /var/lib/quantstream \
    && chown 65532:65532 /var/lib/quantstream \
    && chmod 0700 /var/lib/quantstream
USER 65532:65532
ENTRYPOINT ["java", "-cp", "/app/classes:/app/lib/*", "io.agentvault.e2e.Application"]
