package config_test

import (
	"strings"
	"testing"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/config"
)

func TestRoleConfigurationValidatesLimitsAndRedactsEdgeSecrets(t *testing.T) {
	payer := config.PayerConfig{ScenarioID: "scenario"}
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
