package io.agentvault.e2e.job;

import com.fasterxml.jackson.databind.ObjectMapper;
import io.agentvault.e2e.agent.MCPHelper;
import java.sql.DriverManager;
import java.time.Instant;
import java.util.Map;
import org.springframework.scheduling.annotation.Scheduled;
import org.springframework.stereotype.Component;

@Component
public final class IngestionJob implements AutoCloseable {
    private static final ObjectMapper JSON = new ObjectMapper();
    private final MCPHelper mcp;
    private final String database = required("QUANTSTREAM_DATABASE");
    private final boolean failAfterSettlement = "once".equals(System.getenv("QUANTSTREAM_E2E_FAIL_AFTER_SETTLEMENT"));

    public IngestionJob(MCPHelper mcp) {
        this.mcp = mcp;
        if (":memory:".equals(database)) throw new IllegalStateException("QUANTSTREAM_DATABASE must be persistent");
    }

    // First periodic function: free delayed-news ingestion.
    @Scheduled(cron = "${QUANTSTREAM_YESTERDAY_CRON:0 0 2 * * *}")
    public void ingestYesterdayNews() { execute(() -> ingestYesterdayNews("java-yesterday-" + Instant.now().getEpochSecond())); }
    public MCPHelper.ToolResult ingestYesterdayNews(String runId) throws Exception { return mcp.yesterdayNews(runId); }
    // Second periodic function: paid realtime-news ingestion.
    @Scheduled(cron = "${QUANTSTREAM_REALTIME_CRON:0 */1 * * * *}")
    public void ingestRealtimeNews() { execute(() -> ingestRealtimeNews("java-realtime-" + Instant.now().getEpochSecond(), false)); }
    public MCPHelper.ToolResult ingestRealtimeNews(String runId, boolean injectFailure) throws Exception {
        return mcp.realtimeNews(runId, injectFailure);
    }

    public Map<String, Object> runOnce(int sequence) throws Exception {
        String runId = "java-" + Instant.now().getEpochSecond() + "-" + sequence;
        var free = ingestYesterdayNews(runId);
        var paid = ingestRealtimeNews(runId, failAfterSettlement && sequence == 1);
        persist(runId, free, paid);
        return Map.of("runId", runId, "intentId", paid.paymentIntentId(), "freeStatus", 200, "paidStatus", 200);
    }

    private void persist(String runId, MCPHelper.ToolResult free, MCPHelper.ToolResult paid) throws Exception {
        if (paid.paymentIntentId() == null || paid.paymentIntentId().isBlank()) throw new IllegalStateException("paid result omitted intent");
        String freeJson = JSON.writeValueAsString(JSON.readTree(free.body()));
        String paidJson = JSON.writeValueAsString(JSON.readTree(paid.body()));
        try (var connection = DriverManager.getConnection("jdbc:sqlite:" + database)) {
            try (var statement = connection.createStatement()) {
                statement.execute("PRAGMA journal_mode=DELETE"); statement.execute("PRAGMA synchronous=FULL");
                statement.executeUpdate("CREATE TABLE IF NOT EXISTS ingestion_run (run_id TEXT PRIMARY KEY, "
                        + "free_response_json TEXT NOT NULL, paid_response_json TEXT NOT NULL, "
                        + "payment_intent_id TEXT NOT NULL UNIQUE, completed_at INTEGER NOT NULL)");
            }
            try (var insert = connection.prepareStatement("INSERT INTO ingestion_run VALUES(?,?,?,?,?)")) {
                insert.setString(1, runId); insert.setString(2, freeJson); insert.setString(3, paidJson);
                insert.setString(4, paid.paymentIntentId()); insert.setLong(5, Instant.now().getEpochSecond()); insert.executeUpdate();
            }
        }
    }

    @Override public void close() { mcp.close(); }
    private static void execute(CheckedAction action) { try { action.run(); } catch (Exception error) { System.err.println("ingestion failed: " + error.getClass().getSimpleName()); } }
    private static String required(String name) { String value = System.getenv(name); if (value == null || value.isBlank()) throw new IllegalStateException(name + " is required"); return value; }
    @FunctionalInterface private interface CheckedAction { void run() throws Exception; }
}
