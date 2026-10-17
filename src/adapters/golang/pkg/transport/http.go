package transport

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"time"
)

type Method string

const (
	MethodGet  Method = "GET"
	MethodPost Method = "POST"
)

type TransportRequest struct {
	Method  Method
	Path    string
	Headers map[string]string
	Body    []byte
}

func (request TransportRequest) String() string {
	return fmt.Sprintf("TransportRequest{Method:%s Path:%q Headers:<redacted> Body:<redacted>}", request.Method, request.Path)
}

func (request TransportRequest) GoString() string { return request.String() }

type TransportResponse struct {
	Status  int
	Headers map[string]string
	Body    []byte
}

func (response TransportResponse) String() string {
	return fmt.Sprintf("TransportResponse{Status:%d Headers:<redacted> Body:<redacted>}", response.Status)
}

func (response TransportResponse) GoString() string { return response.String() }

type Transport interface {
	Do(context.Context, TransportRequest) (TransportResponse, error)
}

var ErrRunConflict = errors.New("agentvault run conflict")

type APIError struct {
	Status  int
	Code    string
	Message string
}

func (failure *APIError) Error() string {
	return fmt.Sprintf("AgentVault API error %d (%s): %s", failure.Status, failure.Code, failure.Message)
}

func (failure *APIError) Unwrap() error {
	if failure.Status == http.StatusConflict {
		return ErrRunConflict
	}
	return nil
}

type Client struct {
	transport Transport
}

func NewClient(wire Transport) *Client { return &Client{transport: wire} }

func (client *Client) Get(ctx context.Context, path string, result any) error {
	return client.execute(ctx, TransportRequest{Method: MethodGet, Path: path, Headers: map[string]string{"accept": "application/json"}}, result)
}

func (client *Client) Post(ctx context.Context, path string, body, result any) error {
	encoded, err := json.Marshal(body)
	if err != nil {
		return fmt.Errorf("encode AgentVault request: %w", err)
	}
	headers := map[string]string{"accept": "application/json", "content-type": "application/json"}
	return client.execute(ctx, TransportRequest{Method: MethodPost, Path: path, Headers: headers, Body: encoded}, result)
}

func (client *Client) execute(ctx context.Context, request TransportRequest, result any) error {
	if client == nil || client.transport == nil {
		return errors.New("AgentVault transport is not configured")
	}
	response, err := client.transport.Do(ctx, request)
	if err != nil {
		return fmt.Errorf("AgentVault transport: %w", err)
	}
	if response.Status >= 200 && response.Status < 300 {
		if err := json.Unmarshal(response.Body, result); err != nil {
			return fmt.Errorf("decode AgentVault response: %w", err)
		}
		return nil
	}
	var envelope struct {
		Error struct {
			Code    string `json:"code"`
			Message string `json:"message"`
		} `json:"error"`
	}
	if err := json.Unmarshal(response.Body, &envelope); err != nil || envelope.Error.Code == "" {
		envelope.Error.Code = "HTTP_ERROR"
		envelope.Error.Message = fmt.Sprintf("AgentVault returned HTTP %d", response.Status)
	}
	return &APIError{Status: response.Status, Code: envelope.Error.Code, Message: envelope.Error.Message}
}

const defaultMaxResponseBytes int64 = 2 * 1024 * 1024

// HttpVaultTransport calls AgentVault through its deployment edge. AgentVault does
// not authenticate the optional bearer value; AuthGuard consumes it when the
// integration is enabled.
type HttpVaultTransport struct {
	client           *http.Client
	baseURL          *url.URL
	authguardBearer  string
	maxResponseBytes int64
}

func NewHttpVaultTransport(baseURL string) (*HttpVaultTransport, error) {
	return NewHttpVaultTransportWithOptions(baseURL, "", 30*time.Second, defaultMaxResponseBytes)
}

func NewHttpVaultTransportWithOptions(baseURL, authguardBearer string, timeout time.Duration, maxResponseBytes int64) (*HttpVaultTransport, error) {
	parsed, err := url.Parse(baseURL)
	if err != nil || (parsed.Scheme != "http" && parsed.Scheme != "https") || parsed.Host == "" ||
		parsed.User != nil || parsed.RawQuery != "" || parsed.Fragment != "" || timeout <= 0 || maxResponseBytes <= 0 {
		return nil, errors.New("unsafe AgentVault HTTP transport configuration")
	}
	if strings.TrimSpace(authguardBearer) != authguardBearer || strings.ContainsAny(authguardBearer, "\r\n") {
		return nil, errors.New("invalid AuthGuard bearer credential")
	}
	client := &http.Client{
		Timeout: timeout,
		CheckRedirect: func(_ *http.Request, _ []*http.Request) error {
			return http.ErrUseLastResponse
		},
	}
	return &HttpVaultTransport{client: client, baseURL: parsed, authguardBearer: authguardBearer, maxResponseBytes: maxResponseBytes}, nil
}

func (t *HttpVaultTransport) String() string {
	edge := "disabled"
	if t != nil && t.authguardBearer != "" {
		edge = "<redacted>"
	}
	if t == nil || t.baseURL == nil {
		return "HttpVaultTransport{unconfigured}"
	}
	return fmt.Sprintf("HttpVaultTransport{Origin:%q AuthguardBearer:%s}", t.baseURL.Scheme+"://"+t.baseURL.Host, edge)
}

func (t *HttpVaultTransport) Do(ctx context.Context, request TransportRequest) (TransportResponse, error) {
	if t == nil || t.client == nil || t.baseURL == nil {
		return TransportResponse{}, errors.New("AgentVault HTTP transport is not configured")
	}
	if !strings.HasPrefix(request.Path, "/") || strings.HasPrefix(request.Path, "//") || strings.Contains(request.Path, "\\") {
		return TransportResponse{}, errors.New("AgentVault request path must be origin-relative")
	}
	target := *t.baseURL
	target.Path, target.RawPath, target.RawQuery, target.Fragment = "", "", "", ""
	resolved, err := target.Parse(request.Path)
	if err != nil || resolved.Scheme != t.baseURL.Scheme || resolved.Host != t.baseURL.Host {
		return TransportResponse{}, errors.New("AgentVault request changed configured authority")
	}
	method := string(request.Method)
	outbound, err := http.NewRequestWithContext(ctx, method, resolved.String(), bytes.NewReader(request.Body))
	if err != nil {
		return TransportResponse{}, fmt.Errorf("build AgentVault HTTP request: %w", err)
	}
	for name, value := range request.Headers {
		if strings.EqualFold(name, "authorization") || strings.EqualFold(name, "host") {
			return TransportResponse{}, errors.New("reserved AgentVault request header")
		}
		outbound.Header.Set(name, value)
	}
	if t.authguardBearer != "" {
		outbound.Header.Set("Authorization", "Bearer "+t.authguardBearer)
	}
	response, err := t.client.Do(outbound)
	if err != nil {
		return TransportResponse{}, fmt.Errorf("send AgentVault HTTP request: %w", err)
	}
	defer response.Body.Close()
	if response.ContentLength > t.maxResponseBytes {
		return TransportResponse{}, errors.New("AgentVault response exceeds configured limit")
	}
	body, err := io.ReadAll(io.LimitReader(response.Body, t.maxResponseBytes+1))
	if err != nil {
		return TransportResponse{}, fmt.Errorf("read AgentVault HTTP response: %w", err)
	}
	if int64(len(body)) > t.maxResponseBytes {
		return TransportResponse{}, errors.New("AgentVault response exceeds configured limit")
	}
	headers := make(map[string]string, len(response.Header))
	for name, values := range response.Header {
		headers[name] = strings.Join(values, ", ")
	}
	return TransportResponse{Status: response.StatusCode, Headers: headers, Body: body}, nil
}
