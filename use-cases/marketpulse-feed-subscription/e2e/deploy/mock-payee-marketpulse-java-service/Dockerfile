# syntax=docker/dockerfile:1.7
ARG E2E_MARKETPULSE_JAVA_BUILD_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/maven:3.9.9-eclipse-temurin-17
ARG E2E_MARKETPULSE_JAVA_RUNTIME_IMAGE=gcr.io/distroless/cc-debian12:nonroot
FROM ${E2E_MARKETPULSE_JAVA_BUILD_IMAGE} AS builder
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
COPY use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-java-service ./use-case
RUN --mount=type=cache,target=/root/.m2 \
    mvn -q -f use-case/pom.xml -DskipTests package \
    && install -d -m 0700 /runtime/var/lib/marketpulse

FROM ${E2E_MARKETPULSE_JAVA_RUNTIME_IMAGE}
ENV JAVA_HOME=/opt/java
WORKDIR /app
COPY --from=builder /opt/e2e-jre /opt/java
COPY --from=builder /build/use-case/target/mock-payee-marketpulse-java-service-0.1.0.jar ./marketpulse.jar
COPY --from=builder --chown=65532:65532 /runtime/var/lib/marketpulse /var/lib/marketpulse
USER 65532:65532
ENTRYPOINT ["/opt/java/bin/java", "-jar", "/app/marketpulse.jar"]
