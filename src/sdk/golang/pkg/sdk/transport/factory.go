package transport

import (
	"errors"

	"github.com/wl4g-blockchain/agentvault/src/sdk/golang/pkg/sdk/config"
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
	return NewHttpVaultTransport(*settings.Http)
}
