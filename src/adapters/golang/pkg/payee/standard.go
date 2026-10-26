package payee

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"strings"

	"github.com/a2aproject/a2a-go/a2asrv"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/protocol"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/transport"
)

type StandardPayeeRoute struct {
	Method     string
	Path       string
	ScenarioID string
	PriceID    string
	Resource   string
	Origin     string
	Protocol   string
	MCPTool    string
	// A2ATaskStore must be the same store used by the application's A2A server.
	// A new paid task uses run_id as its Task ID; existing Task IDs are preserved.
	A2ATaskStore a2asrv.TaskStore
}

func NewStandardMCPToolRoute(path, tool, scenarioID, priceID, resource, origin string) StandardPayeeRoute {
	return NewStandardMCPToolRouteWithProtocol(path, tool, scenarioID, priceID, resource, origin, "x402")
}

func NewStandardMCPToolRouteWithProtocol(
	path, tool, scenarioID, priceID, resource, origin, protocolName string,
) StandardPayeeRoute {
	return StandardPayeeRoute{
		Method: http.MethodPost, Path: path, ScenarioID: scenarioID, PriceID: priceID,
		Resource: resource, Origin: origin, Protocol: protocolName, MCPTool: tool,
	}
}

type standardPaymentKey struct{}

type standardMCPResponseCapture struct {
	header http.Header
	body   bytes.Buffer
	status int
	limit  int64
	err    error
}

type standardReceiptResponseWriter struct {
	http.ResponseWriter
	receiptHeaders  map[string]string
	evidenceHeaders []string
	status          int
	committed       bool
	rejected        bool
}

func (StdHttpMiddleware) newStandardReceiptResponseWriter(
	writer http.ResponseWriter,
	receiptHeaders map[string]string,
	evidenceHeaders []string,
) *standardReceiptResponseWriter {
	return &standardReceiptResponseWriter{
		ResponseWriter: writer, receiptHeaders: receiptHeaders, evidenceHeaders: evidenceHeaders,
	}
}

func (writer *standardReceiptResponseWriter) WriteHeader(status int) {
	writer.commit(status)
}

func (writer *standardReceiptResponseWriter) Write(value []byte) (int, error) {
	if !writer.committed {
		writer.commit(http.StatusOK)
	}
	if writer.rejected {
		return len(value), nil
	}
	return writer.ResponseWriter.Write(value)
}

func (writer *standardReceiptResponseWriter) Flush() {
	if !writer.committed {
		writer.commit(http.StatusOK)
	}
	if !writer.rejected {
		if flusher, ok := writer.ResponseWriter.(http.Flusher); ok {
			flusher.Flush()
		}
	}
}

func (writer *standardReceiptResponseWriter) Unwrap() http.ResponseWriter {
	return writer.ResponseWriter
}

func (writer *standardReceiptResponseWriter) finish() {
	if !writer.committed {
		writer.commit(http.StatusOK)
	}
}

func (writer *standardReceiptResponseWriter) released() bool {
	return !writer.rejected && writer.status >= http.StatusOK && writer.status < http.StatusMultipleChoices
}

func (writer *standardReceiptResponseWriter) commit(status int) {
	if writer.committed {
		return
	}
	writer.committed = true
	for name := range writer.Header() {
		for _, owned := range writer.evidenceHeaders {
			if strings.EqualFold(name, owned) {
				writer.rejected = true
				writer.status = http.StatusBadGateway
				for _, evidence := range writer.evidenceHeaders {
					writer.Header().Del(evidence)
				}
				writer.Header().Set("Content-Type", "application/json")
				writer.Header().Del("Content-Length")
				writer.ResponseWriter.WriteHeader(http.StatusBadGateway)
				_, _ = writer.ResponseWriter.Write([]byte(
					`{"error":{"code":"SETTLEMENT_EVIDENCE_INVALID","message":"paid handler attempted to supply settlement evidence"}}`,
				))
				return
			}
		}
	}
	for name, value := range writer.receiptHeaders {
		writer.Header().Set(name, value)
	}
	writer.status = status
	writer.ResponseWriter.WriteHeader(status)
}

func (capture *standardMCPResponseCapture) Header() http.Header { return capture.header }

func (capture *standardMCPResponseCapture) WriteHeader(status int) {
	if capture.status == 0 {
		capture.status = status
	}
}

func (capture *standardMCPResponseCapture) Write(value []byte) (int, error) {
	if capture.status == 0 {
		capture.status = http.StatusOK
	}
	if int64(capture.body.Len()+len(value)) > capture.limit {
		capture.err = errors.New("paid MCP response exceeds configured limit")
		return 0, capture.err
	}
	return capture.body.Write(value)
}

// StdHttpMiddleware protects exact routes in the standard net/http stack and
// composes the transport-neutral PayeeMiddleware orchestration core.
type StdHttpMiddleware struct {
	*PayeeMiddleware
	routes       map[string]StandardPayeeRoute
	maxBodyBytes int64
}

func NewStdHttpMiddleware(core *PayeeMiddleware, routes ...StandardPayeeRoute) *StdHttpMiddleware {
	indexed := make(map[string]StandardPayeeRoute, len(routes))
	for _, route := range routes {
		indexed[route.Method+" "+route.Path] = route
	}
	return &StdHttpMiddleware{PayeeMiddleware: core, routes: indexed, maxBodyBytes: 1024 * 1024}
}

var _ interface {
	Handler(http.Handler) http.Handler
} = (*StdHttpMiddleware)(nil)

func (middleware *StdHttpMiddleware) WithMaxBodyBytes(limit int64) *StdHttpMiddleware {
	if limit > 0 {
		middleware.maxBodyBytes = limit
	}
	return middleware
}

func (middleware *StdHttpMiddleware) Handler(next http.Handler) http.Handler {
	return http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		route, protected := middleware.routes[request.Method+" "+request.URL.Path]
		if !protected {
			next.ServeHTTP(writer, request)
			return
		}
		if middleware.PayeeMiddleware == nil || middleware.maxBodyBytes <= 0 {
			(StdHttpMiddleware{}).writeStandardError(writer, http.StatusInternalServerError, "PAYEE_CONFIG_INVALID", "payee middleware is not configured")
			return
		}
		body, err := middleware.readBody(request, middleware.maxBodyBytes)
		if err != nil {
			(StdHttpMiddleware{}).writeStandardError(writer, http.StatusRequestEntityTooLarge, "REQUEST_TOO_LARGE", err.Error())
			return
		}
		var call *mcpCall
		if route.MCPTool != "" && route.A2ATaskStore != nil {
			(StdHttpMiddleware{}).writeStandardError(writer, http.StatusInternalServerError, "PAYEE_CONFIG_INVALID", "MCP and A2A routes are mutually exclusive")
			return
		}
		if route.MCPTool != "" {
			call, err = middleware.parseMCPCall(body, route.MCPTool, route.Protocol)
			if err != nil {
				(StdHttpMiddleware{}).writeStandardError(writer, http.StatusBadRequest, "PAYMENT_INVALID", err.Error())
				return
			}
			if call == nil {
				next.ServeHTTP(writer, request)
				return
			}
			hasCredential, credentialErr := middleware.PayeeMiddleware.hasMCPHTTPPaymentCredential(request.Header)
			if credentialErr != nil {
				(StdHttpMiddleware{}).writeStandardError(writer, http.StatusBadRequest, "PAYMENT_INVALID", credentialErr.Error())
				return
			}
			if hasCredential {
				(StdHttpMiddleware{}).writeStandardError(writer, http.StatusBadRequest, "PAYMENT_INVALID", "MCP payment credentials must use protocol metadata, not HTTP headers")
				return
			}
		}
		runID := strings.TrimSpace(request.Header.Get("X-Run-Id"))
		if route.A2ATaskStore != nil {
			call, err = middleware.prepareA2ACall(request.Context(), body, runID, model.PayeeOperation{
				ScenarioID: route.ScenarioID, PriceID: route.PriceID, Resource: route.Resource, Origin: route.Origin, Protocol: route.Protocol,
			}, route.A2ATaskStore, request.Header)
			if err != nil {
				(StdHttpMiddleware{}).writeStandardError(writer, http.StatusBadRequest, "PAYMENT_INVALID", err.Error())
				return
			}
			if call == nil {
				next.ServeHTTP(writer, request)
				return
			}
		}
		if runID == "" {
			(StdHttpMiddleware{}).writeStandardError(writer, http.StatusBadRequest, "PAYMENT_CONTEXT_REQUIRED", "x-run-id is required")
			return
		}
		adapter, err := middleware.Protocol(route.Protocol)
		if err != nil {
			(StdHttpMiddleware{}).writeStandardError(writer, http.StatusBadGateway, "PAYMENT_PROTOCOL_UNAVAILABLE", err.Error())
			return
		}
		var payload any
		requestBody := body
		if call != nil {
			payload = call.CredentialPayload
			requestBody = call.CanonicalBody
		} else {
			payload, err = middleware.PresentedCredentialForProtocol(
				route.Protocol, request.Header, middleware.maxBodyBytes,
			)
			if err != nil {
				(StdHttpMiddleware{}).writeStandardError(writer, http.StatusBadRequest, "PAYMENT_INVALID", err.Error())
				return
			}
		}
		outcome, err := middleware.Protect(request.Context(), model.PayeeOperation{
			ScenarioID: route.ScenarioID, RunID: runID,
			PriceID: route.PriceID, Resource: route.Resource, Origin: route.Origin,
			Protocol: route.Protocol, RequestHash: middleware.requestHash(request, requestBody), CredentialPayload: payload,
		})
		if err != nil {
			var rejected *PaymentRejectedError
			if errors.As(err, &rejected) && rejected.Challenge != nil {
				if call != nil {
					middleware.writeMCPChallenge(writer, call, rejected.Challenge, true)
				} else {
					middleware.writeChallenge(writer, adapter, rejected.Challenge)
				}
				return
			}
			status, code := http.StatusBadGateway, "AGENTVAULT_UNAVAILABLE"
			var apiError *transport.APIError
			if errors.As(err, &apiError) {
				status, code = apiError.Status, apiError.Code
			} else if errors.Is(err, ErrInvalidPayment) {
				status, code = http.StatusPaymentRequired, "PAYMENT_INVALID"
			}
			(StdHttpMiddleware{}).writeStandardError(writer, status, code, err.Error())
			return
		}
		if outcome.Outcome == "payment_required" && outcome.Challenge != nil {
			if call != nil {
				middleware.writeMCPChallenge(writer, call, outcome.Challenge, false)
			} else {
				middleware.writeChallenge(writer, adapter, outcome.Challenge)
			}
			return
		}
		if outcome.Outcome != "authorized" || outcome.Payment == nil {
			(StdHttpMiddleware{}).writeStandardError(writer, http.StatusBadGateway, "SETTLEMENT_INVALID", "AgentVault returned an invalid protection outcome")
			return
		}
		request = request.WithContext(context.WithValue(request.Context(), standardPaymentKey{}, outcome.Payment))
		if call != nil {
			middleware.replayBody(request, call.CanonicalBody)
			if middleware.serveMCPResult(writer, request, next, call, outcome.Challenge, outcome.Payment) {
				slog.Info("payee middleware released protected MCP resource",
					"event.name", "agentvault.sdk.payee.resource_released", "run.id", runID,
					"payment.intent_id", outcome.Payment.Intent.ID)
			}
			return
		}
		receipt, err := adapter.Receipt(outcome.Payment)
		if err != nil {
			(StdHttpMiddleware{}).writeStandardError(writer, http.StatusBadGateway, "SETTLEMENT_EVIDENCE_INVALID", err.Error())
			return
		}
		receiptWriter := (StdHttpMiddleware{}).newStandardReceiptResponseWriter(
			writer, receipt.Headers, middleware.MerchantEvidenceHeaderNames(),
		)
		next.ServeHTTP(receiptWriter, request)
		receiptWriter.finish()
		if receiptWriter.released() {
			slog.Info("payee middleware released protected resource",
				"event.name", "agentvault.sdk.payee.resource_released", "run.id", runID,
				"payment.intent_id", outcome.Payment.Intent.ID)
		}
	})
}

func (middleware *StdHttpMiddleware) writeMCPChallenge(
	writer http.ResponseWriter,
	call *mcpCall,
	challenge *model.PaymentChallenge,
	rejected bool,
) {
	if call.a2a != nil {
		if adapter, err := middleware.protocols.A2A(challenge.Protocol); err == nil && adapter.A2AExtension() != "" {
			writer.Header().Set("X-A2A-Extensions", adapter.A2AExtension())
		}
	}
	body, err := middleware.encodeMCPPaymentRequired(call, challenge, rejected)
	if err != nil {
		(StdHttpMiddleware{}).writeStandardError(writer, http.StatusBadGateway, "CHALLENGE_INVALID", err.Error())
		return
	}
	writer.Header().Set("Content-Type", "application/json")
	writer.Header().Set("Payment-Protocol", challenge.Protocol)
	writer.Header().Set("Payment-Expires-At", fmt.Sprint(challenge.ExpiresAt))
	writer.WriteHeader(http.StatusOK)
	_, _ = writer.Write(body)
}

func (middleware *StdHttpMiddleware) serveMCPResult(
	writer http.ResponseWriter,
	request *http.Request,
	next http.Handler,
	call *mcpCall,
	challenge *model.PaymentChallenge,
	payment *model.SettledIncomingPayment,
) bool {
	capture := &standardMCPResponseCapture{header: make(http.Header), limit: middleware.maxBodyBytes}
	next.ServeHTTP(capture, request)
	// a2a-go 0.3's non-streaming JSON-RPC handler leaves Content-Type unset.
	// This explicitly configured carrier still undergoes bounded JSON-RPC and
	// Task binding validation before any response is released.
	if call.a2a != nil && capture.header.Get("Content-Type") == "" {
		capture.header.Set("Content-Type", "application/json")
	}
	if capture.err != nil ||
		!strings.HasPrefix(strings.ToLower(capture.header.Get("Content-Type")), "application/json") {
		(StdHttpMiddleware{}).writeStandardError(writer, http.StatusBadGateway, "MCP_RESPONSE_INVALID", "paid MCP handlers must return a bounded application/json result")
		return false
	}
	if middleware.hasMerchantEvidenceHeader(capture.header) {
		(StdHttpMiddleware{}).writeStandardError(writer, http.StatusBadGateway, "MCP_RESPONSE_INVALID", "paid MCP handler attempted to supply settlement evidence")
		return false
	}
	if capture.status < http.StatusOK || capture.status >= http.StatusMultipleChoices {
		if _, _, err := middleware.decodeMCPResponse(call, capture.body.Bytes(), false); err != nil {
			(StdHttpMiddleware{}).writeStandardError(writer, http.StatusBadGateway, "MCP_RESPONSE_INVALID", err.Error())
			return false
		}
		for name, values := range capture.header {
			for _, value := range values {
				writer.Header().Add(name, value)
			}
		}
		writer.WriteHeader(capture.status)
		_, _ = writer.Write(capture.body.Bytes())
		return false
	}
	body, err := middleware.injectMCPReceipt(call, challenge, payment, capture.body.Bytes())
	if err != nil {
		(StdHttpMiddleware{}).writeStandardError(writer, http.StatusBadGateway, "SETTLEMENT_EVIDENCE_INVALID", err.Error())
		return false
	}
	for name, values := range capture.header {
		if strings.EqualFold(name, "Content-Length") {
			continue
		}
		for _, value := range values {
			writer.Header().Add(name, value)
		}
	}
	writer.Header().Set("Content-Type", "application/json")
	writer.WriteHeader(capture.status)
	_, _ = writer.Write(body)
	return true
}

func PaymentFromRequest(request *http.Request) (*model.SettledIncomingPayment, bool) {
	if request == nil {
		return nil, false
	}
	payment, ok := request.Context().Value(standardPaymentKey{}).(*model.SettledIncomingPayment)
	return payment, ok
}

func (middleware *StdHttpMiddleware) writeChallenge(writer http.ResponseWriter, adapter protocol.Adapter, challenge *model.PaymentChallenge) {
	response, err := adapter.Challenge(challenge)
	if err != nil {
		(StdHttpMiddleware{}).writeStandardError(writer, http.StatusBadGateway, "CHALLENGE_INVALID", err.Error())
		return
	}
	for name, value := range response.Headers {
		writer.Header().Set(name, value)
	}
	writer.Header().Set("Content-Type", response.ContentType)
	writer.Header().Set("Payment-Protocol", challenge.Protocol)
	writer.Header().Set("Payment-Expires-At", fmt.Sprint(challenge.ExpiresAt))
	writer.WriteHeader(http.StatusPaymentRequired)
	_, _ = writer.Write(response.Body)
}

func (StdHttpMiddleware) writeStandardError(writer http.ResponseWriter, status int, code, message string) {
	writer.Header().Set("Content-Type", "application/json")
	writer.WriteHeader(status)
	_ = json.NewEncoder(writer).Encode(map[string]any{"error": map[string]string{"code": code, "message": message}})
}
