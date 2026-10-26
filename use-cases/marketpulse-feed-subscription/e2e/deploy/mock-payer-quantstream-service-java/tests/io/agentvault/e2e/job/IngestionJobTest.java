package io.agentvault.e2e.job;

import static org.junit.jupiter.api.Assertions.assertNotNull;

import org.junit.jupiter.api.Test;

final class IngestionJobTest {
    @Test void jobDeclaresSeparateFreeAndPaidPeriodicFunctions() throws Exception {
        assertNotNull(IngestionJob.class.getDeclaredMethod("ingestYesterdayNews", String.class));
        assertNotNull(IngestionJob.class.getDeclaredMethod("ingestRealtimeNews", String.class, boolean.class));
    }
}
