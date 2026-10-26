package payer

import (
	"context"
	"encoding/json"
	"errors"
	"github.com/a2aproject/a2a-go/a2a"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	"strings"
)

// AuthorizeA2A only maps the message carrier. Signing, approval and reservation
// are performed by Authorize, exactly as for HTTP and MCP.
func (m *PayerMiddleware) AuthorizeA2A(ctx context.Context, request model.PayerHTTPRequest,
	response model.PayerHTTPResponse, operation model.PayerRequestContext) (*model.AuthorizedPayment, error) {
	call, err := (model.A2ACall{}).Parse(request.Body, m.config.MaxChallengeBytes)
	if err != nil || call == nil {
		return nil, err
	}
	if response.Status < 200 || response.Status >= 300 {
		return nil, nil
	}
	if int64(len(response.Body)) > m.config.MaxChallengeBytes {
		return nil, errors.New("A2A challenge exceeds limit")
	}
	message, ok := m.boundMCPResponse(response.Body, call.Envelope["id"], false)
	if !ok {
		return nil, errors.New("A2A response id does not match request")
	}
	adapter, challenge, err := m.protocols.A2AChallenge(message, m.config.MaxChallengeBytes)
	if err != nil || challenge == nil {
		return nil, err
	}
	if adapter.A2AExtension() != "" && !m.a2aExtensionActive(response.Headers, adapter.A2AExtension()) {
		return nil, errors.New("A2A payment extension activation was not acknowledged")
	}
	if header(response.Headers, "payment-required") != "" || header(response.Headers, "www-authenticate") != "" {
		return nil, errors.New("A2A message challenge conflicts with HTTP authentication headers")
	}
	challenge.Headers["payment-protocol"] = adapter.Name()
	challenge.Headers["content-type"] = challenge.ContentType
	return m.Authorize(ctx, request, model.PayerHTTPResponse{Status: 402, Headers: challenge.Headers, Body: challenge.Body}, operation)
}

func (m *PayerMiddleware) a2aExtensionActive(headers map[string]string, extension string) bool {
	for _, value := range strings.Split(header(headers, "x-a2a-extensions"), ",") {
		if strings.TrimSpace(value) == extension {
			return true
		}
	}
	return false
}

func (m *PayerMiddleware) A2AExtensions(existing string) string {
	values := []string{}
	for _, value := range strings.Split(existing, ",") {
		if strings.TrimSpace(value) != "" {
			values = append(values, strings.TrimSpace(value))
		}
	}
	for _, name := range m.config.Protocols {
		adapter, err := m.protocols.A2A(name)
		if err != nil || adapter.A2AExtension() == "" {
			continue
		}
		found := false
		for _, value := range values {
			found = found || value == adapter.A2AExtension()
		}
		if !found {
			values = append(values, adapter.A2AExtension())
		}
	}
	return strings.Join(values, ", ")
}

func (m *PayerMiddleware) A2ACredential(payment *model.AuthorizedPayment, request, challenge []byte, origin string) ([]byte, error) {
	if payment == nil || payment.Intent.Protocol != payment.Credential.Protocol {
		return nil, errors.New("invalid outbound payment binding")
	}
	call, err := (model.A2ACall{}).Parse(request, m.config.MaxChallengeBytes)
	if err != nil || call == nil {
		return nil, errors.New("paid A2A request is invalid")
	}
	adapter, err := m.protocols.A2A(payment.Credential.Protocol)
	if err != nil {
		return nil, err
	}
	delivery, err := adapter.Credential(payment.Credential)
	if err != nil || !strings.EqualFold(delivery.BoundOrigin, origin) {
		return nil, errors.New("A2A credential origin mismatch")
	}
	if _, payload, err := m.protocols.PresentedA2ACredential(call.Envelope, m.config.MaxChallengeBytes); err != nil {
		return nil, err
	} else if payload != nil {
		return nil, errors.New("A2A request already contains a payment credential")
	}
	message, ok := m.boundMCPResponse(challenge, call.Envelope["id"], false)
	if !ok {
		return nil, errors.New("A2A challenge id mismatch")
	}
	if result, ok := message["result"].(map[string]any); ok && result["kind"] == "task" {
		raw, err := json.Marshal(result)
		if err != nil {
			return nil, err
		}
		var task a2a.Task
		if err := json.Unmarshal(raw, &task); err != nil {
			return nil, err
		}
		if err := call.BindTask(&task); err != nil {
			return nil, err
		}
	}
	if err := adapter.A2AApplyCredential(call.Envelope, payment.Credential); err != nil {
		return nil, err
	}
	return call.Bytes()
}

// A2AResponse maps untrusted carrier evidence onto Report's existing DTO. It
// never marks an intent settled; independent Vault reconciliation does that.
func (m *PayerMiddleware) A2AResponse(payment *model.AuthorizedPayment, request []byte, response model.PayerHTTPResponse) (model.PayerHTTPResponse, error) {
	call, err := (model.A2ACall{}).Parse(request, m.config.MaxChallengeBytes)
	if err != nil || call == nil {
		return response, errors.New("invalid paid A2A request")
	}
	message, ok := m.boundMCPResponse(response.Body, call.Envelope["id"], false)
	if !ok {
		if response.Status >= 400 {
			return response, nil
		}
		return response, errors.New("paid A2A response id mismatch")
	}
	params, _ := call.Params()
	if result, ok := message["result"].(map[string]any); ok && result["kind"] == "task" && params.Message.TaskID != "" {
		if result["id"] != string(params.Message.TaskID) || result["contextId"] != params.Message.ContextID {
			return response, errors.New("paid A2A response changed its task/context")
		}
	}
	for _, name := range m.config.Protocols {
		adapter, err := m.protocols.A2A(name)
		if err != nil {
			continue
		}
		evidence, err := adapter.A2AMerchantEvidence(message, m.config.MaxChallengeBytes)
		if err != nil {
			return response, err
		}
		if evidence == nil {
			continue
		}
		if payment == nil || adapter.Name() != payment.Credential.Protocol {
			return response, errors.New("A2A receipt protocol mismatch")
		}
		if existing := header(response.Headers, evidence.HeaderName); existing != "" && existing != evidence.HeaderValue {
			return response, errors.New("HTTP and A2A payment receipts conflict")
		}
		if response.Headers == nil {
			response.Headers = map[string]string{}
		}
		response.Headers[evidence.HeaderName] = evidence.HeaderValue
	}
	return response, nil
}
