ARG E2E_MARKETPULSE_JAVA_BUILD_IMAGE
ARG E2E_MARKETPULSE_JAVA_RUNTIME_IMAGE
ARG E2E_MARKETPULSE_JAVA_PREBUILT_RUNTIME_IMAGE

# Explicit target for a verified host artifact; inherit the existing JRE17,
# nonroot identity, data directory, and entrypoint. The default final stage
# below continues to build all application sources inside the builder.
FROM ${E2E_MARKETPULSE_JAVA_PREBUILT_RUNTIME_IMAGE} AS prebuilt
COPY --chown=65532:65532 use-cases/marketpulse-feed-subscription/reports/.runtime/marketpulse-java.jar /app/marketpulse.jar

FROM ${E2E_MARKETPULSE_JAVA_BUILD_IMAGE} AS builder
WORKDIR /build
RUN "${JAVA_HOME}/bin/jlink" \
    --add-modules ALL-MODULE-PATH \
    --strip-debug \
    --no-man-pages \
    --no-header-files \
    --compress=2 \
    --output /opt/e2e-jre
COPY src/sdk/java ./src/sdk/java
RUN --mount=type=cache,target=/root/.m2 mvn -q -f src/sdk/java/pom.xml -DskipTests install
COPY use-cases/marketpulse-feed-subscription/deploy/mock-payee-marketpulse-java-service/source ./use-case
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
