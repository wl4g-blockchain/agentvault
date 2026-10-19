package payer_test

import (
	"context"
	"encoding/json"
	"testing"

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
