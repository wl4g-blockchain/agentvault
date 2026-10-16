package payee_test

import (
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
		"/mcp", "marketpulse.realtime_news", "scenario", "offer",
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
