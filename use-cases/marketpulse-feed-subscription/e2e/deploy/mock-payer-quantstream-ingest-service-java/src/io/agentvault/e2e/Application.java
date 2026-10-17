package io.agentvault.e2e;

import io.agentvault.e2e.job.IngestionJob;
import org.springframework.boot.ApplicationArguments;
import org.springframework.boot.ApplicationRunner;
import org.springframework.boot.SpringApplication;
import org.springframework.boot.WebApplicationType;
import org.springframework.boot.autoconfigure.SpringBootApplication;
import org.springframework.context.ConfigurableApplicationContext;
import org.springframework.context.annotation.Bean;
import org.springframework.scheduling.annotation.EnableScheduling;

@SpringBootApplication
@EnableScheduling
public class Application {
    public static void main(String[] args) {
        for (String argument : args) {
            if (!"--once".equals(argument)) {
                throw new IllegalArgumentException("usage: quantstream-java [--once]");
            }
        }
        SpringApplication application = new SpringApplication(Application.class);
        application.setWebApplicationType(webApplicationType(args));
        application.run(args);
    }

    static WebApplicationType webApplicationType(String[] args) {
        return java.util.List.of(args).contains("--once")
                ? WebApplicationType.NONE
                : WebApplicationType.REACTIVE;
    }

    @Bean
    ApplicationRunner onceRunner(IngestionJob job, ConfigurableApplicationContext application) {
        return arguments -> runOnceWhenRequested(arguments, job, application);
    }

    static void runOnceWhenRequested(
            ApplicationArguments arguments,
            IngestionJob job,
            ConfigurableApplicationContext application) throws Exception {
        if (!arguments.containsOption("once")) return;
        System.out.println(job.runOnce(1));
        SpringApplication.exit(application);
    }
}
