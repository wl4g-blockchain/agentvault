package payer_test

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"strings"
	"testing"

	mpp "github.com/tempoxyz/mpp-go/pkg/mpp"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/config"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	. "github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/payer"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/transport"
)

type payerTransport func(context.Context, transport.TransportRequest) (transport.TransportResponse, error)

func (function payerTransport) Do(ctx context.Context, request transport.TransportRequest) (transport.TransportResponse, error) {
	return function(ctx, request)
}

func TestPayerMiddlewareUsesRunIDAsOnlyReplayIdentity(t *testing.T) {
	operation := model.PayerRequestContext{RunID: "run-1"}
	middleware, err := NewPayerMiddleware(payerTransport(func(_ context.Context, request transport.TransportRequest) (transport.TransportResponse, error) {
		var body map[string]any
		if err := json.Unmarshal(request.Body, &body); err != nil {
			t.Fatal(err)
		}
		if body["run_id"] != "run-1" {
			t.Fatalf("unexpected replay identity: %s", request.Body)
		}
		if len(body) != 3 || body["scenario_id"] != "scenario" || body["challenge"] == nil {
			t.Fatalf("authorization wire must contain only scenario_id, run_id, and challenge: %s", request.Body)
		}
		return transport.TransportResponse{Status: 200, Body: []byte(`{"authorization":{"outcome":"denied","intent_id":"intent-1","reason":"policy"},"intent":{"id":"intent-1"}}`)}, nil
	}), config.PayerConfig{ScenarioID: "scenario"})
	if err != nil {
		t.Fatal(err)
	}
	_, err = middleware.Authorize(context.Background(), model.PayerHTTPRequest{
		Method: "POST", URL: "https://merchant.example/mcp", Origin: "https://merchant.example",
	}, model.PayerHTTPResponse{
		Status: 402, Headers: map[string]string{"payment-protocol": "x402"}, Body: []byte(`{}`),
	}, operation)
	if err == nil {
		t.Fatal("expected policy denial after the public authorization call")
	}
}

func TestPayerMiddlewareForwardsCapturedProtocolHintWithoutLocalProjection(t *testing.T) {
	called := false
	middleware, err := NewPayerMiddleware(payerTransport(func(_ context.Context, request transport.TransportRequest) (transport.TransportResponse, error) {
		called = true
		var body map[string]any
		if err := json.Unmarshal(request.Body, &body); err != nil {
			t.Fatal(err)
		}
		challenge := body["challenge"].(map[string]any)
		if challenge["protocol_hint"] != "mpp" {
			t.Fatalf("captured hint missing: %s", request.Body)
		}
		return transport.TransportResponse{Status: 409, Body: []byte(`{"error":{"code":"PAYMENT_CONFLICT","message":"conflict"}}`)}, nil
	}), config.PayerConfig{ScenarioID: "scenario"})
	if err != nil {
		t.Fatal(err)
	}
	operation, err := model.NewPayerRequestContext("run-1")
	if err != nil {
		t.Fatal(err)
	}
	_, err = middleware.Authorize(context.Background(), model.PayerHTTPRequest{
		Method: "POST", URL: "https://merchant.example/mcp", Origin: "https://merchant.example",
	}, model.PayerHTTPResponse{
		Status: 402, Headers: map[string]string{"payment-protocol": "mpp"}, Body: []byte(`{}`),
	}, operation)
	if err == nil || !called {
		t.Fatalf("captured protocol hint was not forwarded: err=%v called=%v", err, called)
	}
}

func TestPayerMiddlewareForwardsNativeChallengeHeadersToAgentVault(t *testing.T) {
	for _, testCase := range []struct {
		name   string
		header string
		value  string
	}{
		{name: "x402", header: "payment-required", value: "eyJ4NDAyVmVyc2lvbiI6Mn0="},
		{name: "mpp", header: "www-authenticate", value: `Payment id="charge", method="tempo"`},
	} {
		t.Run(testCase.name, func(t *testing.T) {
			called := false
			middleware, err := NewPayerMiddleware(payerTransport(func(_ context.Context, request transport.TransportRequest) (transport.TransportResponse, error) {
				called = true
				var body map[string]any
				if err := json.Unmarshal(request.Body, &body); err != nil {
					t.Fatal(err)
				}
				challenge := body["challenge"].(map[string]any)
				headers := challenge["headers"].(map[string]any)
				if headers[testCase.header] != testCase.value {
					t.Fatalf("%s challenge header was not preserved: %s", testCase.name, request.Body)
				}
				return transport.TransportResponse{Status: 409, Body: []byte(`{"error":{"code":"PAYMENT_CONFLICT","message":"conflict"}}`)}, nil
			}), config.PayerConfig{ScenarioID: "scenario"})
			if err != nil {
				t.Fatal(err)
			}
			operation, err := model.NewPayerRequestContext("run-1")
			if err != nil {
				t.Fatal(err)
			}
			_, err = middleware.Authorize(context.Background(), model.PayerHTTPRequest{
				Method: "POST", URL: "https://merchant.example/mcp", Origin: "https://merchant.example",
			}, model.PayerHTTPResponse{
				Status: 402, Headers: map[string]string{testCase.header: testCase.value}, Body: []byte(`{}`),
			}, operation)
			if err == nil || !called {
				t.Fatalf("native %s challenge did not reach AgentVault: err=%v called=%v", testCase.name, err, called)
			}
		})
	}
}

func TestPayerMiddlewareRejectsMCPChallengeWithDifferentJSONRPCIdentity(t *testing.T) {
	called := false
	middleware, err := NewPayerMiddleware(payerTransport(func(context.Context, transport.TransportRequest) (transport.TransportResponse, error) {
		called = true
		return transport.TransportResponse{}, nil
	}), config.PayerConfig{ScenarioID: "scenario"})
	if err != nil {
		t.Fatal(err)
	}
	operation, err := model.NewPayerRequestContext("run-1")
	if err != nil {
		t.Fatal(err)
	}
	request := model.PayerHTTPRequest{
		Method: "POST", URL: "https://merchant.example/mcp", Origin: "https://merchant.example",
		Body: []byte(`{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"marketpulse.realtime"}}`),
	}
	for _, response := range []string{
		`{"jsonrpc":"2.0","id":2,"result":{"isError":true,"structuredContent":{"x402Version":2,"accepts":[]}}}`,
		`{"jsonrpc":"1.0","id":1,"result":{"isError":true,"structuredContent":{"x402Version":2,"accepts":[]}}}`,
	} {
		payment, authorizeErr := middleware.AuthorizeMCP(
			context.Background(), request,
			model.PayerHTTPResponse{Status: 200, Body: []byte(response)}, operation,
		)
		if authorizeErr != nil || payment != nil {
			t.Fatalf("mismatched MCP response was not ignored: payment=%#v err=%v", payment, authorizeErr)
		}
	}
	if called {
		t.Fatal("mismatched MCP response reached AgentVault")
	}
}

func TestPayerReportsMPPReceiptThroughProtocolEvidence(t *testing.T) {
	called := false
	receipt := mpp.FormatReceipt(mpp.Success("0x1111", mpp.WithReceiptMethod("tempo")))
	middleware, err := NewPayerMiddleware(payerTransport(func(_ context.Context, request transport.TransportRequest) (transport.TransportResponse, error) {
		called = true
		var body map[string]any
		if err := json.Unmarshal(request.Body, &body); err != nil {
			t.Fatal(err)
		}
		if body["protocol"] != "mpp" || body["payment_response"] != nil {
			t.Fatalf("MPP receipt used the x402-only contract: %s", request.Body)
		}
		evidence := body["evidence"].(map[string]any)
		if evidence["header_name"] != "payment-receipt" || evidence["header_value"] != receipt {
			t.Fatalf("invalid protocol evidence: %s", request.Body)
		}
		return transport.TransportResponse{Status: 200, Body: []byte(`{"intent":{"id":"intent-1"},"accepted":true,"replayed":false}`)}, nil
	}), config.PayerConfig{ScenarioID: "scenario"})
	if err != nil {
		t.Fatal(err)
	}
	payment := &model.AuthorizedPayment{
		Intent:     model.PaymentIntent{ID: "intent-1", Protocol: "mpp"},
		Credential: model.PaymentCredential{Protocol: "mpp"},
	}
	if err := middleware.Report(context.Background(), model.PayerRequestContext{RunID: "run-1"}, payment,
		model.PayerHTTPResponse{Status: 200, Headers: map[string]string{"Payment-Receipt": receipt}}); err != nil {
		t.Fatal(err)
	}
	if !called {
		t.Fatal("MPP receipt was not reported")
	}
}

func TestMPPCredentialUsesDedicatedHeaderWithoutOverwritingBearer(t *testing.T) {
	middleware, err := NewPayerMiddleware(payerTransport(func(context.Context, transport.TransportRequest) (transport.TransportResponse, error) {
		return transport.TransportResponse{}, nil
	}), config.PayerConfig{ScenarioID: "scenario"})
	if err != nil {
		t.Fatal(err)
	}
	challenge := mpp.NewChallenge(
		"0123456789abcdef0123456789abcdef", "merchant.example", "tempo", "charge",
		map[string]any{"amount": "1000", "currency": "USDC"},
	)
	authorization := payerCredentialWithDedicatedHeader(t,
		challenge.NewCredential(map[string]any{"type": "hash", "hash": "0x1111"}).ToAuthorization())
	payment := &model.AuthorizedPayment{Credential: model.PaymentCredential{
		Protocol: "mpp",
		HTTP: &model.HTTPCredential{
			BoundOrigin: "https://merchant.example", HeaderName: "Payment-Authorization",
			HeaderValue: authorization,
		},
	}}
	name, _, err := middleware.CredentialHeader(payment, "https://merchant.example", map[string]string{
		"Authorization": "Bearer application-token",
	})
	if err != nil || name != "Payment-Authorization" {
		t.Fatalf("dedicated MPP header was rejected: name=%q err=%v", name, err)
	}
	payment.Credential.HTTP.HeaderName = "Authorization"
	if _, _, err := middleware.CredentialHeader(payment, "https://merchant.example", map[string]string{
		"Authorization": "Bearer application-token",
	}); err == nil {
		t.Fatal("MPP credential overwrote an existing Bearer header")
	}
}

func payerCredentialWithDedicatedHeader(t *testing.T, authorization string) string {
	t.Helper()
	raw, err := base64.RawURLEncoding.DecodeString(strings.TrimSpace(strings.TrimPrefix(authorization, "Payment ")))
	if err != nil {
		t.Fatal(err)
	}
	var envelope map[string]any
	if err := json.Unmarshal(raw, &envelope); err != nil {
		t.Fatal(err)
	}
	challenge, ok := envelope["challenge"].(map[string]any)
	if !ok {
		t.Fatal("MPP credential challenge is missing")
	}
	challenge["header"] = "Payment-Authorization"
	raw, err = json.Marshal(envelope)
	if err != nil {
		t.Fatal(err)
	}
	return "Payment " + base64.RawURLEncoding.EncodeToString(raw)
}
