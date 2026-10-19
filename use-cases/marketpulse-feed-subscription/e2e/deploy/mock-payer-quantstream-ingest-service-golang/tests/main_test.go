package main_test

import (
	"os/exec"
	"strings"
	"testing"
)

func TestMainRejectsUnknownArguments(t *testing.T) {
	command := exec.Command("go", "run", "..", "--unknown")
	output, err := command.CombinedOutput()
	if err == nil || !strings.Contains(string(output), "flag provided but not defined") {
		t.Fatalf("unexpected command result: err=%v output=%s", err, output)
	}
}
