package io.agentvault.e2e.storage;

import static org.junit.jupiter.api.Assertions.assertEquals;

import java.nio.file.Files;
import org.junit.jupiter.api.Test;

final class FeedSQLiteStorageTest {
    @Test void initializesExactlyOneHundredRows() throws Exception {
        var directory = Files.createTempDirectory("marketpulse-java-");
        try (var storage = new FeedSQLiteStorage(directory.resolve("marketpulse.sqlite3").toString())) {
            assertEquals(50, storage.news("yesterday", 100).size());
            assertEquals(50, storage.news("realtime", 100).size());
        }
    }
}
