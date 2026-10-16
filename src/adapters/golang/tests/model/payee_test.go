package model_test

import (
	"testing"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
)

func TestPayeePhaseKeysAreBoundToOneExecution(t *testing.T) {
	operation := model.PayeeOperation{
		ScenarioID: "scenario", RunID: "run", ExecutionKey: "run.realtime", OfferID: "offer",
		Resource: "mcp://marketpulse/realtime", Origin: "https://marketpulse.test", Protocol: "x402", RequestHash: "hash",
	}
	if err := operation.Validate(); err != nil {
		t.Fatal(err)
	}
	challenge, _ := operation.ChallengeContext()
	settlement, _ := operation.SettlementContext()
	if challenge.IdempotencyKey != "run.realtime.challenge" || settlement.IdempotencyKey != "run.realtime.settle" {
		t.Fatal("phase keys are not stable and distinct")
	}
}
