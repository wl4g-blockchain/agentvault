// Package transport contains the transport-neutral AgentVault request contract.
package transport

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
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

// TransportResult is delivered by an AsyncTransport without blocking the
// caller that submitted the request.
type TransportResult struct {
	Response TransportResponse
	Err      error
}

// AsyncTransport is implemented by transports whose native request lifecycle
// is callback/future based. The result channel is buffered and receives
// exactly one value before it is closed.
type AsyncTransport interface {
	DoAsync(context.Context, TransportRequest) <-chan TransportResult
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
	if failure.Status == 409 {
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
		envelope.Error.Code = "TRANSPORT_ERROR"
		envelope.Error.Message = fmt.Sprintf("AgentVault returned status %d", response.Status)
	}
	return &APIError{Status: response.Status, Code: envelope.Error.Code, Message: envelope.Error.Message}
}
