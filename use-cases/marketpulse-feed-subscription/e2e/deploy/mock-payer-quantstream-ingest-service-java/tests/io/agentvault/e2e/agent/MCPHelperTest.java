package io.agentvault.e2e.agent;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;

import io.modelcontextprotocol.json.McpJsonDefaults;
import java.nio.charset.StandardCharsets;
import org.junit.jupiter.api.Test;

final class MCPHelperTest {
    @Test void toolResultSeparatesBusinessBodyFromPaymentEvidence() {
        byte[] body = "{\"paid\":true}".getBytes(StandardCharsets.UTF_8);
        var result = new MCPHelper.ToolResult(body, "intent-java-1");
        assertArrayEquals(body, result.body());
        assertEquals("intent-java-1", result.paymentIntentId());
    }

    @Test void officialMcpClientHasAJsonMapperProvider() {
        assertNotNull(McpJsonDefaults.getMapper());
    }
}
