package payee_test

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	. "github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/payee"
)

func TestStandardPayeeMiddlewarePassesUnprotectedRoute(t *testing.T) {
	next := http.HandlerFunc(func(writer http.ResponseWriter, _ *http.Request) { writer.WriteHeader(http.StatusNoContent) })
	middleware := NewStandardPayeeMiddleware(nil)
	recorder := httptest.NewRecorder()
	middleware.Handler(next).ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/health", nil))
	if recorder.Code != http.StatusNoContent {
		t.Fatalf("unexpected status %d", recorder.Code)
	}
}

func TestStandardPayeeMiddlewareBypassesFreeToolOnProtectedMCPRoute(t *testing.T) {
	next := http.HandlerFunc(func(writer http.ResponseWriter, _ *http.Request) { writer.WriteHeader(http.StatusNoContent) })
	middleware := NewStandardPayeeMiddleware(&PayeeMiddleware{}, NewStandardMCPToolRoute(
		"/mcp", "marketpulse.realtime_news", "scenario", "offer",
		"mcp://marketpulse/realtime", "https://marketpulse.example",
	))
	recorder := httptest.NewRecorder()
	request := httptest.NewRequest(http.MethodPost, "/mcp", strings.NewReader(
		`{"jsonrpc":"2.0","method":"tools/call","params":{"name":"marketpulse.yesterday_news"}}`,
	))
	middleware.Handler(next).ServeHTTP(recorder, request)
	if recorder.Code != http.StatusNoContent {
		t.Fatalf("free MCP tool was intercepted: status=%d body=%s", recorder.Code, recorder.Body.String())
	}
}
