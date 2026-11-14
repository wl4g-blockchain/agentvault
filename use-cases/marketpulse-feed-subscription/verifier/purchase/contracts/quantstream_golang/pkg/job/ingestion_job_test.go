package job_test

import (
	"context"
	"testing"

	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/deploy/mock-payer-quantstream-service-golang/source/pkg/job"
)

func TestIngestionJobFailsClosedWithoutVaultConfiguration(t *testing.T) {
	t.Setenv("AGENTVAULT_HTTP_BASE_URL", "")
	ingestion, err := job.NewFromEnv(context.Background())
	if err == nil || ingestion != nil {
		t.Fatalf("missing Vault configuration was accepted: job=%v err=%v", ingestion, err)
	}
}
