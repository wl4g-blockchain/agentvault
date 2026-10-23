package config

import (
	"errors"
	"fmt"
	"net"
	"strings"
	"time"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
)

type GrpcConfig struct {
	Endpoint        string
	TLS             bool
	AuthguardBearer string
	RequestTimeout  time.Duration
	MaxMessageBytes int
}

func (config *GrpcConfig) ApplyDefaults() {
	if config.RequestTimeout == 0 {
		config.RequestTimeout = 30 * time.Second
	}
	if config.MaxMessageBytes == 0 {
		config.MaxMessageBytes = 2 * 1024 * 1024
	}
}

func (config GrpcConfig) Validate() error {
	host, _, err := net.SplitHostPort(config.Endpoint)
	if err != nil || config.RequestTimeout <= 0 || config.MaxMessageBytes <= 0 ||
		strings.ContainsAny(config.AuthguardBearer, "\r\n") ||
		strings.TrimSpace(config.AuthguardBearer) != config.AuthguardBearer ||
		(!config.TLS && !loopback(host)) {
		return errors.New("invalid or unsafe AgentVault gRPC configuration")
	}
	return nil
}

func (config GrpcConfig) String() string {
	return fmt.Sprintf("GrpcConfig{Endpoint:%q TLS:%t AuthguardBearer:<redacted> RequestTimeout:%s MaxMessageBytes:%d}",
		config.Endpoint, config.TLS, config.RequestTimeout, config.MaxMessageBytes)
}

type MqttConfig struct {
	Host             string
	Port             uint16
	TLS              bool
	Username         string
	Password         string
	ConnectionPrefix string
	VaultClientID    string
	RequestTimeout   time.Duration
	MaxResponseBytes int
}

type TransportProvider string

const (
	TransportProviderGrpc TransportProvider = "grpc"
	TransportProviderMqtt TransportProvider = "mqtt"
)

// VaultTransportConfig is the single typed transport selection entry. Exactly
// one provider-specific block must match Provider.
type VaultTransportConfig struct {
	Provider TransportProvider `json:"provider" yaml:"provider"`
	Grpc     *GrpcConfig       `json:"grpc,omitempty" yaml:"grpc,omitempty"`
	Mqtt     *MqttConfig       `json:"mqtt,omitempty" yaml:"mqtt,omitempty"`
}

func (config *VaultTransportConfig) ApplyDefaults() {
	if config == nil {
		return
	}
	if config.Grpc != nil {
		config.Grpc.ApplyDefaults()
	}
	if config.Mqtt != nil {
		config.Mqtt.ApplyDefaults()
	}
}

func (config VaultTransportConfig) Validate() error {
	switch config.Provider {
	case TransportProviderGrpc:
		if config.Grpc == nil || config.Mqtt != nil {
			return errors.New("grpc transport requires only the grpc configuration block")
		}
		return config.Grpc.Validate()
	case TransportProviderMqtt:
		if config.Mqtt == nil || config.Grpc != nil {
			return errors.New("mqtt transport requires only the mqtt configuration block")
		}
		return config.Mqtt.Validate()
	default:
		return errors.New("transport provider must be grpc or mqtt")
	}
}

func (config *MqttConfig) ApplyDefaults() {
	if config.RequestTimeout == 0 {
		config.RequestTimeout = 30 * time.Second
	}
	if config.MaxResponseBytes == 0 {
		config.MaxResponseBytes = 2 * 1024 * 1024
	}
}

func (config MqttConfig) Validate() error {
	if config.Host == "" || config.Port == 0 || config.RequestTimeout <= 0 || config.MaxResponseBytes <= 0 ||
		!identifier(config.Username) || config.Password == "" ||
		!identifier(config.ConnectionPrefix) || !identifier(config.VaultClientID) ||
		(!config.TLS && !loopback(config.Host)) {
		return errors.New("invalid or unsafe AgentVault MQTT configuration")
	}
	return nil
}

func (config MqttConfig) String() string {
	return fmt.Sprintf(
		"MqttConfig{Host:%q Port:%d TLS:%t Username:<redacted> Password:<redacted> ConnectionPrefix:%q VaultClientID:%q RequestTimeout:%s MaxResponseBytes:%d}",
		config.Host, config.Port, config.TLS, config.ConnectionPrefix, config.VaultClientID,
		config.RequestTimeout, config.MaxResponseBytes)
}

func identifier(value string) bool {
	if value == "" || len(value) > 128 {
		return false
	}
	for _, character := range value {
		if (character >= 'a' && character <= 'z') || (character >= 'A' && character <= 'Z') ||
			(character >= '0' && character <= '9') || strings.ContainsRune("-_.", character) {
			continue
		}
		return false
	}
	return true
}

func loopback(host string) bool {
	if strings.EqualFold(host, "localhost") {
		return true
	}
	address := net.ParseIP(strings.Trim(host, "[]"))
	return address != nil && address.IsLoopback()
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
