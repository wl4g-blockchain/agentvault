package model_test

import (
	"encoding/json"
	"testing"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
)

func TestApprovalWireValuesAreStableSnakeCase(t *testing.T) {
	encoded, err := json.Marshal(model.ApprovalStatusPending)
	if err != nil || string(encoded) != `"pending"` {
		t.Fatalf("unexpected approval wire value: %s %v", encoded, err)
	}
	if model.ApprovalDecisionApprove != "approve" || model.ApprovalStatusConsumed != "consumed" {
		t.Fatal("approval contract values changed")
	}
}
