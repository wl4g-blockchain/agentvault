package model

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/url"
	"strings"
)

// IdempotencyKey identifies one logical operation and must be reused by retries.
type IdempotencyKey string

func NewIdempotencyKey(value string) (IdempotencyKey, error) {
	key := IdempotencyKey(value)
	if err := key.Validate(); err != nil {
		return "", err
	}
	return key, nil
}

func OperationKey(runID, operation string) (IdempotencyKey, error) {
	if err := Required("run_id", runID); err != nil {
		return "", err
	}
	if err := Required("operation", operation); err != nil {
		return "", err
	}
	if strings.ContainsRune(runID, ':') || strings.ContainsRune(operation, ':') {
		return "", errors.New("run_id and operation cannot contain ':' when deriving an idempotency key")
	}
	return NewIdempotencyKey(runID + ":" + operation)
}

func (key IdempotencyKey) Validate() error {
	if len(key) == 0 || len(key) > 128 {
		return errors.New("idempotency_key must contain 1..=128 characters")
	}
	for _, character := range []byte(key) {
		if (character >= 'a' && character <= 'z') || (character >= 'A' && character <= 'Z') ||
			(character >= '0' && character <= '9') || strings.ContainsRune("-_.:", rune(character)) {
			continue
		}
		return errors.New("idempotency_key may contain only ASCII letters, digits, '-', '_', '.', and ':'")
	}
	return nil
}

func (key *IdempotencyKey) UnmarshalJSON(data []byte) error {
	var value string
	if err := json.Unmarshal(data, &value); err != nil {
		return err
	}
	parsed, err := NewIdempotencyKey(value)
	if err != nil {
		return err
	}
	*key = parsed
	return nil
}

type OperationContext struct {
	ScenarioID     string         `json:"scenario_id"`
	RunID          string         `json:"run_id,omitempty"`
	IdempotencyKey IdempotencyKey `json:"idempotency_key"`
}

func NewOperationContext(scenarioID string, key IdempotencyKey) (OperationContext, error) {
	context := OperationContext{ScenarioID: scenarioID, IdempotencyKey: key}
	if err := context.Validate(); err != nil {
		return OperationContext{}, err
	}
	return context, nil
}

func (context OperationContext) Validate() error {
	if err := Required("scenario_id", context.ScenarioID); err != nil {
		return err
	}
	if context.RunID != "" && strings.TrimSpace(context.RunID) == "" {
		return errors.New("run_id cannot be blank")
	}
	return context.IdempotencyKey.Validate()
}

func (context OperationContext) ValidateWithRunID() error {
	if err := context.Validate(); err != nil {
		return err
	}
	if context.RunID == "" {
		return errors.New("run_id is required for payment operations")
	}
	return nil
}

type PaymentDirection string

const (
	PaymentDirectionInbound  PaymentDirection = "inbound"
	PaymentDirectionOutbound PaymentDirection = "outbound"
)

type PaymentStatus string

const (
	PaymentStatusCreated           PaymentStatus = "created"
	PaymentStatusPendingApproval   PaymentStatus = "pending_approval"
	PaymentStatusAuthorized        PaymentStatus = "authorized"
	PaymentStatusSigned            PaymentStatus = "signed"
	PaymentStatusSubmitted         PaymentStatus = "submitted"
	PaymentStatusPendingSettlement PaymentStatus = "pending_settlement"
	PaymentStatusSettled           PaymentStatus = "settled"
	PaymentStatusFailed            PaymentStatus = "failed"
	PaymentStatusExpired           PaymentStatus = "expired"
	PaymentStatusUnknown           PaymentStatus = "unknown"
)

type ReservationStatus string

const (
	ReservationStatusUnreserved ReservationStatus = "unreserved"
	ReservationStatusReserved   ReservationStatus = "reserved"
	ReservationStatusConsumed   ReservationStatus = "consumed"
	ReservationStatusReleased   ReservationStatus = "released"
)

type AssetIdentity struct {
	Network       string `json:"network"`
	Asset         string `json:"asset"`
	TokenContract string `json:"token_contract,omitempty"`
	Decimals      uint8  `json:"decimals"`
}

type PaymentChallenge struct {
	ScenarioID    string        `json:"scenario_id"`
	OfferID       string        `json:"offer_id,omitempty"`
	Resource      string        `json:"resource"`
	Protocol      string        `json:"protocol"`
	Asset         AssetIdentity `json:"asset"`
	Amount        string        `json:"amount"`
	Recipient     string        `json:"recipient"`
	Nonce         string        `json:"nonce"`
	ChallengeHash string        `json:"challenge_hash"`
	ExpiresAt     int64         `json:"expires_at"`
	Payload       any           `json:"payload"`
}

type HTTPCredential struct {
	BoundOrigin string `json:"bound_origin"`
	HeaderName  string `json:"header_name"`
	HeaderValue string `json:"header_value"`
}

func (credential HTTPCredential) String() string {
	return fmt.Sprintf("HTTPCredential{BoundOrigin:%q HeaderName:%q HeaderValue:[REDACTED]}", credential.BoundOrigin, credential.HeaderName)
}

func (credential HTTPCredential) GoString() string { return credential.String() }

type PaymentCredential struct {
	Protocol       string          `json:"protocol"`
	ChallengeHash  string          `json:"challenge_hash"`
	CredentialHash string          `json:"credential_hash"`
	Payload        any             `json:"payload"`
	HTTP           *HTTPCredential `json:"http,omitempty"`
}

func (credential PaymentCredential) String() string {
	return fmt.Sprintf("PaymentCredential{Protocol:%q ChallengeHash:[REDACTED] CredentialHash:[REDACTED] Payload:[REDACTED] HTTP:[REDACTED]}", credential.Protocol)
}

func (credential PaymentCredential) GoString() string { return credential.String() }

type PaymentIntent struct {
	ID                string            `json:"id"`
	ScenarioID        string            `json:"scenario_id"`
	RunID             string            `json:"run_id"`
	WalletID          *string           `json:"wallet_id,omitempty"`
	PolicyID          string            `json:"policy_id,omitempty"`
	OfferID           string            `json:"offer_id,omitempty"`
	Direction         PaymentDirection  `json:"direction"`
	Protocol          string            `json:"protocol"`
	Asset             AssetIdentity     `json:"asset"`
	Recipient         string            `json:"recipient"`
	Amount            string            `json:"amount"`
	SettledAmount     string            `json:"settled_amount"`
	Resource          string            `json:"resource"`
	ChallengeHash     string            `json:"challenge_hash,omitempty"`
	IdempotencyKey    string            `json:"idempotency_key"`
	RequestHash       string            `json:"request_hash"`
	Status            PaymentStatus     `json:"status"`
	ReservationStatus ReservationStatus `json:"reservation_status"`
	ReservedAmount    string            `json:"reserved_amount"`
	TransactionHash   string            `json:"tx_hash,omitempty"`
	SettlementRef     string            `json:"settlement_ref,omitempty"`
	NextReconcileAt   *int64            `json:"next_reconcile_at,omitempty"`
	CreatedAt         int64             `json:"created_at"`
	UpdatedAt         int64             `json:"updated_at"`
	ExpiresAt         *int64            `json:"expires_at,omitempty"`
	SettledAt         *int64            `json:"settled_at,omitempty"`
}

type PaymentReceipt struct {
	IntentID        string           `json:"intent_id"`
	ScenarioID      string           `json:"scenario_id"`
	Direction       PaymentDirection `json:"direction"`
	Protocol        string           `json:"protocol"`
	Asset           AssetIdentity    `json:"asset"`
	Recipient       string           `json:"recipient"`
	Payer           string           `json:"payer,omitempty"`
	RequestedAmount string           `json:"requested_amount"`
	SettledAmount   string           `json:"settled_amount"`
	Status          PaymentStatus    `json:"status"`
	TransactionHash string           `json:"tx_hash,omitempty"`
	SettlementRef   string           `json:"settlement_ref,omitempty"`
	EvidenceHash    string           `json:"evidence_hash"`
	ConfirmedAt     int64            `json:"confirmed_at"`
}

func Required(name, value string) error {
	if strings.TrimSpace(value) == "" {
		return fmt.Errorf("%s cannot be blank", name)
	}
	return nil
}

func ValidateURLOrigin(rawURL, origin string) error {
	parsed, err := url.Parse(rawURL)
	if err != nil || parsed.User != nil || parsed.Host == "" || (parsed.Scheme != "https" && parsed.Scheme != "http") {
		return errors.New("request URL is not an absolute HTTP(S) URL")
	}
	actual := parsed.Scheme + "://" + parsed.Host
	if !strings.EqualFold(actual, origin) {
		return errors.New("request URL does not match its canonical origin")
	}
	return nil
}
