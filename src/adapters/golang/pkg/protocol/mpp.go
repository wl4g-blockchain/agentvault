package protocol

import (
	"bytes"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"reflect"
	"strings"
	"time"

	"github.com/a2aproject/a2a-go/a2a"
	mpp "github.com/tempoxyz/mpp-go/pkg/mpp"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
)

const (
	mppCredentialMetaKey      = "org.paymentauth/credential"
	mppPaymentRequiredMetaKey = "org.paymentauth/payment-required"
	mppReceiptMetaKey         = "org.paymentauth/receipt"
	mppPaymentRequiredCode    = -32042
	mppVerificationFailedCode = -32043
)

type MppAdapter struct{}

// MPP uses its standard JSON-RPC root _meta binding, not x402 A2A metadata.
func (adapter *MppAdapter) A2AExtension() string { return "" }

func (adapter *MppAdapter) A2AChallenge(message map[string]any, limit int64) (*HTTPResponse, error) {
	if message["error"] == nil {
		return nil, nil
	}
	return adapter.MCPChallenge(message, limit)
}

func (adapter *MppAdapter) A2AApplyCredential(message map[string]any, credential model.PaymentCredential) error {
	meta, err := (a2aWire{}).rootMetadata(message)
	if err != nil {
		return err
	}
	if _, exists := meta[mppCredentialMetaKey]; exists {
		return errors.New("JSON-RPC root already contains MPP credential")
	}
	value, err := adapter.MCPCredential(credential)
	if err != nil {
		return err
	}
	meta[value.Key] = value.Value
	return nil
}

func (adapter *MppAdapter) PresentedA2ACredential(message map[string]any, limit int64) (any, error) {
	meta, err := (a2aWire{}).rootMetadata(message)
	if err != nil {
		return nil, err
	}
	// A2A is plain JSON-RPC. MCP params._meta is not a second credential channel.
	params, _ := message["params"].(map[string]any)
	if nested, ok := params["_meta"].(map[string]any); ok && nested[mppCredentialMetaKey] != nil {
		return nil, errors.New("MPP A2A credential must use JSON-RPC root _meta")
	}
	return adapter.PresentedMCPCredential(meta, limit)
}

func (adapter *MppAdapter) A2AStripCredential(message map[string]any) error {
	meta, err := (a2aWire{}).rootMetadata(message)
	if err != nil {
		return err
	}
	delete(meta, mppCredentialMetaKey)
	return nil
}

func (adapter *MppAdapter) A2APaymentRequired(challenge *model.PaymentChallenge, _ *a2a.Task, rejected bool) (MCPPaymentRequired, error) {
	if rejected {
		return adapter.MCPPaymentRejected(challenge, "Payment verification failed")
	}
	return adapter.MCPPaymentRequired(challenge)
}

func (adapter *MppAdapter) A2AMerchantEvidence(message map[string]any, limit int64) (*MerchantEvidence, error) {
	meta, err := (a2aWire{}).rootMetadata(message)
	if err != nil {
		return nil, err
	}
	return adapter.MCPMerchantEvidence(meta, limit)
}

func (adapter *MppAdapter) A2AInjectReceipt(message map[string]any, challenge *model.PaymentChallenge, payment *model.SettledIncomingPayment, _ int64) error {
	meta, err := (a2aWire{}).rootMetadata(message)
	if err != nil {
		return err
	}
	if _, exists := meta[mppReceiptMetaKey]; exists {
		return errors.New("A2A handler attempted to supply MPP receipt")
	}
	value, err := adapter.MCPReceipt(challenge, payment)
	if err != nil {
		return err
	}
	meta[value.Key] = value.Value
	return nil
}

type mppMCPErrorData struct {
	HTTPStatus int             `json:"httpStatus"`
	Challenges []mpp.Challenge `json:"challenges"`
	Problem    any             `json:"problem,omitempty"`
}

type mppMCPError struct {
	Code    int             `json:"code"`
	Message string          `json:"message"`
	Data    mppMCPErrorData `json:"data"`
}

type mppMCPReceipt struct {
	*mpp.Receipt
	ChallengeID string `json:"challengeId"`
}

func (adapter *MppAdapter) Name() string { return "mpp" }

func (adapter *MppAdapter) CredentialHeaderNames() []string {
	return []string{"payment-authorization", "authorization"}
}

func (adapter *MppAdapter) MerchantEvidenceHeaderNames() []string {
	return []string{"payment-receipt"}
}

func (adapter *MppAdapter) MCPRequestMetadataKey() string     { return mppCredentialMetaKey }
func (adapter *MppAdapter) MCPRootRequestMetadataKey() string { return mppCredentialMetaKey }
func (adapter *MppAdapter) MCPReceiptMetadataKey() string     { return mppReceiptMetaKey }

func (adapter *MppAdapter) Credential(credential model.PaymentCredential) (model.HTTPCredential, error) {
	if credential.Protocol != adapter.Name() || credential.HTTP == nil ||
		(!strings.EqualFold(credential.HTTP.HeaderName, "authorization") &&
			!strings.EqualFold(credential.HTTP.HeaderName, "payment-authorization")) {
		return model.HTTPCredential{}, errors.New("MPP credential has no allowed Payment authorization delivery")
	}
	parsed, err := mpp.FromAuthorization(credential.HTTP.HeaderValue)
	if err != nil || parsed.Challenge.Method != "tempo" || parsed.Challenge.Intent != "charge" {
		return model.HTTPCredential{}, errors.New("MPP credential is not a valid Tempo charge authorization")
	}
	if err := adapter.requireCredentialHeader(credential.HTTP.HeaderValue, credential.HTTP.HeaderName); err != nil {
		return model.HTTPCredential{}, err
	}
	return *credential.HTTP, nil
}

func (adapter *MppAdapter) MerchantEvidence(headers map[string]string) (*MerchantEvidence, error) {
	value := adapter.header(headers, "payment-receipt")
	if value == "" {
		return nil, nil
	}
	receipt, err := mpp.ParseReceipt(value)
	if err != nil || receipt.Status != "success" || receipt.Method != "tempo" || receipt.Reference == "" {
		return nil, errors.New("MPP Payment-Receipt is invalid")
	}
	return &MerchantEvidence{HeaderName: "payment-receipt", HeaderValue: value}, nil
}

func (adapter *MppAdapter) PresentedCredential(headers map[string]string, limit int64) (any, error) {
	dedicated := adapter.header(headers, "payment-authorization")
	authorization := adapter.header(headers, "authorization")
	paymentAuthorization := adapter.paymentScheme(authorization)
	if dedicated != "" && paymentAuthorization != "" {
		return nil, errors.New("multiple MPP payment credentials were supplied")
	}
	value := dedicated
	if value == "" {
		value = paymentAuthorization
	}
	if value == "" {
		return nil, nil
	}
	if int64(len(value)) > limit || strings.ContainsAny(value, "\r\n") {
		return nil, errors.New("MPP Payment authorization is invalid or oversized")
	}
	credential, err := mpp.FromAuthorization(value)
	if err != nil || credential.Challenge.Method != "tempo" || credential.Challenge.Intent != "charge" {
		return nil, errors.New("MPP Payment authorization is not a valid Tempo charge credential")
	}
	actualHeader := "Authorization"
	if dedicated != "" {
		actualHeader = "Payment-Authorization"
	}
	if err := adapter.requireCredentialHeader(value, actualHeader); err != nil {
		return nil, err
	}
	return map[string]any{"authorization": value}, nil
}

func (adapter *MppAdapter) Challenge(challenge *model.PaymentChallenge) (HTTPResponse, error) {
	if challenge == nil || challenge.Protocol != adapter.Name() {
		return HTTPResponse{}, errors.New("MPP challenge protocol mismatch")
	}
	payload, err := adapter.object(challenge.Payload, "MPP challenge payload")
	if err != nil {
		return HTTPResponse{}, err
	}
	value, ok := payload["www_authenticate"].(string)
	if !ok || strings.ContainsAny(value, "\r\n") {
		return HTTPResponse{}, errors.New("MPP challenge has no valid WWW-Authenticate value")
	}
	if _, err := adapter.supportedChallengeHeader(value); err != nil {
		return HTTPResponse{}, err
	}
	body, err := json.Marshal(map[string]any{"error": map[string]string{"code": "PAYMENT_REQUIRED"}})
	if err != nil {
		return HTTPResponse{}, err
	}
	return HTTPResponse{
		Headers:     map[string]string{"WWW-Authenticate": value},
		ContentType: "application/json",
		Body:        body,
	}, nil
}

func (adapter *MppAdapter) Receipt(payment *model.SettledIncomingPayment) (HTTPResponse, error) {
	receipt, err := adapter.paymentReceipt(payment)
	if err != nil {
		return HTTPResponse{}, err
	}
	return HTTPResponse{Headers: map[string]string{
		"Payment-Receipt": mpp.FormatReceipt(receipt),
		"Cache-Control":   "private",
	}}, nil
}

func (adapter *MppAdapter) MCPChallenge(message any, limit int64) (*HTTPResponse, error) {
	encoded, err := json.Marshal(message)
	if err != nil || int64(len(encoded)) > limit {
		return nil, errors.New("MPP MCP message is invalid or oversized")
	}
	var envelope map[string]json.RawMessage
	if err := json.Unmarshal(encoded, &envelope); err != nil {
		return nil, nil
	}
	var errorData *mppMCPErrorData
	if raw, exists := envelope["error"]; exists {
		var paymentError mppMCPError
		if err := json.Unmarshal(raw, &paymentError); err == nil &&
			(paymentError.Code == mppPaymentRequiredCode || paymentError.Code == mppVerificationFailedCode) {
			errorData = &paymentError.Data
		}
	}
	var resultData *mppMCPErrorData
	if raw, exists := envelope["result"]; exists {
		var result struct {
			Meta map[string]json.RawMessage `json:"_meta"`
		}
		if err := json.Unmarshal(raw, &result); err == nil {
			if required, exists := result.Meta[mppPaymentRequiredMetaKey]; exists {
				var data mppMCPErrorData
				if err := json.Unmarshal(required, &data); err != nil {
					return nil, errors.New("MPP MCP result challenge is invalid")
				}
				resultData = &data
			}
		}
	}
	if errorData == nil && resultData == nil {
		return nil, nil
	}
	var rawEnvelope map[string]any
	decoder := json.NewDecoder(bytes.NewReader(encoded))
	decoder.UseNumber()
	if err := decoder.Decode(&rawEnvelope); err != nil {
		return nil, errors.New("MPP MCP message is invalid")
	}
	errorRaw := adapter.mcpChallenges(rawEnvelope, "error")
	resultRaw := adapter.mcpChallenges(rawEnvelope, "result")
	if errorData != nil && resultData != nil && !reflect.DeepEqual(errorRaw, resultRaw) {
		return nil, errors.New("MPP MCP error challenge conflicts with result metadata")
	}
	data := errorData
	if data == nil {
		data = resultData
	}
	if data.HTTPStatus != 402 || len(data.Challenges) == 0 {
		return nil, errors.New("MPP MCP challenge must carry HTTP 402 and at least one challenge")
	}
	for index := range data.Challenges {
		challenge := &data.Challenges[index]
		if challenge.Method != "tempo" || challenge.Intent != "charge" {
			continue
		}
		rawChallenges := errorRaw
		if rawChallenges == nil {
			rawChallenges = resultRaw
		}
		if index >= len(rawChallenges) {
			return nil, errors.New("MPP MCP challenge projection is invalid")
		}
		header, err := adapter.challengeHeader(challenge, rawChallenges[index])
		if err != nil {
			return nil, err
		}
		return &HTTPResponse{
			Headers:     map[string]string{"www-authenticate": header},
			ContentType: "application/json",
			Body:        []byte(`{"error":{"code":"PAYMENT_REQUIRED"}}`),
		}, nil
	}
	return nil, errors.New("MPP MCP challenge has no supported Tempo charge")
}

func (adapter *MppAdapter) MCPCredential(credential model.PaymentCredential) (MCPMetadata, error) {
	delivery, err := adapter.Credential(credential)
	if err != nil {
		return MCPMetadata{}, err
	}
	value, err := adapter.authorizationJSON(delivery.HeaderValue)
	if err != nil {
		return MCPMetadata{}, err
	}
	return MCPMetadata{Key: mppCredentialMetaKey, Value: value}, nil
}

func (adapter *MppAdapter) PresentedMCPCredential(meta any, limit int64) (any, error) {
	object, err := adapter.object(meta, "MCP params._meta")
	if err != nil {
		return nil, err
	}
	value, exists := object[mppCredentialMetaKey]
	if !exists {
		return nil, nil
	}
	body, err := json.Marshal(value)
	if err != nil || int64(len(body)) > limit {
		return nil, errors.New("MPP MCP credential is invalid or oversized")
	}
	authorization := "Payment " + base64.RawURLEncoding.EncodeToString(body)
	credential, err := mpp.FromAuthorization(authorization)
	if err != nil || credential.Challenge.Method != "tempo" || credential.Challenge.Intent != "charge" {
		return nil, errors.New("MPP MCP credential is not a valid Tempo charge credential")
	}
	return map[string]any{"authorization": authorization}, nil
}

func (adapter *MppAdapter) MCPPaymentRequired(challenge *model.PaymentChallenge) (MCPPaymentRequired, error) {
	if challenge == nil || challenge.Protocol != adapter.Name() {
		return MCPPaymentRequired{}, errors.New("MPP challenge protocol mismatch")
	}
	payload, err := adapter.object(challenge.Payload, "MPP challenge payload")
	if err != nil {
		return MCPPaymentRequired{}, err
	}
	value, exists := payload["mcp_challenge"]
	if !exists {
		return MCPPaymentRequired{}, errors.New("MPP challenge omitted MCP wire projection")
	}
	body, err := json.Marshal(value)
	if err != nil {
		return MCPPaymentRequired{}, err
	}
	var parsed mpp.Challenge
	if err := json.Unmarshal(body, &parsed); err != nil || parsed.Method != "tempo" || parsed.Intent != "charge" {
		return MCPPaymentRequired{}, errors.New("MPP MCP supports only the implemented Tempo charge flow")
	}
	return MCPPaymentRequired{Error: map[string]any{
		"code": mppPaymentRequiredCode, "message": "Payment Required",
		"data": map[string]any{"httpStatus": 402, "challenges": []any{value}},
	}}, nil
}

func (adapter *MppAdapter) MCPPaymentRejected(
	challenge *model.PaymentChallenge,
	message string,
) (MCPPaymentRequired, error) {
	if challenge == nil || challenge.Protocol != adapter.Name() {
		return MCPPaymentRequired{}, errors.New("MPP challenge protocol mismatch")
	}
	payload, err := adapter.object(challenge.Payload, "MPP challenge payload")
	if err != nil {
		return MCPPaymentRequired{}, err
	}
	value, exists := payload["mcp_challenge"]
	if !exists {
		return MCPPaymentRequired{}, errors.New("MPP challenge omitted MCP wire projection")
	}
	body, err := json.Marshal(value)
	if err != nil {
		return MCPPaymentRequired{}, err
	}
	var parsed mpp.Challenge
	if err := json.Unmarshal(body, &parsed); err != nil || parsed.Method != "tempo" || parsed.Intent != "charge" {
		return MCPPaymentRequired{}, errors.New("MPP MCP supports only the implemented Tempo charge flow")
	}
	return MCPPaymentRequired{Error: map[string]any{
		"code":    mppVerificationFailedCode,
		"message": message,
		"data": map[string]any{
			"httpStatus": 402,
			"challenges": []any{value},
			"problem": map[string]any{
				"type": "https://paymentauth.org/problems/verification-failed", "title": "Payment Verification Failed",
				"status": 402, "detail": message, "challengeId": parsed.ID,
			},
		},
	}}, nil
}

func (adapter *MppAdapter) MCPReceipt(
	challenge *model.PaymentChallenge,
	payment *model.SettledIncomingPayment,
) (MCPMetadata, error) {
	if challenge == nil || payment == nil || challenge.Protocol != adapter.Name() ||
		challenge.ChallengeHash != payment.Intent.ChallengeHash {
		return MCPMetadata{}, errors.New("MPP MCP receipt challenge binding is invalid")
	}
	receipt, err := adapter.paymentReceipt(payment)
	if err != nil {
		return MCPMetadata{}, err
	}
	return MCPMetadata{
		Key:   mppReceiptMetaKey,
		Value: mppMCPReceipt{Receipt: receipt, ChallengeID: challenge.Nonce},
	}, nil
}

func (adapter *MppAdapter) MCPMerchantEvidence(meta any, limit int64) (*MerchantEvidence, error) {
	object, err := adapter.object(meta, "MCP result._meta")
	if err != nil {
		return nil, err
	}
	value, exists := object[mppReceiptMetaKey]
	if !exists {
		return nil, nil
	}
	body, err := json.Marshal(value)
	if err != nil || int64(len(body)) > limit {
		return nil, errors.New("MPP MCP receipt metadata is invalid or oversized")
	}
	var receipt mppMCPReceipt
	if err := json.Unmarshal(body, &receipt); err != nil || receipt.Receipt == nil || receipt.ChallengeID == "" ||
		receipt.Status != "success" || receipt.Method != "tempo" || receipt.Reference == "" {
		return nil, errors.New("MPP MCP receipt metadata is invalid")
	}
	return &MerchantEvidence{
		HeaderName: "payment-receipt", HeaderValue: mpp.FormatReceipt(receipt.Receipt),
	}, nil
}

func (adapter *MppAdapter) supportedChallengeHeader(value string) (*mpp.Challenge, error) {
	for _, candidate := range mpp.SplitAuthenticate(value) {
		challenge, err := mpp.FromAuthenticate(candidate)
		if err != nil {
			continue
		}
		if challenge.Method == "tempo" && challenge.Intent == "charge" {
			return challenge, nil
		}
	}
	return nil, errors.New("MPP challenge has no supported Tempo charge")
}

func (adapter *MppAdapter) paymentReceipt(payment *model.SettledIncomingPayment) (*mpp.Receipt, error) {
	if payment == nil || payment.Receipt.Protocol != adapter.Name() || payment.Receipt.TransactionHash == "" ||
		payment.Receipt.ConfirmedAt <= 0 {
		return nil, errors.New("confirmed MPP receipt omitted transaction or confirmation time")
	}
	receipt := mpp.Success(
		payment.Receipt.TransactionHash,
		mpp.WithReceiptMethod("tempo"),
		mpp.WithExternalID(payment.Intent.ID),
	)
	receipt.Timestamp = time.Unix(payment.Receipt.ConfirmedAt, 0).UTC()
	return receipt, nil
}

func (adapter *MppAdapter) authorizationJSON(value string) (any, error) {
	credential, err := mpp.FromAuthorization(value)
	if err != nil || credential.Challenge.Method != "tempo" || credential.Challenge.Intent != "charge" {
		return nil, errors.New("MPP credential is not a valid Tempo charge authorization")
	}
	payment := adapter.paymentScheme(value)
	encoded := strings.TrimSpace(strings.TrimPrefix(payment, "Payment "))
	body, err := base64.RawURLEncoding.DecodeString(encoded)
	if err != nil {
		return nil, errors.New("MPP credential is not canonical base64url")
	}
	var raw any
	if err := json.Unmarshal(body, &raw); err != nil {
		return nil, errors.New("MPP credential is not valid JSON")
	}
	return raw, nil
}

func (adapter *MppAdapter) requireCredentialHeader(value string, actual string) error {
	raw, err := adapter.authorizationJSON(value)
	if err != nil {
		return err
	}
	envelope, err := adapter.object(raw, "MPP credential")
	if err != nil {
		return err
	}
	challenge, err := adapter.object(envelope["challenge"], "MPP credential challenge")
	if err != nil {
		return err
	}
	expected := "Authorization"
	if header, exists := challenge["header"]; exists {
		selected, ok := header.(string)
		if !ok || !strings.EqualFold(selected, "Payment-Authorization") {
			return errors.New("MPP credential selected an unsupported credential header")
		}
		expected = "Payment-Authorization"
	}
	if !strings.EqualFold(expected, actual) {
		return errors.New("MPP credential delivery does not match its challenged header")
	}
	return nil
}

func (adapter *MppAdapter) paymentScheme(value string) string {
	for _, candidate := range strings.Split(value, ",") {
		candidate = strings.TrimSpace(candidate)
		if len(candidate) > len("Payment ") && strings.EqualFold(candidate[:len("Payment ")], "Payment ") {
			return "Payment " + strings.TrimSpace(candidate[len("Payment "):])
		}
	}
	return ""
}

// mpp-go v0.2 validates the core challenge but predates the optional `header`
// field. Preserve that single upstream extension from the original MCP value;
// all core fields continue to come from mpp.Challenge.
func (adapter *MppAdapter) challengeHeader(challenge *mpp.Challenge, rawChallenge any) (string, error) {
	header, err := challenge.ToAuthenticateStrict(challenge.Realm)
	if err != nil {
		return "", err
	}
	object, err := adapter.object(rawChallenge, "MPP MCP challenge")
	if err != nil {
		return "", err
	}
	if selectedHeader, _ := object["header"].(string); selectedHeader != "" {
		if !strings.EqualFold(selectedHeader, "Payment-Authorization") {
			return "", errors.New("MPP challenge selected an unsupported credential header")
		}
		header += `, header="Payment-Authorization"`
	}
	return header, nil
}

func (adapter *MppAdapter) mcpChallenges(envelope map[string]any, source string) []any {
	var data map[string]any
	if source == "error" {
		errorObject, _ := envelope["error"].(map[string]any)
		data, _ = errorObject["data"].(map[string]any)
	} else {
		result, _ := envelope["result"].(map[string]any)
		meta, _ := result["_meta"].(map[string]any)
		data, _ = meta[mppPaymentRequiredMetaKey].(map[string]any)
	}
	challenges, _ := data["challenges"].([]any)
	return challenges
}

func (adapter *MppAdapter) object(value any, label string) (map[string]any, error) {
	body, err := json.Marshal(value)
	if err != nil {
		return nil, fmt.Errorf("%s cannot be encoded", label)
	}
	decoder := json.NewDecoder(bytes.NewReader(body))
	decoder.UseNumber()
	var object map[string]any
	if err := decoder.Decode(&object); err != nil || object == nil {
		return nil, fmt.Errorf("%s must be an object", label)
	}
	return object, nil
}

func (adapter *MppAdapter) header(headers map[string]string, wanted string) string {
	for name, value := range headers {
		if strings.EqualFold(name, wanted) {
			return value
		}
	}
	return ""
}
