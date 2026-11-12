package io.agentvault.e2e.route;

import io.agentvault.e2e.handler.IngestionHandler;
import java.util.Map;
import org.springframework.http.MediaType;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RestController;

/** Spring Boot operational endpoints; ingestion runs only through scheduled jobs. */
@RestController
@RequestMapping
public final class IngestionController {
    private final IngestionHandler handler;

    public IngestionController(IngestionHandler handler) {
        this.handler = handler;
    }

    @GetMapping("/healthz")
    public Map<String, Object> health() {
        return handler.health();
    }

    @GetMapping(value = "/metrics", produces = MediaType.TEXT_PLAIN_VALUE)
    public String metrics() {
        return handler.metrics();
    }
}
