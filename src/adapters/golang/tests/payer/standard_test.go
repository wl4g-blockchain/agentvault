package payer_test

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/config"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	. "github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/payer"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/protocol"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/transport"
)

func TestStandardPayerMiddlewareRetriesPaidRequest(t *testing.T) {
	requirements := map[string]any{
		"scheme": "exact", "network": "eip155:31337", "amount": "1000",
		"payTo": "0x0000000000000000000000000000000000000001", "maxTimeoutSeconds": 60,
		"asset": "0x0000000000000000000000000000000000000002",
		"extra": map[string]any{"name": "USDC", "version": "2"},
	}
	requiredBody, _ := json.Marshal(map[string]any{
		"x402Version": 2, "resource": map[string]any{"url": "mcp://tool/test"}, "accepts": []any{requirements},
	})
	payloadBody, _ := json.Marshal(map[string]any{
		"x402Version": 2, "accepted": requirements, "payload": map[string]any{
			"signature": "0x" + strings.Repeat("ab", 65),
			"authorization": map[string]any{
				"from": "0x0000000000000000000000000000000000000003",
				"to":   "0x0000000000000000000000000000000000000001", "value": "1000",
				"validAfter": "0", "validBefore": "2000000000", "nonce": "0x" + strings.Repeat("42", 32),
			},
		},
	})
	paymentSignature := base64.StdEncoding.EncodeToString(payloadBody)
	settlementBody, _ := json.Marshal(map[string]any{
		"success": true, "payer": "0x0000000000000000000000000000000000000003",
		"transaction": "0x1111", "network": "eip155:31337",
	})
	paymentResponse := base64.StdEncoding.EncodeToString(settlementBody)
	merchant := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		if request.Header.Get("payment-signature") == "" {
			writer.Header().Set("payment-protocol", "x402")
			writer.WriteHeader(http.StatusPaymentRequired)
			_, _ = writer.Write(requiredBody)
			return
		}
		writer.Header().Set("payment-response", paymentResponse)
		writer.WriteHeader(http.StatusOK)
	}))
	defer merchant.Close()

	core, err := NewPayerMiddleware(payerTransport(func(_ context.Context, request transport.TransportRequest) (transport.TransportResponse, error) {
		if request.Path == "/api/v1/payments/authorize" {
			body, marshalErr := json.Marshal(map[string]any{
				"authorization": map[string]any{"outcome": "authorized", "intent_id": "intent-1"},
				"intent": map[string]any{
					"id": "intent-1", "scenario_id": "scenario", "run_id": "run-1", "wallet_id": "wallet",
					"policy_id": "policy", "direction": "outbound", "protocol": "x402",
					"asset":     map[string]any{"network": "eip155:31337", "asset": "USDC", "decimals": 6},
					"recipient": "0x0000000000000000000000000000000000000001", "amount": "1000",
					"resource": merchant.URL + "/mcp", "challenge_hash": "challenge",
				},
				"credential": map[string]any{
					"protocol": "x402", "challenge_hash": "challenge", "credential_hash": "hash", "payload": map[string]any{},
					"http": map[string]any{"bound_origin": merchant.URL, "header_name": "payment-signature", "header_value": paymentSignature},
				},
			})
			if marshalErr != nil {
				t.Fatal(marshalErr)
			}
			return transport.TransportResponse{Status: 200, Body: body}, nil
		}
		if strings.HasSuffix(request.Path, "/outbound-response") {
			return transport.TransportResponse{Status: 200, Body: []byte(`{"intent":{"id":"intent-1"},"accepted":true,"replayed":false}`)}, nil
		}
		return transport.TransportResponse{Status: 500, Body: []byte(`{"error":{"code":"UNEXPECTED","message":"unexpected"}}`)}, nil
	}), config.PayerConfig{ScenarioID: "scenario"})
	if err != nil {
		t.Fatal(err)
	}
	standard := core.Standard(nil)
	if standard.PayerMiddleware != core || standard.Config().ScenarioID != "scenario" {
		t.Fatal("standard adapter does not embed the shared payer middleware")
	}
	request, _ := http.NewRequest(http.MethodPost, merchant.URL+"/mcp", strings.NewReader(`{}`))
	operation, _ := model.NewPayerRequestContext("run-1")
	response, err := standard.Client().Do(WithPayment(request, operation))
	if err != nil {
		t.Fatal(err)
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		t.Fatalf("unexpected status %d", response.StatusCode)
	}
	if payment, ok := PaymentFromResponse(response); !ok || payment.Intent.ID != "intent-1" {
		t.Fatalf("trusted payment metadata missing: %#v", payment)
	}
}

func TestStandardPayerMiddlewareCompletesOfficialMCPFlows(t *testing.T) {
	fixtureBody, err := os.ReadFile(filepath.Join("..", "..", "..", "tests", "payment_protocol_mcp.json"))
	if err != nil {
		t.Fatal(err)
	}
	var fixture map[string]any
	if err := json.Unmarshal(fixtureBody, &fixture); err != nil {
		t.Fatal(err)
	}
	for _, protocolName := range []string{"x402", "mpp"} {
		t.Run(protocolName, func(t *testing.T) {
			entry := fixture["protocols"].(map[string]any)[protocolName].(map[string]any)
			credentialMeta := entry["credential_meta"].(map[string]any)
			receiptMeta := entry["receipt_meta"].(map[string]any)
			credentialKey := "x402/payment"
			if protocolName == "mpp" {
				credentialKey = "org.paymentauth/credential"
			}
			var calls int
			merchant := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
				calls++
				body, readErr := io.ReadAll(request.Body)
				if readErr != nil {
					t.Fatal(readErr)
				}
				var message map[string]any
				if json.Unmarshal(body, &message) != nil {
					t.Fatalf("invalid MCP request: %s", body)
				}
				params := message["params"].(map[string]any)
				meta, _ := params["_meta"].(map[string]any)
				writer.Header().Set("Content-Type", "application/json")
				if meta[credentialKey] == nil {
					_ = json.NewEncoder(writer).Encode(entry["challenge_response"])
					return
				}
				if protocolName == "mpp" && request.Header.Get("Authorization") != "Bearer application-token" {
					t.Fatal("MPP MCP retry did not preserve application Bearer authorization")
				}
				_ = json.NewEncoder(writer).Encode(map[string]any{
					"jsonrpc": "2.0", "id": float64(1),
					"result": map[string]any{"content": []any{}, "_meta": receiptMeta},
				})
			}))
			defer merchant.Close()

			credentialValue := credentialMeta[credentialKey]
			credentialJSON, marshalErr := json.Marshal(credentialValue)
			if marshalErr != nil {
				t.Fatal(marshalErr)
			}
			headerName := "payment-signature"
			headerValue := base64.StdEncoding.EncodeToString(credentialJSON)
			if protocolName == "mpp" {
				headerName = "payment-authorization"
				native, err := (&protocol.MppAdapter{}).PresentedMCPCredential(credentialMeta, 64*1024)
				if err != nil {
					t.Fatal(err)
				}
				headerValue = native.(map[string]any)["authorization"].(string)
			}
			var reported bool
			core, coreErr := NewPayerMiddleware(payerTransport(func(_ context.Context, request transport.TransportRequest) (transport.TransportResponse, error) {
				switch {
				case request.Path == "/api/v1/payments/authorize":
					body, marshalErr := json.Marshal(map[string]any{
						"authorization": map[string]any{"outcome": "authorized", "intent_id": "intent-1"},
						"intent": map[string]any{
							"id": "intent-1", "scenario_id": "scenario", "run_id": "run-1", "wallet_id": "wallet",
							"policy_id": "policy", "direction": "outbound", "protocol": protocolName,
							"asset":     map[string]any{"network": "eip155:42431", "asset": "USDC", "decimals": 6},
							"recipient": "0x1111111111111111111111111111111111111111", "amount": "1000",
							"resource": merchant.URL + "/mcp", "challenge_hash": "challenge",
						},
						"credential": map[string]any{
							"protocol": protocolName, "challenge_hash": "challenge", "credential_hash": "hash",
							"payload": map[string]any{}, "http": map[string]any{
								"bound_origin": merchant.URL, "header_name": headerName, "header_value": headerValue,
							},
						},
					})
					if marshalErr != nil {
						t.Fatal(marshalErr)
					}
					return transport.TransportResponse{Status: 200, Body: body}, nil
				case strings.HasSuffix(request.Path, "/outbound-response"):
					reported = true
					return transport.TransportResponse{Status: 200, Body: []byte(`{"intent":{"id":"intent-1"},"accepted":true,"replayed":false}`)}, nil
				default:
					return transport.TransportResponse{Status: 500, Body: []byte(`{"error":{"code":"UNEXPECTED","message":"unexpected"}}`)}, nil
				}
			}), config.PayerConfig{ScenarioID: "scenario"})
			if coreErr != nil {
				t.Fatal(coreErr)
			}
			request, _ := http.NewRequest(
				http.MethodPost,
				merchant.URL+"/mcp",
				strings.NewReader(`{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"marketpulse.realtime","arguments":{}}}`),
			)
			request.Header.Set("Content-Type", "application/json")
			request.Header.Set("Authorization", "Bearer application-token")
			operation, _ := model.NewPayerRequestContext("run-1")
			response, requestErr := core.Standard(nil).Client().Do(WithPayment(request, operation))
			if requestErr != nil {
				t.Fatal(requestErr)
			}
			defer response.Body.Close()
			if response.StatusCode != http.StatusOK || calls != 2 || !reported {
				t.Fatalf("incomplete MCP flow: status=%d calls=%d reported=%v", response.StatusCode, calls, reported)
			}
		})
	}
}
