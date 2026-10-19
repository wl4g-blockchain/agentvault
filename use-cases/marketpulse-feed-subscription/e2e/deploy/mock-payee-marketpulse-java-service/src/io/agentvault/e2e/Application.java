package io.agentvault.e2e;

import io.agentvault.e2e.server.MarketPulseServer;
import org.springframework.boot.SpringApplication;
import org.springframework.boot.autoconfigure.SpringBootApplication;

@SpringBootApplication
public class Application {
    public static void main(String[] args) {
        MarketPulseServer.configureListener();
        SpringApplication.run(Application.class, args);
    }
}
