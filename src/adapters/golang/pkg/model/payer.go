package model

import (
	"errors"
	"fmt"
	"strings"
)

type AuthorizationOutcome string

const (
	AuthorizationOutcomeAuthorized       AuthorizationOutcome = "authorized"
	AuthorizationOutcomeRequiresApproval AuthorizationOutcome = "requires_approval"
	AuthorizationOutcomeDenied           AuthorizationOutcome = "denied"
)

// PaymentRequired is opaque protocol wire data. AgentVault validates it.
type PaymentRequired struct {
	ProtocolHint string            `json:"protocol_hint,omitempty"`
	Origin       string            `json:"origin"`
	Resource     string            `json:"resource"`
	ContentType  string            `json:"content_type"`
	Headers      map[string]string `json:"headers,omitempty"`
	Body         string            `json:"body"`
	ExpiresAt    string            `json:"expires_at,omitempty"`
}

type AuthorizePaymentRequest struct {
	OperationContext
	Challenge PaymentRequired `json:"challenge"`
}

func (request AuthorizePaymentRequest) Validate() error {
	if err := request.OperationContext.Validate(); err != nil {
		return err
	}
	for name, value := range map[string]string{
		"challenge.origin": request.Challenge.Origin, "challenge.resource": request.Challenge.Resource,
		"challenge.content_type": request.Challenge.ContentType,
	} {
		if err := Required(name, value); err != nil {
			return err
		}
	}
	if request.Challenge.Origin != strings.TrimSpace(request.Challenge.Origin) {
		return errors.New("challenge origin cannot contain surrounding whitespace")
	}
	return ValidateURLOrigin(request.Challenge.Resource, request.Challenge.Origin)
}

type PaymentAuthorizationResult struct {
	Outcome        AuthorizationOutcome `json:"outcome"`
	IntentID       string               `json:"intent_id"`
	Reason         string               `json:"reason,omitempty"`
	ApprovalID     string               `json:"approval_id,omitempty"`
	ReservedAmount string               `json:"reserved_amount,omitempty"`
}

type AuthorizePaymentResponse struct {
	Authorization PaymentAuthorizationResult `json:"authorization"`
	Intent        PaymentIntent              `json:"intent"`
	Credential    *PaymentCredential         `json:"credential,omitempty"`
	Approval      *ApprovalRequest           `json:"approval,omitempty"`
}

type ReportOutboundResponseRequest struct {
	OperationContext
	Protocol string           `json:"protocol"`
	Evidence ProtocolEvidence `json:"evidence"`
}

type ProtocolEvidence struct {
	HeaderName  string `json:"header_name"`
	HeaderValue string `json:"header_value"`
}

func (request ReportOutboundResponseRequest) Validate() error {
	if err := request.OperationContext.Validate(); err != nil {
		return err
	}
	if err := ValidateIdentifier("protocol", request.Protocol); err != nil {
		return err
	}
	if err := Required("evidence.header_name", request.Evidence.HeaderName); err != nil {
		return err
	}
	if len(request.Evidence.HeaderName) > 64 {
		return errors.New("evidence.header_name is invalid")
	}
	for _, character := range []byte(request.Evidence.HeaderName) {
		if !((character >= 'a' && character <= 'z') || (character >= 'A' && character <= 'Z') ||
			(character >= '0' && character <= '9') || character == '-') {
			return errors.New("evidence.header_name is invalid")
		}
	}
	value := request.Evidence.HeaderValue
	if len(value) == 0 || len(value) > 64*1024 {
		return errors.New("merchant evidence must be a bounded HTTP header value")
	}
	for _, character := range []byte(value) {
		if character <= 0x20 || character >= 0x7f {
			return errors.New("merchant evidence must be a bounded HTTP header value")
		}
	}
	return nil
}

type ReportOutboundResponseResponse struct {
	Intent   PaymentIntent `json:"intent"`
	Accepted bool          `json:"accepted"`
	Replayed bool          `json:"replayed"`
}

type BalanceQuery struct {
	ScenarioID string
	WalletID   string
	Asset      AssetIdentity
}

type PaymentBalance struct {
	ScenarioID        string        `json:"scenario_id"`
	WalletID          string        `json:"wallet_id"`
	Asset             AssetIdentity `json:"asset"`
	ConfirmedInbound  string        `json:"confirmed_inbound"`
	ConfirmedOutbound string        `json:"confirmed_outbound"`
	ReservedOutbound  string        `json:"reserved_outbound"`
	Available         string        `json:"available,omitempty"`
	AsOf              int64         `json:"as_of"`
}

// PayerRequestContext is created once for one logical workflow operation and
// must be reused unchanged by every HTTP/MCP retry.
type PayerRequestContext struct {
	RunID string
}

func NewPayerRequestContext(runID string) (PayerRequestContext, error) {
	if err := ValidateIdentifier("run_id", runID); err != nil {
		return PayerRequestContext{}, err
	}
	return PayerRequestContext{RunID: runID}, nil
}

type PayerHTTPRequest struct {
	Method          string
	URL             string
	Origin          string
	Headers         map[string]string
	Body            []byte
	FollowRedirects bool
}

func (request PayerHTTPRequest) String() string {
	return fmt.Sprintf("PayerHTTPRequest{Method:%q URL:%q Origin:%q Headers:[REDACTED] Body:[REDACTED] FollowRedirects:%t}",
		request.Method, request.URL, request.Origin, request.FollowRedirects)
}

func (request PayerHTTPRequest) GoString() string { return request.String() }

type PayerHTTPResponse struct {
	Status  int
	Headers map[string]string
	Body    []byte
}

type AuthorizedPayment struct {
	Intent     PaymentIntent
	Credential PaymentCredential
}
