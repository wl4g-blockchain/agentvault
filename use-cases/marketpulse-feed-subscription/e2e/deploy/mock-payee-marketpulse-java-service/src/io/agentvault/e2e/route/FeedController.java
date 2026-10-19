package io.agentvault.e2e.route;

import io.agentvault.e2e.handler.FeedHandler;
import org.springframework.http.ResponseEntity;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RestController;

/** Ordinary operational endpoints; the MCP endpoint belongs to MarketPulseServer. */
@RestController
@RequestMapping
public final class FeedController {
    private final FeedHandler handler;

    public FeedController(FeedHandler handler) {
        this.handler = handler;
    }

    @GetMapping("/healthz")
    public ResponseEntity<byte[]> health() {
        return handler.health();
    }

    @GetMapping("/metrics")
    public ResponseEntity<byte[]> metrics() {
        return handler.metrics();
    }
}
