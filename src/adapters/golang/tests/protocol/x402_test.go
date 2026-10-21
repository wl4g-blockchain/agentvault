package protocol_test

import (
	"encoding/base64"
	"encoding/json"
	"testing"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/protocol"
)

func TestX402AdapterUsesNativeWireHeaders(t *testing.T) {
	wire := map[string]any{
		"x402Version": float64(2),
		"resource":    map[string]any{"url": "https://merchant.example/tool", "mimeType": "application/json"},
		"accepts": []any{map[string]any{
			"scheme": "exact", "network": "eip155:42431", "amount": "1000",
			"payTo": "0x1111111111111111111111111111111111111111", "maxTimeoutSeconds": float64(60),
			"asset": "0x2222222222222222222222222222222222222222",
		}},
	}
	challenge, err := (&protocol.X402Adapter{}).Challenge(&model.PaymentChallenge{
		Protocol: "x402", Payload: map[string]any{"payment_required": wire},
	})
	if err != nil {
		t.Fatal(err)
	}
	decoded, err := base64.StdEncoding.DecodeString(challenge.Headers["Payment-Required"])
	if err != nil {
		t.Fatal(err)
	}
	var actual map[string]any
	if err := json.Unmarshal(decoded, &actual); err != nil || actual["x402Version"] != float64(2) {
		t.Fatalf("invalid x402 challenge: %s", decoded)
	}
	if challenge.Headers["WWW-Authenticate"] != "" {
		t.Fatal("x402 challenge used an MPP header")
	}
}
