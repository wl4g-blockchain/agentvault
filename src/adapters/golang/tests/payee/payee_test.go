package payee_test

import (
	"context"
	"errors"
	"testing"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/config"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	. "github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/payee"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/transport"
)

type payeeTransport func(context.Context, transport.TransportRequest) (transport.TransportResponse, error)

func (function payeeTransport) Do(ctx context.Context, request transport.TransportRequest) (transport.TransportResponse, error) {
	return function(ctx, request)
}

func TestPayeeMiddlewareRejectsInvalidPaymentBeforeSettlement(t *testing.T) {
	paths := make([]string, 0, 2)
	core, err := NewPayeeMiddleware(payeeTransport(func(_ context.Context, request transport.TransportRequest) (transport.TransportResponse, error) {
		paths = append(paths, request.Path)
		switch request.Path {
		case "/api/v1/payee/challenges":
			return transport.TransportResponse{Status: 200, Body: []byte(`{"scenario_id":"scenario","offer_id":"offer","resource":"mcp://marketpulse/realtime","protocol":"x402","asset":{"network":"eip155:31337","asset":"USDC","decimals":6},"amount":"1000","recipient":"0x1","nonce":"nonce","challenge_hash":"challenge","expires_at":1,"payload":{"payment_required":{}}}`)}, nil
		case "/api/v1/payee/verify":
			return transport.TransportResponse{Status: 200, Body: []byte(`{"intent":{"id":"intent","scenario_id":"scenario","run_id":"run","offer_id":"offer","direction":"inbound","protocol":"x402","asset":{"network":"eip155:31337","asset":"USDC","decimals":6},"recipient":"0x1","amount":"1000","settled_amount":"0","resource":"mcp://marketpulse/realtime","challenge_hash":"challenge","idempotency_key":"execution.challenge","request_hash":"request-hash","status":"pending_settlement","reservation_status":"reserved","reserved_amount":"1000","created_at":1,"updated_at":1},"valid":false}`)}, nil
		default:
			t.Fatalf("unexpected Vault call after invalid verification: %s", request.Path)
			return transport.TransportResponse{}, nil
		}
	}), config.PayeeConfig{})
	if err != nil {
		t.Fatal(err)
	}
	_, err = core.Protect(context.Background(), model.PayeeOperation{
		ScenarioID: "scenario", RunID: "run", ExecutionKey: "execution", OfferID: "offer",
		Resource: "mcp://marketpulse/realtime", Origin: "https://marketpulse.example",
		Protocol: "x402", RequestHash: "request-hash", CredentialPayload: map[string]any{"signature": "present"},
	})
	if !errors.Is(err, ErrInvalidPayment) {
		t.Fatalf("expected invalid payment, got %v", err)
	}
	if len(paths) != 2 || paths[0] != "/api/v1/payee/challenges" || paths[1] != "/api/v1/payee/verify" {
		t.Fatalf("unexpected payee state machine: %#v", paths)
	}
}

func TestPayeeMiddlewareCreatesBoundChallenge(t *testing.T) {
	core, err := NewPayeeMiddleware(payeeTransport(func(_ context.Context, request transport.TransportRequest) (transport.TransportResponse, error) {
		if request.Path != "/api/v1/payee/challenges" {
			t.Fatalf("unexpected path %s", request.Path)
		}
		return transport.TransportResponse{Status: 200, Body: []byte(`{"scenario_id":"scenario","offer_id":"offer","resource":"mcp://marketpulse/realtime","protocol":"x402","asset":{"network":"eip155:31337","asset":"USDC","decimals":6},"amount":"1000","recipient":"0x1","nonce":"n","challenge_hash":"hash","expires_at":1,"payload":{"payment_required":{}}}`)}, nil
	}), config.PayeeConfig{})
	if err != nil {
		t.Fatal(err)
	}
	challenge, err := core.Challenge(context.Background(), model.PayeeOperation{
		ScenarioID: "scenario", RunID: "run", ExecutionKey: "execution", OfferID: "offer",
		Resource: "mcp://marketpulse/realtime", Origin: "https://marketpulse.example",
		Protocol: "x402", RequestHash: "hash",
	})
	if err != nil || challenge.ChallengeHash != "hash" {
		t.Fatalf("challenge failed: %#v %v", challenge, err)
	}
	_ = model.PaymentChallenge{}
}
