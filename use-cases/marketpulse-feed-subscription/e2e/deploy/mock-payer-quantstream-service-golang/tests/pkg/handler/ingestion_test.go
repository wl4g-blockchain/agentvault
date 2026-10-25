package handler_test

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/gin-gonic/gin"
	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-service-golang/pkg/handler"
)

func TestIngestionHandlerHealthIdentifiesTheGoIngestor(t *testing.T) {
	gin.SetMode(gin.TestMode)
	recorder := httptest.NewRecorder()
	context, _ := gin.CreateTestContext(recorder)
	context.Request = httptest.NewRequest(http.MethodGet, "/healthz", nil)
	handler.NewIngestionHandler().Health(context)
	if recorder.Code != http.StatusOK || !strings.Contains(recorder.Body.String(), `"service":"quantstream-golang"`) {
		t.Fatalf("unexpected health response: %d %s", recorder.Code, recorder.Body.String())
	}
	metrics := httptest.NewRecorder()
	metricsContext, _ := gin.CreateTestContext(metrics)
	handler.NewIngestionHandler().Metrics(metricsContext)
	if metrics.Code != http.StatusOK || !strings.Contains(metrics.Body.String(), "quantstream_up 1") {
		t.Fatalf("unexpected metrics response: %d %s", metrics.Code, metrics.Body.String())
	}
}
