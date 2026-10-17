package agentvault_test

import (
	"context"
	"testing"

	. "github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/config"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/transport"
)

type facadeTransport func(context.Context, transport.TransportRequest) (transport.TransportResponse, error)

func (function facadeTransport) Do(ctx context.Context, request transport.TransportRequest) (transport.TransportResponse, error) {
	return function(ctx, request)
}

func TestAgentVaultBuildsBothRoleMiddlewares(t *testing.T) {
	vault := New(facadeTransport(func(context.Context, transport.TransportRequest) (transport.TransportResponse, error) {
		return transport.TransportResponse{}, nil
	}))
	payer, err := vault.Payer(config.PayerConfig{ScenarioID: "scenario"})
	if err != nil || payer == nil {
		t.Fatalf("build payer middleware: %v", err)
	}
	payee, err := vault.Payee(config.PayeeConfig{})
	if err != nil || payee == nil {
		t.Fatalf("build payee middleware: %v", err)
	}
}
