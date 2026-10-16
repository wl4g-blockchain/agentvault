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

func TestPayerMiddlewareReusesBodyAndHeaderIdempotency(t *testing.T) {
	key, _ := model.NewIdempotencyKey("run-1:paid")
	operation := model.PayerRequestContext{RunID: "run-1", IdempotencyKey: key}
	middleware, err := NewPayerMiddleware(payerTransport(func(_ context.Context, request transport.TransportRequest) (transport.TransportResponse, error) {
		if request.Headers["idempotency-key"] != string(key) {
			t.Fatalf("missing idempotency header: %#v", request.Headers)
		}
		var body map[string]any
		if err := json.Unmarshal(request.Body, &body); err != nil {
			t.Fatal(err)
		}
		if body["idempotency_key"] != string(key) {
			t.Fatalf("body and header keys differ: %s", request.Body)
		}
		return transport.TransportResponse{Status: 200, Body: []byte(`{"authorization":{"outcome":"denied","intent_id":"intent-1","reason":"policy"},"intent":{"id":"intent-1"}}`)}, nil
	}), config.PayerConfig{
		ScenarioID: "scenario", WalletID: "wallet", Protocol: "x402",
		Asset:  model.AssetIdentity{Network: "eip155:31337", Asset: "USDC", Decimals: 6},
		Amount: "1000", Recipient: "0x0000000000000000000000000000000000000001",
	})
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

func TestPayerMiddlewareRejectsProtocolHintMismatchBeforeVaultCall(t *testing.T) {
	called := false
	middleware, err := NewPayerMiddleware(payerTransport(func(_ context.Context, _ transport.TransportRequest) (transport.TransportResponse, error) {
		called = true
		return transport.TransportResponse{}, nil
	}), config.PayerConfig{
		ScenarioID: "scenario", WalletID: "wallet", Protocol: "x402",
		Asset:  model.AssetIdentity{Network: "eip155:31337", Asset: "USDC", Decimals: 6},
		Amount: "1000", Recipient: "0x0000000000000000000000000000000000000001",
	})
	if err != nil {
		t.Fatal(err)
	}
	operation, err := model.NewPayerRequestContext("run-1", "paid")
	if err != nil {
		t.Fatal(err)
	}
	_, err = middleware.Authorize(context.Background(), model.PayerHTTPRequest{
		Method: "POST", URL: "https://merchant.example/mcp", Origin: "https://merchant.example",
	}, model.PayerHTTPResponse{
		Status: 402, Headers: map[string]string{"payment-protocol": "mpp"}, Body: []byte(`{}`),
	}, operation)
	if err == nil || called {
		t.Fatalf("protocol mismatch was not rejected locally: err=%v called=%v", err, called)
	}
}
