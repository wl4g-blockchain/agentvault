package io.agentvault.e2e;

import io.agentvault.e2e.server.MarketPulseServer;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.time.Duration;
import org.springframework.boot.SpringApplication;
import org.springframework.boot.autoconfigure.SpringBootApplication;

@SpringBootApplication
public class Application {
    public static void main(String[] args) throws Exception {
        if (healthcheckRequested(args)) {
            verifyHealth();
            return;
        }
        MarketPulseServer.configureListener();
        SpringApplication.run(Application.class, args);
    }

    static boolean healthcheckRequested(String[] args) {
        return args.length == 1 && "--healthcheck".equals(args[0]);
    }

    private static void verifyHealth() throws Exception {
        String bind = System.getenv().getOrDefault("MARKETPULSE_BIND", "0.0.0.0:8081");
        int separator = bind.lastIndexOf(':');
        if (separator < 0 || separator == bind.length() - 1) {
            throw new IllegalArgumentException("MARKETPULSE_BIND must contain a port");
        }
        URI endpoint = URI.create("http://127.0.0.1:" + bind.substring(separator + 1) + "/healthz");
        HttpRequest request = HttpRequest.newBuilder(endpoint).timeout(Duration.ofSeconds(2)).GET().build();
        HttpResponse<Void> response = HttpClient.newHttpClient().send(request, HttpResponse.BodyHandlers.discarding());
        if (response.statusCode() != 200) {
            throw new IllegalStateException("MarketPulse health endpoint returned " + response.statusCode());
        }
    }
}
