package io.agentvault.e2e.storage;

import static org.junit.jupiter.api.Assertions.assertEquals;

import java.nio.file.Files;
import org.junit.jupiter.api.Test;

final class FeedSqliteRepositoryTest {
    @Test void initializesExactlyOneHundredRows() throws Exception {
        var directory = Files.createTempDirectory("marketpulse-java-");
        try (var storage = new FeedSqliteRepository(directory.resolve("marketpulse.sqlite3").toString())) {
            assertEquals(50, storage.news("yesterday", 100).size());
            assertEquals(50, storage.news("realtime", 100).size());
        }
    }
    @Test void nativeTaskPaymentScopeSurvivesRepositoryReopen() throws Exception {
        var directory = Files.createTempDirectory("marketpulse-java-a2a-");
        var path = directory.resolve("marketpulse.sqlite3").toString();
        var task = new com.fasterxml.jackson.databind.ObjectMapper().findAndRegisterModules().readValue(
                "{\"kind\":\"task\",\"id\":\"task-1\",\"contextId\":\"context-1\",\"status\":{\"state\":\"submitted\"},\"metadata\":{\"agentvault.payment.run_id\":\"run-1\"}}", io.a2a.spec.Task.class);
        try(var repository=new FeedSqliteRepository(path)) {repository.save(task);}
        try(var repository=new FeedSqliteRepository(path)) {
            assertEquals("context-1",repository.get("task-1").getContextId());
            assertEquals("run-1",repository.get("task-1").getMetadata().get("agentvault.payment.run_id"));
        }
    }
}
