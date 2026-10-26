package payer_test

import (
	"context"
	"encoding/json"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/config"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/payer"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/transport"
	"os"
	"strings"
	"testing"
)

func TestA2ARequiresAcknowledgementBeforeAuthorization(t *testing.T) {
	raw, err := os.ReadFile("../../../tests/payment_protocol_a2a.json")
	if err != nil {
		t.Fatal(err)
	}
	var fixture struct {
		Request   json.RawMessage `json:"request"`
		Extension string          `json:"x402_extension"`
		Protocols map[string]struct {
			Challenge json.RawMessage `json:"challenge_response"`
		} `json:"protocols"`
	}
	if err = json.Unmarshal(raw, &fixture); err != nil {
		t.Fatal(err)
	}
	core, err := payer.NewPayerMiddleware(payerTransport(func(context.Context, transport.TransportRequest) (transport.TransportResponse, error) {
		t.Fatal("Vault must not be called")
		return transport.TransportResponse{}, nil
	}), config.PayerConfig{ScenarioID: "scenario"})
	if err != nil {
		t.Fatal(err)
	}
	_, err = core.AuthorizeA2A(context.Background(), model.PayerHTTPRequest{Method: "POST", URL: "https://merchant.test/a2a", Origin: "https://merchant.test", Body: fixture.Request}, model.PayerHTTPResponse{Status: 200, Body: fixture.Protocols["x402"].Challenge}, model.PayerRequestContext{RunID: "run-1"})
	if err == nil || !strings.Contains(err.Error(), "acknowledged") {
		t.Fatal("missing extension accepted", err)
	}
	if core.A2AExtensions(fixture.Extension) != fixture.Extension {
		t.Fatal("duplicate extension")
	}
}
