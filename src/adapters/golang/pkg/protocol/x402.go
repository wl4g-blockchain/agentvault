package protocol

import (
	"bytes"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"reflect"
	"strings"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	x402 "github.com/x402-foundation/x402/go/v2"
	x402mcp "github.com/x402-foundation/x402/go/v2/mcp"
	x402types "github.com/x402-foundation/x402/go/v2/types"
)

type X402Adapter struct{}

// These private projections decode only the opaque Payload field of the
// official x402types.PaymentPayload envelope. Importing the upstream mechanism
// packages solely for these DTOs would also pull EVM/Solana RPC clients into
// this transport-only SDK boundary.
type x402EIP3009Authorization struct {
	From        string `json:"from"`
	To          string `json:"to"`
	Value       string `json:"value"`
	ValidAfter  string `json:"validAfter"`
	ValidBefore string `json:"validBefore"`
	Nonce       string `json:"nonce"`
}

type x402EIP3009Payload struct {
	Signature     string                   `json:"signature"`
	Authorization x402EIP3009Authorization `json:"authorization"`
}

type x402SolanaExactPayload struct {
	Transaction string `json:"transaction"`
}

func (adapter *X402Adapter) Name() string { return "x402" }

func (adapter *X402Adapter) CredentialHeaderNames() []string { return []string{"payment-signature"} }

func (adapter *X402Adapter) MerchantEvidenceHeaderNames() []string {
	return []string{"payment-response"}
}

func (adapter *X402Adapter) MCPRequestMetadataKey() string { return x402mcp.MCP_PAYMENT_META_KEY }
func (adapter *X402Adapter) MCPReceiptMetadataKey() string {
	return x402mcp.MCP_PAYMENT_RESPONSE_META_KEY
}

func (adapter *X402Adapter) Credential(credential model.PaymentCredential) (model.HTTPCredential, error) {
	if credential.Protocol != adapter.Name() || credential.HTTP == nil ||
		!strings.EqualFold(credential.HTTP.HeaderName, "payment-signature") {
		return model.HTTPCredential{}, errors.New("x402 credential has no PAYMENT-SIGNATURE delivery")
	}
	raw, err := adapter.decodeBase64JSON(credential.HTTP.HeaderValue, 64*1024, "PAYMENT-SIGNATURE")
	if err != nil {
		return model.HTTPCredential{}, err
	}
	if _, err := adapter.paymentPayload(raw, "PAYMENT-SIGNATURE"); err != nil {
		return model.HTTPCredential{}, err
	}
	return *credential.HTTP, nil
}

func (adapter *X402Adapter) MerchantEvidence(headers map[string]string) (*MerchantEvidence, error) {
	value := adapter.header(headers, "payment-response")
	if value == "" {
		return nil, nil
	}
	raw, err := adapter.decodeBase64JSON(value, 64*1024, "PAYMENT-RESPONSE")
	if err != nil {
		return nil, err
	}
	if _, err := adapter.settlement(raw); err != nil {
		return nil, err
	}
	return &MerchantEvidence{HeaderName: "payment-response", HeaderValue: value}, nil
}

func (adapter *X402Adapter) PresentedCredential(headers map[string]string, limit int64) (any, error) {
	encoded := adapter.header(headers, "payment-signature")
	if encoded == "" {
		return nil, nil
	}
	raw, err := adapter.decodeBase64JSON(encoded, limit, "PAYMENT-SIGNATURE")
	if err != nil {
		return nil, err
	}
	return adapter.paymentPayload(raw, "PAYMENT-SIGNATURE")
}

func (adapter *X402Adapter) Challenge(challenge *model.PaymentChallenge) (HTTPResponse, error) {
	required, err := adapter.challengePaymentRequired(challenge)
	if err != nil {
		return HTTPResponse{}, err
	}
	body, err := json.Marshal(required)
	if err != nil {
		return HTTPResponse{}, errors.New("x402 challenge cannot be encoded")
	}
	return HTTPResponse{
		Headers:     map[string]string{"Payment-Required": base64.StdEncoding.EncodeToString(body)},
		ContentType: "application/json",
		Body:        body,
	}, nil
}

func (adapter *X402Adapter) Receipt(payment *model.SettledIncomingPayment) (HTTPResponse, error) {
	settlement, err := adapter.paymentSettlement(payment)
	if err != nil {
		return HTTPResponse{}, err
	}
	body, err := json.Marshal(settlement)
	if err != nil {
		return HTTPResponse{}, err
	}
	return HTTPResponse{Headers: map[string]string{
		"Payment-Response": base64.StdEncoding.EncodeToString(body),
	}}, nil
}

func (adapter *X402Adapter) MCPChallenge(message any, limit int64) (*HTTPResponse, error) {
	encoded, err := json.Marshal(message)
	if err != nil || int64(len(encoded)) > limit {
		return nil, errors.New("MCP x402 message is invalid or oversized")
	}
	var wire struct {
		Result struct {
			IsError           bool            `json:"isError"`
			StructuredContent json.RawMessage `json:"structuredContent"`
			Content           []struct {
				Type string `json:"type"`
				Text string `json:"text"`
			} `json:"content"`
		} `json:"result"`
	}
	if err := json.Unmarshal(encoded, &wire); err != nil || !wire.Result.IsError {
		return nil, nil
	}
	var structured *x402types.PaymentRequired
	var structuredRaw any
	if len(bytes.TrimSpace(wire.Result.StructuredContent)) > 0 &&
		!bytes.Equal(bytes.TrimSpace(wire.Result.StructuredContent), []byte("null")) {
		structuredRaw, err = adapter.decodeJSON(wire.Result.StructuredContent, "MCP x402 structuredContent")
		if err != nil {
			return nil, err
		}
		if declared, ok := structuredRaw.(map[string]any); ok &&
			(declared["x402Version"] != nil || declared["accepts"] != nil) {
			structured, err = adapter.paymentRequired(structuredRaw, "MCP x402 structuredContent")
			if err != nil {
				return nil, err
			}
		} else {
			structuredRaw = nil
		}
	}
	var textValue *x402types.PaymentRequired
	var textRaw any
	for _, content := range wire.Result.Content {
		if content.Type != "text" || content.Text == "" {
			continue
		}
		candidate, decodeErr := adapter.decodeJSON([]byte(content.Text), "MCP x402 content text")
		if decodeErr != nil {
			continue
		}
		parsed, parseErr := adapter.paymentRequired(candidate, "MCP x402 content text")
		if parseErr == nil {
			textRaw, textValue = candidate, parsed
			break
		}
	}
	if structured == nil && textValue == nil {
		return nil, nil
	}
	if structured != nil && textValue != nil && !reflect.DeepEqual(structured, textValue) {
		return nil, errors.New("MCP x402 structuredContent conflicts with content text")
	}
	selected := structuredRaw
	if selected == nil {
		selected = textRaw
	}
	body, err := json.Marshal(selected)
	if err != nil || int64(len(body)) > limit {
		return nil, errors.New("MCP x402 PaymentRequired is oversized")
	}
	return &HTTPResponse{
		Headers:     map[string]string{"payment-required": base64.StdEncoding.EncodeToString(body)},
		ContentType: "application/json",
		Body:        body,
	}, nil
}

func (adapter *X402Adapter) MCPCredential(credential model.PaymentCredential) (MCPMetadata, error) {
	delivery, err := adapter.Credential(credential)
	if err != nil {
		return MCPMetadata{}, err
	}
	raw, err := adapter.decodeBase64JSON(delivery.HeaderValue, 64*1024, "PAYMENT-SIGNATURE")
	if err != nil {
		return MCPMetadata{}, err
	}
	value, err := adapter.paymentPayload(raw, "PAYMENT-SIGNATURE")
	if err != nil {
		return MCPMetadata{}, err
	}
	return MCPMetadata{Key: x402mcp.MCP_PAYMENT_META_KEY, Value: value}, nil
}

func (adapter *X402Adapter) PresentedMCPCredential(meta any, limit int64) (any, error) {
	object, err := adapter.object(meta, "MCP params._meta")
	if err != nil {
		return nil, err
	}
	value, exists := object[x402mcp.MCP_PAYMENT_META_KEY]
	if !exists {
		return nil, nil
	}
	body, err := json.Marshal(value)
	if err != nil || int64(len(body)) > limit {
		return nil, errors.New("MCP x402 payment metadata is invalid or oversized")
	}
	params := map[string]interface{}{"_meta": object}
	parsed, err := x402mcp.ExtractPaymentFromMeta(params)
	if err != nil || parsed == nil || parsed.X402Version != 2 || len(parsed.Accepted.Scheme) == 0 {
		return nil, errors.New("MCP x402 payment metadata is not a valid v2 PaymentPayload")
	}
	return adapter.paymentPayload(value, "MCP x402 payment metadata")
}

func (adapter *X402Adapter) MCPPaymentRequired(challenge *model.PaymentChallenge) (MCPPaymentRequired, error) {
	required, err := adapter.challengePaymentRequired(challenge)
	if err != nil {
		return MCPPaymentRequired{}, err
	}
	text, err := json.Marshal(required)
	if err != nil {
		return MCPPaymentRequired{}, err
	}
	return MCPPaymentRequired{Result: map[string]any{
		"content":           []any{map[string]any{"type": "text", "text": string(text)}},
		"structuredContent": required,
		"isError":           true,
	}}, nil
}

func (adapter *X402Adapter) MCPPaymentRejected(
	challenge *model.PaymentChallenge,
	message string,
) (MCPPaymentRequired, error) {
	required, err := adapter.challengePaymentRequired(challenge)
	if err != nil {
		return MCPPaymentRequired{}, err
	}
	body, err := json.Marshal(required)
	if err != nil {
		return MCPPaymentRequired{}, err
	}
	var rejected map[string]any
	if err := json.Unmarshal(body, &rejected); err != nil {
		return MCPPaymentRequired{}, err
	}
	rejected["error"] = message
	copy := *challenge
	payload, err := adapter.object(challenge.Payload, "x402 challenge payload")
	if err != nil {
		return MCPPaymentRequired{}, err
	}
	payload["payment_required"] = rejected
	copy.Payload = payload
	return adapter.MCPPaymentRequired(&copy)
}

func (adapter *X402Adapter) MCPReceipt(_ *model.PaymentChallenge, payment *model.SettledIncomingPayment) (MCPMetadata, error) {
	settlement, err := adapter.paymentSettlement(payment)
	if err != nil {
		return MCPMetadata{}, err
	}
	return MCPMetadata{Key: x402mcp.MCP_PAYMENT_RESPONSE_META_KEY, Value: settlement}, nil
}

func (adapter *X402Adapter) MCPMerchantEvidence(meta any, limit int64) (*MerchantEvidence, error) {
	object, err := adapter.object(meta, "MCP result._meta")
	if err != nil {
		return nil, err
	}
	value, exists := object[x402mcp.MCP_PAYMENT_RESPONSE_META_KEY]
	if !exists {
		return nil, nil
	}
	body, err := json.Marshal(value)
	if err != nil || int64(len(body)) > limit {
		return nil, errors.New("MCP x402 settlement metadata is invalid or oversized")
	}
	if _, err := adapter.settlement(value); err != nil {
		return nil, err
	}
	return &MerchantEvidence{
		HeaderName: "payment-response", HeaderValue: base64.StdEncoding.EncodeToString(body),
	}, nil
}

func (adapter *X402Adapter) challengePaymentRequired(challenge *model.PaymentChallenge) (any, error) {
	if challenge == nil || challenge.Protocol != adapter.Name() {
		return nil, errors.New("x402 challenge protocol mismatch")
	}
	payload, err := adapter.object(challenge.Payload, "x402 challenge payload")
	if err != nil {
		return nil, err
	}
	required, exists := payload["payment_required"]
	if !exists {
		return nil, errors.New("x402 challenge has no payment_required wire value")
	}
	if _, err := adapter.paymentRequired(required, "x402 challenge"); err != nil {
		return nil, err
	}
	return required, nil
}

func (adapter *X402Adapter) paymentRequired(value any, label string) (*x402types.PaymentRequired, error) {
	body, err := json.Marshal(value)
	if err != nil {
		return nil, fmt.Errorf("%s cannot be encoded", label)
	}
	required, err := x402types.ToPaymentRequired(body)
	if err != nil || required.X402Version != 2 || len(required.Accepts) == 0 {
		return nil, fmt.Errorf("%s is not an official x402 v2 PaymentRequired", label)
	}
	for _, accepted := range required.Accepts {
		if accepted.Scheme == "" || accepted.Network == "" || accepted.Amount == "" || accepted.PayTo == "" ||
			accepted.Asset == "" || accepted.MaxTimeoutSeconds <= 0 {
			return nil, fmt.Errorf("%s contains incomplete x402 payment requirements", label)
		}
	}
	return required, nil
}

func (adapter *X402Adapter) paymentPayload(value any, label string) (any, error) {
	body, err := json.Marshal(value)
	if err != nil {
		return nil, fmt.Errorf("%s cannot be encoded", label)
	}
	payload, err := x402types.ToPaymentPayload(body)
	if err != nil || payload.X402Version != 2 || payload.Payload == nil || payload.Accepted.Scheme == "" ||
		payload.Accepted.Network == "" || payload.Accepted.Amount == "" || payload.Accepted.PayTo == "" ||
		payload.Accepted.Asset == "" || payload.Accepted.MaxTimeoutSeconds <= 0 {
		return nil, fmt.Errorf("%s is not a valid x402 v2 PaymentPayload", label)
	}
	if payload.Accepted.Scheme != "exact" {
		return nil, fmt.Errorf("%s uses an unsupported x402 scheme", label)
	}
	if err := adapter.validateExactPayload(payload, label); err != nil {
		return nil, err
	}
	return value, nil
}

func (adapter *X402Adapter) validateExactPayload(payload *x402types.PaymentPayload, label string) error {
	body, err := json.Marshal(payload.Payload)
	if err != nil {
		return fmt.Errorf("%s payload cannot be encoded", label)
	}
	decode := func(target any) error {
		decoder := json.NewDecoder(bytes.NewReader(body))
		decoder.DisallowUnknownFields()
		return decoder.Decode(target)
	}
	switch {
	case strings.HasPrefix(payload.Accepted.Network, "eip155:"):
		var wire x402EIP3009Payload
		if err := decode(&wire); err != nil || wire.Signature == "" ||
			wire.Authorization.From == "" || wire.Authorization.To == "" || wire.Authorization.Value == "" ||
			wire.Authorization.ValidAfter == "" || wire.Authorization.ValidBefore == "" ||
			wire.Authorization.Nonce == "" {
			return fmt.Errorf("%s is not an official EIP-3009 payload", label)
		}
		name, hasName := payload.Accepted.Extra["name"].(string)
		version, hasVersion := payload.Accepted.Extra["version"].(string)
		if !hasName || !hasVersion || name == "" || version == "" {
			return fmt.Errorf("%s has no EIP-3009 domain metadata", label)
		}
	case strings.HasPrefix(payload.Accepted.Network, "solana:"):
		var wire x402SolanaExactPayload
		if err := decode(&wire); err != nil || wire.Transaction == "" {
			return fmt.Errorf("%s is not an official Solana exact payload", label)
		}
		decoded, err := base64.StdEncoding.DecodeString(wire.Transaction)
		if err != nil || len(decoded) == 0 || base64.StdEncoding.EncodeToString(decoded) != wire.Transaction {
			return fmt.Errorf("%s has no canonical base64 Solana transaction", label)
		}
		feePayer, ok := payload.Accepted.Extra["feePayer"].(string)
		if !ok || feePayer == "" {
			return fmt.Errorf("%s has no Solana facilitator fee payer", label)
		}
	default:
		return fmt.Errorf("%s uses an unsupported x402 network", label)
	}
	return nil
}

func (adapter *X402Adapter) paymentSettlement(payment *model.SettledIncomingPayment) (x402.SettleResponse, error) {
	if payment == nil || payment.Receipt.Protocol != adapter.Name() || payment.Receipt.TransactionHash == "" ||
		payment.Receipt.Payer == "" || payment.Receipt.Asset.Network == "" {
		return x402.SettleResponse{}, errors.New("confirmed x402 receipt omitted transaction, payer, or network")
	}
	return x402.SettleResponse{
		Success: true, Payer: payment.Receipt.Payer, Transaction: payment.Receipt.TransactionHash,
		Network: x402.Network(payment.Receipt.Asset.Network),
	}, nil
}

func (adapter *X402Adapter) settlement(value any) (*x402.SettleResponse, error) {
	body, err := json.Marshal(value)
	if err != nil {
		return nil, errors.New("x402 settlement response cannot be encoded")
	}
	var response x402.SettleResponse
	if err := json.Unmarshal(body, &response); err != nil || !response.Success || response.Payer == "" ||
		response.Transaction == "" || response.Network == "" {
		return nil, errors.New("x402 settlement response is invalid")
	}
	return &response, nil
}

func (adapter *X402Adapter) decodeBase64JSON(encoded string, limit int64, label string) (any, error) {
	if limit <= 0 || int64(len(encoded)) > (limit*4+2)/3+4 {
		return nil, fmt.Errorf("%s exceeds configured limit", label)
	}
	raw, err := base64.StdEncoding.DecodeString(encoded)
	if err != nil || int64(len(raw)) > limit || base64.StdEncoding.EncodeToString(raw) != encoded {
		return nil, fmt.Errorf("%s is non-canonical or oversized", label)
	}
	return adapter.decodeJSON(raw, label)
}

func (adapter *X402Adapter) decodeJSON(raw []byte, label string) (any, error) {
	var value any
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.UseNumber()
	if err := decoder.Decode(&value); err != nil || value == nil {
		return nil, fmt.Errorf("%s is not valid JSON", label)
	}
	return value, nil
}

func (adapter *X402Adapter) object(value any, label string) (map[string]any, error) {
	body, err := json.Marshal(value)
	if err != nil {
		return nil, fmt.Errorf("%s cannot be encoded", label)
	}
	var object map[string]any
	if err := json.Unmarshal(body, &object); err != nil || object == nil {
		return nil, fmt.Errorf("%s must be an object", label)
	}
	return object, nil
}

func (adapter *X402Adapter) header(headers map[string]string, wanted string) string {
	for name, value := range headers {
		if strings.EqualFold(name, wanted) {
			return value
		}
	}
	return ""
}
