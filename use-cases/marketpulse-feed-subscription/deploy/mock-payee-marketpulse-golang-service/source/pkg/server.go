package marketpulse

import (
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/gin-gonic/gin"
	sdk "github.com/wl4g-blockchain/agentvault/src/sdk/golang/pkg/sdk"
	"github.com/wl4g-blockchain/agentvault/src/sdk/golang/pkg/sdk/payee"
	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/deploy/mock-payee-marketpulse-golang-service/source/pkg/handler"
	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/deploy/mock-payee-marketpulse-golang-service/source/pkg/route"
	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/deploy/mock-payee-marketpulse-golang-service/source/pkg/storage"
)

// Server owns the Gin listener, MCP endpoint, SDK middleware and component lifecycle.
type Server struct {
	bind    string
	engine  *gin.Engine
	storage *storage.FeedSqliteRepository
	vault   *sdk.AgentVault
}

// NewServerFromEnv builds the complete MarketPulse MCP server from runtime configuration.
func NewServerFromEnv() (*Server, error) {
	vault, err := payeeVaultFromEnv()
	if err != nil {
		return nil, err
	}
	completed := false
	var database *storage.FeedSqliteRepository
	defer func() {
		if !completed {
			if database != nil {
				_ = database.Close()
			}
			_ = vault.Close()
		}
	}()
	protocolName := envDefault("MARKETPULSE_PAYMENT_PROTOCOL", "x402")
	core, err := vault.Payee(sdk.PayeeConfig{Protocols: []string{protocolName}})
	if err != nil {
		return nil, err
	}
	databasePath, err := required("MARKETPULSE_DATABASE")
	if err != nil {
		return nil, err
	}
	database, err = storage.NewFeedSqliteRepository(databasePath)
	if err != nil {
		return nil, err
	}
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
	a2aRoute := feedRoute
	a2aRoute.Path, a2aRoute.MCPTool = "/a2a", ""
	a2aRoute.PriceID = envDefault("MARKETPULSE_A2A_PRICE_ID", feedRoute.PriceID)
	a2aRoute.Resource = envDefault("MARKETPULSE_A2A_RESOURCE_URL", strings.Replace(feedRoute.Resource, "/mcp", "/a2a", 1))
	a2aRoute.A2ATaskStore = database
	engine.POST("/a2a", core.Gin(a2aRoute).Handler(), (&handler.A2AHandler{Feed: feedHandler}).Send)
	bind, err := required("MARKETPULSE_BIND")
	if err != nil {
		return nil, err
	}
	completed = true
	return &Server{bind: bind, engine: engine, storage: database, vault: vault}, nil
}

func mcpRoute() (payee.GinPayeeRoute, error) {
	scenarioID, err := required("MARKETPULSE_SCENARIO_ID")
	if err != nil {
		return payee.GinPayeeRoute{}, err
	}
	priceID, err := required("MARKETPULSE_PRICE_ID")
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
	return payee.NewMCPToolRouteWithProtocol("/mcp", "marketpulse.realtime_news", scenarioID, priceID, resourceURL, origin, envDefault("MARKETPULSE_PAYMENT_PROTOCOL", "x402")), nil
}

// Run starts the HTTP listener. The /mcp endpoint is the only payment-protected route.
func (server *Server) Run() error { return server.engine.Run(server.bind) }

// Close releases the durable feed store.
func (server *Server) Close() error {
	transportError := server.vault.Close()
	storageError := server.storage.Close()
	if transportError != nil {
		return transportError
	}
	return storageError
}

func payeeVaultFromEnv() (*sdk.AgentVault, error) {
	baseURL, err := required("AGENTVAULT_HTTP_BASE_URL")
	if err != nil {
		return nil, err
	}
	timeout, err := positiveSeconds("AGENTVAULT_HTTP_TIMEOUT_SECONDS")
	if err != nil {
		return nil, err
	}
	return sdk.NewFromConfig(sdk.VaultTransportConfig{Provider: sdk.TransportProviderHttp, Http: &sdk.HttpConfig{BaseURL: baseURL, AuthguardBearer: os.Getenv("AGENTVAULT_AUTHGUARD_BEARER"), RequestTimeout: timeout}})
}

func envDefault(name, fallback string) string {
	if value := strings.TrimSpace(os.Getenv(name)); value != "" {
		return value
	}
	return fallback
}

func positiveSeconds(name string) (time.Duration, error) {
	seconds, err := strconv.Atoi(envDefault(name, "15"))
	if err != nil || seconds <= 0 {
		return 0, fmt.Errorf("%s must be positive", name)
	}
	return time.Duration(seconds) * time.Second, nil
}

func required(name string) (string, error) {
	value := strings.TrimSpace(os.Getenv(name))
	if value == "" {
		return "", fmt.Errorf("%s is required", name)
	}
	return value, nil
}
