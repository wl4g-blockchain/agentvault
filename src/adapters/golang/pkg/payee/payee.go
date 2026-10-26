package payee

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/url"
	"reflect"
	"strings"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/config"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/protocol"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/transport"
)

var ErrInvalidPayment = errors.New("AgentVault rejected the presented payment")

type PaymentRejectedError struct {
	Challenge *model.PaymentChallenge
}

func (errorValue *PaymentRejectedError) Error() string { return ErrInvalidPayment.Error() }

func (errorValue *PaymentRejectedError) Unwrap() error { return ErrInvalidPayment }

type mcpCall struct {
	ID                 any
	CredentialProtocol string
	CredentialPayload  any
	CanonicalBody      []byte
	a2a                *a2aPaymentCall
}

// PayeeMiddleware owns challenge, credential verification, and settlement.
// Framework adapters only translate their request/response types.
type PayeeMiddleware struct {
	api       *transport.Client
	config    config.PayeeConfig
	protocols *protocol.Registry
}

func NewPayeeMiddleware(wire transport.Transport, settings config.PayeeConfig) (*PayeeMiddleware, error) {
	settings.ApplyDefaults()
	if wire == nil {
		return nil, errors.New("AgentVault transport is not configured")
	}
	if err := settings.Validate(); err != nil {
		return nil, err
	}
	protocols, err := protocol.NewPaymentProtocolFactory().Create(settings.Protocols)
	if err != nil {
		return nil, err
	}
	return &PayeeMiddleware{api: transport.NewClient(wire), config: settings, protocols: protocols}, nil
}

func (middleware *PayeeMiddleware) Gin(routes ...GinPayeeRoute) *GinPayeeMiddleware {
	return NewGinPayeeMiddleware(middleware, routes...).WithMaxBodyBytes(middleware.config.MaxBodyBytes)
}

func (middleware *PayeeMiddleware) Standard(routes ...StandardPayeeRoute) *StandardPayeeMiddleware {
	return NewStdHttpMiddleware(middleware, routes...).WithMaxBodyBytes(middleware.config.MaxBodyBytes)
}

func (middleware *PayeeMiddleware) Config() config.PayeeConfig {
	return middleware.config
}

func (middleware *PayeeMiddleware) Protocol(name string) (protocol.Adapter, error) {
	if middleware == nil {
		return nil, errors.New("payee middleware is not configured")
	}
	return middleware.protocols.Get(name)
}

func (middleware *PayeeMiddleware) MerchantEvidenceHeaderNames() []string {
	if middleware == nil || middleware.protocols == nil {
		return nil
	}
	return middleware.protocols.MerchantEvidenceHeaderNames()
}

// PresentedCredential normalizes only payment-bearing HTTP fields and rejects
// ambiguous duplicates before any credential is forwarded to AgentVault.
func (middleware *PayeeMiddleware) PresentedCredential(
	adapter protocol.Adapter,
	headers map[string][]string,
	limit int64,
) (any, error) {
	if middleware == nil || adapter == nil {
		return nil, errors.New("payee payment protocol is not configured")
	}
	normalized, err := (PayeeMiddleware{}).normalizeCredentialHeaders(headers, adapter.CredentialHeaderNames())
	if err != nil {
		return nil, err
	}
	return adapter.PresentedCredential(normalized, limit)
}

// PresentedCredentialForProtocol lets every enabled provider inspect its own
// headers, rejects cross-protocol ambiguity, then enforces the protected route.
func (middleware *PayeeMiddleware) PresentedCredentialForProtocol(
	protocolName string,
	headers map[string][]string,
	limit int64,
) (any, error) {
	if middleware == nil {
		return nil, errors.New("payee payment protocol is not configured")
	}
	if _, err := middleware.protocols.Get(protocolName); err != nil {
		return nil, err
	}
	normalized, err := (PayeeMiddleware{}).normalizeCredentialHeaders(headers, middleware.protocols.CredentialHeaderNames())
	if err != nil {
		return nil, err
	}
	selected, payload, err := middleware.protocols.PresentedCredential(normalized, limit)
	if err != nil {
		return nil, err
	}
	if selected != "" && selected != protocolName {
		return nil, errors.New("payment credential protocol does not match the protected route")
	}
	return payload, nil
}

func (PayeeMiddleware) normalizeCredentialHeaders(headers map[string][]string, names []string) (map[string]string, error) {
	normalized := make(map[string]string, len(names))
	for name, values := range headers {
		owned := false
		for _, candidate := range names {
			if strings.EqualFold(name, candidate) {
				owned = true
				break
			}
		}
		if !owned {
			continue
		}
		if len(values) != 1 {
			return nil, fmt.Errorf("duplicate payment request header is not allowed: %s", name)
		}
		normalized[name] = values[0]
	}
	return normalized, nil
}

func (middleware *PayeeMiddleware) readBody(request *http.Request, limit int64) ([]byte, error) {
	if request.ContentLength > limit {
		return nil, errors.New("protected request body exceeds configured limit")
	}
	body, err := io.ReadAll(io.LimitReader(request.Body, limit+1))
	if err != nil {
		return nil, fmt.Errorf("read protected request: %w", err)
	}
	if int64(len(body)) > limit {
		return nil, errors.New("protected request body exceeds configured limit")
	}
	request.Body = io.NopCloser(bytes.NewReader(body))
	return body, nil
}

func (middleware *PayeeMiddleware) replayBody(request *http.Request, body []byte) {
	request.Body = io.NopCloser(bytes.NewReader(body))
	request.ContentLength = int64(len(body))
	request.GetBody = func() (io.ReadCloser, error) {
		return io.NopCloser(bytes.NewReader(body)), nil
	}
}

func (middleware *PayeeMiddleware) hasMerchantEvidenceHeader(headers http.Header) bool {
	for name := range headers {
		for _, owned := range middleware.MerchantEvidenceHeaderNames() {
			if strings.EqualFold(name, owned) {
				return true
			}
		}
	}
	return false
}

func (middleware *PayeeMiddleware) requestHash(request *http.Request, body []byte) string {
	hash := sha256.New()
	hash.Write([]byte("agentvault.payee.http.v1\x00"))
	hash.Write([]byte(request.Method))
	hash.Write([]byte{0})
	hash.Write([]byte(request.URL.Path))
	hash.Write([]byte{0})
	hash.Write(body)
	return hex.EncodeToString(hash.Sum(nil))
}

func (middleware *PayeeMiddleware) hasMCPHTTPPaymentCredential(headers http.Header) (bool, error) {
	normalized, err := (PayeeMiddleware{}).normalizeCredentialHeaders(headers, middleware.protocols.CredentialHeaderNames())
	if err != nil {
		return false, err
	}
	_, payload, err := middleware.protocols.PresentedCredential(normalized, middleware.config.MaxBodyBytes)
	return payload != nil, err
}

func (middleware *PayeeMiddleware) parseMCPCall(body []byte, expectedTool, protocolName string) (*mcpCall, error) {
	decoder := json.NewDecoder(bytes.NewReader(body))
	decoder.UseNumber()
	var message map[string]any
	if err := decoder.Decode(&message); err != nil {
		return nil, nil
	}
	var trailing any
	if err := decoder.Decode(&trailing); !errors.Is(err, io.EOF) {
		return nil, nil
	}
	params, paramsOK := message["params"].(map[string]any)
	if message["jsonrpc"] != "2.0" || message["method"] != "tools/call" || !paramsOK || params["name"] != expectedTool {
		return nil, nil
	}
	id := message["id"]
	if _, stringID := id.(string); !stringID {
		if _, numberID := id.(json.Number); !numberID {
			return nil, errors.New("paid MCP tools/call requires a string or numeric JSON-RPC id")
		}
	}
	meta := map[string]any{}
	if raw, exists := params["_meta"]; exists {
		nested, ok := raw.(map[string]any)
		if !ok {
			return nil, errors.New("MCP params._meta must be an object")
		}
		for key, value := range nested {
			meta[key] = value
		}
	}
	rootMeta := map[string]any{}
	if raw, exists := message["_meta"]; exists {
		var ok bool
		rootMeta, ok = raw.(map[string]any)
		if !ok {
			return nil, errors.New("JSON-RPC root _meta must be an object")
		}
	}
	for _, key := range middleware.protocols.MCPRootRequestMetadataKeys() {
		if value, exists := rootMeta[key]; exists {
			if _, duplicate := meta[key]; duplicate {
				return nil, errors.New("duplicate MCP payment credential metadata")
			}
			meta[key] = value
		}
	}
	credentialProtocol, credential, err := middleware.protocols.PresentedMCPCredential(
		meta, middleware.config.MaxBodyBytes,
	)
	if err != nil {
		return nil, err
	}
	if credentialProtocol != "" && credentialProtocol != protocolName {
		return nil, errors.New("MCP payment credential protocol does not match the protected route")
	}
	for _, key := range middleware.protocols.MCPRequestMetadataKeys() {
		if nested, ok := params["_meta"].(map[string]any); ok {
			delete(nested, key)
		}
	}
	if nested, ok := params["_meta"].(map[string]any); ok && len(nested) == 0 {
		delete(params, "_meta")
	}
	for _, key := range middleware.protocols.MCPRootRequestMetadataKeys() {
		delete(rootMeta, key)
	}
	if _, exists := message["_meta"]; exists && len(rootMeta) == 0 {
		delete(message, "_meta")
	}
	canonical, err := json.Marshal(message)
	if err != nil {
		return nil, errors.New("MCP request cannot be canonicalized")
	}
	return &mcpCall{
		ID: id, CredentialProtocol: credentialProtocol, CredentialPayload: credential, CanonicalBody: canonical,
	}, nil
}

func (middleware *PayeeMiddleware) mcpPaymentRequired(
	challenge *model.PaymentChallenge,
	rejected bool,
) (protocol.MCPPaymentRequired, error) {
	adapter, err := middleware.protocols.MCP(challenge.Protocol)
	if err != nil {
		return protocol.MCPPaymentRequired{}, err
	}
	if rejected {
		return adapter.MCPPaymentRejected(challenge, ErrInvalidPayment.Error())
	}
	return adapter.MCPPaymentRequired(challenge)
}

func (middleware *PayeeMiddleware) encodeMCPPaymentRequired(
	call *mcpCall,
	challenge *model.PaymentChallenge,
	rejected bool,
) ([]byte, error) {
	if call == nil {
		return nil, errors.New("MCP call context is missing")
	}
	var wire protocol.MCPPaymentRequired
	var err error
	if call.a2a != nil {
		var adapter protocol.A2AAdapter
		adapter, err = middleware.protocols.A2A(challenge.Protocol)
		if err == nil {
			wire, err = adapter.A2APaymentRequired(challenge, call.a2a.task, rejected)
		}
	} else {
		wire, err = middleware.mcpPaymentRequired(challenge, rejected)
	}
	if err != nil {
		return nil, err
	}
	message := map[string]any{"jsonrpc": "2.0", "id": call.ID}
	if wire.Result != nil {
		message["result"] = wire.Result
	} else if wire.Error != nil {
		message["error"] = wire.Error
	} else {
		return nil, errors.New("protocol returned no MCP challenge")
	}
	body, err := json.Marshal(message)
	if err != nil || int64(len(body)) > middleware.config.MaxBodyBytes {
		return nil, errors.New("MCP payment challenge is invalid or oversized")
	}
	if err := middleware.persistA2ATask(call, body); err != nil {
		return nil, err
	}
	return body, nil
}

func (middleware *PayeeMiddleware) injectMCPReceipt(
	call *mcpCall,
	challenge *model.PaymentChallenge,
	payment *model.SettledIncomingPayment,
	body []byte,
) ([]byte, error) {
	if call.a2a != nil {
		return middleware.injectA2AReceipt(call, challenge, payment, body)
	}
	message, meta, err := middleware.decodeMCPResponse(call, body, true)
	if err != nil {
		return nil, err
	}
	adapter, err := middleware.protocols.MCP(payment.Receipt.Protocol)
	if err != nil {
		return nil, err
	}
	metadata, err := adapter.MCPReceipt(challenge, payment)
	if err != nil {
		return nil, err
	}
	if _, exists := meta[metadata.Key]; exists {
		return nil, errors.New("paid MCP handler attempted to supply settlement metadata")
	}
	meta[metadata.Key] = metadata.Value
	encoded, err := json.Marshal(message)
	if err != nil || int64(len(encoded)) > middleware.config.MaxBodyBytes {
		return nil, errors.New("paid MCP response is invalid or oversized")
	}
	return encoded, nil
}

func (middleware *PayeeMiddleware) decodeMCPResponse(
	call *mcpCall,
	body []byte,
	requireResult bool,
) (map[string]any, map[string]any, error) {
	decoder := json.NewDecoder(bytes.NewReader(body))
	decoder.UseNumber()
	var message map[string]any
	if err := decoder.Decode(&message); err != nil || !reflect.DeepEqual(message["id"], call.ID) {
		return nil, nil, errors.New("paid MCP response id does not match its request")
	}
	result, ok := message["result"].(map[string]any)
	if !ok {
		if requireResult {
			return nil, nil, errors.New("paid MCP handler must return a JSON-RPC result")
		}
		if _, ok := message["error"].(map[string]any); !ok {
			return nil, nil, errors.New("paid MCP handler must return a JSON-RPC result or error")
		}
		return message, nil, nil
	}
	meta := map[string]any{}
	if raw, exists := result["_meta"]; exists {
		meta, ok = raw.(map[string]any)
		if !ok {
			return nil, nil, errors.New("paid MCP result._meta must be an object")
		}
	}
	if protocolName, evidence, err := middleware.protocols.MCPMerchantEvidence(meta, middleware.config.MaxBodyBytes); err != nil {
		return nil, nil, err
	} else if evidence != nil {
		return nil, nil, fmt.Errorf("paid MCP handler attempted to supply %s settlement metadata", protocolName)
	}
	result["_meta"] = meta
	return message, meta, nil
}

func (middleware *PayeeMiddleware) Challenge(ctx context.Context, operation model.PayeeOperation) (*model.PaymentChallenge, error) {
	if err := operation.Validate(); err != nil {
		return nil, err
	}
	operationContext, err := operation.ChallengeContext()
	if err != nil {
		return nil, err
	}
	challenge, err := middleware.createChallenge(ctx, model.CreateChallengeRequest{
		OperationContext: operationContext,
		PriceID:          operation.PriceID, Resource: operation.Resource, Origin: operation.Origin,
		Protocol: operation.Protocol, RequestHash: operation.RequestHash,
	})
	if err != nil {
		return nil, err
	}
	if err := (PayeeMiddleware{}).validateChallengeBinding(operation, challenge); err != nil {
		return nil, err
	}
	return challenge, nil
}

func (middleware *PayeeMiddleware) Protect(ctx context.Context, operation model.PayeeOperation) (*model.ProtectionOutcome, error) {
	slog.Info("payee middleware intercepted protected operation",
		"event.name", "agentvault.sdk.payee.intercepted", "run.id", operation.RunID,
		"scenario.id", operation.ScenarioID)
	challenge, err := middleware.Challenge(ctx, operation)
	if err != nil {
		return nil, err
	}
	slog.Info("payee middleware created payment challenge",
		"event.name", "agentvault.sdk.payee.challenge_created", "run.id", operation.RunID,
		"payment.challenge_hash", challenge.ChallengeHash)
	if operation.CredentialPayload == nil {
		return &model.ProtectionOutcome{Outcome: "payment_required", Challenge: challenge}, nil
	}
	verifyContext, err := operation.ChallengeContext()
	if err != nil {
		return nil, err
	}
	verification, err := middleware.verifyPayment(ctx, model.VerifyPaymentRequest{
		OperationContext: verifyContext,
		ChallengeHash:    challenge.ChallengeHash, RequestHash: operation.RequestHash,
		Credential: model.PaymentCredential{
			Protocol: operation.Protocol, ChallengeHash: challenge.ChallengeHash,
			CredentialHash: "untrusted", Payload: operation.CredentialPayload,
		},
	})
	if err != nil {
		return nil, err
	}
	if !verification.Valid {
		return nil, &PaymentRejectedError{Challenge: challenge}
	}
	if verification.VerificationRef == "" {
		return nil, errors.New("valid AgentVault verification omitted verification_ref")
	}
	if err := (PayeeMiddleware{}).validateVerificationBinding(operation, challenge, verification); err != nil {
		return nil, err
	}
	slog.Info("payee middleware verified payment credential",
		"event.name", "agentvault.sdk.payee.verified", "run.id", operation.RunID,
		"payment.intent_id", verification.Intent.ID)
	settlementContext, err := operation.SettlementContext()
	if err != nil {
		return nil, err
	}
	payment, err := middleware.settlePayment(ctx, model.SettlePaymentRequest{
		OperationContext: settlementContext,
		IntentID:         verification.Intent.ID,
		VerificationRef:  verification.VerificationRef,
	})
	if err != nil {
		return nil, err
	}
	if err := (PayeeMiddleware{}).validateSettlementBinding(operation, verification, payment); err != nil {
		return nil, err
	}
	slog.Info("payee middleware settled payment",
		"event.name", "agentvault.sdk.payee.settled", "run.id", operation.RunID,
		"payment.intent_id", payment.Intent.ID)
	return &model.ProtectionOutcome{Outcome: "authorized", Challenge: challenge, Payment: payment}, nil
}

func (PayeeMiddleware) validateChallengeBinding(operation model.PayeeOperation, challenge *model.PaymentChallenge) error {
	if challenge == nil || challenge.ScenarioID != operation.ScenarioID || challenge.PriceID != operation.PriceID ||
		challenge.Resource != operation.Resource || challenge.Protocol != operation.Protocol {
		return errors.New("AgentVault challenge is not bound to the protected operation")
	}
	for name, value := range map[string]string{
		"challenge.asset.network": challenge.Asset.Network, "challenge.asset.asset": challenge.Asset.Asset,
		"challenge.amount": challenge.Amount, "challenge.recipient": challenge.Recipient,
		"challenge.nonce": challenge.Nonce, "challenge.challenge_hash": challenge.ChallengeHash,
	} {
		if err := model.Required(name, value); err != nil {
			return err
		}
	}
	return nil
}

func (PayeeMiddleware) validateVerificationBinding(operation model.PayeeOperation, challenge *model.PaymentChallenge,
	verification *model.PaymentVerification) error {
	intent := verification.Intent
	if intent.ID == "" || intent.Direction != model.PaymentDirectionInbound || intent.ScenarioID != operation.ScenarioID ||
		intent.PriceID != operation.PriceID || intent.Resource != operation.Resource || intent.Protocol != operation.Protocol ||
		intent.ChallengeHash != challenge.ChallengeHash || intent.RequestHash != operation.RequestHash ||
		intent.Asset != challenge.Asset || intent.Amount != challenge.Amount || intent.Recipient != challenge.Recipient {
		return errors.New("AgentVault verification is not bound to the protected operation")
	}
	return nil
}

func (PayeeMiddleware) validateSettlementBinding(operation model.PayeeOperation, verification *model.PaymentVerification,
	payment *model.SettledIncomingPayment) error {
	verified := verification.Intent
	if payment == nil || payment.Intent.ID != verified.ID || payment.Intent.Direction != model.PaymentDirectionInbound ||
		payment.Intent.ScenarioID != verified.ScenarioID || payment.Intent.PriceID != verified.PriceID ||
		payment.Intent.Resource != verified.Resource || payment.Intent.Protocol != verified.Protocol ||
		payment.Intent.ChallengeHash != verified.ChallengeHash || payment.Intent.RequestHash != verified.RequestHash ||
		payment.Intent.Asset != verified.Asset || payment.Intent.Amount != verified.Amount ||
		payment.Intent.Recipient != verified.Recipient || payment.Intent.Status != model.PaymentStatusSettled ||
		payment.Receipt.IntentID != payment.Intent.ID || payment.Receipt.ScenarioID != operation.ScenarioID ||
		payment.Receipt.Direction != model.PaymentDirectionInbound || payment.Receipt.Protocol != operation.Protocol ||
		payment.Receipt.Asset != payment.Intent.Asset || payment.Receipt.Recipient != payment.Intent.Recipient ||
		payment.Receipt.RequestedAmount != payment.Intent.Amount ||
		payment.Receipt.SettledAmount != payment.Intent.SettledAmount ||
		payment.Receipt.Status != model.PaymentStatusSettled {
		return fmt.Errorf("invalid AgentVault settlement response")
	}
	return nil
}

func (middleware *PayeeMiddleware) ResolvePrice(ctx context.Context, request model.ResolvePriceRequest) (*model.PaymentPrice, error) {
	if err := model.Required("scenario_id", request.ScenarioID); err != nil {
		return nil, err
	}
	if err := model.Required("resource", request.Resource); err != nil {
		return nil, err
	}
	var result model.PaymentPrice
	if err := middleware.api.Post(ctx, "/api/v1/offers/resolve", request, &result); err != nil {
		return nil, err
	}
	return &result, nil
}

func (middleware *PayeeMiddleware) createChallenge(ctx context.Context, request model.CreateChallengeRequest) (*model.PaymentChallenge, error) {
	if err := request.OperationContext.Validate(); err != nil {
		return nil, err
	}
	for name, value := range map[string]string{
		"offer_id": request.PriceID, "resource": request.Resource, "origin": request.Origin,
		"protocol": request.Protocol, "request_hash": request.RequestHash,
	} {
		if err := model.Required(name, value); err != nil {
			return nil, err
		}
	}
	var result model.PaymentChallenge
	if err := middleware.api.Post(ctx, "/api/v1/payee/challenges", request, &result); err != nil {
		return nil, err
	}
	return &result, nil
}

func (middleware *PayeeMiddleware) verifyPayment(ctx context.Context, request model.VerifyPaymentRequest) (*model.PaymentVerification, error) {
	if err := request.OperationContext.Validate(); err != nil {
		return nil, err
	}
	for name, value := range map[string]string{
		"challenge_hash": request.ChallengeHash, "request_hash": request.RequestHash,
		"credential.protocol":        request.Credential.Protocol,
		"credential.challenge_hash":  request.Credential.ChallengeHash,
		"credential.credential_hash": request.Credential.CredentialHash,
	} {
		if err := model.Required(name, value); err != nil {
			return nil, err
		}
	}
	if request.ChallengeHash != request.Credential.ChallengeHash {
		return nil, errors.New("credential challenge_hash does not match the request")
	}
	var result model.PaymentVerification
	if err := middleware.api.Post(ctx, "/api/v1/payee/verify", request, &result); err != nil {
		return nil, err
	}
	return &result, nil
}

func (middleware *PayeeMiddleware) settlePayment(ctx context.Context, request model.SettlePaymentRequest) (*model.SettledIncomingPayment, error) {
	if err := request.OperationContext.Validate(); err != nil {
		return nil, err
	}
	if err := model.Required("intent_id", request.IntentID); err != nil {
		return nil, err
	}
	if err := model.Required("verification_ref", request.VerificationRef); err != nil {
		return nil, err
	}
	var result model.SettledIncomingPayment
	if err := middleware.api.Post(ctx, "/api/v1/payee/settle", request, &result); err != nil {
		return nil, err
	}
	return &result, nil
}

func (middleware *PayeeMiddleware) GetReceipt(ctx context.Context, scenarioID, intentID string) (*model.PaymentReceipt, error) {
	if err := model.Required("scenario_id", scenarioID); err != nil {
		return nil, err
	}
	if err := model.Required("intent_id", intentID); err != nil {
		return nil, err
	}
	path := "/api/v1/payments/" + url.PathEscape(intentID) + "/receipt?scenario_id=" + url.QueryEscape(scenarioID)
	var result model.PaymentReceipt
	if err := middleware.api.Get(ctx, path, &result); err != nil {
		return nil, err
	}
	return &result, nil
}
