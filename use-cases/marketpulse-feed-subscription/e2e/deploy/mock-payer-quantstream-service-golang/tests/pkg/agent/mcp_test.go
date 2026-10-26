package agent_test

import (
	"context"
	"testing"

	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-service-golang/pkg/agent"
)

func TestMCPHelperRequiresExplicitRuntimeConfiguration(t *testing.T) {
	t.Setenv("AGENTVAULT_MQTT_PORT", "")
	helper, err := agent.NewMCPHelperFromEnv(context.Background())
	if err == nil || helper != nil {
		t.Fatalf("missing runtime configuration was accepted: helper=%v err=%v", helper, err)
	}
}

func TestToolResultSeparatesBusinessBodyFromPaymentEvidence(t *testing.T) {
	result := agent.ToolResult{Body: []byte(`{"paid":true}`), PaymentIntentID: "intent-go-1"}
	if string(result.Body) != `{"paid":true}` || result.PaymentIntentID != "intent-go-1" {
		t.Fatalf("unexpected result: %#v", result)
	}
}
