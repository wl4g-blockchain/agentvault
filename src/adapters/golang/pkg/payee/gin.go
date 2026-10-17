package payee

import (
	"bytes"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"

	"github.com/gin-gonic/gin"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
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
	return GinPayeeRoute{
		Method: http.MethodPost, Path: path, ScenarioID: scenarioID, OfferID: offerID,
		Resource: resource, Origin: origin, Protocol: "x402", MCPTool: tool,
	}
}

// GinPayeeMiddleware is registered with router.Use(middleware.Handler()).
// Only exact configured routes are buffered and protected.
type GinPayeeMiddleware struct {
	core         *PayeeMiddleware
	routes       map[string]GinPayeeRoute
	maxBodyBytes int64
}

func NewGinPayeeMiddleware(core *PayeeMiddleware, routes ...GinPayeeRoute) *GinPayeeMiddleware {
	indexed := make(map[string]GinPayeeRoute, len(routes))
	for _, route := range routes {
		indexed[route.Method+" "+route.Path] = route
	}
	return &GinPayeeMiddleware{core: core, routes: indexed, maxBodyBytes: defaultMaxBodyBytes}
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
		if middleware.core == nil || middleware.maxBodyBytes <= 0 {
			abort(ginContext, http.StatusInternalServerError, "PAYEE_CONFIG_INVALID", "payee middleware is not configured")
			return
		}
		body, err := readBody(ginContext.Request, middleware.maxBodyBytes)
		if err != nil {
			abort(ginContext, http.StatusRequestEntityTooLarge, "REQUEST_TOO_LARGE", err.Error())
			return
		}
		if route.MCPTool != "" && !isMCPToolCall(body, route.MCPTool) {
			ginContext.Next()
			return
		}
		runID := strings.TrimSpace(ginContext.GetHeader("X-Run-Id"))
		if runID == "" {
			abort(ginContext, http.StatusBadRequest, "PAYMENT_CONTEXT_REQUIRED", "x-run-id is required")
			return
		}
		payload, err := presentedCredential(ginContext.GetHeader("Payment-Signature"), middleware.maxBodyBytes)
		if err != nil {
			abort(ginContext, http.StatusBadRequest, "PAYMENT_INVALID", err.Error())
			return
		}
		outcome, err := middleware.core.Protect(ginContext.Request.Context(), model.PayeeOperation{
			ScenarioID: route.ScenarioID, RunID: runID,
			OfferID: route.OfferID, Resource: route.Resource, Origin: route.Origin,
			Protocol: route.Protocol, RequestHash: hashRequest(ginContext.Request, body),
			CredentialPayload: payload,
		})
		if err != nil {
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
			writeChallenge(ginContext, outcome.Challenge)
			return
		}
		if outcome.Outcome != "authorized" || outcome.Payment == nil {
			abort(ginContext, http.StatusBadGateway, "SETTLEMENT_INVALID", "AgentVault returned an invalid protection outcome")
			return
		}
		paymentResponse, err := encodePaymentResponse(outcome.Payment)
		if err != nil {
			abort(ginContext, http.StatusBadGateway, "SETTLEMENT_EVIDENCE_INVALID", err.Error())
			return
		}
		ginContext.Header("Payment-Response", paymentResponse)
		ginContext.Set(settlementContextKey, outcome.Payment)
		ginContext.Next()
	}
}

func isMCPToolCall(body []byte, expected string) bool {
	var request struct {
		Method string `json:"method"`
		Params struct {
			Name string `json:"name"`
		} `json:"params"`
	}
	return json.Unmarshal(body, &request) == nil && request.Method == "tools/call" && request.Params.Name == expected
}

func PaymentFromContext(ginContext *gin.Context) (*model.SettledIncomingPayment, bool) {
	value, exists := ginContext.Get(settlementContextKey)
	payment, valid := value.(*model.SettledIncomingPayment)
	return payment, exists && valid
}

func readBody(request *http.Request, limit int64) ([]byte, error) {
	if request.ContentLength > limit {
		return nil, errors.New("protected request body exceeds configured limit")
	}
	body, err := io.ReadAll(io.LimitReader(request.Body, limit+1))
	if err != nil {
		return nil, fmt.Errorf("read protected request: %w", err)
	}
	if int64(len(body)) > limit {
		return nil, errors.New("protected request body exceeds configured limit")
	}
	request.Body = io.NopCloser(bytes.NewReader(body))
	return body, nil
}

func presentedCredential(encoded string, limit int64) (any, error) {
	if encoded == "" {
		return nil, nil
	}
	if int64(len(encoded)) > (limit*4+2)/3+4 {
		return nil, errors.New("PAYMENT-SIGNATURE exceeds configured limit")
	}
	raw, err := base64.StdEncoding.DecodeString(encoded)
	if err != nil || int64(len(raw)) > limit || base64.StdEncoding.EncodeToString(raw) != encoded {
		return nil, errors.New("PAYMENT-SIGNATURE is non-canonical or oversized")
	}
	var payload map[string]any
	if err := json.Unmarshal(raw, &payload); err != nil || payload == nil {
		return nil, errors.New("PAYMENT-SIGNATURE must be an x402 JSON object")
	}
	return payload, nil
}

func hashRequest(request *http.Request, body []byte) string {
	hash := sha256.New()
	hash.Write([]byte("agentvault.payee.http.v1\x00"))
	hash.Write([]byte(request.Method))
	hash.Write([]byte{0})
	hash.Write([]byte(request.URL.Path))
	hash.Write([]byte{0})
	hash.Write(body)
	return hex.EncodeToString(hash.Sum(nil))
}

func writeChallenge(ginContext *gin.Context, challenge *model.PaymentChallenge) {
	payload, ok := challenge.Payload.(map[string]any)
	if !ok {
		abort(ginContext, http.StatusBadGateway, "CHALLENGE_INVALID", "AgentVault challenge payload is invalid")
		return
	}
	required, ok := payload["payment_required"]
	if !ok {
		abort(ginContext, http.StatusBadGateway, "CHALLENGE_INVALID", "AgentVault returned no protocol wire challenge")
		return
	}
	body, err := json.Marshal(required)
	if err != nil {
		abort(ginContext, http.StatusBadGateway, "CHALLENGE_INVALID", "AgentVault challenge cannot be encoded")
		return
	}
	ginContext.Header("Payment-Protocol", challenge.Protocol)
	ginContext.Header("Payment-Expires-At", fmt.Sprint(challenge.ExpiresAt))
	ginContext.Header("Payment-Required", base64.StdEncoding.EncodeToString(body))
	ginContext.Data(http.StatusPaymentRequired, "application/json", body)
	ginContext.Abort()
}

func encodePaymentResponse(payment *model.SettledIncomingPayment) (string, error) {
	if payment.Receipt.TransactionHash == "" || payment.Receipt.Payer == "" {
		return "", errors.New("confirmed x402 receipt omitted transaction or payer")
	}
	body, err := json.Marshal(map[string]any{
		"success": true, "payer": payment.Receipt.Payer,
		"transaction": payment.Receipt.TransactionHash, "network": payment.Receipt.Asset.Network,
	})
	if err != nil {
		return "", err
	}
	return base64.StdEncoding.EncodeToString(body), nil
}

func abort(ginContext *gin.Context, status int, code, message string) {
	ginContext.AbortWithStatusJSON(status, gin.H{"error": gin.H{"code": code, "message": message}})
}
