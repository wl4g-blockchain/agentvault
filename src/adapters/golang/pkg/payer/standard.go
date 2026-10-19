package payer

import (
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
	template, err := cloneReplayable(request)
	if err != nil {
		return nil, err
	}
	first, err := m.base.RoundTrip(request)
	if err != nil {
		return nil, err
	}
	if first.StatusCode != http.StatusPaymentRequired {
		return first, nil
	}
	slog.Info("payer middleware detected payment challenge",
		"event.name", "agentvault.sdk.payer.x402_detected", "run.id", operation.RunID)
	body, err := readBounded(first.Body, first.ContentLength, m.config.MaxChallengeBytes)
	first.Body.Close()
	if err != nil {
		return nil, err
	}
	origin, err := canonicalOrigin(template)
	if err != nil {
		return nil, err
	}
	slog.Info("payer middleware requested AgentVault signature",
		"event.name", "agentvault.sdk.payer.signing_requested", "run.id", operation.RunID)
	authorized, err := m.Authorize(request.Context(), model.PayerHTTPRequest{
		Method: request.Method, URL: request.URL.String(), Origin: origin,
		Headers: flattenHeaders(request.Header), FollowRedirects: false,
	}, model.PayerHTTPResponse{
		Status: first.StatusCode, Headers: flattenHeaders(first.Header), Body: body,
	}, operation)
	if err != nil {
		return nil, err
	}
	slog.Info("payer middleware received signed credential",
		"event.name", "agentvault.sdk.payer.signed", "run.id", operation.RunID,
		"payment.intent_id", authorized.Intent.ID)
	headerName, headerValue, err := m.CredentialHeader(authorized, origin, flattenHeaders(template.Header))
	if err != nil {
		return nil, err
	}
	for attempt := 0; attempt <= m.config.DeliveryRetries; attempt++ {
		slog.Info("payer middleware is delivering signed request",
			"event.name", "agentvault.sdk.payer.retrying", "run.id", operation.RunID,
			"attempt.no", attempt+1, "payment.intent_id", authorized.Intent.ID)
		paid, cloneErr := cloneReplayable(template)
		if cloneErr != nil {
			return nil, cloneErr
		}
		paid.Header.Set(headerName, headerValue)
		response, sendErr := m.base.RoundTrip(paid)
		if sendErr != nil {
			return nil, sendErr
		}
		if reportErr := m.Report(request.Context(), operation, authorized, model.PayerHTTPResponse{
			Status: response.StatusCode, Headers: flattenHeaders(response.Header),
		}); reportErr != nil {
			response.Body.Close()
			return nil, reportErr
		}
		if response.StatusCode != http.StatusServiceUnavailable || attempt == m.config.DeliveryRetries {
			response.Request = paid.WithContext(context.WithValue(paid.Context(), paymentResultKey{}, authorized))
			return response, nil
		}
		response.Body.Close()
	}
	return nil, errors.New("payer retry loop ended unexpectedly")
}

func flattenHeaders(headers http.Header) map[string]string {
	result := make(map[string]string, len(headers))
	for name, values := range headers {
		result[name] = strings.Join(values, ", ")
	}
	return result
}

func cloneReplayable(request *http.Request) (*http.Request, error) {
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

func readBounded(body io.Reader, length, limit int64) ([]byte, error) {
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

func canonicalOrigin(request *http.Request) (string, error) {
	if request == nil || request.URL == nil || request.URL.User != nil || request.URL.Host == "" ||
		(request.URL.Scheme != "http" && request.URL.Scheme != "https") {
		return "", errors.New("paid request URL must be absolute HTTP(S) without userinfo")
	}
	return (&url.URL{Scheme: request.URL.Scheme, Host: request.URL.Host}).String(), nil
}
