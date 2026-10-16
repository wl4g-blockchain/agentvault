# syntax=docker/dockerfile:1.7
ARG E2E_MARKETPULSE_JAVA_BUILD_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/maven:3.9.9-eclipse-temurin-17
ARG E2E_MARKETPULSE_JAVA_RUNTIME_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/eclipse-temurin:17-jre
FROM ${E2E_MARKETPULSE_JAVA_BUILD_IMAGE} AS builder
WORKDIR /build
COPY src/adapters/java ./src/adapters/java
RUN --mount=type=cache,target=/root/.m2 mvn -q -f src/adapters/java/pom.xml -DskipTests install
COPY use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-java-service ./use-case
RUN --mount=type=cache,target=/root/.m2 mvn -q -f use-case/pom.xml -DskipTests package

FROM ${E2E_MARKETPULSE_JAVA_RUNTIME_IMAGE}
WORKDIR /app
COPY --from=builder /build/use-case/target/mock-payee-marketpulse-java-service-0.1.0.jar ./marketpulse.jar
RUN apt-get update \
    && apt-get install --yes --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /var/lib/marketpulse \
    && chown 65532:65532 /var/lib/marketpulse \
    && chmod 0700 /var/lib/marketpulse
USER 65532:65532
ENTRYPOINT ["java", "-jar", "/app/marketpulse.jar"]
