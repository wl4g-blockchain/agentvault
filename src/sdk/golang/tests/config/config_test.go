package config_test

import (
	"strings"
	"testing"

	"github.com/wl4g-blockchain/agentvault/src/sdk/golang/pkg/sdk/config"
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
	http := config.HttpConfig{BaseURL: "https://vault.example", AuthguardBearer: "edge-secret"}
	http.ApplyDefaults()
	if strings.Contains(http.String(), "edge-secret") {
		t.Fatal("edge bearer leaked")
	}
}
