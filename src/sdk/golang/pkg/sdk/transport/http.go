package transport

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"github.com/wl4g-blockchain/agentvault/src/sdk/golang/pkg/sdk/config"
	"io"
	"log/slog"
	"net/http"
	"strings"
	"sync/atomic"
)

type HttpVaultTransport struct {
	client *http.Client
	config config.HttpConfig
	closed atomic.Bool
}

func NewHttpVaultTransport(settings config.HttpConfig) (*HttpVaultTransport, error) {
	settings.ApplyDefaults()
	if e := settings.Validate(); e != nil {
		return nil, e
	}
	settings.BaseURL = strings.TrimRight(settings.BaseURL, "/")
	return &HttpVaultTransport{config: settings, client: &http.Client{Timeout: settings.RequestTimeout, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}}, nil
}
func (t *HttpVaultTransport) Do(ctx context.Context, request TransportRequest) (TransportResponse, error) {
	if t.closed.Load() {
		return TransportResponse{}, errors.New("AgentVault HTTP transport is closed")
	}
	if !strings.HasPrefix(request.Path, "/api/v1/") || strings.Contains(request.Path, "\\") || len(request.Body) > t.config.MaxResponseBytes {
		return TransportResponse{}, errors.New("invalid AgentVault HTTP request")
	}
	trace, e := request.producerTraceContext()
	if e != nil {
		return TransportResponse{}, e
	}
	r, e := http.NewRequestWithContext(ctx, string(request.Method), t.config.BaseURL+request.Path, bytes.NewReader(request.Body))
	if e != nil {
		return TransportResponse{}, e
	}
	for k, v := range trace.headers {
		r.Header.Set(k, v)
	}
	if t.config.AuthguardBearer != "" {
		r.Header.Set("Authorization", "Bearer "+t.config.AuthguardBearer)
	}
	slog.Info("agentvault.sdk.transport.http.produced", "trace_id", trace.traceID, "span_id", trace.spanID, "parent_id", trace.parentSpanID)
	response, e := t.client.Do(r)
	if e != nil {
		return TransportResponse{}, fmt.Errorf("AgentVault HTTP request failed: %w", e)
	}
	defer response.Body.Close()
	body, e := io.ReadAll(io.LimitReader(response.Body, int64(t.config.MaxResponseBytes)+1))
	if e != nil {
		return TransportResponse{}, e
	}
	if len(body) > t.config.MaxResponseBytes {
		return TransportResponse{}, errors.New("AgentVault HTTP response exceeds configured limit")
	}
	headers := map[string]string{}
	for k, v := range response.Header {
		if len(v) > 0 {
			headers[strings.ToLower(k)] = v[0]
		}
	}
	return TransportResponse{Status: response.StatusCode, Headers: headers, Body: body}, nil
}
func (t *HttpVaultTransport) Close() error {
	if !t.closed.Swap(true) {
		t.client.CloseIdleConnections()
	}
	return nil
}

func (t *HttpVaultTransport) String() string {
	return "HttpVaultTransport{Config:" + t.config.String() + "}"
}
func (t *HttpVaultTransport) GoString() string { return t.String() }
