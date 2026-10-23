package payee

import (
	"bytes"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"strings"

	"github.com/gin-gonic/gin"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/protocol"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/transport"
)

const (
	defaultMaxBodyBytes  = int64(1024 * 1024)
	settlementContextKey = "agentvault.settled_payment"
)

type GinPayeeRoute struct {
	Method     string
	Path       string
	ScenarioID string
	OfferID    string
	Resource   string
	Origin     string
	Protocol   string
	MCPTool    string
}

func NewMCPToolRoute(path, tool, scenarioID, offerID, resource, origin string) GinPayeeRoute {
	return NewMCPToolRouteWithProtocol(path, tool, scenarioID, offerID, resource, origin, "x402")
}

func NewMCPToolRouteWithProtocol(
	path, tool, scenarioID, offerID, resource, origin, protocolName string,
) GinPayeeRoute {
	return GinPayeeRoute{
		Method: http.MethodPost, Path: path, ScenarioID: scenarioID, OfferID: offerID,
		Resource: resource, Origin: origin, Protocol: protocolName, MCPTool: tool,
	}
}

type ginMCPResponseCapture struct {
	gin.ResponseWriter
	header http.Header
	body   bytes.Buffer
	status int
	limit  int64
	err    error
}

type ginReceiptResponseWriter struct {
	gin.ResponseWriter
	receiptHeaders  map[string]string
	evidenceHeaders []string
	prepared        bool
	rejected        bool
}

func newGinReceiptResponseWriter(
	writer gin.ResponseWriter,
	receiptHeaders map[string]string,
	evidenceHeaders []string,
) *ginReceiptResponseWriter {
	return &ginReceiptResponseWriter{
		ResponseWriter: writer, receiptHeaders: receiptHeaders, evidenceHeaders: evidenceHeaders,
	}
}

func (writer *ginReceiptResponseWriter) WriteHeaderNow() {
	writer.prepare()
	if !writer.rejected {
		writer.ResponseWriter.WriteHeaderNow()
	}
}

func (writer *ginReceiptResponseWriter) Write(value []byte) (int, error) {
	writer.prepare()
	if writer.rejected {
		return len(value), nil
	}
	return writer.ResponseWriter.Write(value)
}

func (writer *ginReceiptResponseWriter) WriteString(value string) (int, error) {
	writer.prepare()
	if writer.rejected {
		return len(value), nil
	}
	return writer.ResponseWriter.WriteString(value)
}

func (writer *ginReceiptResponseWriter) Flush() {
	writer.prepare()
	if !writer.rejected {
		writer.ResponseWriter.Flush()
	}
}

func (writer *ginReceiptResponseWriter) released() bool {
	status := writer.ResponseWriter.Status()
	return !writer.rejected && status >= http.StatusOK && status < http.StatusMultipleChoices
}

func (writer *ginReceiptResponseWriter) prepare() {
	if writer.prepared {
		return
	}
	writer.prepared = true
	for name := range writer.Header() {
		for _, owned := range writer.evidenceHeaders {
			if strings.EqualFold(name, owned) {
				writer.rejected = true
				for _, evidence := range writer.evidenceHeaders {
					writer.Header().Del(evidence)
				}
				writer.Header().Set("Content-Type", "application/json")
				writer.Header().Del("Content-Length")
				writer.ResponseWriter.WriteHeader(http.StatusBadGateway)
				writer.ResponseWriter.WriteHeaderNow()
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
}

func (capture *ginMCPResponseCapture) Header() http.Header { return capture.header }

func (capture *ginMCPResponseCapture) WriteHeader(status int) {
	if capture.status == 0 {
		capture.status = status
	}
}

func (capture *ginMCPResponseCapture) WriteHeaderNow() {
	if capture.status == 0 {
		capture.status = http.StatusOK
	}
}

func (capture *ginMCPResponseCapture) Write(value []byte) (int, error) {
	capture.WriteHeaderNow()
	if int64(capture.body.Len()+len(value)) > capture.limit {
		capture.err = errors.New("paid MCP response exceeds configured limit")
		return 0, capture.err
	}
	return capture.body.Write(value)
}

func (capture *ginMCPResponseCapture) WriteString(value string) (int, error) {
	return capture.Write([]byte(value))
}

func (capture *ginMCPResponseCapture) Status() int {
	if capture.status == 0 {
		return http.StatusOK
	}
	return capture.status
}

func (capture *ginMCPResponseCapture) Size() int     { return capture.body.Len() }
func (capture *ginMCPResponseCapture) Written() bool { return capture.status != 0 }
func (capture *ginMCPResponseCapture) Flush() {
	capture.err = errors.New("streaming paid MCP responses are not replayable")
}

// GinPayeeMiddleware is registered with router.Use(middleware.Handler()).
// Only exact configured routes are buffered and protected.
type GinPayeeMiddleware struct {
	*PayeeMiddleware
	routes       map[string]GinPayeeRoute
	maxBodyBytes int64
}

func NewGinPayeeMiddleware(core *PayeeMiddleware, routes ...GinPayeeRoute) *GinPayeeMiddleware {
	indexed := make(map[string]GinPayeeRoute, len(routes))
	for _, route := range routes {
		indexed[route.Method+" "+route.Path] = route
	}
	return &GinPayeeMiddleware{PayeeMiddleware: core, routes: indexed, maxBodyBytes: defaultMaxBodyBytes}
}

func (middleware *GinPayeeMiddleware) WithMaxBodyBytes(limit int64) *GinPayeeMiddleware {
	if limit > 0 {
		middleware.maxBodyBytes = limit
	}
	return middleware
}

func (middleware *GinPayeeMiddleware) Handler() gin.HandlerFunc {
	return func(ginContext *gin.Context) {
		route, protected := middleware.routes[ginContext.Request.Method+" "+ginContext.Request.URL.Path]
		if !protected {
			ginContext.Next()
			return
		}
		if middleware.PayeeMiddleware == nil || middleware.maxBodyBytes <= 0 {
			abort(ginContext, http.StatusInternalServerError, "PAYEE_CONFIG_INVALID", "payee middleware is not configured")
			return
		}
		body, err := middleware.readBody(ginContext.Request, middleware.maxBodyBytes)
		if err != nil {
			abort(ginContext, http.StatusRequestEntityTooLarge, "REQUEST_TOO_LARGE", err.Error())
			return
		}
		var call *mcpCall
		if route.MCPTool != "" {
			call, err = middleware.parseMCPCall(body, route.MCPTool, route.Protocol)
			if err != nil {
				abort(ginContext, http.StatusBadRequest, "PAYMENT_INVALID", err.Error())
				return
			}
			if call == nil {
				ginContext.Next()
				return
			}
			hasCredential, credentialErr := middleware.PayeeMiddleware.hasMCPHTTPPaymentCredential(
				ginContext.Request.Header,
			)
			if credentialErr != nil {
				abort(ginContext, http.StatusBadRequest, "PAYMENT_INVALID", credentialErr.Error())
				return
			}
			if hasCredential {
				abort(ginContext, http.StatusBadRequest, "PAYMENT_INVALID", "MCP payment credentials must use protocol metadata, not HTTP headers")
				return
			}
		}
		runID := strings.TrimSpace(ginContext.GetHeader("X-Run-Id"))
		if runID == "" {
			abort(ginContext, http.StatusBadRequest, "PAYMENT_CONTEXT_REQUIRED", "x-run-id is required")
			return
		}
		adapter, err := middleware.Protocol(route.Protocol)
		if err != nil {
			abort(ginContext, http.StatusBadGateway, "PAYMENT_PROTOCOL_UNAVAILABLE", err.Error())
			return
		}
		var payload any
		requestBody := body
		if call != nil {
			payload = call.CredentialPayload
			requestBody = call.CanonicalBody
		} else {
			payload, err = middleware.PresentedCredentialForProtocol(
				route.Protocol, ginContext.Request.Header, middleware.maxBodyBytes,
			)
			if err != nil {
				abort(ginContext, http.StatusBadRequest, "PAYMENT_INVALID", err.Error())
				return
			}
		}
		outcome, err := middleware.Protect(ginContext.Request.Context(), model.PayeeOperation{
			ScenarioID: route.ScenarioID, RunID: runID,
			OfferID: route.OfferID, Resource: route.Resource, Origin: route.Origin,
			Protocol: route.Protocol, RequestHash: middleware.requestHash(ginContext.Request, requestBody),
			CredentialPayload: payload,
		})
		if err != nil {
			var rejected *PaymentRejectedError
			if errors.As(err, &rejected) && rejected.Challenge != nil {
				if call != nil {
					middleware.writeMCPChallenge(ginContext, call, rejected.Challenge, true)
				} else {
					middleware.writeChallenge(ginContext, adapter, rejected.Challenge)
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
			abort(ginContext, status, code, err.Error())
			return
		}
		if outcome.Outcome == "payment_required" && outcome.Challenge != nil {
			if call != nil {
				middleware.writeMCPChallenge(ginContext, call, outcome.Challenge, false)
			} else {
				middleware.writeChallenge(ginContext, adapter, outcome.Challenge)
			}
			return
		}
		if outcome.Outcome != "authorized" || outcome.Payment == nil {
			abort(ginContext, http.StatusBadGateway, "SETTLEMENT_INVALID", "AgentVault returned an invalid protection outcome")
			return
		}
		ginContext.Set(settlementContextKey, outcome.Payment)
		if call != nil {
			middleware.replayBody(ginContext.Request, call.CanonicalBody)
			if middleware.serveMCPResult(ginContext, call, outcome.Challenge, outcome.Payment) {
				slog.Info("payee middleware released protected MCP resource",
					"event.name", "agentvault.sdk.payee.resource_released", "run.id", runID,
					"payment.intent_id", outcome.Payment.Intent.ID)
			}
			return
		}
		receipt, err := adapter.Receipt(outcome.Payment)
		if err != nil {
			abort(ginContext, http.StatusBadGateway, "SETTLEMENT_EVIDENCE_INVALID", err.Error())
			return
		}
		original := ginContext.Writer
		receiptWriter := newGinReceiptResponseWriter(
			original, receipt.Headers, middleware.MerchantEvidenceHeaderNames(),
		)
		ginContext.Writer = receiptWriter
		ginContext.Next()
		receiptWriter.WriteHeaderNow()
		ginContext.Writer = original
		if receiptWriter.released() {
			slog.Info("payee middleware released protected resource",
				"event.name", "agentvault.sdk.payee.resource_released", "run.id", runID,
				"payment.intent_id", outcome.Payment.Intent.ID)
		}
	}
}

func PaymentFromContext(ginContext *gin.Context) (*model.SettledIncomingPayment, bool) {
	value, exists := ginContext.Get(settlementContextKey)
	payment, valid := value.(*model.SettledIncomingPayment)
	return payment, exists && valid
}

func (middleware *GinPayeeMiddleware) writeMCPChallenge(
	ginContext *gin.Context,
	call *mcpCall,
	challenge *model.PaymentChallenge,
	rejected bool,
) {
	body, err := middleware.encodeMCPPaymentRequired(call, challenge, rejected)
	if err != nil {
		abort(ginContext, http.StatusBadGateway, "CHALLENGE_INVALID", err.Error())
		return
	}
	ginContext.Header("Payment-Protocol", challenge.Protocol)
	ginContext.Header("Payment-Expires-At", fmt.Sprint(challenge.ExpiresAt))
	ginContext.Data(http.StatusOK, "application/json", body)
	ginContext.Abort()
}

func (middleware *GinPayeeMiddleware) serveMCPResult(
	ginContext *gin.Context,
	call *mcpCall,
	challenge *model.PaymentChallenge,
	payment *model.SettledIncomingPayment,
) bool {
	original := ginContext.Writer
	capture := &ginMCPResponseCapture{
		ResponseWriter: original, header: make(http.Header), limit: middleware.maxBodyBytes,
	}
	ginContext.Writer = capture
	ginContext.Next()
	ginContext.Writer = original
	if capture.err != nil ||
		!strings.HasPrefix(strings.ToLower(capture.header.Get("Content-Type")), "application/json") {
		abort(ginContext, http.StatusBadGateway, "MCP_RESPONSE_INVALID", "paid MCP handlers must return a bounded, non-streaming application/json result")
		return false
	}
	if middleware.hasMerchantEvidenceHeader(capture.header) {
		abort(ginContext, http.StatusBadGateway, "MCP_RESPONSE_INVALID", "paid MCP handler attempted to supply settlement evidence")
		return false
	}
	if capture.status < http.StatusOK || capture.status >= http.StatusMultipleChoices {
		if _, _, err := middleware.decodeMCPResponse(call, capture.body.Bytes(), false); err != nil {
			abort(ginContext, http.StatusBadGateway, "MCP_RESPONSE_INVALID", err.Error())
			return false
		}
		for name, values := range capture.header {
			for _, value := range values {
				original.Header().Add(name, value)
			}
		}
		original.WriteHeader(capture.status)
		_, _ = original.Write(capture.body.Bytes())
		ginContext.Abort()
		return false
	}
	body, err := middleware.injectMCPReceipt(call, challenge, payment, capture.body.Bytes())
	if err != nil {
		abort(ginContext, http.StatusBadGateway, "SETTLEMENT_EVIDENCE_INVALID", err.Error())
		return false
	}
	for name, values := range capture.header {
		if strings.EqualFold(name, "Content-Length") {
			continue
		}
		for _, value := range values {
			original.Header().Add(name, value)
		}
	}
	original.Header().Set("Content-Type", "application/json")
	original.WriteHeader(capture.status)
	_, _ = original.Write(body)
	ginContext.Abort()
	return true
}

func (middleware *GinPayeeMiddleware) writeChallenge(ginContext *gin.Context, adapter protocol.Adapter, challenge *model.PaymentChallenge) {
	response, err := adapter.Challenge(challenge)
	if err != nil {
		abort(ginContext, http.StatusBadGateway, "CHALLENGE_INVALID", err.Error())
		return
	}
	for name, value := range response.Headers {
		ginContext.Header(name, value)
	}
	ginContext.Header("Payment-Protocol", challenge.Protocol)
	ginContext.Header("Payment-Expires-At", fmt.Sprint(challenge.ExpiresAt))
	ginContext.Data(http.StatusPaymentRequired, response.ContentType, response.Body)
	ginContext.Abort()
}

func abort(ginContext *gin.Context, status int, code, message string) {
	ginContext.AbortWithStatusJSON(status, gin.H{"error": gin.H{"code": code, "message": message}})
}
