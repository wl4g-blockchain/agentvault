// Package transport contains the interchangeable AgentVault wire transports.
package transport

import (
	"context"
	"crypto/tls"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"strings"
	"sync"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/config"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/metadata"
	"google.golang.org/protobuf/types/known/wrapperspb"
)

const grpcExecuteMethod = "/agentvault.transport.v1.PaymentTransport/Execute"

type GrpcVaultTransport struct {
	connection *grpc.ClientConn
	config     config.GrpcConfig
	closeOnce  sync.Once
	closeErr   error
}

func NewGrpcVaultTransport(settings config.GrpcConfig) (*GrpcVaultTransport, error) {
	return (&GrpcVaultTransport{}).new(settings)
}

func (*GrpcVaultTransport) new(settings config.GrpcConfig) (*GrpcVaultTransport, error) {
	settings.ApplyDefaults()
	if err := settings.Validate(); err != nil {
		return nil, err
	}
	var transportCredentials credentials.TransportCredentials
	if settings.TLS {
		transportCredentials = credentials.NewTLS(&tls.Config{MinVersion: tls.VersionTLS12})
	} else {
		transportCredentials = insecure.NewCredentials()
	}
	connection, err := grpc.NewClient(settings.Endpoint,
		grpc.WithTransportCredentials(transportCredentials),
		grpc.WithDefaultCallOptions(
			grpc.MaxCallSendMsgSize(settings.MaxMessageBytes),
			grpc.MaxCallRecvMsgSize(settings.MaxMessageBytes),
		),
	)
	if err != nil {
		return nil, fmt.Errorf("create AgentVault gRPC client: %w", err)
	}
	return &GrpcVaultTransport{connection: connection, config: settings}, nil
}

type grpcWireRequest struct {
	Method  string            `json:"method"`
	Path    string            `json:"path"`
	Headers map[string]string `json:"headers"`
	Body    string            `json:"body"`
}

type grpcWireResponse struct {
	Status  int               `json:"status"`
	Headers map[string]string `json:"headers"`
	Body    string            `json:"body"`
}

func (transport *GrpcVaultTransport) Do(ctx context.Context, request TransportRequest) (TransportResponse, error) {
	if transport == nil || transport.connection == nil {
		return TransportResponse{}, errors.New("AgentVault gRPC transport is not configured")
	}
	traceContext, err := request.producerTraceContext()
	if err != nil {
		return TransportResponse{}, err
	}
	wireHeaders := make(map[string]string, len(request.Headers))
	for name, value := range request.Headers {
		if !strings.EqualFold(name, "traceparent") && !strings.EqualFold(name, "tracestate") {
			wireHeaders[name] = value
		}
	}
	wire, err := json.Marshal(grpcWireRequest{Method: string(request.Method), Path: request.Path,
		Headers: wireHeaders, Body: base64.RawURLEncoding.EncodeToString(request.Body)})
	if err != nil {
		return TransportResponse{}, fmt.Errorf("encode AgentVault gRPC request: %w", err)
	}
	if len(wire) > transport.config.MaxMessageBytes {
		return TransportResponse{}, errors.New("AgentVault gRPC request exceeds configured limit")
	}
	requestContext, cancel := context.WithTimeout(ctx, transport.config.RequestTimeout)
	defer cancel()
	requestContext = metadata.AppendToOutgoingContext(
		requestContext,
		"traceparent", traceContext.headers["traceparent"],
	)
	if tracestate := (TransportRequest{Headers: traceContext.headers}).headerValue("tracestate"); tracestate != "" {
		requestContext = metadata.AppendToOutgoingContext(requestContext, "tracestate", tracestate)
	}
	slog.Info("calling AgentVault gRPC with W3C Trace Context",
		"event.name", "agentvault.sdk.transport.grpc.produced",
		"trace.id", traceContext.traceID, "span.id", traceContext.spanID,
		"parent.id", traceContext.parentSpanID)
	if transport.config.AuthguardBearer != "" {
		requestContext = metadata.AppendToOutgoingContext(requestContext, "authorization", "Bearer "+transport.config.AuthguardBearer)
	}
	response := new(wrapperspb.BytesValue)
	if err := transport.connection.Invoke(requestContext, grpcExecuteMethod, wrapperspb.Bytes(wire), response); err != nil {
		return TransportResponse{}, fmt.Errorf("AgentVault gRPC request: %w", err)
	}
	if len(response.Value) > transport.config.MaxMessageBytes {
		return TransportResponse{}, errors.New("AgentVault gRPC response exceeds configured limit")
	}
	var result grpcWireResponse
	if err := json.Unmarshal(response.Value, &result); err != nil || result.Status < 100 || result.Status > 599 {
		return TransportResponse{}, errors.New("invalid AgentVault gRPC response")
	}
	body, err := base64.RawURLEncoding.DecodeString(result.Body)
	if err != nil || len(body) > transport.config.MaxMessageBytes || base64.RawURLEncoding.EncodeToString(body) != result.Body {
		return TransportResponse{}, errors.New("invalid or oversized AgentVault gRPC response body")
	}
	return TransportResponse{Status: result.Status, Headers: result.Headers, Body: body}, nil
}

func (transport *GrpcVaultTransport) Close() error {
	if transport == nil || transport.connection == nil {
		return nil
	}
	transport.closeOnce.Do(func() { transport.closeErr = transport.connection.Close() })
	return transport.closeErr
}
