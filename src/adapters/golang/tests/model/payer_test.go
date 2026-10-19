package model_test

import (
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
