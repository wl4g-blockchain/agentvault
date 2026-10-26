package model

// PayeeOperation is the stable business identity and server-owned Price
// binding for one inbound request. Framework adapters only populate it; the
// shared PayeeMiddleware owns challenge, verification, and settlement.
type PayeeOperation struct {
	ScenarioID        string
	RunID             string
	PriceID           string
	Resource          string
	Origin            string
	Protocol          string
	RequestHash       string
	CredentialPayload any
}

func (operation PayeeOperation) Validate() error {
	for name, value := range map[string]string{
		"scenario_id": operation.ScenarioID, "run_id": operation.RunID,
		"price_id": operation.PriceID,
		"resource": operation.Resource, "origin": operation.Origin,
		"protocol": operation.Protocol, "request_hash": operation.RequestHash,
	} {
		if err := Required(name, value); err != nil {
			return err
		}
	}
	return nil
}

func (operation PayeeOperation) ChallengeContext() (OperationContext, error) {
	return NewOperationContext(operation.ScenarioID, operation.RunID)
}

func (operation PayeeOperation) SettlementContext() (OperationContext, error) {
	return NewOperationContext(operation.ScenarioID, operation.RunID)
}

type ResolvePriceRequest struct {
	ScenarioID string `json:"scenario_id"`
	Resource   string `json:"resource"`
	Protocol   string `json:"protocol,omitempty"`
}

type PaymentPrice struct {
	ID         string        `json:"id"`
	ScenarioID string        `json:"scenario_id"`
	Resource   string        `json:"resource"`
	Protocols  []string      `json:"protocols"`
	Asset      AssetIdentity `json:"asset"`
	Amount     string        `json:"amount"`
	WalletID   string        `json:"wallet_id"`
	Enabled    bool          `json:"enabled"`
	CreatedAt  int64         `json:"created_at"`
	UpdatedAt  int64         `json:"updated_at"`
}

type CreateChallengeRequest struct {
	OperationContext
	PriceID     string `json:"price_id"`
	Resource    string `json:"resource"`
	Origin      string `json:"origin"`
	Protocol    string `json:"protocol"`
	RequestHash string `json:"request_hash"`
}

type VerifyPaymentRequest struct {
	OperationContext
	ChallengeHash string            `json:"challenge_hash"`
	RequestHash   string            `json:"request_hash"`
	Credential    PaymentCredential `json:"credential"`
}

type PaymentVerification struct {
	Intent          PaymentIntent `json:"intent"`
	Valid           bool          `json:"valid"`
	VerificationRef string        `json:"verification_ref,omitempty"`
}

type SettlePaymentRequest struct {
	OperationContext
	IntentID        string `json:"intent_id"`
	VerificationRef string `json:"verification_ref"`
}

type SettledIncomingPayment struct {
	Intent  PaymentIntent  `json:"intent"`
	Receipt PaymentReceipt `json:"receipt"`
}

type HTTPProtectionRequest struct {
	OperationContext
	Resource    string             `json:"resource"`
	Origin      string             `json:"origin"`
	Credential  *PaymentCredential `json:"credential,omitempty"`
	RequestHash string             `json:"request_hash"`
}

type MCPProtectionRequest struct {
	OperationContext
	Tool          string             `json:"tool"`
	ArgumentsHash string             `json:"arguments_hash"`
	Credential    *PaymentCredential `json:"credential,omitempty"`
}

type ProtectionOutcome struct {
	Outcome   string                  `json:"outcome"`
	Challenge *PaymentChallenge       `json:"challenge,omitempty"`
	Payment   *SettledIncomingPayment `json:"payment,omitempty"`
}
