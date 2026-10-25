// Package transport contains the interchangeable AgentVault wire transports.
package transport

import (
	"bytes"
	"context"
	"crypto/rand"
	"crypto/tls"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/url"
	"strings"
	"sync"
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
	prefix string
	client mqtt.Client

	mu        sync.Mutex
	pending   map[string]*mqttPendingRequest
	started   bool
	ready     bool
	closed    bool
	closeOnce sync.Once
}

type mqttPendingRequest struct {
	expectedType string
	commandTopic string
	payload      []byte
	result       chan TransportResult
	cancel       context.CancelFunc
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
	transport := &MqttVaultTransport{
		config:  settings,
		prefix:  fmt.Sprintf("agentvault/v1/clients/%s", settings.VaultClientID),
		pending: make(map[string]*mqttPendingRequest),
	}
	connectionSuffix, err := randomHex(6)
	if err != nil {
		return nil, err
	}
	transport.client = mqtt.NewClient(transport.clientOptions(settings.ConnectionPrefix + "-" + connectionSuffix))
	return transport, nil
}

// Close is idempotent and fails every outstanding future. It never closes a
// broker connection owned by another transport instance.
func (transport *MqttVaultTransport) Close() error {
	if transport == nil {
		return nil
	}
	transport.closeOnce.Do(func() {
		transport.mu.Lock()
		transport.closed = true
		transport.ready = false
		client := transport.client
		transport.mu.Unlock()
		if client != nil && client.IsConnected() {
			client.Disconnect(250)
		}
		transport.failAll(errors.New("AgentVault MQTT transport is closed"))
	})
	return nil
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
	result := <-transport.DoAsync(ctx, request)
	return result.Response, result.Err
}

// DoAsync submits a QoS 1 request and returns immediately. A reconnect never
// creates a new payment attempt: after the result subscription is restored,
// the exact same serialized envelope is republished and the server's
// requestSpanId/payload digest cache coalesces duplicates.
func (transport *MqttVaultTransport) DoAsync(ctx context.Context, request TransportRequest) <-chan TransportResult {
	result := make(chan TransportResult, 1)
	if transport == nil {
		completeTransportResult(result, TransportResult{Err: errors.New("AgentVault MQTT transport is not configured")})
		return result
	}
	operation, err := mqttOperationFor(request)
	if err != nil {
		completeTransportResult(result, TransportResult{Err: err})
		return result
	}
	traceContext, err := newProducerTraceContext(request.Headers)
	if err != nil {
		completeTransportResult(result, TransportResult{Err: err})
		return result
	}
	requestSpanID := traceContext.spanID
	slog.Info("publishing an AgentVault MQTT command with W3C Trace Context",
		"event.name", "agentvault.sdk.transport.mqtt.produced",
		"trace.id", traceContext.traceID, "span.id", traceContext.spanID,
		"parent.id", traceContext.parentSpanID)
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
		completeTransportResult(result, TransportResult{Err: fmt.Errorf("encode AgentVault MQTT request: %w", err)})
		return result
	}
	if len(payload) > transport.config.MaxResponseBytes {
		completeTransportResult(result, TransportResult{Err: errors.New("AgentVault MQTT request exceeds configured limit")})
		return result
	}
	requestContext, cancel := context.WithTimeout(ctx, transport.config.RequestTimeout)
	waiting := &mqttPendingRequest{
		expectedType: operation.responseType,
		commandTopic: fmt.Sprintf("%s/commands/%s", transport.prefix, operation.command),
		payload:      append([]byte(nil), payload...),
		result:       result,
		cancel:       cancel,
	}
	transport.mu.Lock()
	if transport.closed {
		transport.mu.Unlock()
		cancel()
		completeTransportResult(result, TransportResult{Err: errors.New("AgentVault MQTT transport is closed")})
		return result
	}
	if len(transport.pending) >= 256 {
		transport.mu.Unlock()
		cancel()
		completeTransportResult(result, TransportResult{Err: errors.New("too many pending MQTT payment requests")})
		return result
	}
	if _, exists := transport.pending[requestSpanID]; exists {
		transport.mu.Unlock()
		cancel()
		completeTransportResult(result, TransportResult{Err: errors.New("MQTT requestSpanId collision")})
		return result
	}
	transport.pending[requestSpanID] = waiting
	ready := transport.ready
	transport.startLocked()
	transport.mu.Unlock()
	if ready {
		transport.publishPending(requestSpanID)
	}
	go func() {
		<-requestContext.Done()
		if errors.Is(requestContext.Err(), context.DeadlineExceeded) {
			transport.complete(requestSpanID, TransportResult{Err: errors.New("AgentVault MQTT request timed out")})
		} else {
			transport.complete(requestSpanID, TransportResult{Err: requestContext.Err()})
		}
	}()
	return result
}

func (transport *MqttVaultTransport) clientOptions(connectionID string) *mqtt.ClientOptions {
	scheme := "tcp"
	if transport.config.TLS {
		scheme = "ssl"
	}
	broker := (&url.URL{Scheme: scheme, Host: net.JoinHostPort(transport.config.Host, fmt.Sprint(transport.config.Port))}).String()
	options := mqtt.NewClientOptions().
		AddBroker(broker).
		SetClientID(connectionID).
		SetUsername(transport.config.Username).
		SetPassword(transport.config.Password).
		SetCleanSession(true).
		SetAutoReconnect(true).
		SetConnectRetry(true).
		SetConnectRetryInterval(time.Second).
		SetResumeSubs(false).
		SetOrderMatters(false).
		SetConnectTimeout(transport.config.RequestTimeout).
		SetWriteTimeout(transport.config.RequestTimeout).
		SetKeepAlive(15 * time.Second)
	options.SetOnConnectHandler(transport.onConnect)
	options.SetConnectionLostHandler(func(_ mqtt.Client, _ error) { transport.markDisconnected() })
	options.SetReconnectingHandler(func(_ mqtt.Client, _ *mqtt.ClientOptions) { transport.markDisconnected() })
	if transport.config.TLS {
		options.SetTLSConfig(&tls.Config{MinVersion: tls.VersionTLS12})
	}
	return options
}

func (transport *MqttVaultTransport) startLocked() {
	if transport.started {
		return
	}
	transport.started = true
	transport.client.Connect()
}

func (transport *MqttVaultTransport) onConnect(client mqtt.Client) {
	transport.markDisconnected()
	filter := transport.prefix + "/results/+"
	token := client.Subscribe(filter, 1, transport.onMessage)
	go func() {
		if !token.WaitTimeout(transport.config.RequestTimeout) || token.Error() != nil {
			transport.failAll(errors.New("AgentVault MQTT result subscription failed"))
			return
		}
		grants, ok := token.(*mqtt.SubscribeToken)
		if !ok || !mqttSubscriptionGranted(grants.Result(), filter) {
			transport.failAll(errors.New("AgentVault MQTT result subscription was rejected or downgraded"))
			return
		}
		transport.mu.Lock()
		if transport.closed {
			transport.mu.Unlock()
			return
		}
		transport.ready = true
		ids := make([]string, 0, len(transport.pending))
		for id := range transport.pending {
			ids = append(ids, id)
		}
		transport.mu.Unlock()
		for _, id := range ids {
			transport.publishPending(id)
		}
	}()
}

func (transport *MqttVaultTransport) markDisconnected() {
	transport.mu.Lock()
	transport.ready = false
	transport.mu.Unlock()
}

func (transport *MqttVaultTransport) publishPending(requestSpanID string) {
	transport.mu.Lock()
	waiting := transport.pending[requestSpanID]
	ready := transport.ready && !transport.closed
	client := transport.client
	transport.mu.Unlock()
	if waiting == nil || !ready || client == nil {
		return
	}
	// Paho owns the QoS 1 PUBACK retry. If the connection drops before the
	// result arrives, onConnect republishes this exact payload after restoring
	// the result subscription. Any duplicate is intentionally handled by Vault.
	client.Publish(waiting.commandTopic, 1, false, waiting.payload)
}

func (transport *MqttVaultTransport) onMessage(_ mqtt.Client, message mqtt.Message) {
	if len(message.Payload()) > transport.config.MaxResponseBytes {
		return
	}
	var response mqttResponseEnvelope
	decoder := json.NewDecoder(bytes.NewReader(message.Payload()))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&response); err != nil || decoder.Decode(new(any)) != io.EOF {
		return
	}
	transport.mu.Lock()
	waiting := transport.pending[response.RequestSpanID]
	transport.mu.Unlock()
	if waiting == nil || message.Topic() != transport.prefix+"/results/"+response.RequestSpanID ||
		response.Version != 1 || response.Status < 100 || response.Status > 599 ||
		!mqttResponseTypeMatches(response.Type, response.Status, waiting.expectedType) {
		return
	}
	body, err := base64.RawURLEncoding.DecodeString(response.Body)
	if err != nil || len(body) > transport.config.MaxResponseBytes || base64.RawURLEncoding.EncodeToString(body) != response.Body {
		return
	}
	transport.complete(response.RequestSpanID, TransportResult{Response: TransportResponse{
		Status: response.Status, Headers: response.Headers, Body: body,
	}})
}

func (transport *MqttVaultTransport) complete(requestSpanID string, result TransportResult) {
	transport.mu.Lock()
	waiting := transport.pending[requestSpanID]
	if waiting != nil {
		delete(transport.pending, requestSpanID)
	}
	transport.mu.Unlock()
	if waiting == nil {
		return
	}
	waiting.cancel()
	completeTransportResult(waiting.result, result)
}

func (transport *MqttVaultTransport) failAll(err error) {
	transport.mu.Lock()
	pending := transport.pending
	transport.pending = make(map[string]*mqttPendingRequest)
	transport.mu.Unlock()
	for _, waiting := range pending {
		waiting.cancel()
		completeTransportResult(waiting.result, TransportResult{Err: err})
	}
}

func completeTransportResult(result chan TransportResult, value TransportResult) {
	result <- value
	close(result)
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
