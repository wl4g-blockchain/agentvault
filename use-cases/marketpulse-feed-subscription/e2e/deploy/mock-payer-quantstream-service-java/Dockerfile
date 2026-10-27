# syntax=docker/dockerfile:1.7
ARG E2E_QUANTSTREAM_JAVA_BUILD_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/maven:3.9.9-eclipse-temurin-17
ARG E2E_QUANTSTREAM_JAVA_RUNTIME_IMAGE=gcr.io/distroless/cc-debian12:nonroot
ARG E2E_QUANTSTREAM_JAVA_PREBUILT_RUNTIME_IMAGE=localhost/quantstream-ingestor-java-e2e:local

# Explicit target for a verified host artifact; inherit the existing JRE17,
# nonroot identity, data directory, and entrypoint. The default final stage
# below continues to build all application sources inside the builder.
FROM ${E2E_QUANTSTREAM_JAVA_PREBUILT_RUNTIME_IMAGE} AS prebuilt
COPY --chown=65532:65532 use-cases/marketpulse-feed-subscription/e2e/reports/.runtime/agentvault-sdk-java.jar /app/lib/agentvault-sdk-0.1.0.jar

FROM ${E2E_QUANTSTREAM_JAVA_BUILD_IMAGE} AS builder
WORKDIR /build
RUN "${JAVA_HOME}/bin/jlink" \
    --add-modules ALL-MODULE-PATH \
    --strip-debug \
    --no-man-pages \
    --no-header-files \
    --compress=2 \
    --output /opt/e2e-jre
COPY src/adapters/java ./src/adapters/java
RUN --mount=type=cache,target=/root/.m2 mvn -q -f src/adapters/java/pom.xml -DskipTests install
COPY use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-service-java ./use-case
RUN --mount=type=cache,target=/root/.m2 \
    mvn -q -f use-case/pom.xml -DskipTests package dependency:copy-dependencies \
    && mkdir -p /application \
    && cp -R use-case/target/classes /application/classes \
    && cp -R use-case/target/dependency /application/lib \
    && install -d -m 0700 /runtime/var/lib/quantstream

FROM ${E2E_QUANTSTREAM_JAVA_RUNTIME_IMAGE}
ENV JAVA_HOME=/opt/java
WORKDIR /app
COPY --from=builder /opt/e2e-jre /opt/java
COPY --from=builder /application/classes ./classes
COPY --from=builder /application/lib ./lib
COPY --from=builder --chown=65532:65532 /runtime/var/lib/quantstream /var/lib/quantstream
USER 65532:65532
ENTRYPOINT ["/opt/java/bin/java", "-cp", "/app/classes:/app/lib/*", "io.agentvault.e2e.Application"]
