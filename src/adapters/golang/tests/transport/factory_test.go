package transport_test

import (
	"testing"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/config"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/transport"
)

func TestVaultTransportFactoryCreatesIsolatedIdempotentlyClosableProviders(t *testing.T) {
	factory := transport.NewVaultTransportFactory()
	settings := config.VaultTransportConfig{
		Provider: config.TransportProviderMqtt,
		Mqtt: &config.MqttConfig{
			Host: "127.0.0.1", Port: 1883, Username: "client", Password: "secret",
			ConnectionPrefix: "agent", VaultClientID: "vault-client",
		},
	}
	first, err := factory.Create(settings)
	if err != nil {
		t.Fatal(err)
	}
	second, err := factory.Create(settings)
	if err != nil {
		t.Fatal(err)
	}
	if first == second {
		t.Fatal("transport factory reused a provider instance")
	}
	if err := first.Close(); err != nil {
		t.Fatal(err)
	}
	if err := first.Close(); err != nil {
		t.Fatalf("second close was not idempotent: %v", err)
	}

	invalid := settings
	invalid.Grpc = &config.GrpcConfig{Endpoint: "127.0.0.1:50051"}
	if _, err := factory.Create(invalid); err == nil {
		t.Fatal("mixed transport provider blocks were accepted")
	}
}
