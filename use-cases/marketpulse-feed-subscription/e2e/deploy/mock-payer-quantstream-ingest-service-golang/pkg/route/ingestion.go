package route

import (
	"github.com/gin-gonic/gin"
	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-service-golang/pkg/handler"
)

// IngestionRouter owns only HTTP route composition.
type IngestionRouter struct {
	handler *handler.IngestionHandler
}

func NewIngestionRouter(ingestion *handler.IngestionHandler) *IngestionRouter {
	return &IngestionRouter{handler: ingestion}
}

func (router *IngestionRouter) Engine() *gin.Engine {
	engine := gin.New()
	engine.Use(gin.Recovery())
	engine.GET("/healthz", router.handler.Health)
	engine.GET("/metrics", router.handler.Metrics)
	return engine
}
