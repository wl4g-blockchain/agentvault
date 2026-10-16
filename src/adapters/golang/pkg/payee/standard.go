package payee

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strings"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/transport"
)

type StandardPayeeRoute struct {
	Method     string
	Path       string
	ScenarioID string
	OfferID    string
	Resource   string
	Origin     string
	Protocol   string
	MCPTool    string
}

func NewStandardMCPToolRoute(path, tool, scenarioID, offerID, resource, origin string) StandardPayeeRoute {
	return StandardPayeeRoute{
		Method: http.MethodPost, Path: path, ScenarioID: scenarioID, OfferID: offerID,
		Resource: resource, Origin: origin, Protocol: "x402", MCPTool: tool,
	}
}

type standardPaymentKey struct{}

// StandardPayeeMiddleware protects exact routes in the standard net/http stack.
type StandardPayeeMiddleware struct {
	core         *PayeeMiddleware
	routes       map[string]StandardPayeeRoute
	maxBodyBytes int64
}

func NewStandardPayeeMiddleware(core *PayeeMiddleware, routes ...StandardPayeeRoute) *StandardPayeeMiddleware {
	indexed := make(map[string]StandardPayeeRoute, len(routes))
	for _, route := range routes {
		indexed[route.Method+" "+route.Path] = route
	}
	return &StandardPayeeMiddleware{core: core, routes: indexed, maxBodyBytes: 1024 * 1024}
}

func (middleware *StandardPayeeMiddleware) WithMaxBodyBytes(limit int64) *StandardPayeeMiddleware {
	if limit > 0 {
		middleware.maxBodyBytes = limit
	}
	return middleware
}

func (middleware *StandardPayeeMiddleware) Handler(next http.Handler) http.Handler {
	return http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		route, protected := middleware.routes[request.Method+" "+request.URL.Path]
		if !protected {
			next.ServeHTTP(writer, request)
			return
		}
		if middleware.core == nil || middleware.maxBodyBytes <= 0 {
			writeStandardError(writer, http.StatusInternalServerError, "PAYEE_CONFIG_INVALID", "payee middleware is not configured")
			return
		}
		body, err := readBody(request, middleware.maxBodyBytes)
		if err != nil {
			writeStandardError(writer, http.StatusRequestEntityTooLarge, "REQUEST_TOO_LARGE", err.Error())
			return
		}
		if route.MCPTool != "" && !isMCPToolCall(body, route.MCPTool) {
			next.ServeHTTP(writer, request)
			return
		}
		runID := strings.TrimSpace(request.Header.Get("X-Run-Id"))
		executionKey := strings.TrimSpace(request.Header.Get("Mcp-Execution-Key"))
		if runID == "" || executionKey == "" {
			writeStandardError(writer, http.StatusBadRequest, "PAYMENT_CONTEXT_REQUIRED", "x-run-id and mcp-execution-key are required")
			return
		}
		payload, err := presentedCredential(request.Header.Get("Payment-Signature"), middleware.maxBodyBytes)
		if err != nil {
			writeStandardError(writer, http.StatusBadRequest, "PAYMENT_INVALID", err.Error())
			return
		}
		outcome, err := middleware.core.Protect(request.Context(), model.PayeeOperation{
			ScenarioID: route.ScenarioID, RunID: runID, ExecutionKey: executionKey,
			OfferID: route.OfferID, Resource: route.Resource, Origin: route.Origin,
			Protocol: route.Protocol, RequestHash: hashRequest(request, body), CredentialPayload: payload,
		})
		if err != nil {
			status, code := http.StatusBadGateway, "AGENTVAULT_UNAVAILABLE"
			var apiError *transport.APIError
			if errors.As(err, &apiError) {
				status, code = apiError.Status, apiError.Code
			} else if errors.Is(err, ErrInvalidPayment) {
				status, code = http.StatusPaymentRequired, "PAYMENT_INVALID"
			}
			writeStandardError(writer, status, code, err.Error())
			return
		}
		if outcome.Outcome == "payment_required" && outcome.Challenge != nil {
			writeStandardChallenge(writer, outcome.Challenge)
			return
		}
		if outcome.Outcome != "authorized" || outcome.Payment == nil {
			writeStandardError(writer, http.StatusBadGateway, "SETTLEMENT_INVALID", "AgentVault returned an invalid protection outcome")
			return
		}
		paymentResponse, err := encodePaymentResponse(outcome.Payment)
		if err != nil {
			writeStandardError(writer, http.StatusBadGateway, "SETTLEMENT_EVIDENCE_INVALID", err.Error())
			return
		}
		writer.Header().Set("Payment-Response", paymentResponse)
		request = request.WithContext(context.WithValue(request.Context(), standardPaymentKey{}, outcome.Payment))
		next.ServeHTTP(writer, request)
	})
}

func PaymentFromRequest(request *http.Request) (*model.SettledIncomingPayment, bool) {
	if request == nil {
		return nil, false
	}
	payment, ok := request.Context().Value(standardPaymentKey{}).(*model.SettledIncomingPayment)
	return payment, ok
}

func writeStandardChallenge(writer http.ResponseWriter, challenge *model.PaymentChallenge) {
	payload, ok := challenge.Payload.(map[string]any)
	if !ok {
		writeStandardError(writer, http.StatusBadGateway, "CHALLENGE_INVALID", "AgentVault challenge payload is invalid")
		return
	}
	required, ok := payload["payment_required"]
	if !ok {
		writeStandardError(writer, http.StatusBadGateway, "CHALLENGE_INVALID", "AgentVault returned no protocol wire challenge")
		return
	}
	body, err := json.Marshal(required)
	if err != nil {
		writeStandardError(writer, http.StatusBadGateway, "CHALLENGE_INVALID", "AgentVault challenge cannot be encoded")
		return
	}
	writer.Header().Set("Content-Type", "application/json")
	writer.Header().Set("Payment-Protocol", challenge.Protocol)
	writer.Header().Set("Payment-Expires-At", fmt.Sprint(challenge.ExpiresAt))
	writer.Header().Set("Payment-Required", base64.StdEncoding.EncodeToString(body))
	writer.WriteHeader(http.StatusPaymentRequired)
	_, _ = writer.Write(body)
}

func writeStandardError(writer http.ResponseWriter, status int, code, message string) {
	writer.Header().Set("Content-Type", "application/json")
	writer.WriteHeader(status)
	_ = json.NewEncoder(writer).Encode(map[string]any{"error": map[string]string{"code": code, "message": message}})
}
