package config_test

import (
	"strings"
	"testing"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/config"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
)

func TestRoleConfigurationValidatesLimitsAndRedactsEdgeSecrets(t *testing.T) {
	payer := config.PayerConfig{
		ScenarioID: "scenario", WalletID: "wallet", Protocol: "x402",
		Asset:  model.AssetIdentity{Network: "eip155:1", Asset: "USDC", Decimals: 6},
		Amount: "100", Recipient: "recipient",
	}
	payer.ApplyDefaults()
	if err := payer.Validate(); err != nil {
		t.Fatal(err)
	}
	payer.DeliveryRetries = 4
	if err := payer.Validate(); err == nil {
		t.Fatal("unsafe retry limit must be rejected")
	}
	grpc := config.GrpcConfig{Endpoint: "vault.example:443", TLS: true, AuthguardBearer: "edge-secret"}
	grpc.ApplyDefaults()
	if strings.Contains(grpc.String(), "edge-secret") {
		t.Fatal("edge bearer leaked")
	}
}
