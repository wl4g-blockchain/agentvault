package payee_test

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/config"
	. "github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/payee"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/protocol"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/transport"
)

func TestStdHttpMiddlewarePassesUnprotectedRoute(t *testing.T) {
	next := http.HandlerFunc(func(writer http.ResponseWriter, _ *http.Request) { writer.WriteHeader(http.StatusNoContent) })
	var middleware *StdHttpMiddleware = NewStdHttpMiddleware(nil)
	recorder := httptest.NewRecorder()
	middleware.Handler(next).ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/health", nil))
	if recorder.Code != http.StatusNoContent {
		t.Fatalf("unexpected status %d", recorder.Code)
	}
}

func TestStdHttpMiddlewareBypassesFreeToolOnProtectedMCPRoute(t *testing.T) {
	next := http.HandlerFunc(func(writer http.ResponseWriter, _ *http.Request) { writer.WriteHeader(http.StatusNoContent) })
	core := &PayeeMiddleware{}
	middleware := NewStdHttpMiddleware(core, NewStandardMCPToolRoute(
		"/mcp", "marketpulse.realtime_news", "scenario", "price",
		"mcp://marketpulse/realtime", "https://marketpulse.example",
	))
	if middleware.PayeeMiddleware != core {
		t.Fatal("standard adapter does not embed the shared payee middleware")
	}
	recorder := httptest.NewRecorder()
	request := httptest.NewRequest(http.MethodPost, "/mcp", strings.NewReader(
		`{"jsonrpc":"2.0","method":"tools/call","params":{"name":"marketpulse.yesterday_news"}}`,
	))
	middleware.Handler(next).ServeHTTP(recorder, request)
	if recorder.Code != http.StatusNoContent {
		t.Fatalf("free MCP tool was intercepted: status=%d body=%s", recorder.Code, recorder.Body.String())
	}
	recorder = httptest.NewRecorder()
	request = httptest.NewRequest(http.MethodPost, "/mcp", strings.NewReader(
		`{"jsonrpc":"1.0","id":1,"method":"tools/call","params":{"name":"marketpulse.realtime"}}`,
	))
	middleware.Handler(next).ServeHTTP(recorder, request)
	if recorder.Code != http.StatusNoContent {
		t.Fatalf("non-2.0 JSON-RPC request was intercepted: status=%d body=%s", recorder.Code, recorder.Body.String())
	}
}

func TestPayeeCoreRejectsDuplicatePaymentAuthorizationBeforeVault(t *testing.T) {
	core := &PayeeMiddleware{}
	_, err := core.PresentedCredential(&protocol.MppAdapter{}, map[string][]string{
		"Authorization":         {"Bearer application-token"},
		"Payment-Authorization": {"Payment first", "Payment second"},
	}, 1024)
	if err == nil || !strings.Contains(err.Error(), "duplicate payment request header") {
		t.Fatalf("duplicate credential did not fail closed: %v", err)
	}
}

func TestStdHttpMiddlewareCompletesOfficialMCPFlows(t *testing.T) {
	fixture := readPayeeMCPFixture(t)
	for _, protocolName := range []string{"x402", "mpp"} {
		t.Run(protocolName, func(t *testing.T) {
			entry := fixture["protocols"].(map[string]any)[protocolName].(map[string]any)
			core, requestHashes, paths := newMCPPayeeCore(t, protocolName, entry, true)
			route := NewStandardMCPToolRouteWithProtocol(
				"/mcp", "marketpulse.realtime", "scenario", "price",
				"mcp://tool/marketpulse.realtime", "https://marketpulse.example", protocolName,
			)
			var handlerCalls int
			next := http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
				handlerCalls++
				if request.Header.Get("Authorization") != "Bearer application-token" {
					t.Fatal("application Bearer authorization was not preserved")
				}
				if payment, ok := PaymentFromRequest(request); !ok || payment.Intent.ID != "intent-1" {
					t.Fatalf("trusted settlement was not attached to request: %#v", payment)
				}
				var forwarded map[string]any
				if err := json.NewDecoder(request.Body).Decode(&forwarded); err != nil {
					t.Fatal(err)
				}
				meta, _ := forwarded["params"].(map[string]any)["_meta"].(map[string]any)
				if meta["x402/payment"] != nil || meta["org.paymentauth/credential"] != nil ||
					meta["application/trace"] != "preserved" {
					t.Fatalf("protocol payment metadata leaked to the business handler: %#v", meta)
				}
				rootMeta, _ := forwarded["_meta"].(map[string]any)
				if rootMeta["org.paymentauth/credential"] != nil {
					t.Fatalf("root MPP payment metadata leaked to the business handler: %#v", rootMeta)
				}
				writer.Header().Set("Content-Type", "application/json")
				_ = json.NewEncoder(writer).Encode(map[string]any{
					"jsonrpc": "2.0", "id": 1,
					"result": map[string]any{"content": []any{}, "_meta": map[string]any{"application/trace": "preserved"}},
				})
			})
			protected := NewStdHttpMiddleware(core, route).Handler(next)

			unpaid := payeeMCPRequest(t, nil)
			unpaidRecorder := httptest.NewRecorder()
			protected.ServeHTTP(unpaidRecorder, unpaid)
			if unpaidRecorder.Code != http.StatusOK || handlerCalls != 0 {
				t.Fatalf("unpaid MCP call was not challenged: status=%d calls=%d body=%s",
					unpaidRecorder.Code, handlerCalls, unpaidRecorder.Body.String())
			}
			if unpaidRecorder.Header().Get("Payment-Protocol") != protocolName ||
				unpaidRecorder.Header().Get("Payment-Expires-At") != "2000000000" {
				t.Fatalf("MCP challenge omitted stable payment profile metadata: %#v", unpaidRecorder.Header())
			}
			var challengeEnvelope map[string]any
			if err := json.Unmarshal(unpaidRecorder.Body.Bytes(), &challengeEnvelope); err != nil {
				t.Fatal(err)
			}
			if protocolName == "x402" {
				result, _ := challengeEnvelope["result"].(map[string]any)
				if result["isError"] != true || result["structuredContent"] == nil {
					t.Fatalf("invalid official x402 MCP challenge: %#v", challengeEnvelope)
				}
			} else {
				paymentError, _ := challengeEnvelope["error"].(map[string]any)
				if paymentError["code"] != float64(-32042) {
					t.Fatalf("invalid official MPP MCP challenge: %#v", challengeEnvelope)
				}
			}

			credential := entry["credential_meta"].(map[string]any)
			paid := payeeMCPRequest(t, credential)
			if protocolName == "mpp" {
				paid = payeeMCPRootRequest(t, credential)
			}
			paidRecorder := httptest.NewRecorder()
			protected.ServeHTTP(paidRecorder, paid)
			if paidRecorder.Code != http.StatusOK || handlerCalls != 1 {
				t.Fatalf("paid MCP call was not released once: status=%d calls=%d body=%s",
					paidRecorder.Code, handlerCalls, paidRecorder.Body.String())
			}
			var paidEnvelope map[string]any
			if err := json.Unmarshal(paidRecorder.Body.Bytes(), &paidEnvelope); err != nil {
				t.Fatal(err)
			}
			result := paidEnvelope["result"].(map[string]any)
			meta := result["_meta"].(map[string]any)
			receiptKey := "x402/payment-response"
			if protocolName == "mpp" {
				receiptKey = "org.paymentauth/receipt"
			}
			if meta[receiptKey] == nil || meta["application/trace"] != "preserved" {
				t.Fatalf("protocol receipt or application metadata missing: %#v", meta)
			}
			if len(*requestHashes) != 2 || (*requestHashes)[0] != (*requestHashes)[1] {
				t.Fatalf("MCP credential changed canonical request binding: %#v", *requestHashes)
			}
			wantPaths := []string{
				"/api/v1/payee/challenges", "/api/v1/payee/challenges",
				"/api/v1/payee/verify", "/api/v1/payee/settle",
			}
			if strings.Join(*paths, ",") != strings.Join(wantPaths, ",") {
				t.Fatalf("unexpected payee state machine: %#v", *paths)
			}
		})
	}
}

func TestStdHttpMiddlewareRejectsAmbiguousMCPPaymentMetadataPlacement(t *testing.T) {
	fixture := readPayeeMCPFixture(t)
	entry := fixture["protocols"].(map[string]any)["mpp"].(map[string]any)
	core, _, paths := newMCPPayeeCore(t, "mpp", entry, true)
	protected := NewStdHttpMiddleware(core, NewStandardMCPToolRouteWithProtocol(
		"/mcp", "marketpulse.realtime", "scenario", "price",
		"mcp://tool/marketpulse.realtime", "https://marketpulse.example", "mpp",
	)).Handler(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {
		t.Fatal("ambiguous MPP credential reached the application handler")
	}))
	credential := entry["credential_meta"].(map[string]any)
	request := payeeMCPRequest(t, credential)
	var message map[string]any
	if err := json.NewDecoder(request.Body).Decode(&message); err != nil {
		t.Fatal(err)
	}
	message["_meta"] = credential
	body, err := json.Marshal(message)
	if err != nil {
		t.Fatal(err)
	}
	request.Body = io.NopCloser(bytes.NewReader(body))
	request.ContentLength = int64(len(body))
	recorder := httptest.NewRecorder()
	protected.ServeHTTP(recorder, request)
	if recorder.Code != http.StatusBadRequest || !strings.Contains(recorder.Body.String(), "duplicate MCP payment") {
		t.Fatalf("ambiguous MPP placement did not fail closed: status=%d body=%s", recorder.Code, recorder.Body.String())
	}
	if len(*paths) != 0 {
		t.Fatalf("ambiguous MPP placement reached AgentVault: %#v", *paths)
	}
}

func TestStdHttpMiddlewarePreservesPaidMCPBusinessFailure(t *testing.T) {
	fixture := readPayeeMCPFixture(t)
	entry := fixture["protocols"].(map[string]any)["x402"].(map[string]any)
	core, _, _ := newMCPPayeeCore(t, "x402", entry, true)
	protected := NewStdHttpMiddleware(core, NewStandardMCPToolRoute(
		"/mcp", "marketpulse.realtime", "scenario", "price",
		"mcp://tool/marketpulse.realtime", "https://marketpulse.example",
	)).Handler(http.HandlerFunc(func(writer http.ResponseWriter, _ *http.Request) {
		writer.Header().Set("Content-Type", "application/json")
		writer.WriteHeader(http.StatusServiceUnavailable)
		_, _ = writer.Write([]byte(`{"jsonrpc":"2.0","id":1,"error":{"code":-32004,"message":"retry delivery"}}`))
	}))
	recorder := httptest.NewRecorder()
	protected.ServeHTTP(recorder, payeeMCPRequest(t, entry["credential_meta"].(map[string]any)))
	if recorder.Code != http.StatusServiceUnavailable || !strings.Contains(recorder.Body.String(), "retry delivery") {
		t.Fatalf("paid MCP business failure was rewritten: status=%d body=%s", recorder.Code, recorder.Body.String())
	}
}

func TestStdHttpMiddlewareReturnsMPPVerificationFailedChallenge(t *testing.T) {
	fixture := readPayeeMCPFixture(t)
	entry := fixture["protocols"].(map[string]any)["mpp"].(map[string]any)
	core, _, paths := newMCPPayeeCore(t, "mpp", entry, false)
	protected := NewStdHttpMiddleware(core, NewStandardMCPToolRouteWithProtocol(
		"/mcp", "marketpulse.realtime", "scenario", "price",
		"mcp://tool/marketpulse.realtime", "https://marketpulse.example", "mpp",
	)).Handler(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {
		t.Fatal("invalid MPP credential reached the application handler")
	}))
	recorder := httptest.NewRecorder()
	protected.ServeHTTP(recorder, payeeMCPRequest(t, entry["credential_meta"].(map[string]any)))
	var envelope map[string]any
	if recorder.Code != http.StatusOK || json.Unmarshal(recorder.Body.Bytes(), &envelope) != nil {
		t.Fatalf("invalid MPP rejection response: status=%d body=%s", recorder.Code, recorder.Body.String())
	}
	paymentError := envelope["error"].(map[string]any)
	if paymentError["code"] != float64(-32043) {
		t.Fatalf("invalid MPP credential did not use verification-failed: %#v", envelope)
	}
	if len(*paths) != 2 || (*paths)[1] != "/api/v1/payee/verify" {
		t.Fatalf("invalid credential reached settlement: %#v", *paths)
	}
}

func payeeMCPRequest(t *testing.T, paymentMeta map[string]any) *http.Request {
	t.Helper()
	meta := map[string]any{"application/trace": "preserved"}
	for key, value := range paymentMeta {
		meta[key] = value
	}
	body, err := json.Marshal(map[string]any{
		"jsonrpc": "2.0", "id": 1, "method": "tools/call",
		"params": map[string]any{"name": "marketpulse.realtime", "arguments": map[string]any{}, "_meta": meta},
	})
	if err != nil {
		t.Fatal(err)
	}
	request := httptest.NewRequest(http.MethodPost, "/mcp", strings.NewReader(string(body)))
	request.Header.Set("Content-Type", "application/json")
	request.Header.Set("Authorization", "Bearer application-token")
	request.Header.Set("X-Run-Id", "run-1")
	return request
}

func payeeMCPRootRequest(t *testing.T, paymentMeta map[string]any) *http.Request {
	t.Helper()
	body, err := json.Marshal(map[string]any{
		"jsonrpc": "2.0", "id": 1, "method": "tools/call", "_meta": paymentMeta,
		"params": map[string]any{
			"name": "marketpulse.realtime", "arguments": map[string]any{},
			"_meta": map[string]any{"application/trace": "preserved"},
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	request := httptest.NewRequest(http.MethodPost, "/mcp", bytes.NewReader(body))
	request.Header.Set("Content-Type", "application/json")
	request.Header.Set("Authorization", "Bearer application-token")
	request.Header.Set("X-Run-Id", "run-1")
	return request
}

func newMCPPayeeCore(
	t *testing.T,
	protocolName string,
	entry map[string]any,
	verificationValid bool,
) (*PayeeMiddleware, *[]string, *[]string) {
	t.Helper()
	challengePayload := map[string]any{}
	if protocolName == "x402" {
		challengePayload["payment_required"] = entry["challenge_response"].(map[string]any)["result"].(map[string]any)["structuredContent"]
	} else {
		mcpChallenge := entry["challenge_response"].(map[string]any)["error"].(map[string]any)["data"].(map[string]any)["challenges"].([]any)[0]
		mcpWire, err := (&protocol.MppAdapter{}).MCPChallenge(entry["challenge_response"], 64*1024)
		if err != nil {
			t.Fatal(err)
		}
		challengePayload["mcp_challenge"] = mcpChallenge
		challengePayload["www_authenticate"] = mcpWire.Headers["www-authenticate"]
	}
	requestHashes := make([]string, 0, 2)
	paths := make([]string, 0, 4)
	core, err := NewPayeeMiddleware(payeeTransport(func(_ context.Context, request transport.TransportRequest) (transport.TransportResponse, error) {
		paths = append(paths, request.Path)
		var input map[string]any
		if err := json.Unmarshal(request.Body, &input); err != nil {
			t.Fatal(err)
		}
		switch request.Path {
		case "/api/v1/payee/challenges":
			requestHash := input["request_hash"].(string)
			requestHashes = append(requestHashes, requestHash)
			return payeeJSONResponse(t, map[string]any{
				"scenario_id": "scenario", "price_id": "price", "resource": "mcp://tool/marketpulse.realtime",
				"protocol": protocolName, "asset": map[string]any{"network": "eip155:42431", "asset": "USDC", "decimals": 6},
				"amount": "1000", "recipient": "0x1111111111111111111111111111111111111111",
				"nonce": "challenge-0123456789abcdef", "challenge_hash": "challenge-hash",
				"expires_at": 2000000000, "payload": challengePayload,
			}), nil
		case "/api/v1/payee/verify":
			requestHash := input["request_hash"].(string)
			response := map[string]any{
				"intent": payeeIntent(protocolName, requestHash, "pending_settlement", "0"),
				"valid":  verificationValid,
			}
			if verificationValid {
				response["verification_ref"] = "verification-1"
			}
			return payeeJSONResponse(t, response), nil
		case "/api/v1/payee/settle":
			requestHash := requestHashes[len(requestHashes)-1]
			intent := payeeIntent(protocolName, requestHash, "settled", "1000")
			return payeeJSONResponse(t, map[string]any{
				"intent": intent,
				"receipt": map[string]any{
					"intent_id": "intent-1", "scenario_id": "scenario", "direction": "inbound",
					"protocol": protocolName, "asset": intent["asset"],
					"recipient": intent["recipient"], "payer": "0x3333333333333333333333333333333333333333",
					"requested_amount": "1000", "settled_amount": "1000", "status": "settled",
					"tx_hash": "0x1111", "settlement_ref": "local-1", "evidence_hash": "evidence-1",
					"confirmed_at": 2000000000,
				},
			}), nil
		default:
			t.Fatalf("unexpected AgentVault path: %s", request.Path)
			return transport.TransportResponse{}, nil
		}
	}), config.PayeeConfig{Protocols: []string{"x402", "mpp"}})
	if err != nil {
		t.Fatal(err)
	}
	return core, &requestHashes, &paths
}

func payeeIntent(protocolName, requestHash, status, settledAmount string) map[string]any {
	return map[string]any{
		"id": "intent-1", "scenario_id": "scenario", "run_id": "run-1", "price_id": "price",
		"direction": "inbound", "protocol": protocolName,
		"asset":     map[string]any{"network": "eip155:42431", "asset": "USDC", "decimals": 6},
		"recipient": "0x1111111111111111111111111111111111111111", "amount": "1000",
		"settled_amount": settledAmount, "resource": "mcp://tool/marketpulse.realtime",
		"challenge_hash": "challenge-hash", "request_hash": requestHash, "status": status,
		"reservation_status": "consumed", "reserved_amount": "1000", "created_at": 1, "updated_at": 2,
	}
}

func payeeJSONResponse(t *testing.T, value any) transport.TransportResponse {
	t.Helper()
	body, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	return transport.TransportResponse{Status: http.StatusOK, Body: body}
}

func readPayeeMCPFixture(t *testing.T) map[string]any {
	t.Helper()
	body, err := os.ReadFile(filepath.Join("..", "..", "..", "tests", "payment_protocol_mcp.json"))
	if err != nil {
		t.Fatal(err)
	}
	var fixture map[string]any
	if err := json.Unmarshal(body, &fixture); err != nil {
		t.Fatal(err)
	}
	return fixture
}
