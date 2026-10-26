// Package protocol owns payment-protocol HTTP wire mappings. It never owns
// wallet keys, policy, settlement authority, or transport connections.
package protocol

import (
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"sync"

	"github.com/a2aproject/a2a-go/a2a"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
)

type HTTPResponse struct {
	Headers     map[string]string
	ContentType string
	Body        []byte
}

type MerchantEvidence struct {
	HeaderName  string
	HeaderValue string
}

// MCPMetadata is one protocol-owned entry in an MCP _meta object.
// Framework adapters preserve every unrelated metadata entry.
type MCPMetadata struct {
	Key   string
	Value any
}

// MCPPaymentRequired contains exactly one JSON-RPC result or error payload.
// x402 uses Result; MPP uses Error.
type MCPPaymentRequired struct {
	Result any
	Error  any
}

type Adapter interface {
	Name() string
	CredentialHeaderNames() []string
	Credential(model.PaymentCredential) (model.HTTPCredential, error)
	MerchantEvidence(map[string]string) (*MerchantEvidence, error)
	PresentedCredential(map[string]string, int64) (any, error)
	Challenge(*model.PaymentChallenge) (HTTPResponse, error)
	Receipt(*model.SettledIncomingPayment) (HTTPResponse, error)
}

// EvidenceHeaderAdapter optionally declares protocol-owned HTTP settlement
// headers. Keeping this as a capability interface preserves compatibility for
// existing custom HTTP-only Adapter implementations.
type EvidenceHeaderAdapter interface {
	MerchantEvidenceHeaderNames() []string
}

// MCPAdapter is optional so existing custom HTTP-only providers remain wire
// compatible. Built-in x402 and MPP providers implement this interface.
type MCPAdapter interface {
	Adapter
	MCPRequestMetadataKey() string
	MCPChallenge(any, int64) (*HTTPResponse, error)
	MCPCredential(model.PaymentCredential) (MCPMetadata, error)
	PresentedMCPCredential(any, int64) (any, error)
	MCPPaymentRequired(*model.PaymentChallenge) (MCPPaymentRequired, error)
	MCPPaymentRejected(*model.PaymentChallenge, string) (MCPPaymentRequired, error)
	MCPReceipt(*model.PaymentChallenge, *model.SettledIncomingPayment) (MCPMetadata, error)
	MCPMerchantEvidence(any, int64) (*MerchantEvidence, error)
}

type MCPReceiptKeyAdapter interface {
	MCPReceiptMetadataKey() string
}

// A2AAdapter projects non-streaming A2A 0.3 JSON-RPC messages onto the same
// Vault contracts as HTTP and MCP. Task ownership remains with the host's A2A
// TaskStore; payment providers own only their metadata.
type A2AAdapter interface {
	Adapter
	A2AExtension() string
	A2AChallenge(map[string]any, int64) (*HTTPResponse, error)
	A2AApplyCredential(map[string]any, model.PaymentCredential) error
	PresentedA2ACredential(map[string]any, int64) (any, error)
	A2AStripCredential(map[string]any) error
	A2APaymentRequired(*model.PaymentChallenge, *a2a.Task, bool) (MCPPaymentRequired, error)
	A2AInjectReceipt(map[string]any, *model.PaymentChallenge, *model.SettledIncomingPayment, int64) error
	A2AMerchantEvidence(map[string]any, int64) (*MerchantEvidence, error)
}

// a2aWire validates carrier objects with the official SDK while preserving
// unrelated extension metadata in the original JSON-RPC envelope.
type a2aWire struct{}

func (a2aWire) requestMetadata(envelope map[string]any) (map[string]any, error) {
	params, _ := envelope["params"].(map[string]any)
	message, _ := params["message"].(map[string]any)
	if envelope["method"] != "message/send" || message == nil {
		return nil, errors.New("only A2A message/send is supported")
	}
	meta, ok := message["metadata"].(map[string]any)
	if message["metadata"] != nil && !ok {
		return nil, errors.New("A2A message metadata must be an object")
	}
	if meta == nil {
		meta = map[string]any{}
		message["metadata"] = meta
	}
	return meta, nil
}

func (a2aWire) rootMetadata(envelope map[string]any) (map[string]any, error) {
	meta, ok := envelope["_meta"].(map[string]any)
	if envelope["_meta"] != nil && !ok {
		return nil, errors.New("JSON-RPC root _meta must be an object")
	}
	if meta == nil {
		meta = map[string]any{}
		envelope["_meta"] = meta
	}
	return meta, nil
}

func (a2aWire) task(value map[string]any, limit int64) (*a2a.Task, error) {
	encoded, err := json.Marshal(value)
	if err != nil || int64(len(encoded)) > limit || value["kind"] != "task" {
		return nil, errors.New("A2A requires a bounded Task response")
	}
	var task a2a.Task
	if err := json.Unmarshal(encoded, &task); err != nil || task.ID == "" || task.ContextID == "" || task.Status.State == "" {
		return nil, errors.New("A2A response has an invalid Task binding")
	}
	return &task, nil
}

// MCPRootRequestKeyAdapter declares the protocol credential key that may also
// appear in JSON-RPC root `_meta`. Protocols without that transport rule do
// not implement this capability.
type MCPRootRequestKeyAdapter interface {
	MCPRootRequestMetadataKey() string
}

type AdapterFactory func() Adapter

type Registry struct {
	adapters map[string]Adapter
	priority []string
}

func (registry *Registry) Get(name string) (Adapter, error) {
	if registry == nil {
		return nil, errors.New("payment protocol registry is not configured")
	}
	adapter, exists := registry.adapters[name]
	if !exists {
		return nil, fmt.Errorf("payment protocol is not enabled: %s", name)
	}
	return adapter, nil
}

func (registry *Registry) MCP(name string) (MCPAdapter, error) {
	adapter, err := registry.Get(name)
	if err != nil {
		return nil, err
	}
	mcp, ok := adapter.(MCPAdapter)
	if !ok {
		return nil, fmt.Errorf("payment protocol does not support MCP: %s", name)
	}
	return mcp, nil
}

func (registry *Registry) A2A(name string) (A2AAdapter, error) {
	adapter, err := registry.Get(name)
	if err != nil {
		return nil, err
	}
	a2a, ok := adapter.(A2AAdapter)
	if !ok {
		return nil, fmt.Errorf("payment protocol does not support A2A: %s", name)
	}
	return a2a, nil
}

func (registry *Registry) A2AChallenge(message map[string]any, limit int64) (A2AAdapter, *HTTPResponse, error) {
	for _, name := range registry.priority {
		adapter, err := registry.A2A(name)
		if err != nil {
			continue
		}
		challenge, err := adapter.A2AChallenge(message, limit)
		if err != nil {
			return nil, nil, err
		}
		if challenge != nil {
			return adapter, challenge, nil
		}
	}
	return nil, nil, nil
}

func (registry *Registry) PresentedA2ACredential(message map[string]any, limit int64) (string, any, error) {
	var selected string
	var payload any
	for _, name := range registry.priority {
		adapter, err := registry.A2A(name)
		if err != nil {
			continue
		}
		value, err := adapter.PresentedA2ACredential(message, limit)
		if err != nil {
			return "", nil, err
		}
		if value == nil {
			continue
		}
		if selected != "" {
			return "", nil, errors.New("multiple A2A payment credentials")
		}
		selected, payload = name, value
	}
	return selected, payload, nil
}

func (registry *Registry) StripA2ACredentials(message map[string]any) error {
	for _, name := range registry.priority {
		adapter, err := registry.A2A(name)
		if err != nil {
			continue
		}
		if err := adapter.A2AStripCredential(message); err != nil {
			return err
		}
	}
	return nil
}

// CredentialHeaderNames returns the case-insensitive union owned by enabled
// protocol providers. Framework integrations use it only to preserve duplicate
// detection; providers still decide whether a value is a payment credential.
func (registry *Registry) CredentialHeaderNames() []string {
	names := make([]string, 0, len(registry.priority)*2)
	for _, name := range registry.priority {
		adapter, err := registry.Get(name)
		if err != nil {
			continue
		}
		for _, candidate := range adapter.CredentialHeaderNames() {
			duplicate := false
			for _, existing := range names {
				if strings.EqualFold(existing, candidate) {
					duplicate = true
					break
				}
			}
			if !duplicate {
				names = append(names, candidate)
			}
		}
	}
	return names
}

// MerchantEvidenceHeaderNames returns the case-insensitive union of response
// headers owned by enabled providers.
func (registry *Registry) MerchantEvidenceHeaderNames() []string {
	names := make([]string, 0, len(registry.priority))
	for _, name := range registry.priority {
		adapter, err := registry.Get(name)
		if err != nil {
			continue
		}
		owner, ok := adapter.(EvidenceHeaderAdapter)
		if !ok {
			continue
		}
		for _, candidate := range owner.MerchantEvidenceHeaderNames() {
			duplicate := false
			for _, existing := range names {
				if strings.EqualFold(existing, candidate) {
					duplicate = true
					break
				}
			}
			if !duplicate {
				names = append(names, candidate)
			}
		}
	}
	return names
}

// PresentedCredential detects exactly one enabled HTTP protocol credential.
func (registry *Registry) PresentedCredential(headers map[string]string, limit int64) (string, any, error) {
	var selected string
	var payload any
	for _, name := range registry.priority {
		adapter, err := registry.Get(name)
		if err != nil {
			return "", nil, err
		}
		value, err := adapter.PresentedCredential(headers, limit)
		if err != nil {
			return "", nil, err
		}
		if value == nil {
			continue
		}
		if selected != "" {
			return "", nil, errors.New("multiple payment protocol credentials were supplied")
		}
		selected, payload = name, value
	}
	return selected, payload, nil
}

// MerchantEvidence detects exactly one enabled HTTP protocol receipt.
func (registry *Registry) MerchantEvidence(headers map[string]string) (string, *MerchantEvidence, error) {
	var selected string
	var evidence *MerchantEvidence
	for _, name := range registry.priority {
		adapter, err := registry.Get(name)
		if err != nil {
			return "", nil, err
		}
		value, err := adapter.MerchantEvidence(headers)
		if err != nil {
			return "", nil, err
		}
		if value == nil {
			continue
		}
		if selected != "" {
			return "", nil, errors.New("multiple payment protocol receipts were supplied")
		}
		selected, evidence = name, value
	}
	return selected, evidence, nil
}

// MCPChallenge resolves message-level challenges in configured priority
// order. A message can trigger at most one protocol and therefore one payment.
func (registry *Registry) MCPChallenge(message any, limit int64) (MCPAdapter, *HTTPResponse, error) {
	if registry == nil {
		return nil, nil, errors.New("payment protocol registry is not configured")
	}
	for _, name := range registry.priority {
		adapter, err := registry.MCP(name)
		if err != nil {
			continue
		}
		challenge, err := adapter.MCPChallenge(message, limit)
		if err != nil {
			return nil, nil, err
		}
		if challenge != nil {
			return adapter, challenge, nil
		}
	}
	return nil, nil, nil
}

// PresentedMCPCredential detects at most one enabled protocol credential. It
// rejects malformed or ambiguous metadata before any value reaches Vault.
func (registry *Registry) PresentedMCPCredential(meta any, limit int64) (string, any, error) {
	var selected string
	var payload any
	for _, name := range registry.priority {
		adapter, err := registry.MCP(name)
		if err != nil {
			continue
		}
		value, err := adapter.PresentedMCPCredential(meta, limit)
		if err != nil {
			return "", nil, err
		}
		if value == nil {
			continue
		}
		if selected != "" {
			return "", nil, errors.New("multiple MCP payment credentials were supplied")
		}
		selected, payload = name, value
	}
	return selected, payload, nil
}

// MCPMerchantEvidence detects at most one protocol receipt in a result.
func (registry *Registry) MCPMerchantEvidence(meta any, limit int64) (string, *MerchantEvidence, error) {
	var selected string
	var evidence *MerchantEvidence
	for _, name := range registry.priority {
		adapter, err := registry.MCP(name)
		if err != nil {
			continue
		}
		value, err := adapter.MCPMerchantEvidence(meta, limit)
		if err != nil {
			return "", nil, err
		}
		if value == nil {
			continue
		}
		if selected != "" {
			return "", nil, errors.New("multiple MCP settlement receipts were supplied")
		}
		selected, evidence = name, value
	}
	return selected, evidence, nil
}

func (registry *Registry) MCPRequestMetadataKeys() []string {
	keys := make([]string, 0, len(registry.priority))
	for _, name := range registry.priority {
		if adapter, err := registry.MCP(name); err == nil {
			keys = append(keys, adapter.MCPRequestMetadataKey())
		}
	}
	return keys
}

func (registry *Registry) MCPRootRequestMetadataKeys() []string {
	keys := make([]string, 0, len(registry.priority))
	for _, name := range registry.priority {
		if adapter, err := registry.MCP(name); err == nil {
			if owner, ok := adapter.(MCPRootRequestKeyAdapter); ok {
				keys = append(keys, owner.MCPRootRequestMetadataKey())
			}
		}
	}
	return keys
}

func (registry *Registry) MCPReceiptMetadataKeys() []string {
	keys := make([]string, 0, len(registry.priority))
	for _, name := range registry.priority {
		if adapter, err := registry.MCP(name); err == nil {
			if owner, ok := adapter.(MCPReceiptKeyAdapter); ok {
				keys = append(keys, owner.MCPReceiptMetadataKey())
			}
		}
	}
	return keys
}

// PaymentProtocolFactory creates isolated provider instances. Registration is
// concurrency-safe and Create never caches a failed or partially built set.
type PaymentProtocolFactory struct {
	mutex     sync.RWMutex
	providers map[string]AdapterFactory
}

func NewPaymentProtocolFactory() *PaymentProtocolFactory {
	return &PaymentProtocolFactory{providers: map[string]AdapterFactory{
		"x402": func() Adapter { return &X402Adapter{} },
		"mpp":  func() Adapter { return &MppAdapter{} },
	}}
}

func (factory *PaymentProtocolFactory) Register(name string, provider AdapterFactory) error {
	if factory == nil || provider == nil || name == "" || name != strings.ToLower(name) {
		return errors.New("payment protocol provider registration is invalid")
	}
	factory.mutex.Lock()
	defer factory.mutex.Unlock()
	if _, exists := factory.providers[name]; exists {
		return fmt.Errorf("payment protocol provider is already registered: %s", name)
	}
	factory.providers[name] = provider
	return nil
}

func (factory *PaymentProtocolFactory) Create(enabled []string) (*Registry, error) {
	if factory == nil || len(enabled) == 0 {
		return nil, errors.New("payment protocol configuration is empty")
	}
	factory.mutex.RLock()
	defer factory.mutex.RUnlock()
	adapters := make(map[string]Adapter, len(enabled))
	priority := make([]string, 0, len(enabled))
	for _, name := range enabled {
		provider, exists := factory.providers[name]
		if !exists {
			return nil, fmt.Errorf("payment protocol provider is unavailable: %s", name)
		}
		if _, duplicate := adapters[name]; duplicate {
			return nil, fmt.Errorf("duplicate payment protocol provider: %s", name)
		}
		adapter := provider()
		if adapter == nil || adapter.Name() != name {
			return nil, fmt.Errorf("payment protocol provider identity mismatch: %s", name)
		}
		adapters[name] = adapter
		priority = append(priority, name)
	}
	return &Registry{adapters: adapters, priority: priority}, nil
}
