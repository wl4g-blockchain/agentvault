package payer

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/config"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/transport"
)

// PayerMiddleware owns the transport-neutral 402, approval, credential, and
// settlement-evidence state machine.
type PayerMiddleware struct {
	api    *transport.Client
	config config.PayerConfig
}

func NewPayerMiddleware(wire transport.Transport, settings config.PayerConfig) (*PayerMiddleware, error) {
	settings.ApplyDefaults()
	if wire == nil {
		return nil, errors.New("AgentVault transport is not configured")
	}
	if err := settings.Validate(); err != nil {
		return nil, err
	}
	return &PayerMiddleware{api: transport.NewClient(wire), config: settings}, nil
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

func (m *PayerMiddleware) Report(ctx context.Context, operation model.PayerRequestContext, payment *model.AuthorizedPayment, response model.PayerHTTPResponse) error {
	if payment.Credential.Protocol != "x402" {
		return nil
	}
	value := header(response.Headers, "payment-response")
	if value == "" {
		if response.Status >= 200 && response.Status < 300 {
			return errors.New("successful x402 response has no PAYMENT-RESPONSE settlement hint; payment remains reserved")
		}
		return nil
	}
	operationContext, err := model.NewOperationContext(m.config.ScenarioID, operation.RunID)
	if err != nil {
		return err
	}
	_, err = m.reportOutboundResponse(ctx, payment.Intent.ID, model.ReportOutboundResponseRequest{
		OperationContext: operationContext,
		PaymentResponse:  value,
	})
	return err
}

func (m *PayerMiddleware) CredentialHeader(payment *model.AuthorizedPayment, origin string, existing map[string]string) (string, string, error) {
	if payment == nil || payment.Credential.HTTP == nil {
		return "", "", errors.New("Vault credential has no HTTP delivery material")
	}
	delivery := payment.Credential.HTTP
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
		"host", "content-length", "authorization", "connection", "transfer-encoding", "upgrade",
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
