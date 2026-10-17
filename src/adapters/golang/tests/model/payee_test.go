package model_test

import (
	"testing"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
)

func TestPayeePhasesReuseOneRunIdentity(t *testing.T) {
	operation := model.PayeeOperation{
		ScenarioID: "scenario", RunID: "run", OfferID: "offer",
		Resource: "mcp://marketpulse/realtime", Origin: "https://marketpulse.test", Protocol: "x402", RequestHash: "hash",
	}
	if err := operation.Validate(); err != nil {
		t.Fatal(err)
	}
	challenge, _ := operation.ChallengeContext()
	settlement, _ := operation.SettlementContext()
	if challenge.RunID != "run" || settlement.RunID != "run" {
		t.Fatal("payee phases changed the run identity")
	}
}
