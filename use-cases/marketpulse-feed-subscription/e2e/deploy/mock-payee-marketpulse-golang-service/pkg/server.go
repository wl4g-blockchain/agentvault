package marketpulse

import (
	"fmt"
	"os"
	"strings"

	"github.com/gin-gonic/gin"
	agentvault "github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/config"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/payee"
	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-golang-service/pkg/handler"
	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-golang-service/pkg/route"
	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-golang-service/pkg/storage"
)

// Server owns the Gin listener, MCP endpoint, SDK middleware and component lifecycle.
type Server struct {
	bind    string
	engine  *gin.Engine
	storage *storage.FeedSQLiteStorage
}

// NewServerFromEnv builds the complete MarketPulse MCP server from runtime configuration.
func NewServerFromEnv() (*Server, error) {
	vaultURL, err := required("AGENTVAULT_URL")
	if err != nil {
		return nil, err
	}
	wire, err := agentvault.NewHttpVaultTransport(vaultURL)
	if err != nil {
		return nil, err
	}
	core, err := agentvault.New(wire).Payee(config.PayeeConfig{})
	if err != nil {
		return nil, err
	}
	databasePath, err := required("MARKETPULSE_DATABASE")
	if err != nil {
		return nil, err
	}
	database, err := storage.NewFeedSQLiteStorage(databasePath)
	if err != nil {
		return nil, err
	}
	closeOnFailure := true
	defer func() {
		if closeOnFailure {
			database.Close()
		}
	}()
	feedHandler := handler.NewFeedHandler(database, os.Getenv("MARKETPULSE_E2E_ALLOW_FAILURE_INJECTION") == "true")
	feedRoute, err := mcpRoute()
	if err != nil {
		return nil, err
	}
	middleware := payee.NewGinPayeeMiddleware(core, feedRoute)
	engine := gin.New()
	engine.Use(gin.Recovery())
	route.NewFeedRouter(feedHandler).Register(engine)
	engine.POST("/mcp", middleware.Handler(), feedHandler.MCP)
	bind, err := required("MARKETPULSE_BIND")
	if err != nil {
		return nil, err
	}
	closeOnFailure = false
	return &Server{bind: bind, engine: engine, storage: database}, nil
}

func mcpRoute() (payee.GinPayeeRoute, error) {
	scenarioID, err := required("MARKETPULSE_SCENARIO_ID")
	if err != nil {
		return payee.GinPayeeRoute{}, err
	}
	offerID, err := required("MARKETPULSE_OFFER_ID")
	if err != nil {
		return payee.GinPayeeRoute{}, err
	}
	resourceURL, err := required("MARKETPULSE_RESOURCE_URL")
	if err != nil {
		return payee.GinPayeeRoute{}, err
	}
	origin, err := required("MARKETPULSE_ORIGIN")
	if err != nil {
		return payee.GinPayeeRoute{}, err
	}
	return payee.NewMCPToolRoute("/mcp", "marketpulse.realtime_news", scenarioID, offerID, resourceURL, origin), nil
}

// Run starts the HTTP listener. The /mcp endpoint is the only payment-protected route.
func (server *Server) Run() error { return server.engine.Run(server.bind) }

// Close releases the durable feed store.
func (server *Server) Close() error { return server.storage.Close() }

func required(name string) (string, error) {
	value := strings.TrimSpace(os.Getenv(name))
	if value == "" {
		return "", fmt.Errorf("%s is required", name)
	}
	return value, nil
}
