package model_test

import (
	"strings"
	"testing"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
)

func TestBaseModelsEnforceSafeRunIdentityAndRedactCredentials(t *testing.T) {
	context, err := model.NewOperationContext("scenario-1", "run-1")
	if err != nil || context.RunID != "run-1" {
		t.Fatalf("unexpected run identity: %#v %v", context, err)
	}
	if _, err := model.NewOperationContext("scenario-1", "unsafe/run"); err == nil {
		t.Fatal("unsafe run ID accepted")
	}
	credential := model.PaymentCredential{Protocol: "x402", ChallengeHash: "challenge-secret", CredentialHash: "credential-secret"}
	if strings.Contains(credential.String(), "challenge-secret") || strings.Contains(credential.String(), "credential-secret") {
		t.Fatal("credential debug value leaked")
	}
}
