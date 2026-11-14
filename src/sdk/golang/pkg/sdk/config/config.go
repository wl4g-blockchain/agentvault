package config

import (
	"errors"
	"fmt"
	"github.com/wl4g-blockchain/agentvault/src/sdk/golang/pkg/sdk/model"
	"net/url"
	"strings"
	"time"
)

type HttpConfig struct {
	BaseURL          string
	AuthguardBearer  string
	RequestTimeout   time.Duration
	MaxResponseBytes int
}

func (config *HttpConfig) ApplyDefaults() {
	if config.RequestTimeout == 0 {
		config.RequestTimeout = 30 * time.Second
	}
	if config.MaxResponseBytes == 0 {
		config.MaxResponseBytes = 2 * 1024 * 1024
	}
}
func (config HttpConfig) Validate() error {
	u, e := url.Parse(config.BaseURL)
	if e != nil || u.Host == "" || (u.Scheme != "http" && u.Scheme != "https") || u.User != nil || u.RawQuery != "" || u.Fragment != "" || (u.Path != "" && u.Path != "/") || config.RequestTimeout <= 0 || config.MaxResponseBytes <= 0 || strings.ContainsAny(config.AuthguardBearer, "\r\n") {
		return errors.New("invalid AgentVault HTTP configuration")
	}
	return nil
}
func (config HttpConfig) String() string {
	return fmt.Sprintf("HttpConfig{BaseURL:%q AuthguardBearer:<redacted>}", config.BaseURL)
}

type TransportProvider string

const TransportProviderHttp TransportProvider = "http"

type VaultTransportConfig struct {
	Provider TransportProvider `json:"provider" yaml:"provider"`
	Http     *HttpConfig       `json:"http" yaml:"http"`
}

func (config *VaultTransportConfig) ApplyDefaults() {
	if config != nil && config.Http != nil {
		config.Http.ApplyDefaults()
	}
}
func (config VaultTransportConfig) Validate() error {
	if config.Provider != TransportProviderHttp || config.Http == nil {
		return errors.New("transport provider must be http")
	}
	return config.Http.Validate()
}

const defaultMaxChallengeBytes int64 = 256 * 1024

// PayerConfig is stable for one application integration. A workflow supplies
// only its stable run context for each logical request.
type PayerConfig struct {
	ScenarioID           string
	Protocols            []string
	ApprovalTimeout      time.Duration
	ApprovalPollInterval time.Duration
	DeliveryRetries      int
	MaxChallengeBytes    int64
}

func (config *PayerConfig) ApplyDefaults() {
	if config.Protocols == nil {
		config.Protocols = []string{"x402", "mpp"}
	}
	if config.ApprovalTimeout == 0 {
		config.ApprovalTimeout = 2 * time.Minute
	}
	if config.ApprovalPollInterval == 0 {
		config.ApprovalPollInterval = time.Second
	}
	if config.MaxChallengeBytes == 0 {
		config.MaxChallengeBytes = defaultMaxChallengeBytes
	}
}

func (config PayerConfig) Validate() error {
	if err := model.Required("scenario_id", config.ScenarioID); err != nil {
		return err
	}
	if config.ApprovalTimeout <= 0 || config.ApprovalPollInterval <= 0 ||
		config.ApprovalPollInterval > config.ApprovalTimeout || config.DeliveryRetries < 0 ||
		config.DeliveryRetries > 3 || config.MaxChallengeBytes <= 0 {
		return errors.New("payer middleware retry, timeout, or challenge limits are invalid")
	}
	if err := protocolNames(config.Protocols).validate(); err != nil {
		return err
	}
	return nil
}

type PayeeConfig struct {
	MaxBodyBytes int64
	Protocols    []string
}

func (config *PayeeConfig) ApplyDefaults() {
	if config.Protocols == nil {
		config.Protocols = []string{"x402", "mpp"}
	}
	if config.MaxBodyBytes == 0 {
		config.MaxBodyBytes = 1024 * 1024
	}
}

func (config PayeeConfig) Validate() error {
	if config.MaxBodyBytes <= 0 {
		return errors.New("payee middleware body limit must be positive")
	}
	return protocolNames(config.Protocols).validate()
}

type protocolNames []string

func (protocols protocolNames) validate() error {
	if len(protocols) == 0 {
		return errors.New("at least one payment protocol must be enabled")
	}
	seen := make(map[string]struct{}, len(protocols))
	for _, name := range protocols {
		if len(name) == 0 || len(name) > 32 {
			return fmt.Errorf("invalid payment protocol name: %s", name)
		}
		for _, character := range name {
			if (character < 'a' || character > 'z') && (character < '0' || character > '9') && character != '-' {
				return fmt.Errorf("invalid payment protocol name: %s", name)
			}
		}
		if _, exists := seen[name]; exists {
			return fmt.Errorf("duplicate payment protocol: %s", name)
		}
		seen[name] = struct{}{}
	}
	return nil
}

func (config HttpConfig) GoString() string { return config.String() }
