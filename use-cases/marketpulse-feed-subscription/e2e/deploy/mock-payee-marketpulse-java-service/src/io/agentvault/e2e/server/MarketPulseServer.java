package io.agentvault.e2e.server;

import io.agentvault.e2e.handler.FeedHandler;
import io.agentvault.e2e.storage.FeedSQLiteStorage;
import io.agentvault.sdk.AgentVault;
import io.agentvault.sdk.payee.SpringInterceptorPayeeMiddleware;
import io.agentvault.sdk.transport.HttpVaultTransport;
import jakarta.servlet.http.HttpServletRequest;
import java.util.List;
import org.springframework.boot.web.servlet.FilterRegistrationBean;
import org.springframework.context.annotation.Bean;
import org.springframework.context.annotation.Configuration;
import org.springframework.http.ResponseEntity;
import org.springframework.web.bind.annotation.PostMapping;
import org.springframework.web.bind.annotation.RequestBody;
import org.springframework.web.bind.annotation.RestController;

/** Owns the Spring listener configuration, MCP endpoint and SDK middleware. */
@Configuration(proxyBeanMethods = false)
public class MarketPulseServer {
    public static void configureListener() {
        String bind = required("MARKETPULSE_BIND");
        System.setProperty("server.address", bindHost(bind));
        System.setProperty("server.port", Integer.toString(bindPort(bind)));
    }

    @Bean(destroyMethod = "close")
    FeedSQLiteStorage feedSQLiteStorage() {
        return new FeedSQLiteStorage(required("MARKETPULSE_DATABASE"));
    }

    @Bean
    FilterRegistrationBean<SpringInterceptorPayeeMiddleware> payeeMiddleware() {
        String bearer = System.getenv("MARKETPULSE_AUTHGUARD_BEARER");
        HttpVaultTransport transport = bearer == null || bearer.isBlank()
                ? new HttpVaultTransport(required("AGENTVAULT_URL"))
                : new HttpVaultTransport(required("AGENTVAULT_URL"), bearer);
        var payee = new AgentVault(transport).payee();
        var route = SpringInterceptorPayeeMiddleware.Route.mcpTool(
                "/mcp",
                "marketpulse.realtime_news",
                required("MARKETPULSE_SCENARIO_ID"),
                required("MARKETPULSE_OFFER_ID"),
                required("MARKETPULSE_RESOURCE_URL"),
                required("MARKETPULSE_ORIGIN"));
        var registration = new FilterRegistrationBean<>(payee.spring(List.of(route)));
        registration.setOrder(-100);
        registration.addUrlPatterns("/mcp");
        return registration;
    }

    public static String required(String name) {
        String value = System.getenv(name);
        if (value == null || value.isBlank()) throw new IllegalStateException(name + " is required");
        return value;
    }

    static String bindHost(String bind) {
        int separator = bind.lastIndexOf(':');
        if (separator <= 0) throw new IllegalArgumentException("MARKETPULSE_BIND must be host:port");
        return bind.substring(0, separator);
    }

    static int bindPort(String bind) {
        int separator = bind.lastIndexOf(':');
        if (separator <= 0) throw new IllegalArgumentException("MARKETPULSE_BIND must be host:port");
        int port = Integer.parseInt(bind.substring(separator + 1));
        if (port <= 0 || port > 65535) throw new IllegalArgumentException("MARKETPULSE_BIND port is out of range");
        return port;
    }

    @RestController
    public static final class McpEndpoint {
        private final FeedHandler handler;

        public McpEndpoint(FeedHandler handler) {
            this.handler = handler;
        }

        @PostMapping("/mcp")
        public ResponseEntity<byte[]> mcp(HttpServletRequest request, @RequestBody byte[] body) {
            return handler.mcp(request, body);
        }
    }
}
