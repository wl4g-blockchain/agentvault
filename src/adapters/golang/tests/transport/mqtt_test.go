package transport_test

import (
	"context"
	"fmt"
	"strings"
	"testing"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/config"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/transport"
)

func TestMqttVaultTransportValidatesBoundaryAndRedactsSecrets(t *testing.T) {
	unsafe := config.MqttConfig{
		Host: "broker.example", Port: 1883, TLS: false, Username: "client", Password: "secret",
		ConnectionPrefix: "worker", VaultClientID: "client-a",
	}
	if _, err := transport.NewMqttVaultTransport(unsafe); err == nil {
		t.Fatal("remote cleartext MQTT must be rejected")
	}

	safe := unsafe
	safe.Host = "127.0.0.1"
	instance, err := transport.NewMqttVaultTransport(safe)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(fmt.Sprint(instance), "secret") || strings.Contains(safe.String(), "secret") {
		t.Fatal("MQTT configuration leaked a secret")
	}
	var async transport.AsyncTransport = instance
	result := <-async.DoAsync(context.Background(), transport.TransportRequest{
		Method: transport.MethodGet,
		Path:   "/unsupported",
	})
	if result.Err == nil {
		t.Fatal("asynchronous MQTT submission must fail unsupported operations")
	}
}
