package io.agentvault.e2e;

import io.agentvault.e2e.job.IngestionJob;
import org.springframework.boot.ApplicationArguments;
import org.springframework.boot.ApplicationRunner;
import org.springframework.boot.SpringApplication;
import org.springframework.boot.WebApplicationType;
import org.springframework.boot.autoconfigure.SpringBootApplication;
import org.springframework.context.ConfigurableApplicationContext;
import org.springframework.context.annotation.Bean;
import org.springframework.context.annotation.Configuration;
import org.springframework.context.annotation.Import;
import org.springframework.context.annotation.Profile;
import org.springframework.scheduling.annotation.EnableScheduling;

@SpringBootApplication
@Import(Application.SchedulingConfiguration.class)
public class Application {
    public static void main(String[] args) {
        validateArguments(args);
        application(args).run(args);
    }

    static void validateArguments(String[] args) {
        for (String argument : args) {
            if (!"--once".equals(argument)) {
                throw new IllegalArgumentException("usage: quantstream-java [--once]");
            }
        }
    }

    static SpringApplication application(String[] args) {
        SpringApplication application = new SpringApplication(Application.class);
        application.setWebApplicationType(webApplicationType(args));
        if (isOneShot(args)) application.setAdditionalProfiles("oneshot");
        return application;
    }

    static WebApplicationType webApplicationType(String[] args) {
        return isOneShot(args)
                ? WebApplicationType.NONE
                : WebApplicationType.REACTIVE;
    }

    private static boolean isOneShot(String[] args) {
        return java.util.List.of(args).contains("--once");
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

    @Configuration(proxyBeanMethods = false)
    @Profile("!oneshot")
    @EnableScheduling
    static class SchedulingConfiguration {}
}
