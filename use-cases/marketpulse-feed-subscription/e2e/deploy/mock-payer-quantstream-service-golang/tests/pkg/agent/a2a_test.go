package agent_test

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"

	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-service-golang/pkg/agent"
)

func TestA2ASelectionRejectsBusinessResultWithoutPaymentEvidence(t *testing.T) {
	var sends atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		var call map[string]any
		if json.NewDecoder(request.Body).Decode(&call) != nil {
			writer.WriteHeader(400)
			return
		}
		writer.Header().Set("Content-Type", "application/json")
		var result any
		switch call["method"] {
		case "initialize":
			result = map[string]any{"protocolVersion": "2025-11-25", "capabilities": map[string]any{"tools": map[string]any{}}, "serverInfo": map[string]any{"name": "test", "version": "1"}}
		case "notifications/initialized":
			writer.WriteHeader(202)
			return
		case "tools/list":
			result = map[string]any{"tools": []any{map[string]any{"name": "marketpulse.yesterday_news", "inputSchema": map[string]any{"type": "object"}}, map[string]any{"name": "marketpulse.realtime_news", "inputSchema": map[string]any{"type": "object"}}}}
		case "message/send":
			sends.Add(1)
			message := call["params"].(map[string]any)["message"].(map[string]any)
			if request.URL.Path != "/a2a" || request.Header.Get("X-Run-Id") != "run-a2a-1" || message["taskId"] != nil {
				t.Error("A2A endpoint/run/task carrier is invalid")
			}
			result = map[string]any{"kind": "task", "id": "task-1", "contextId": "context-1", "status": map[string]any{"state": "completed"}, "artifacts": []any{map[string]any{"artifactId": "news", "parts": []any{map[string]any{"kind": "data", "data": map[string]any{"paid": true, "news": []any{}, "intentId": "forged"}}}}}}
		default:
			writer.WriteHeader(400)
			return
		}
		_ = json.NewEncoder(writer).Encode(map[string]any{"jsonrpc": "2.0", "id": call["id"], "result": result})
	}))
	defer server.Close()
	for key, value := range map[string]string{"AGENTVAULT_MQTT_TIMEOUT_SECONDS": "2", "AGENTVAULT_APPROVAL_TIMEOUT_SECONDS": "2", "QUANTSTREAM_SCENARIO_ID": "scenario-1", "MARKETPULSE_URL": server.URL + "/mcp", "MARKETPULSE_MESSAGE_PROTOCOL": "a2a", "AGENTVAULT_TRANSPORT": "grpc", "AGENTVAULT_GRPC_ENDPOINT": "127.0.0.1:1", "AGENTVAULT_GRPC_TLS": "false"} {
		t.Setenv(key, value)
	}
	helper, err := agent.NewMCPHelperFromEnv(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	defer helper.Close()
	_, err = helper.RealtimeNews(context.Background(), "run-a2a-1", false)
	if sends.Load() != 1 || err == nil || !strings.Contains(err.Error(), "payment metadata") {
		t.Fatalf("unpaid A2A response accepted: sends=%d err=%v", sends.Load(), err)
	}
}
