package model_test

import (
	"strings"
	"testing"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
)

func TestBaseModelsEnforceStableIdempotencyAndRedactCredentials(t *testing.T) {
	key, err := model.OperationKey("run-1", "realtime")
	if err != nil || key != "run-1:realtime" {
		t.Fatalf("unexpected stable key: %q %v", key, err)
	}
	if _, err := model.NewIdempotencyKey("unsafe/key"); err == nil {
		t.Fatal("unsafe key accepted")
	}
	credential := model.PaymentCredential{Protocol: "x402", ChallengeHash: "challenge-secret", CredentialHash: "credential-secret"}
	if strings.Contains(credential.String(), "challenge-secret") || strings.Contains(credential.String(), "credential-secret") {
		t.Fatal("credential debug value leaked")
	}
}
