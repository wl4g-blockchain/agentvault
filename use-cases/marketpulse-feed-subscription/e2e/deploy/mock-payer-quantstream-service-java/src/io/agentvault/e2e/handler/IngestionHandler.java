package io.agentvault.e2e.handler;

import java.util.Map;
import org.springframework.stereotype.Service;

/** Business-facing ingestion handler; scheduling stays in IngestionJob. */
@Service
public final class IngestionHandler {
    public Map<String, Object> health() {
        return Map.of("status", "ready", "service", "quantstream-java");
    }

    public String metrics() {
        return "# HELP quantstream_up QuantStream process readiness.\n"
                + "# TYPE quantstream_up gauge\nquantstream_up 1\n";
    }
}
