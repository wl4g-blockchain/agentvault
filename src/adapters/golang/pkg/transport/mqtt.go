// Package transport contains the interchangeable AgentVault wire transports.
package transport

import (
	"context"
	"crypto/rand"
	"crypto/tls"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/url"
	"strings"
	"time"

	mqtt "github.com/eclipse/paho.mqtt.golang"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/config"
)

const (
	mqttAuthorizePath = "/api/v1/payments/authorize"
	mqttPaymentsPath  = "/api/v1/payments"
)

type MqttVaultTransport struct {
	config config.MqttConfig
}

func (transport *MqttVaultTransport) String() string {
	if transport == nil {
		return "MqttVaultTransport<nil>"
	}
	return "MqttVaultTransport{" + transport.config.String() + "}"
}

func (transport *MqttVaultTransport) GoString() string { return transport.String() }

func NewMqttVaultTransport(settings config.MqttConfig) (*MqttVaultTransport, error) {
	settings.ApplyDefaults()
	if err := settings.Validate(); err != nil {
		return nil, err
	}
	return &MqttVaultTransport{config: settings}, nil
}

type mqttWireRequest struct {
	Method  string            `json:"method"`
	Path    string            `json:"path"`
	Headers map[string]string `json:"headers"`
	Body    string            `json:"body"`
}

type mqttRequestEnvelope struct {
	Type          string          `json:"type"`
	Version       int             `json:"version"`
	RequestSpanID string          `json:"requestSpanId"`
	Request       mqttWireRequest `json:"request"`
}

type mqttResponseEnvelope struct {
	Type          string            `json:"type"`
	Version       int               `json:"version"`
	RequestSpanID string            `json:"requestSpanId"`
	Status        int               `json:"status"`
	Headers       map[string]string `json:"headers"`
	Body          string            `json:"body"`
}

func (transport *MqttVaultTransport) Do(ctx context.Context, request TransportRequest) (TransportResponse, error) {
	if transport == nil {
		return TransportResponse{}, errors.New("AgentVault MQTT transport is not configured")
	}
	operation, err := mqttOperationFor(request)
	if err != nil {
		return TransportResponse{}, err
	}
	traceContext, err := newProducerTraceContext(request.Headers)
	if err != nil {
		return TransportResponse{}, err
	}
	requestSpanID := traceContext.spanID
	slog.Info("publishing an AgentVault MQTT command with W3C Trace Context",
		"event.name", "agentvault.sdk.transport.mqtt.produced",
		"trace.id", traceContext.traceID, "span.id", traceContext.spanID,
		"parent.id", traceContext.parentSpanID)
	connectionID := transport.config.ConnectionPrefix + "-" + requestSpanID[:12]
	scheme := "tcp"
	if transport.config.TLS {
		scheme = "ssl"
	}
	broker := (&url.URL{
		Scheme: scheme,
		Host:   net.JoinHostPort(transport.config.Host, fmt.Sprint(transport.config.Port)),
	}).String()
	options := mqtt.NewClientOptions().
		AddBroker(broker).
		SetClientID(connectionID).
		SetUsername(transport.config.Username).
		SetPassword(transport.config.Password).
		SetCleanSession(true).
		SetAutoReconnect(false).
		SetConnectTimeout(transport.config.RequestTimeout).
		SetWriteTimeout(transport.config.RequestTimeout).
		SetKeepAlive(15 * time.Second)
	if transport.config.TLS {
		options.SetTLSConfig(&tls.Config{MinVersion: tls.VersionTLS12})
	}
	client := mqtt.NewClient(options)
	connect := client.Connect()
	if !connect.WaitTimeout(transport.config.RequestTimeout) {
		return TransportResponse{}, errors.New("AgentVault MQTT connect timed out")
	}
	if err := connect.Error(); err != nil {
		return TransportResponse{}, fmt.Errorf("AgentVault MQTT connect: %w", err)
	}
	defer client.Disconnect(100)

	resultTopic := fmt.Sprintf("agentvault/v1/clients/%s/results/%s", transport.config.VaultClientID, requestSpanID)
	resultFilter := fmt.Sprintf("agentvault/v1/clients/%s/results/+", transport.config.VaultClientID)
	commandTopic := fmt.Sprintf("agentvault/v1/clients/%s/commands/%s", transport.config.VaultClientID, operation.command)
	results := make(chan mqttResponseEnvelope, 1)
	errorsChannel := make(chan error, 1)
	subscribe := client.Subscribe(resultFilter, 1, func(_ mqtt.Client, message mqtt.Message) {
		if message.Topic() != resultTopic {
			return
		}
		if len(message.Payload()) > transport.config.MaxResponseBytes {
			select {
			case errorsChannel <- errors.New("AgentVault MQTT response is too large"):
			default:
			}
			return
		}
		var response mqttResponseEnvelope
		if err := json.Unmarshal(message.Payload(), &response); err != nil {
			select {
			case errorsChannel <- errors.New("invalid AgentVault MQTT response"):
			default:
			}
			return
		}
		select {
		case results <- response:
		default:
		}
	})
	if !subscribe.WaitTimeout(transport.config.RequestTimeout) {
		return TransportResponse{}, errors.New("AgentVault MQTT subscribe timed out")
	}
	if err := subscribe.Error(); err != nil {
		return TransportResponse{}, fmt.Errorf("subscribe AgentVault MQTT result: %w", err)
	}
	grants, ok := subscribe.(*mqtt.SubscribeToken)
	if !ok || !mqttSubscriptionGranted(grants.Result(), resultFilter) {
		return TransportResponse{}, errors.New("AgentVault MQTT result subscription was rejected or downgraded")
	}
	envelope := mqttRequestEnvelope{
		Type:          operation.messageType,
		Version:       1,
		RequestSpanID: requestSpanID,
		Request: mqttWireRequest{
			Method:  string(request.Method),
			Path:    request.Path,
			Headers: traceContext.headers,
			Body:    base64.RawURLEncoding.EncodeToString(request.Body),
		},
	}
	payload, err := json.Marshal(envelope)
	if err != nil {
		return TransportResponse{}, fmt.Errorf("encode AgentVault MQTT request: %w", err)
	}
	publish := client.Publish(commandTopic, 1, false, payload)
	if !publish.WaitTimeout(transport.config.RequestTimeout) {
		return TransportResponse{}, errors.New("AgentVault MQTT publish timed out")
	}
	if err := publish.Error(); err != nil {
		return TransportResponse{}, fmt.Errorf("publish AgentVault MQTT command: %w", err)
	}

	timer := time.NewTimer(transport.config.RequestTimeout)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return TransportResponse{}, ctx.Err()
	case <-timer.C:
		return TransportResponse{}, errors.New("AgentVault MQTT request timed out")
	case err := <-errorsChannel:
		return TransportResponse{}, err
	case response := <-results:
		if !mqttResponseTypeMatches(response.Type, response.Status, operation.responseType) || response.Version != 1 || response.RequestSpanID != requestSpanID || response.Status < 100 || response.Status > 599 {
			return TransportResponse{}, errors.New("mismatched AgentVault MQTT response")
		}
		body, err := base64.RawURLEncoding.DecodeString(response.Body)
		if err != nil || len(body) > transport.config.MaxResponseBytes {
			return TransportResponse{}, errors.New("invalid AgentVault MQTT response body")
		}
		return TransportResponse{Status: response.Status, Headers: response.Headers, Body: body}, nil
	}
}

func mqttSubscriptionGranted(results map[string]byte, expectedTopic string) bool {
	if len(results) != 1 {
		return false
	}
	qos, ok := results[expectedTopic]
	return ok && qos == 1
}

func mqttResponseTypeMatches(actual string, status int, expected string) bool {
	if status >= 400 {
		return actual == "payment.error"
	}
	return actual == expected
}

type mqttOperation struct {
	command      string
	messageType  string
	responseType string
}

func mqttOperationFor(request TransportRequest) (mqttOperation, error) {
	if request.Method != MethodPost {
		return mqttOperation{}, errors.New("MQTT payment operation must use POST")
	}
	if request.Path == mqttAuthorizePath {
		return mqttOperation{
			command:      "authorize",
			messageType:  "payment.authorize",
			responseType: "payment.authorization_result",
		}, nil
	}
	payeeOperations := map[string]mqttOperation{
		"/api/v1/offers/resolve":   {command: "resolve-offer", messageType: "payment.resolve_offer", responseType: "payment.offer_resolved"},
		"/api/v1/payee/challenges": {command: "create-challenge", messageType: "payment.create_challenge", responseType: "payment.challenge_created"},
		"/api/v1/payee/verify":     {command: "verify-payment", messageType: "payment.verify", responseType: "payment.verification_result"},
		"/api/v1/payee/settle":     {command: "settle-payment", messageType: "payment.settle", responseType: "payment.settlement_result"},
	}
	if operation, ok := payeeOperations[request.Path]; ok {
		return operation, nil
	}
	intentID := strings.TrimSuffix(strings.TrimPrefix(request.Path, mqttPaymentsPath+"/"), "/outbound-response")
	expectedPath := mqttPaymentsPath + "/" + intentID + "/outbound-response"
	if request.Path == expectedPath && validMQTTIdentifier(intentID) {
		return mqttOperation{
			command:      "outbound-response",
			messageType:  "payment.outbound_response",
			responseType: "payment.outbound_response_result",
		}, nil
	}
	return mqttOperation{}, errors.New("MQTT transport does not support this payment operation")
}

type producerTraceContext struct {
	traceID      string
	spanID       string
	parentSpanID string
	headers      map[string]string
}

func newProducerTraceContext(headers map[string]string) (producerTraceContext, error) {
	traceID, parentSpanID, flags, valid := parseTraceparent(headerValue(headers, "traceparent"))
	if !valid {
		var err error
		traceID, err = randomHex(16)
		if err != nil {
			return producerTraceContext{}, err
		}
		parentSpanID, flags = "", "01"
	}
	spanID, err := randomHex(8)
	if err != nil {
		return producerTraceContext{}, err
	}
	propagated := make(map[string]string, len(headers)+1)
	for name, value := range headers {
		propagated[strings.ToLower(name)] = value
	}
	propagated["traceparent"] = "00-" + traceID + "-" + spanID + "-" + flags
	return producerTraceContext{
		traceID: traceID, spanID: spanID, parentSpanID: parentSpanID, headers: propagated,
	}, nil
}

func parseTraceparent(value string) (string, string, string, bool) {
	fields := strings.Split(value, "-")
	if len(fields) != 4 || fields[0] != "00" || !validLowerHex(fields[1], 32) ||
		!validLowerHex(fields[2], 16) || !validLowerHex(fields[3], 2) ||
		allZero(fields[1]) || allZero(fields[2]) {
		return "", "", "", false
	}
	return fields[1], fields[2], fields[3], true
}

func validLowerHex(value string, length int) bool {
	if len(value) != length {
		return false
	}
	for _, character := range []byte(value) {
		if (character < '0' || character > '9') && (character < 'a' || character > 'f') {
			return false
		}
	}
	return true
}

func allZero(value string) bool { return strings.Trim(value, "0") == "" }

func headerValue(headers map[string]string, wanted string) string {
	for name, value := range headers {
		if strings.EqualFold(name, wanted) {
			return value
		}
	}
	return ""
}

func randomHex(size int) (string, error) {
	buffer := make([]byte, size)
	for {
		if _, err := rand.Read(buffer); err != nil {
			return "", fmt.Errorf("generate producer span context: %w", err)
		}
		if !allZero(hex.EncodeToString(buffer)) {
			return hex.EncodeToString(buffer), nil
		}
	}
}

func validMQTTIdentifier(value string) bool {
	if value == "" || len(value) > 128 {
		return false
	}
	for _, character := range []byte(value) {
		if (character >= 'a' && character <= 'z') ||
			(character >= 'A' && character <= 'Z') ||
			(character >= '0' && character <= '9') || strings.ContainsRune("-_.", rune(character)) {
			continue
		}
		return false
	}
	return true
}

func isLoopbackHost(host string) bool {
	if strings.EqualFold(host, "localhost") {
		return true
	}
	address := net.ParseIP(host)
	return address != nil && address.IsLoopback()
}
