package payer

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/url"
	"strings"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
)

type paymentContextKey struct{}
type paymentResultKey struct{}

// WithPayment attaches one stable logical operation to a paid HTTP/MCP
// request. Requests without this context bypass payment handling.
func WithPayment(request *http.Request, operation model.PayerRequestContext) *http.Request {
	return request.WithContext(context.WithValue(request.Context(), paymentContextKey{}, operation))
}

// PaymentFromResponse returns trusted local metadata attached only after this
// middleware completed an AgentVault-authorized paid retry.
func PaymentFromResponse(response *http.Response) (*model.AuthorizedPayment, bool) {
	if response == nil || response.Request == nil {
		return nil, false
	}
	payment, ok := response.Request.Context().Value(paymentResultKey{}).(*model.AuthorizedPayment)
	return payment, ok
}

// StandardPayerMiddleware is both a standard RoundTripper and a safe client
// client factory. Use Client() so redirects are disabled for credentials.
type StandardPayerMiddleware struct {
	*PayerMiddleware
	base http.RoundTripper
}

func NewStandardPayerMiddleware(core *PayerMiddleware, client *http.Client) *StandardPayerMiddleware {
	base := http.DefaultTransport
	if client != nil && client.Transport != nil {
		base = client.Transport
	}
	return &StandardPayerMiddleware{PayerMiddleware: core, base: base}
}

var _ http.RoundTripper = (*StandardPayerMiddleware)(nil)

// Client returns an http.Client that cannot forward a payment credential over
// redirects. Timeout is copied from the optional source client by callers via
// standard field assignment when a different bound is desired.
func (m *StandardPayerMiddleware) Client() *http.Client {
	return &http.Client{
		Transport: m,
		CheckRedirect: func(_ *http.Request, _ []*http.Request) error {
			return http.ErrUseLastResponse
		},
	}
}

func (m *StandardPayerMiddleware) RoundTrip(request *http.Request) (*http.Response, error) {
	operation, protected := request.Context().Value(paymentContextKey{}).(model.PayerRequestContext)
	if !protected {
		return m.base.RoundTrip(request)
	}
	if m == nil || m.PayerMiddleware == nil || m.base == nil {
		return nil, errors.New("payer HTTP middleware is not configured")
	}
	if err := model.ValidateIdentifier("run_id", operation.RunID); err != nil {
		return nil, errors.New("paid request has an invalid workflow context")
	}
	template, err := (StandardPayerMiddleware{}).cloneReplayable(request)
	if err != nil {
		return nil, err
	}
	mcpBody, mcpID, mcpCandidate, err := m.mcpRequest(template)
	if err != nil {
		return nil, err
	}
	a2aCall, err := (model.A2ACall{}).Parse(mcpBody, m.config.MaxChallengeBytes)
	if err != nil {
		return nil, err
	}
	firstRequest := request
	if a2aCall != nil {
		if existing := template.Header.Get("X-Run-Id"); existing != "" && existing != operation.RunID {
			return nil, errors.New("A2A run_id conflicts with paid operation")
		}
		template.Header.Set("X-Run-Id", operation.RunID)
		template.Header.Set("X-A2A-Extensions", m.A2AExtensions(template.Header.Get("X-A2A-Extensions")))
		firstRequest, err = (StandardPayerMiddleware{}).cloneReplayable(template)
		if err != nil {
			return nil, err
		}
	}
	first, err := m.base.RoundTrip(firstRequest)
	if err != nil {
		return nil, err
	}
	httpChallenge := first.StatusCode == http.StatusPaymentRequired
	messageCandidate := !httpChallenge && (mcpCandidate || a2aCall != nil) && m.isJSONResponse(first)
	if !httpChallenge && !messageCandidate {
		return first, nil
	}
	slog.Info("payer middleware detected payment challenge",
		"event.name", "agentvault.sdk.payer.payment_required", "run.id", operation.RunID)
	body, err := m.bufferResponse(first)
	if err != nil {
		return nil, err
	}
	origin, err := (StandardPayerMiddleware{}).canonicalOrigin(template)
	if err != nil {
		return nil, err
	}
	slog.Info("payer middleware requested AgentVault signature",
		"event.name", "agentvault.sdk.payer.signing_requested", "run.id", operation.RunID)
	sdkRequest := model.PayerHTTPRequest{
		Method: request.Method, URL: request.URL.String(), Origin: origin,
		Headers: (StandardPayerMiddleware{}).flattenHeaders(request.Header), Body: mcpBody, FollowRedirects: false,
	}
	sdkResponse := model.PayerHTTPResponse{
		Status: first.StatusCode, Headers: (StandardPayerMiddleware{}).flattenHeaders(first.Header), Body: body,
	}
	var authorized *model.AuthorizedPayment
	if httpChallenge {
		authorized, err = m.Authorize(request.Context(), sdkRequest, sdkResponse, operation)
	} else if a2aCall != nil {
		authorized, err = m.AuthorizeA2A(request.Context(), sdkRequest, sdkResponse, operation)
	} else {
		authorized, err = m.AuthorizeMCP(request.Context(), sdkRequest, sdkResponse, operation)
	}
	if err != nil {
		return nil, err
	}
	if authorized == nil {
		return first, nil
	}
	slog.Info("payer middleware received signed credential",
		"event.name", "agentvault.sdk.payer.signed", "run.id", operation.RunID,
		"payment.intent_id", authorized.Intent.ID)
	paidTemplate := template
	var a2aPaidBody []byte
	if httpChallenge {
		headerName, headerValue, err := m.CredentialHeader(authorized, origin, (StandardPayerMiddleware{}).flattenHeaders(paidTemplate.Header))
		if err != nil {
			return nil, err
		}
		paidTemplate.Header.Set(headerName, headerValue)
	} else if a2aCall != nil {
		a2aPaidBody, err = m.A2ACredential(authorized, mcpBody, body, origin)
		if err != nil {
			return nil, err
		}
		m.setReplayableBody(paidTemplate, a2aPaidBody)
	} else {
		paidBody, err := m.MCPCredential(authorized, mcpBody)
		if err != nil {
			return nil, err
		}
		m.setReplayableBody(paidTemplate, paidBody)
	}
	var response *http.Response
	var paid *http.Request
	err = m.Deliver(request.Context(), operation, authorized, func(_ int) (model.PayerHTTPResponse, error) {
		var cloneErr error
		paid, cloneErr = (StandardPayerMiddleware{}).cloneReplayable(paidTemplate)
		if cloneErr != nil {
			return model.PayerHTTPResponse{}, cloneErr
		}
		var sendErr error
		response, sendErr = m.base.RoundTrip(paid)
		if sendErr != nil {
			return model.PayerHTTPResponse{}, sendErr
		}
		view := model.PayerHTTPResponse{
			Status: response.StatusCode, Headers: (StandardPayerMiddleware{}).flattenHeaders(response.Header),
		}
		if messageCandidate && m.isJSONResponse(response) {
			view.Body, sendErr = m.bufferResponse(response)
			if sendErr != nil {
				return model.PayerHTTPResponse{}, sendErr
			}
			if a2aCall != nil {
				return m.A2AResponse(authorized, a2aPaidBody, view)
			}
			if view.Status >= 200 && view.Status < 300 {
				if _, valid := m.boundMCPResponse(view.Body, mcpID, true); !valid {
					return model.PayerHTTPResponse{}, errors.New("paid MCP response has no matching JSON-RPC result")
				}
			}
		}
		return view, nil
	}, func() {
		if response != nil && response.Body != nil {
			response.Body.Close()
		}
	})
	if err != nil {
		return nil, err
	}
	response.Request = paid.WithContext(context.WithValue(paid.Context(), paymentResultKey{}, authorized))
	return response, nil
}

func (m *StandardPayerMiddleware) mcpRequest(request *http.Request) ([]byte, any, bool, error) {
	if request.Method != http.MethodPost ||
		!strings.HasPrefix(strings.ToLower(request.Header.Get("Content-Type")), "application/json") ||
		request.GetBody == nil {
		return nil, nil, false, nil
	}
	body, err := request.GetBody()
	if err != nil {
		return nil, nil, false, fmt.Errorf("clone MCP request body: %w", err)
	}
	defer body.Close()
	value, err := io.ReadAll(io.LimitReader(body, m.config.MaxChallengeBytes+1))
	if err != nil {
		return nil, nil, false, fmt.Errorf("read MCP request: %w", err)
	}
	if int64(len(value)) > m.config.MaxChallengeBytes {
		return nil, nil, false, errors.New("MCP tools/call request exceeds configured limit")
	}
	message, ok := m.mcpCall(value)
	if !ok {
		return value, nil, false, nil
	}
	return value, message["id"], true, nil
}

func (m *StandardPayerMiddleware) isJSONResponse(response *http.Response) bool {
	return response != nil && strings.HasPrefix(
		strings.ToLower(response.Header.Get("Content-Type")), "application/json",
	)
}

func (m *StandardPayerMiddleware) bufferResponse(response *http.Response) ([]byte, error) {
	if response == nil || response.Body == nil {
		return nil, nil
	}
	body, err := (StandardPayerMiddleware{}).readBounded(response.Body, response.ContentLength, m.config.MaxChallengeBytes)
	closeErr := response.Body.Close()
	if err != nil {
		return nil, err
	}
	if closeErr != nil {
		return nil, fmt.Errorf("close payment response: %w", closeErr)
	}
	response.Body = io.NopCloser(bytes.NewReader(body))
	response.ContentLength = int64(len(body))
	return body, nil
}

func (m *StandardPayerMiddleware) setReplayableBody(request *http.Request, body []byte) {
	value := append([]byte(nil), body...)
	request.Body = io.NopCloser(bytes.NewReader(value))
	request.GetBody = func() (io.ReadCloser, error) {
		return io.NopCloser(bytes.NewReader(value)), nil
	}
	request.ContentLength = int64(len(value))
	request.Header.Del("Content-Length")
}

func (StandardPayerMiddleware) flattenHeaders(headers http.Header) map[string]string {
	result := make(map[string]string, len(headers))
	for name, values := range headers {
		result[name] = strings.Join(values, ", ")
	}
	return result
}

func (StandardPayerMiddleware) cloneReplayable(request *http.Request) (*http.Request, error) {
	clone := request.Clone(request.Context())
	if request.Body == nil || request.Body == http.NoBody {
		return clone, nil
	}
	if request.GetBody == nil {
		return nil, errors.New("paid requests must use a replayable, non-streaming body")
	}
	body, err := request.GetBody()
	if err != nil {
		return nil, fmt.Errorf("clone paid request body: %w", err)
	}
	clone.Body = body
	return clone, nil
}

func (StandardPayerMiddleware) readBounded(body io.Reader, length, limit int64) ([]byte, error) {
	if length > limit {
		return nil, errors.New("payment challenge exceeds configured limit")
	}
	value, err := io.ReadAll(io.LimitReader(body, limit+1))
	if err != nil {
		return nil, fmt.Errorf("read payment challenge: %w", err)
	}
	if int64(len(value)) > limit {
		return nil, errors.New("payment challenge exceeds configured limit")
	}
	return value, nil
}

func (StandardPayerMiddleware) canonicalOrigin(request *http.Request) (string, error) {
	if request == nil || request.URL == nil || request.URL.User != nil || request.URL.Host == "" ||
		(request.URL.Scheme != "http" && request.URL.Scheme != "https") {
		return "", errors.New("paid request URL must be absolute HTTP(S) without userinfo")
	}
	return (&url.URL{Scheme: request.URL.Scheme, Host: request.URL.Host}).String(), nil
}
