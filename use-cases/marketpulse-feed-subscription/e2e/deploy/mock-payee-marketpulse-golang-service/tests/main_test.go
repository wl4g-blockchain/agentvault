package main_test

import (
	"os"
	"os/exec"
	"strings"
	"testing"
)

func TestMainFailsClosedWithoutRuntimeConfiguration(t *testing.T) {
	command := exec.Command("go", "run", "..")
	command.Env = append(os.Environ(), "AGENTVAULT_TRANSPORT=mqtt", "AGENTVAULT_MQTT_HOST=")
	output, err := command.CombinedOutput()
	if err == nil || !strings.Contains(string(output), "AGENTVAULT_MQTT_HOST is required") {
		t.Fatalf("unexpected command result: err=%v output=%s", err, output)
	}
}
