package payee_test

import (
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/gin-gonic/gin"
	. "github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/payee"
)

func TestGinPayeeMiddlewarePassesUnprotectedRoute(t *testing.T) {
	gin.SetMode(gin.TestMode)
	router := gin.New()
	router.Use(NewGinPayeeMiddleware(nil).Handler())
	router.GET("/health", func(context *gin.Context) { context.Status(http.StatusNoContent) })
	recorder := httptest.NewRecorder()
	router.ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/health", nil))
	if recorder.Code != http.StatusNoContent {
		t.Fatalf("unexpected status %d", recorder.Code)
	}
}

func TestGinPayeeMiddlewareBypassesFreeToolOnProtectedMCPRoute(t *testing.T) {
	gin.SetMode(gin.TestMode)
	router := gin.New()
	router.Use(NewGinPayeeMiddleware(&PayeeMiddleware{}, NewMCPToolRoute(
		"/mcp", "marketpulse.realtime_news", "scenario", "price",
		"mcp://marketpulse/realtime", "https://marketpulse.example",
	)).Handler())
	router.POST("/mcp", func(context *gin.Context) { context.Status(http.StatusNoContent) })
	recorder := httptest.NewRecorder()
	request := httptest.NewRequest(http.MethodPost, "/mcp", strings.NewReader(
		`{"jsonrpc":"2.0","method":"tools/call","params":{"name":"marketpulse.yesterday_news"}}`,
	))
	router.ServeHTTP(recorder, request)
	if recorder.Code != http.StatusNoContent {
		t.Fatalf("free MCP tool was intercepted: status=%d body=%s", recorder.Code, recorder.Body.String())
	}
}

func TestGinPayeeMiddlewareCompletesOfficialMCPFlows(t *testing.T) {
	fixture := readPayeeMCPFixture(t)
	for _, protocolName := range []string{"x402", "mpp"} {
		t.Run(protocolName, func(t *testing.T) {
			entry := fixture["protocols"].(map[string]any)[protocolName].(map[string]any)
			core, requestHashes, paths := newMCPPayeeCore(t, protocolName, entry, true)
			gin.SetMode(gin.TestMode)
			router := gin.New()
			router.Use(NewGinPayeeMiddleware(core, NewMCPToolRouteWithProtocol(
				"/mcp", "marketpulse.realtime", "scenario", "price",
				"mcp://tool/marketpulse.realtime", "https://marketpulse.example", protocolName,
			)).Handler())
			var handlerCalls int
			router.POST("/mcp", func(ginContext *gin.Context) {
				handlerCalls++
				if ginContext.GetHeader("Authorization") != "Bearer application-token" {
					t.Fatal("application Bearer authorization was not preserved")
				}
				if payment, ok := PaymentFromContext(ginContext); !ok || payment.Intent.ID != "intent-1" {
					t.Fatalf("trusted settlement was not attached to context: %#v", payment)
				}
				body, err := io.ReadAll(ginContext.Request.Body)
				if err != nil {
					t.Fatal(err)
				}
				var forwarded map[string]any
				if err := json.Unmarshal(body, &forwarded); err != nil {
					t.Fatal(err)
				}
				meta, _ := forwarded["params"].(map[string]any)["_meta"].(map[string]any)
				if meta["x402/payment"] != nil || meta["org.paymentauth/credential"] != nil ||
					meta["application/trace"] != "preserved" {
					t.Fatalf("protocol payment metadata leaked to the business handler: %#v", meta)
				}
				ginContext.JSON(http.StatusOK, gin.H{
					"jsonrpc": "2.0", "id": 1,
					"result": gin.H{"content": []any{}, "_meta": gin.H{"application/trace": "preserved"}},
				})
			})

			unpaidRecorder := httptest.NewRecorder()
			router.ServeHTTP(unpaidRecorder, payeeMCPRequest(t, nil))
			if unpaidRecorder.Code != http.StatusOK || handlerCalls != 0 {
				t.Fatalf("unpaid MCP call was not challenged: status=%d calls=%d body=%s",
					unpaidRecorder.Code, handlerCalls, unpaidRecorder.Body.String())
			}
			if unpaidRecorder.Header().Get("Payment-Protocol") != protocolName ||
				unpaidRecorder.Header().Get("Payment-Expires-At") != "2000000000" {
				t.Fatalf("MCP challenge omitted stable payment profile metadata: %#v", unpaidRecorder.Header())
			}

			paidRecorder := httptest.NewRecorder()
			router.ServeHTTP(paidRecorder, payeeMCPRequest(t, entry["credential_meta"].(map[string]any)))
			if paidRecorder.Code != http.StatusOK || handlerCalls != 1 {
				t.Fatalf("paid MCP call was not released once: status=%d calls=%d body=%s",
					paidRecorder.Code, handlerCalls, paidRecorder.Body.String())
			}
			var envelope map[string]any
			if err := json.Unmarshal(paidRecorder.Body.Bytes(), &envelope); err != nil {
				t.Fatal(err)
			}
			meta := envelope["result"].(map[string]any)["_meta"].(map[string]any)
			receiptKey := "x402/payment-response"
			if protocolName == "mpp" {
				receiptKey = "org.paymentauth/receipt"
			}
			if meta[receiptKey] == nil || meta["application/trace"] != "preserved" {
				t.Fatalf("protocol receipt or application metadata missing: %#v", meta)
			}
			if len(*requestHashes) != 2 || (*requestHashes)[0] != (*requestHashes)[1] {
				t.Fatalf("MCP credential changed canonical request binding: %#v", *requestHashes)
			}
			wantPaths := []string{
				"/api/v1/payee/challenges", "/api/v1/payee/challenges",
				"/api/v1/payee/verify", "/api/v1/payee/settle",
			}
			if strings.Join(*paths, ",") != strings.Join(wantPaths, ",") {
				t.Fatalf("unexpected payee state machine: %#v", *paths)
			}
		})
	}
}

func TestGinPayeeMiddlewarePreservesPaidMCPBusinessFailure(t *testing.T) {
	fixture := readPayeeMCPFixture(t)
	entry := fixture["protocols"].(map[string]any)["x402"].(map[string]any)
	core, _, _ := newMCPPayeeCore(t, "x402", entry, true)
	gin.SetMode(gin.TestMode)
	router := gin.New()
	router.Use(NewGinPayeeMiddleware(core, NewMCPToolRoute(
		"/mcp", "marketpulse.realtime", "scenario", "price",
		"mcp://tool/marketpulse.realtime", "https://marketpulse.example",
	)).Handler())
	router.POST("/mcp", func(context *gin.Context) {
		context.JSON(http.StatusServiceUnavailable, gin.H{
			"jsonrpc": "2.0", "id": 1,
			"error": gin.H{"code": -32004, "message": "retry delivery"},
		})
	})
	recorder := httptest.NewRecorder()
	router.ServeHTTP(recorder, payeeMCPRequest(t, entry["credential_meta"].(map[string]any)))
	if recorder.Code != http.StatusServiceUnavailable || !strings.Contains(recorder.Body.String(), "retry delivery") {
		t.Fatalf("paid MCP business failure was rewritten: status=%d body=%s", recorder.Code, recorder.Body.String())
	}
}
