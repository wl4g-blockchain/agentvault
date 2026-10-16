package route

import (
	"github.com/gin-gonic/gin"
	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-golang-service/pkg/handler"
)

type FeedRouter struct {
	handler *handler.FeedHandler
}

func NewFeedRouter(feedHandler *handler.FeedHandler) *FeedRouter {
	return &FeedRouter{handler: feedHandler}
}

// Register adds only ordinary operational HTTP routes. MCP belongs to Server.
func (router *FeedRouter) Register(engine *gin.Engine) {
	engine.GET("/healthz", router.handler.Health)
	engine.GET("/metrics", router.handler.Metrics)
}
