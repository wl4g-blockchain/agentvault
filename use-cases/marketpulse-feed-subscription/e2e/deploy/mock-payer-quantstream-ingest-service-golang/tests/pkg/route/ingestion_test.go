package route_test

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/gin-gonic/gin"
	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-service-golang/pkg/handler"
	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-service-golang/pkg/route"
)

func TestIngestionRouterExposesOnlyDeclaredRoutes(t *testing.T) {
	gin.SetMode(gin.TestMode)
	router := route.NewIngestionRouter(handler.NewIngestionHandler()).Engine()
	health := httptest.NewRecorder()
	router.ServeHTTP(health, httptest.NewRequest(http.MethodGet, "/healthz", nil))
	metrics := httptest.NewRecorder()
	router.ServeHTTP(metrics, httptest.NewRequest(http.MethodGet, "/metrics", nil))
	manual := httptest.NewRecorder()
	router.ServeHTTP(manual, httptest.NewRequest(http.MethodPost, "/api/v1/ingestion", nil))
	missing := httptest.NewRecorder()
	router.ServeHTTP(missing, httptest.NewRequest(http.MethodGet, "/unknown", nil))
	if health.Code != http.StatusOK || !strings.Contains(health.Body.String(), "ready") {
		t.Fatalf("unexpected health response: %d %s", health.Code, health.Body.String())
	}
	if missing.Code != http.StatusNotFound {
		t.Fatalf("unexpected unknown-route status: %d", missing.Code)
	}
	if metrics.Code != http.StatusOK || manual.Code != http.StatusNotFound {
		t.Fatalf("operational route boundary violated: metrics=%d manual=%d", metrics.Code, manual.Code)
	}
}
