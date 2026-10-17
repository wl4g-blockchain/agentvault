package model

type ApprovalStatus string

const (
	ApprovalStatusPending  ApprovalStatus = "pending"
	ApprovalStatusApproved ApprovalStatus = "approved"
	ApprovalStatusRejected ApprovalStatus = "rejected"
	ApprovalStatusExpired  ApprovalStatus = "expired"
	ApprovalStatusConsumed ApprovalStatus = "consumed"
)

type ApprovalDecisionKind string

const (
	ApprovalDecisionApprove ApprovalDecisionKind = "approve"
	ApprovalDecisionReject  ApprovalDecisionKind = "reject"
)

type ApprovalRequest struct {
	ID              string         `json:"id"`
	IntentID        string         `json:"intent_id"`
	RunID           string         `json:"run_id"`
	RequestedAmount string         `json:"requested_amount"`
	ApprovedAmount  string         `json:"approved_amount,omitempty"`
	RequestedBy     string         `json:"requested_by"`
	ApprovedBy      string         `json:"approved_by,omitempty"`
	Status          ApprovalStatus `json:"status"`
	IntentHash      string         `json:"intent_hash"`
	GrantJTI        string         `json:"grant_jti,omitempty"`
	GrantClaimsHash string         `json:"grant_claims_hash,omitempty"`
	ExpiresAt       int64          `json:"expires_at"`
	CreatedAt       int64          `json:"created_at"`
	ApprovedAt      *int64         `json:"approved_at,omitempty"`
	ConsumedAt      *int64         `json:"consumed_at,omitempty"`
}

type ApprovalQuery struct {
	ScenarioID string
}

type ApprovalPage struct {
	Items []ApprovalRequest `json:"items"`
	Total int               `json:"total"`
}

type SubmitApprovalDecisionRequest struct {
	OperationContext
	ApprovalID     string               `json:"approval_id"`
	Decision       ApprovalDecisionKind `json:"decision"`
	ApprovedAmount string               `json:"approved_amount,omitempty"`
	Reason         string               `json:"reason,omitempty"`
}
