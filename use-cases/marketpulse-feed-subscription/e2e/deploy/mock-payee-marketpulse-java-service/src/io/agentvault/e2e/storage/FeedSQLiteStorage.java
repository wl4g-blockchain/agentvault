package io.agentvault.e2e.storage;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.databind.ObjectMapper;
import java.sql.Connection;
import java.sql.DriverManager;
import java.sql.SQLException;
import java.time.Instant;
import java.util.ArrayList;
import java.util.List;

public final class FeedSQLiteStorage implements AutoCloseable {
    private static final ObjectMapper JSON = new ObjectMapper();
    private static final String[][] ACTORS = {
        {"Donald Trump", "United States president"}, {"Marco Rubio", "United States secretary of state"},
        {"Ursula von der Leyen", "European Commission president"}, {"Keir Starmer", "United Kingdom prime minister"},
        {"Emmanuel Macron", "French president"}, {"Friedrich Merz", "German chancellor"},
        {"Shigeru Ishiba", "Japanese prime minister"}, {"Narendra Modi", "Indian prime minister"},
        {"Luiz Inacio Lula da Silva", "Brazilian president"}, {"Anthony Albanese", "Australian prime minister"}
    };
    private static final String[][] TOPICS = {
        {"tariff", "proposed reciprocal tariff review for strategic imports", "SPY"},
        {"chips", "outlined semiconductor export-control consultations", "SOXX"},
        {"energy", "commented on energy-security supply coordination", "XLE"},
        {"shipping", "addressed Red Sea shipping-risk countermeasures", "BDRY"},
        {"defense", "discussed allied defense procurement commitments", "ITA"},
        {"currency", "responded to cross-border currency volatility", "DXY"},
        {"sanctions", "announced a review of targeted trade sanctions", "EEM"},
        {"agriculture", "set out agricultural import quota negotiations", "DBA"},
        {"critical-minerals", "signed a critical-minerals supply memorandum", "LIT"},
        {"digital-trade", "opened digital-services tax and AI trade talks", "QQQ"}
    };

    public record NewsItem(
            String id, String observedAt, String author, String role, String topic, List<String> symbols,
            String headline, String source, int sentimentBps) {}
    public record DeliveryState(String intentId, String response) {}

    private final Connection connection;

    public FeedSQLiteStorage(String path) {
        if (path == null || path.isBlank() || ":memory:".equals(path)) {
            throw new IllegalArgumentException("MARKETPULSE_DATABASE must be persistent");
        }
        try {
            connection = DriverManager.getConnection("jdbc:sqlite:" + path);
            try (var statement = connection.createStatement()) {
                statement.execute("PRAGMA journal_mode=DELETE");
                statement.execute("PRAGMA synchronous=FULL");
                statement.executeUpdate("""
                    CREATE TABLE IF NOT EXISTS feed_item(
                      id TEXT PRIMARY KEY,observed_at TEXT NOT NULL,author TEXT NOT NULL,role TEXT NOT NULL,topic TEXT NOT NULL,
                      symbols_json TEXT NOT NULL,headline TEXT NOT NULL,source TEXT NOT NULL,sentiment_bps INTEGER NOT NULL,
                      access_tier TEXT NOT NULL CHECK(access_tier IN ('yesterday','realtime')))""");
                statement.executeUpdate("""
                    CREATE TABLE IF NOT EXISTS mcp_delivery(
                      run_id TEXT PRIMARY KEY,request_hash TEXT NOT NULL,intent_id TEXT NOT NULL,settlement_json TEXT NOT NULL,
                      response_json TEXT,status TEXT NOT NULL CHECK(status IN ('settled','delivered')),
                      failure_injections INTEGER NOT NULL DEFAULT 0 CHECK(failure_injections IN (0,1)),updated_at INTEGER NOT NULL)""");
            }
            seed();
        } catch (SQLException error) {
            throw new IllegalStateException("initialize MarketPulse database", error);
        }
    }

    private void seed() throws SQLException {
        int count;
        try (var statement = connection.createStatement(); var row = statement.executeQuery("SELECT COUNT(*) FROM feed_item")) {
            count = row.getInt(1);
        }
        if (count == 100) return;
        if (count != 0) throw new IllegalStateException("MarketPulse feed seed must contain exactly 100 rows, found " + count);
        connection.setAutoCommit(false);
        try (var insert = connection.prepareStatement("INSERT INTO feed_item VALUES(?,?,?,?,?,?,?,?,?,?)")) {
            for (int index = 0; index < 100; index++) {
                String[] actor = ACTORS[index % 10];
                String[] topic = TOPICS[(index / 10) % 10];
                boolean realtime = index >= 50;
                insert.setString(1, "marketpulse-%03d".formatted(index));
                insert.setString(2, "2026-10-%02dT%02d:%02d:00Z".formatted(realtime ? 15 : 14, index % 24, index % 60));
                insert.setString(3, actor[0]); insert.setString(4, actor[1]); insert.setString(5, topic[0]);
                insert.setString(6, encode(List.of(topic[2])));
                insert.setString(7, actor[0] + " " + topic[1] + "; markets assess geopolitical and tariff implications");
                insert.setString(8, realtime ? "MarketPulse live wire" : "MarketPulse public archive");
                insert.setInt(9, (index - 50) * 7); insert.setString(10, realtime ? "realtime" : "yesterday");
                insert.addBatch();
            }
            insert.executeBatch();
            connection.commit();
        } catch (RuntimeException | SQLException error) {
            connection.rollback();
            throw error;
        } finally {
            connection.setAutoCommit(true);
        }
    }

    public synchronized List<NewsItem> news(String tier, int limit) {
        List<NewsItem> result = new ArrayList<>();
        try (var statement = connection.prepareStatement("""
                SELECT id,observed_at,author,role,topic,symbols_json,headline,source,sentiment_bps
                FROM feed_item WHERE access_tier=? ORDER BY id LIMIT ?""")) {
            statement.setString(1, tier); statement.setInt(2, limit);
            try (var rows = statement.executeQuery()) {
                while (rows.next()) {
                    result.add(new NewsItem(rows.getString(1), rows.getString(2), rows.getString(3), rows.getString(4),
                            rows.getString(5), JSON.readValue(rows.getString(6), JSON.getTypeFactory().constructCollectionType(List.class, String.class)),
                            rows.getString(7), rows.getString(8), rows.getInt(9)));
                }
            }
            return result;
        } catch (Exception error) {
            throw new IllegalStateException("query MarketPulse feed", error);
        }
    }

    public synchronized DeliveryState delivery(String runId, String requestHash) {
        try (var statement = connection.prepareStatement("""
                SELECT request_hash,intent_id,COALESCE(response_json,''),status FROM mcp_delivery WHERE run_id=?""")) {
            statement.setString(1, runId);
            try (var row = statement.executeQuery()) {
                if (!row.next()) return new DeliveryState(null, null);
                if (!requestHash.equals(row.getString(1)) || !("settled".equals(row.getString(4)) || "delivered".equals(row.getString(4)))) {
                    throw new IllegalStateException("MCP run_id conflicts with durable delivery");
                }
                String response = row.getString(3);
                return new DeliveryState(row.getString(2), response.isBlank() ? null : response);
            }
        } catch (SQLException error) {
            throw new IllegalStateException("read MCP delivery", error);
        }
    }

    public synchronized void recordSettled(String runId, String requestHash, String intentId) {
        try (var statement = connection.prepareStatement("""
                INSERT INTO mcp_delivery(run_id,request_hash,intent_id,settlement_json,response_json,status,updated_at)
                VALUES(?,?,?,?,NULL,'settled',?) ON CONFLICT(run_id) DO NOTHING""")) {
            statement.setString(1, runId); statement.setString(2, requestHash); statement.setString(3, intentId);
            statement.setString(4, encode(java.util.Map.of("intentId", intentId, "runId", runId)));
            statement.setLong(5, Instant.now().getEpochSecond()); statement.executeUpdate();
            if (!intentId.equals(delivery(runId, requestHash).intentId())) {
                throw new IllegalStateException("MCP run_id is already bound to another settlement");
            }
        } catch (SQLException error) {
            throw new IllegalStateException("record MCP settlement", error);
        }
    }

    public synchronized void recordDelivered(String runId, String requestHash, String response) {
        if (update("UPDATE mcp_delivery SET response_json=?,status='delivered',updated_at=? "
                + "WHERE run_id=? AND request_hash=? AND status='settled'", response, runId, requestHash) != 1) {
            throw new IllegalStateException("MCP delivery was not in settled state");
        }
    }

    public synchronized boolean recordFailure(String runId, String requestHash) {
        return update("UPDATE mcp_delivery SET response_json=response_json,failure_injections=1,updated_at=? "
                + "WHERE run_id=? AND request_hash=? AND status='settled' AND failure_injections=0",
                null, runId, requestHash) == 1;
    }

    private int update(String sql, String response, String runId, String requestHash) {
        try (var statement = connection.prepareStatement(sql)) {
            int offset = response == null ? 0 : 1;
            if (response != null) statement.setString(1, response);
            statement.setLong(1 + offset, Instant.now().getEpochSecond());
            statement.setString(2 + offset, runId); statement.setString(3 + offset, requestHash);
            return statement.executeUpdate();
        } catch (SQLException error) {
            throw new IllegalStateException("update MCP delivery", error);
        }
    }

    private static String encode(Object value) {
        try { return JSON.writeValueAsString(value); }
        catch (JsonProcessingException error) { throw new IllegalStateException(error); }
    }

    @Override public void close() throws SQLException { connection.close(); }
}
