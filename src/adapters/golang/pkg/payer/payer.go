package payer

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/url"
	"reflect"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/config"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/protocol"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/transport"
)

// PayerMiddleware owns the transport-neutral 402, approval, credential, and
// settlement-evidence state machine.
type PayerMiddleware struct {
	api       *transport.Client
	config    config.PayerConfig
	protocols *protocol.Registry
}

func (m *PayerMiddleware) decodeMCPMessage(body []byte) (map[string]any, bool) {
	decoder := json.NewDecoder(bytes.NewReader(body))
	decoder.UseNumber()
	var message map[string]any
	if err := decoder.Decode(&message); err != nil {
		return nil, false
	}
	var trailing any
	if err := decoder.Decode(&trailing); !errors.Is(err, io.EOF) {
		return nil, false
	}
	return message, true
}

func (m *PayerMiddleware) mcpCall(body []byte) (map[string]any, bool) {
	message, ok := m.decodeMCPMessage(body)
	if !ok || message["jsonrpc"] != "2.0" || message["method"] != "tools/call" {
		return nil, false
	}
	if _, ok := message["params"].(map[string]any); !ok {
		return nil, false
	}
	switch message["id"].(type) {
	case string, json.Number:
		return message, true
	default:
		return nil, false
	}
}

func (m *PayerMiddleware) boundMCPResponse(body []byte, expectedID any, requireResult bool) (map[string]any, bool) {
	message, ok := m.decodeMCPMessage(body)
	if !ok || message["jsonrpc"] != "2.0" || !reflect.DeepEqual(message["id"], expectedID) {
		return nil, false
	}
	if requireResult {
		if _, ok := message["result"].(map[string]any); !ok {
			return nil, false
		}
	}
	return message, true
}

func NewPayerMiddleware(wire transport.Transport, settings config.PayerConfig) (*PayerMiddleware, error) {
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
	return &PayerMiddleware{api: transport.NewClient(wire), config: settings, protocols: protocols}, nil
}

func (middleware *PayerMiddleware) Standard(client *http.Client) *StandardPayerMiddleware {
	return NewStandardPayerMiddleware(middleware, client)
}

func (middleware *PayerMiddleware) Config() config.PayerConfig {
	return middleware.config
}

func (m *PayerMiddleware) Authorize(ctx context.Context, request model.PayerHTTPRequest, response model.PayerHTTPResponse, operation model.PayerRequestContext) (*model.AuthorizedPayment, error) {
	if response.Status != http.StatusPaymentRequired {
		return nil, nil
	}
	if request.FollowRedirects {
		return nil, errors.New("paid requests must disable redirects")
	}
	if err := model.ValidateURLOrigin(request.URL, request.Origin); err != nil {
		return nil, err
	}
	if err := model.ValidateIdentifier("run_id", operation.RunID); err != nil {
		return nil, errors.New("paid request has an invalid workflow context")
	}
	if int64(len(response.Body)) > m.config.MaxChallengeBytes || !utf8.Valid(response.Body) {
		return nil, errors.New("payment challenge is oversized or not UTF-8")
	}
	protocolHint := header(response.Headers, "payment-protocol")
	if protocolHint != "" {
		if _, err := m.protocols.Get(protocolHint); err != nil {
			return nil, err
		}
	}
	contextValue, err := model.NewOperationContext(m.config.ScenarioID, operation.RunID)
	if err != nil {
		return nil, err
	}
	requestValue := model.AuthorizePaymentRequest{
		OperationContext: contextValue,
		Challenge: model.PaymentRequired{
			ProtocolHint: protocolHint, Origin: request.Origin, Resource: request.URL,
			ContentType: valueOr(header(response.Headers, "content-type"), "application/json"),
			Headers:     response.Headers, Body: string(response.Body),
			ExpiresAt: header(response.Headers, "payment-expires-at"),
		},
	}
	authorization, err := m.authorizeUntilTerminal(ctx, requestValue)
	if err != nil {
		return nil, err
	}
	switch authorization.Authorization.Outcome {
	case model.AuthorizationOutcomeDenied:
		return nil, fmt.Errorf("AgentVault denied payment: %s", authorization.Authorization.Reason)
	case model.AuthorizationOutcomeAuthorized:
		if authorization.Credential == nil {
			return nil, errors.New("authorized AgentVault response contained no credential")
		}
		credential := *authorization.Credential
		intent := authorization.Intent
		if _, err := m.protocols.Get(intent.Protocol); err != nil {
			return nil, err
		}
		if intent.ID == "" || intent.ScenarioID != m.config.ScenarioID || intent.RunID != operation.RunID ||
			intent.WalletID == nil || *intent.WalletID == "" || intent.PolicyID == "" ||
			intent.Direction != model.PaymentDirectionOutbound ||
			(protocolHint != "" && intent.Protocol != protocolHint) || intent.Protocol != credential.Protocol ||
			intent.Resource != request.URL || intent.ChallengeHash != credential.ChallengeHash {
			return nil, errors.New("credential, outbound intent, and paid resource are not bound to the same payment")
		}
		return &model.AuthorizedPayment{Intent: intent, Credential: credential}, nil
	default:
		return nil, errors.New("approval wait returned a non-terminal result")
	}
}

// AuthorizeMCP recognizes one official x402/MPP message-level challenge and
// reuses the same Vault authorization state machine as HTTP 402.
func (m *PayerMiddleware) AuthorizeMCP(
	ctx context.Context,
	request model.PayerHTTPRequest,
	response model.PayerHTTPResponse,
	operation model.PayerRequestContext,
) (*model.AuthorizedPayment, error) {
	if int64(len(response.Body)) > m.config.MaxChallengeBytes {
		return nil, errors.New("MCP payment challenge exceeds configured limit")
	}
	requestMessage, ok := m.mcpCall(request.Body)
	if !ok {
		return nil, nil
	}
	message, ok := m.boundMCPResponse(response.Body, requestMessage["id"], false)
	if !ok {
		return nil, nil
	}
	adapter, challenge, err := m.protocols.MCPChallenge(message, m.config.MaxChallengeBytes)
	if err != nil || challenge == nil {
		return nil, err
	}
	if header(response.Headers, "payment-required") != "" {
		return nil, errors.New("MCP payment challenge must not also declare PAYMENT-REQUIRED")
	}
	if advertised := header(response.Headers, "payment-protocol"); advertised != "" &&
		!strings.EqualFold(advertised, adapter.Name()) {
		return nil, errors.New("MCP payment challenge protocol metadata conflicts with its JSON-RPC payload")
	}
	headers := make(map[string]string, len(challenge.Headers)+2)
	for name, value := range challenge.Headers {
		headers[name] = value
	}
	headers["payment-protocol"] = adapter.Name()
	if expiresAt := header(response.Headers, "payment-expires-at"); expiresAt != "" {
		headers["payment-expires-at"] = expiresAt
	}
	if challenge.ContentType != "" {
		headers["content-type"] = challenge.ContentType
	}
	return m.Authorize(ctx, request, model.PayerHTTPResponse{
		Status: http.StatusPaymentRequired, Headers: headers, Body: challenge.Body,
	}, operation)
}

// MCPCredential inserts a Vault-issued protocol credential into one replayable
// JSON-RPC tools/call request while preserving unrelated metadata.
func (m *PayerMiddleware) MCPCredential(payment *model.AuthorizedPayment, request []byte) ([]byte, error) {
	if payment == nil || payment.Intent.Protocol != payment.Credential.Protocol {
		return nil, errors.New("outbound payment protocol binding is invalid")
	}
	adapter, err := m.protocols.MCP(payment.Credential.Protocol)
	if err != nil {
		return nil, err
	}
	metadata, err := adapter.MCPCredential(payment.Credential)
	if err != nil {
		return nil, err
	}
	decoder := json.NewDecoder(bytes.NewReader(request))
	decoder.UseNumber()
	var message map[string]any
	if err := decoder.Decode(&message); err != nil || message["jsonrpc"] != "2.0" || message["method"] != "tools/call" {
		return nil, errors.New("message-level payment is limited to MCP tools/call")
	}
	switch message["id"].(type) {
	case string, json.Number:
	default:
		return nil, errors.New("message-level payment is limited to MCP tools/call")
	}
	params, ok := message["params"].(map[string]any)
	if !ok {
		return nil, errors.New("MCP tools/call params must be an object")
	}
	if raw, exists := message["_meta"]; exists {
		rootMeta, ok := raw.(map[string]any)
		if !ok {
			return nil, errors.New("JSON-RPC root _meta must be an object")
		}
		for _, key := range m.protocols.MCPRootRequestMetadataKeys() {
			if _, exists := rootMeta[key]; exists {
				return nil, errors.New("MCP request already contains root payment credential metadata")
			}
		}
	}
	meta, exists := params["_meta"].(map[string]any)
	if params["_meta"] != nil && !exists {
		return nil, errors.New("MCP params._meta must be an object")
	}
	if meta == nil {
		meta = make(map[string]any)
		params["_meta"] = meta
	}
	if protocolName, existing, err := m.protocols.PresentedMCPCredential(meta, m.config.MaxChallengeBytes); err != nil {
		return nil, err
	} else if existing != nil {
		return nil, fmt.Errorf("MCP request already contains %s payment credential metadata", protocolName)
	}
	if _, exists := meta[metadata.Key]; exists {
		return nil, errors.New("MCP request already contains payment credential metadata")
	}
	meta[metadata.Key] = metadata.Value
	return json.Marshal(message)
}

func (m *PayerMiddleware) Report(ctx context.Context, operation model.PayerRequestContext, payment *model.AuthorizedPayment, response model.PayerHTTPResponse) error {
	if payment == nil || payment.Intent.Protocol != payment.Credential.Protocol {
		return errors.New("outbound payment protocol binding is invalid")
	}
	adapter, err := m.protocols.Get(payment.Credential.Protocol)
	if err != nil {
		return err
	}
	protocolName, evidence, err := m.protocols.MerchantEvidence(response.Headers)
	if err != nil {
		return err
	}
	if evidence != nil && protocolName != adapter.Name() {
		return errors.New("HTTP settlement receipt protocol does not match the paid credential")
	}
	var mcpEvidence *protocol.MerchantEvidence
	if len(response.Body) > 0 {
		decoder := json.NewDecoder(bytes.NewReader(response.Body))
		decoder.UseNumber()
		var message map[string]any
		if err := decoder.Decode(&message); err == nil {
			meta := any(map[string]any{})
			if result, ok := message["result"].(map[string]any); ok && result["_meta"] != nil {
				meta = result["_meta"]
			}
			protocolName, value, err := m.protocols.MCPMerchantEvidence(meta, m.config.MaxChallengeBytes)
			if err != nil {
				return err
			}
			if value != nil && protocolName != adapter.Name() {
				return errors.New("MCP settlement receipt protocol does not match the paid credential")
			}
			mcpEvidence = value
		}
	}
	if evidence != nil && mcpEvidence != nil &&
		(evidence.HeaderName != mcpEvidence.HeaderName || evidence.HeaderValue != mcpEvidence.HeaderValue) {
		return errors.New("HTTP and MCP settlement evidence conflict")
	}
	if evidence == nil {
		evidence = mcpEvidence
	}
	if evidence == nil {
		if response.Status >= 200 && response.Status < 300 {
			return fmt.Errorf("successful %s response has no settlement evidence; payment remains reserved", adapter.Name())
		}
		return nil
	}
	operationContext, err := model.NewOperationContext(m.config.ScenarioID, operation.RunID)
	if err != nil {
		return err
	}
	_, err = m.reportOutboundResponse(ctx, payment.Intent.ID, model.ReportOutboundResponseRequest{
		OperationContext: operationContext,
		Protocol:         adapter.Name(),
		Evidence: &model.ProtocolEvidence{
			HeaderName: evidence.HeaderName, HeaderValue: evidence.HeaderValue,
		},
	})
	return err
}

// Deliver owns the bounded paid-request retry state machine. Framework
// adapters only create replayable requests and expose their response shape.
func (m *PayerMiddleware) Deliver(
	ctx context.Context,
	operation model.PayerRequestContext,
	payment *model.AuthorizedPayment,
	send func(attemptNo int) (model.PayerHTTPResponse, error),
	discard func(),
) error {
	if send == nil || discard == nil {
		return errors.New("payer delivery callbacks are not configured")
	}
	for attempt := 0; attempt <= m.config.DeliveryRetries; attempt++ {
		slog.Info("payer middleware is delivering signed request",
			"event.name", "agentvault.sdk.payer.retrying", "run.id", operation.RunID,
			"attempt.no", attempt+1, "payment.intent_id", payment.Intent.ID)
		response, err := send(attempt + 1)
		if err != nil {
			return err
		}
		if err := m.Report(ctx, operation, payment, response); err != nil {
			discard()
			return err
		}
		if response.Status != http.StatusServiceUnavailable || attempt == m.config.DeliveryRetries {
			return nil
		}
		discard()
	}
	return errors.New("payer retry loop ended unexpectedly")
}

func (m *PayerMiddleware) CredentialHeader(payment *model.AuthorizedPayment, origin string, existing map[string]string) (string, string, error) {
	if payment == nil || payment.Credential.HTTP == nil {
		return "", "", errors.New("Vault credential has no HTTP delivery material")
	}
	adapter, err := m.protocols.Get(payment.Credential.Protocol)
	if err != nil {
		return "", "", err
	}
	delivery, err := adapter.Credential(payment.Credential)
	if err != nil {
		return "", "", err
	}
	if !strings.EqualFold(delivery.BoundOrigin, origin) || !validHeaderName(delivery.HeaderName) ||
		strings.ContainsAny(delivery.HeaderValue, "\r\n") || reservedHeader(delivery.HeaderName) ||
		header(existing, delivery.HeaderName) != "" {
		return "", "", errors.New("unsafe or missing credential HTTP binding")
	}
	return delivery.HeaderName, delivery.HeaderValue, nil
}

func (m *PayerMiddleware) GetPayment(ctx context.Context, scenarioID, intentID string) (*model.PaymentIntent, error) {
	if err := model.Required("scenario_id", scenarioID); err != nil {
		return nil, err
	}
	if err := model.Required("intent_id", intentID); err != nil {
		return nil, err
	}
	path := "/api/v1/payments/" + url.PathEscape(intentID) + "?scenario_id=" + url.QueryEscape(scenarioID)
	var result model.PaymentIntent
	if err := m.api.Get(ctx, path, &result); err != nil {
		return nil, err
	}
	return &result, nil
}

func (m *PayerMiddleware) GetReceipt(ctx context.Context, scenarioID, intentID string) (*model.PaymentReceipt, error) {
	if err := model.Required("scenario_id", scenarioID); err != nil {
		return nil, err
	}
	if err := model.Required("intent_id", intentID); err != nil {
		return nil, err
	}
	path := "/api/v1/payments/" + url.PathEscape(intentID) + "/receipt?scenario_id=" + url.QueryEscape(scenarioID)
	var result model.PaymentReceipt
	if err := m.api.Get(ctx, path, &result); err != nil {
		return nil, err
	}
	return &result, nil
}

func (m *PayerMiddleware) GetBalance(ctx context.Context, query model.BalanceQuery) (*model.PaymentBalance, error) {
	for name, value := range map[string]string{
		"scenario_id": query.ScenarioID, "wallet_id": query.WalletID,
		"asset.network": query.Asset.Network, "asset.asset": query.Asset.Asset,
	} {
		if err := model.Required(name, value); err != nil {
			return nil, err
		}
	}
	values := url.Values{
		"scenario_id": {query.ScenarioID}, "wallet_id": {query.WalletID},
		"network": {query.Asset.Network}, "asset": {query.Asset.Asset},
		"decimals": {fmt.Sprint(query.Asset.Decimals)},
	}
	if query.Asset.TokenContract != "" {
		values.Set("token_contract", query.Asset.TokenContract)
	}
	var result model.PaymentBalance
	if err := m.api.Get(ctx, "/api/v1/payments/balance?"+values.Encode(), &result); err != nil {
		return nil, err
	}
	return &result, nil
}

func (m *PayerMiddleware) authorizePayment(ctx context.Context, request model.AuthorizePaymentRequest) (*model.AuthorizePaymentResponse, error) {
	if err := request.Validate(); err != nil {
		return nil, err
	}
	var result model.AuthorizePaymentResponse
	if err := m.api.Post(ctx, "/api/v1/payments/authorize", request, &result); err != nil {
		return nil, err
	}
	return &result, nil
}

func (m *PayerMiddleware) authorizeUntilTerminal(ctx context.Context, request model.AuthorizePaymentRequest) (*model.AuthorizePaymentResponse, error) {
	deadline := time.Now().Add(m.config.ApprovalTimeout)
	expectedIntent := ""
	for {
		remaining := time.Until(deadline)
		if remaining <= 0 {
			return nil, errors.New("payment approval did not complete before the total timeout")
		}
		attemptContext, cancel := context.WithTimeout(ctx, remaining)
		response, err := m.authorizePayment(attemptContext, request)
		cancel()
		if err != nil {
			return nil, err
		}
		intentID := response.Authorization.IntentID
		if intentID == "" || response.Intent.ID != intentID {
			return nil, errors.New("authorization result and payment intent do not match")
		}
		if expectedIntent == "" {
			expectedIntent = intentID
		} else if expectedIntent != intentID {
			return nil, errors.New("replayed run changed payment intent")
		}
		if response.Authorization.Outcome != model.AuthorizationOutcomeRequiresApproval {
			return response, nil
		}
		timer := time.NewTimer(min(m.config.ApprovalPollInterval, time.Until(deadline)))
		select {
		case <-ctx.Done():
			timer.Stop()
			return nil, ctx.Err()
		case <-timer.C:
		}
	}
}

func (m *PayerMiddleware) reportOutboundResponse(ctx context.Context, intentID string, request model.ReportOutboundResponseRequest) (*model.ReportOutboundResponseResponse, error) {
	if err := model.Required("intent_id", intentID); err != nil {
		return nil, err
	}
	if err := request.Validate(); err != nil {
		return nil, err
	}
	path := "/api/v1/payments/" + url.PathEscape(intentID) + "/outbound-response"
	var result model.ReportOutboundResponseResponse
	if err := m.api.Post(ctx, path, request, &result); err != nil {
		return nil, err
	}
	return &result, nil
}

func header(headers map[string]string, wanted string) string {
	for name, value := range headers {
		if strings.EqualFold(name, wanted) {
			return value
		}
	}
	return ""
}

func validHeaderName(name string) bool {
	if name == "" {
		return false
	}
	for _, character := range []byte(name) {
		if (character >= 'a' && character <= 'z') || (character >= 'A' && character <= 'Z') ||
			(character >= '0' && character <= '9') || strings.ContainsRune("!#$%&'*+-.^_`|~", rune(character)) {
			continue
		}
		return false
	}
	return true
}

func reservedHeader(name string) bool {
	for _, reserved := range []string{
		"host", "content-length", "connection", "transfer-encoding", "upgrade",
		"proxy-authorization", "proxy-authenticate", "te", "trailer", "accept", "content-type",
	} {
		if strings.EqualFold(name, reserved) {
			return true
		}
	}
	return false
}

func valueOr(value, fallback string) string {
	if value == "" {
		return fallback
	}
	return value
}
