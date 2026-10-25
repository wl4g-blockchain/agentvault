package job_test

import (
	"context"
	"testing"

	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-service-golang/pkg/job"
)

func TestIngestionJobFailsClosedWithoutVaultConfiguration(t *testing.T) {
	t.Setenv("AGENTVAULT_MQTT_PORT", "")
	ingestion, err := job.NewFromEnv(context.Background())
	if err == nil || ingestion != nil {
		t.Fatalf("missing Vault configuration was accepted: job=%v err=%v", ingestion, err)
	}
}
