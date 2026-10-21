package transport

import (
	"errors"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/config"
)

// VaultTransport is an owned Transport created by VaultTransportFactory.
// Injected Transport values remain caller-owned and need not implement Close.
type VaultTransport interface {
	Transport
	Close() error
}

// VaultTransportFactory creates a fresh provider for every call and never
// caches failed connection state or credentials across clients.
type VaultTransportFactory struct{}

func NewVaultTransportFactory() *VaultTransportFactory { return &VaultTransportFactory{} }

func (factory *VaultTransportFactory) Create(settings config.VaultTransportConfig) (VaultTransport, error) {
	if factory == nil {
		return nil, errors.New("AgentVault transport factory is not configured")
	}
	settings.ApplyDefaults()
	if err := settings.Validate(); err != nil {
		return nil, err
	}
	switch settings.Provider {
	case config.TransportProviderGrpc:
		return NewGrpcVaultTransport(*settings.Grpc)
	case config.TransportProviderMqtt:
		return NewMqttVaultTransport(*settings.Mqtt)
	default:
		return nil, errors.New("unsupported AgentVault transport provider")
	}
}
