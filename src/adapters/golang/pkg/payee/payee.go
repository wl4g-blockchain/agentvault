package payee

import (
	"context"
	"errors"
	"fmt"
	"net/url"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/config"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/transport"
)

var ErrInvalidPayment = errors.New("AgentVault rejected the presented payment")

// PayeeMiddleware owns challenge, credential verification, and settlement.
// Framework adapters only translate their request/response types.
type PayeeMiddleware struct {
	api    *transport.Client
	config config.PayeeConfig
}

func NewPayeeMiddleware(wire transport.Transport, settings config.PayeeConfig) (*PayeeMiddleware, error) {
	settings.ApplyDefaults()
	if wire == nil {
		return nil, errors.New("AgentVault transport is not configured")
	}
	if err := settings.Validate(); err != nil {
		return nil, err
	}
	return &PayeeMiddleware{api: transport.NewClient(wire), config: settings}, nil
}

func (middleware *PayeeMiddleware) Gin(routes ...GinPayeeRoute) *GinPayeeMiddleware {
	return NewGinPayeeMiddleware(middleware, routes...).WithMaxBodyBytes(middleware.config.MaxBodyBytes)
}

func (middleware *PayeeMiddleware) Standard(routes ...StandardPayeeRoute) *StandardPayeeMiddleware {
	return NewStandardPayeeMiddleware(middleware, routes...).WithMaxBodyBytes(middleware.config.MaxBodyBytes)
}

func (middleware *PayeeMiddleware) Config() config.PayeeConfig {
	return middleware.config
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
		OfferID:          operation.OfferID, Resource: operation.Resource, Origin: operation.Origin,
		Protocol: operation.Protocol, RequestHash: operation.RequestHash,
	})
	if err != nil {
		return nil, err
	}
	if err := validateChallengeBinding(operation, challenge); err != nil {
		return nil, err
	}
	return challenge, nil
}

func (middleware *PayeeMiddleware) Protect(ctx context.Context, operation model.PayeeOperation) (*model.ProtectionOutcome, error) {
	challenge, err := middleware.Challenge(ctx, operation)
	if err != nil {
		return nil, err
	}
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
		return nil, ErrInvalidPayment
	}
	if verification.VerificationRef == "" {
		return nil, errors.New("valid AgentVault verification omitted verification_ref")
	}
	if err := validateVerificationBinding(operation, challenge, verification); err != nil {
		return nil, err
	}
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
	if err := validateSettlementBinding(operation, verification, payment); err != nil {
		return nil, err
	}
	return &model.ProtectionOutcome{Outcome: "authorized", Payment: payment}, nil
}

func validateChallengeBinding(operation model.PayeeOperation, challenge *model.PaymentChallenge) error {
	if challenge == nil || challenge.ScenarioID != operation.ScenarioID || challenge.OfferID != operation.OfferID ||
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

func validateVerificationBinding(operation model.PayeeOperation, challenge *model.PaymentChallenge,
	verification *model.PaymentVerification) error {
	intent := verification.Intent
	if intent.ID == "" || intent.Direction != model.PaymentDirectionInbound || intent.ScenarioID != operation.ScenarioID ||
		intent.OfferID != operation.OfferID || intent.Resource != operation.Resource || intent.Protocol != operation.Protocol ||
		intent.ChallengeHash != challenge.ChallengeHash || intent.RequestHash != operation.RequestHash ||
		intent.Asset != challenge.Asset || intent.Amount != challenge.Amount || intent.Recipient != challenge.Recipient {
		return errors.New("AgentVault verification is not bound to the protected operation")
	}
	return nil
}

func validateSettlementBinding(operation model.PayeeOperation, verification *model.PaymentVerification,
	payment *model.SettledIncomingPayment) error {
	verified := verification.Intent
	if payment == nil || payment.Intent.ID != verified.ID || payment.Intent.Direction != model.PaymentDirectionInbound ||
		payment.Intent.ScenarioID != verified.ScenarioID || payment.Intent.OfferID != verified.OfferID ||
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

func (middleware *PayeeMiddleware) ResolveOffer(ctx context.Context, request model.ResolveOfferRequest) (*model.PaymentOffer, error) {
	if err := model.Required("scenario_id", request.ScenarioID); err != nil {
		return nil, err
	}
	if err := model.Required("resource", request.Resource); err != nil {
		return nil, err
	}
	var result model.PaymentOffer
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
		"offer_id": request.OfferID, "resource": request.Resource, "origin": request.Origin,
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
