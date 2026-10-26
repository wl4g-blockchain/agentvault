package model_test

import (
	"encoding/json"
	"testing"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
)

func TestPayerContextAndOriginBindingAreStable(t *testing.T) {
	first, err := model.NewPayerRequestContext("run-1")
	if err != nil {
		t.Fatal(err)
	}
	retry, _ := model.NewPayerRequestContext("run-1")
	if first != retry {
		t.Fatal("stable operation context changed")
	}
	if err := model.ValidateURLOrigin("https://merchant.test/mcp", "https://merchant.test"); err != nil {
		t.Fatal(err)
	}
	if err := model.ValidateURLOrigin("https://merchant.test.evil/mcp", "https://merchant.test"); err == nil {
		t.Fatal("cross-origin URL accepted")
	}
}

func TestReceiptReportRequiresProtocolEvidence(t *testing.T) {
	context, err := model.NewOperationContext("scenario", "run")
	if err != nil {
		t.Fatal(err)
	}
	request := model.ReportOutboundResponseRequest{
		OperationContext: context,
		Protocol:         "mpp",
		Evidence:         model.ProtocolEvidence{HeaderName: "payment-receipt", HeaderValue: "receipt"},
	}
	if err := request.Validate(); err != nil {
		t.Fatal(err)
	}
	var wire map[string]any
	encoded, err := json.Marshal(request)
	if err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(encoded, &wire); err != nil {
		t.Fatal(err)
	}
	if wire["protocol"] != "mpp" || wire["evidence"] == nil || wire["payment_response"] != nil {
		t.Fatalf("invalid protocol receipt wire: %s", encoded)
	}
	var rejected model.ReportOutboundResponseRequest
	if err := json.Unmarshal([]byte(`{"scenario_id":"scenario","run_id":"run","payment_response":"receipt"}`), &rejected); err != nil {
		t.Fatal(err)
	}
	if err := rejected.Validate(); err == nil {
		t.Fatal("receipt without protocol evidence was accepted")
	}
}
