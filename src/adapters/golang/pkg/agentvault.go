// Package agentvault exposes one ergonomic facade over role-oriented middleware.
package agentvault

import (
	"context"
	"errors"
	"net/url"
	"sync"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/config"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/payee"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/payer"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/transport"
)

type AgentVault struct {
	wire      transport.Transport
	owned     transport.VaultTransport
	closeOnce sync.Once
	closeErr  error
}

func New(wire transport.Transport) *AgentVault {
	return &AgentVault{wire: wire}
}

func NewFromConfig(settings config.VaultTransportConfig) (*AgentVault, error) {
	wire, err := transport.NewVaultTransportFactory().Create(settings)
	if err != nil {
		return nil, err
	}
	return &AgentVault{wire: wire, owned: wire}, nil
}

func (vault *AgentVault) Close() error {
	if vault == nil || vault.owned == nil {
		return nil
	}
	vault.closeOnce.Do(func() { vault.closeErr = vault.owned.Close() })
	return vault.closeErr
}

func (vault *AgentVault) Payer(settings config.PayerConfig) (*payer.PayerMiddleware, error) {
	return payer.NewPayerMiddleware(vault.wire, settings)
}

func (vault *AgentVault) Payee(settings config.PayeeConfig) (*payee.PayeeMiddleware, error) {
	return payee.NewPayeeMiddleware(vault.wire, settings)
}

type PayerMiddleware = payer.PayerMiddleware
type PayerConfig = config.PayerConfig
type PayerRequestContext = model.PayerRequestContext
type AuthorizedPayment = model.AuthorizedPayment
type StandardPayerMiddleware = payer.StandardPayerMiddleware
type PayeeMiddleware = payee.PayeeMiddleware
type PayeeConfig = config.PayeeConfig
type PayeeOperation = model.PayeeOperation
type GinPayeeMiddleware = payee.GinPayeeMiddleware
type GinPayeeRoute = payee.GinPayeeRoute
type StandardPayeeMiddleware = payee.StandardPayeeMiddleware
type StdHttpMiddleware = payee.StdHttpMiddleware
type StandardPayeeRoute = payee.StandardPayeeRoute

var NewPayerMiddleware = payer.NewPayerMiddleware
var NewPayerRequestContext = model.NewPayerRequestContext
var WithPayment = payer.WithPayment
var PaymentFromResponse = payer.PaymentFromResponse
var NewPayeeMiddleware = payee.NewPayeeMiddleware
var NewMCPToolRoute = payee.NewMCPToolRoute
var NewStdHttpMiddleware = payee.NewStdHttpMiddleware
var NewStandardMCPToolRoute = payee.NewStandardMCPToolRoute

type Transport = transport.Transport
type TransportRequest = transport.TransportRequest
type TransportResponse = transport.TransportResponse
type Method = transport.Method
type APIError = transport.APIError
type MqttVaultTransport = transport.MqttVaultTransport
type MqttConfig = config.MqttConfig
type GrpcVaultTransport = transport.GrpcVaultTransport
type GrpcConfig = config.GrpcConfig
type VaultTransportConfig = config.VaultTransportConfig
type TransportProvider = config.TransportProvider
type VaultTransportFactory = transport.VaultTransportFactory

const (
	TransportProviderGrpc = config.TransportProviderGrpc
	TransportProviderMqtt = config.TransportProviderMqtt
)

const (
	MethodGet  = transport.MethodGet
	MethodPost = transport.MethodPost
)

var ErrRunConflict = transport.ErrRunConflict
var NewMqttVaultTransport = transport.NewMqttVaultTransport
var NewGrpcVaultTransport = transport.NewGrpcVaultTransport
var NewVaultTransportFactory = transport.NewVaultTransportFactory

type OperationContext = model.OperationContext
type PaymentDirection = model.PaymentDirection
type PaymentStatus = model.PaymentStatus
type AuthorizationOutcome = model.AuthorizationOutcome
type ApprovalStatus = model.ApprovalStatus
type ApprovalDecisionKind = model.ApprovalDecisionKind
type AssetIdentity = model.AssetIdentity
type PaymentRequired = model.PaymentRequired
type PaymentChallenge = model.PaymentChallenge
type HTTPCredential = model.HTTPCredential
type PaymentCredential = model.PaymentCredential
type AuthorizePaymentRequest = model.AuthorizePaymentRequest
type PaymentIntent = model.PaymentIntent
type ReservationStatus = model.ReservationStatus
type ApprovalRequest = model.ApprovalRequest
type PaymentAuthorizationResult = model.PaymentAuthorizationResult
type AuthorizePaymentResponse = model.AuthorizePaymentResponse
type ReportOutboundResponseRequest = model.ReportOutboundResponseRequest
type ReportOutboundResponseResponse = model.ReportOutboundResponseResponse
type PaymentReceipt = model.PaymentReceipt
type BalanceQuery = model.BalanceQuery
type PaymentBalance = model.PaymentBalance
type ApprovalQuery = model.ApprovalQuery
type ApprovalPage = model.ApprovalPage
type SubmitApprovalDecisionRequest = model.SubmitApprovalDecisionRequest
type ResolvePriceRequest = model.ResolvePriceRequest
type PaymentPrice = model.PaymentPrice
type CreateChallengeRequest = model.CreateChallengeRequest
type VerifyPaymentRequest = model.VerifyPaymentRequest
type PaymentVerification = model.PaymentVerification
type SettlePaymentRequest = model.SettlePaymentRequest
type SettledIncomingPayment = model.SettledIncomingPayment
type PayerHTTPRequest = model.PayerHTTPRequest
type PayerHTTPResponse = model.PayerHTTPResponse
type HTTPProtectionRequest = model.HTTPProtectionRequest
type MCPProtectionRequest = model.MCPProtectionRequest
type ProtectionOutcome = model.ProtectionOutcome

const (
	PaymentDirectionInbound  = model.PaymentDirectionInbound
	PaymentDirectionOutbound = model.PaymentDirectionOutbound

	PaymentStatusCreated           = model.PaymentStatusCreated
	PaymentStatusPendingApproval   = model.PaymentStatusPendingApproval
	PaymentStatusAuthorized        = model.PaymentStatusAuthorized
	PaymentStatusSigned            = model.PaymentStatusSigned
	PaymentStatusSubmitted         = model.PaymentStatusSubmitted
	PaymentStatusPendingSettlement = model.PaymentStatusPendingSettlement
	PaymentStatusSettled           = model.PaymentStatusSettled
	PaymentStatusFailed            = model.PaymentStatusFailed
	PaymentStatusExpired           = model.PaymentStatusExpired
	PaymentStatusUnknown           = model.PaymentStatusUnknown

	AuthorizationOutcomeAuthorized       = model.AuthorizationOutcomeAuthorized
	AuthorizationOutcomeRequiresApproval = model.AuthorizationOutcomeRequiresApproval
	AuthorizationOutcomeDenied           = model.AuthorizationOutcomeDenied

	ApprovalStatusPending  = model.ApprovalStatusPending
	ApprovalStatusApproved = model.ApprovalStatusApproved
	ApprovalStatusRejected = model.ApprovalStatusRejected
	ApprovalStatusExpired  = model.ApprovalStatusExpired
	ApprovalStatusConsumed = model.ApprovalStatusConsumed

	ApprovalDecisionApprove = model.ApprovalDecisionApprove
	ApprovalDecisionReject  = model.ApprovalDecisionReject

	ReservationStatusUnreserved = model.ReservationStatusUnreserved
	ReservationStatusReserved   = model.ReservationStatusReserved
	ReservationStatusConsumed   = model.ReservationStatusConsumed
	ReservationStatusReleased   = model.ReservationStatusReleased
)

func NewOperationContext(scenarioID, runID string) (model.OperationContext, error) {
	return model.NewOperationContext(scenarioID, runID)
}

func (vault *AgentVault) GetPendingApprovals(ctx context.Context, query model.ApprovalQuery) (*model.ApprovalPage, error) {
	if err := model.Required("scenario_id", query.ScenarioID); err != nil {
		return nil, err
	}
	var result model.ApprovalPage
	path := "/api/v1/approvals?scenario_id=" + url.QueryEscape(query.ScenarioID) + "&status=pending"
	if err := transport.NewClient(vault.wire).Get(ctx, path, &result); err != nil {
		return nil, err
	}
	return &result, nil
}

func (vault *AgentVault) SubmitApprovalDecision(ctx context.Context, request model.SubmitApprovalDecisionRequest) (*model.ApprovalRequest, error) {
	if err := request.OperationContext.Validate(); err != nil {
		return nil, err
	}
	if err := model.Required("approval_id", request.ApprovalID); err != nil {
		return nil, err
	}
	if request.Decision == model.ApprovalDecisionApprove {
		if err := model.Required("approved_amount", request.ApprovedAmount); err != nil {
			return nil, err
		}
	} else if request.Decision != model.ApprovalDecisionReject || request.ApprovedAmount != "" {
		return nil, errors.New("approval decision and approved_amount are inconsistent")
	}
	path := "/api/v1/approvals/" + url.PathEscape(request.ApprovalID) + "/decision"
	var result model.ApprovalRequest
	if err := transport.NewClient(vault.wire).Post(ctx, path, request, &result); err != nil {
		return nil, err
	}
	return &result, nil
}

func (vault *AgentVault) GetApprovalStatus(ctx context.Context, scenarioID, approvalID string) (*model.ApprovalRequest, error) {
	if err := model.Required("scenario_id", scenarioID); err != nil {
		return nil, err
	}
	if err := model.Required("approval_id", approvalID); err != nil {
		return nil, err
	}
	path := "/api/v1/approvals/" + url.PathEscape(approvalID) + "?scenario_id=" + url.QueryEscape(scenarioID)
	var result model.ApprovalRequest
	if err := transport.NewClient(vault.wire).Get(ctx, path, &result); err != nil {
		return nil, err
	}
	return &result, nil
}
