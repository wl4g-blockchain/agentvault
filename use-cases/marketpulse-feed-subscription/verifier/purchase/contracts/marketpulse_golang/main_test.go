package main_test

import (
	"os"
	"os/exec"
	"strings"
	"testing"
)

func TestMainFailsClosedWithoutRuntimeConfiguration(t *testing.T) {
	command := exec.Command("go", "run", "github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/deploy/mock-payee-marketpulse-golang-service/source")
	command.Env = append(os.Environ(), "AGENTVAULT_TRANSPORT=http", "AGENTVAULT_HTTP_BASE_URL=")
	output, err := command.CombinedOutput()
	if err == nil || !strings.Contains(string(output), "AGENTVAULT_HTTP_BASE_URL is required") {
		t.Fatalf("unexpected command result: err=%v output=%s", err, output)
	}
}
