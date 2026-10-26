package marketpulse

import (
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/gin-gonic/gin"
	agentvault "github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/payee"
	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-golang-service/pkg/handler"
	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-golang-service/pkg/route"
	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-golang-service/pkg/storage"
)

// Server owns the Gin listener, MCP endpoint, SDK middleware and component lifecycle.
type Server struct {
	bind    string
	engine  *gin.Engine
	storage *storage.FeedSqliteRepository
	vault   *agentvault.AgentVault
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
	core, err := vault.Payee(agentvault.PayeeConfig{})
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
	return payee.NewMCPToolRoute("/mcp", "marketpulse.realtime_news", scenarioID, priceID, resourceURL, origin), nil
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

func payeeVaultFromEnv() (*agentvault.AgentVault, error) {
	var settings agentvault.VaultTransportConfig
	switch strings.ToLower(envDefault("AGENTVAULT_TRANSPORT", "mqtt")) {
	case "mqtt":
		timeout, err := positiveSeconds("AGENTVAULT_MQTT_TIMEOUT_SECONDS")
		if err != nil {
			return nil, err
		}
		port, parseErr := strconv.ParseUint(envDefault("AGENTVAULT_MQTT_PORT", "1883"), 10, 16)
		if parseErr != nil || port == 0 {
			return nil, fmt.Errorf("AGENTVAULT_MQTT_PORT must be valid")
		}
		host, err := required("AGENTVAULT_MQTT_HOST")
		if err != nil {
			return nil, err
		}
		username, err := required("AGENTVAULT_MQTT_USERNAME")
		if err != nil {
			return nil, err
		}
		password, err := required("AGENTVAULT_MQTT_PASSWORD")
		if err != nil {
			return nil, err
		}
		clientID, err := required("AGENTVAULT_MQTT_CLIENT_ID")
		if err != nil {
			return nil, err
		}
		settings = agentvault.VaultTransportConfig{Provider: agentvault.TransportProviderMqtt, Mqtt: &agentvault.MqttConfig{
			Host: host, Port: uint16(port),
			TLS:      envDefault("AGENTVAULT_MQTT_TLS", "false") == "true",
			Username: username, Password: password,
			ConnectionPrefix: "marketpulse-go", VaultClientID: clientID,
			RequestTimeout: timeout,
		}}
	case "grpc":
		timeout, err := positiveSeconds("AGENTVAULT_GRPC_TIMEOUT_SECONDS")
		if err != nil {
			return nil, err
		}
		endpoint, err := required("AGENTVAULT_GRPC_ENDPOINT")
		if err != nil {
			return nil, err
		}
		settings = agentvault.VaultTransportConfig{Provider: agentvault.TransportProviderGrpc, Grpc: &agentvault.GrpcConfig{
			Endpoint:       endpoint,
			TLS:            envDefault("AGENTVAULT_GRPC_TLS", "false") == "true",
			RequestTimeout: timeout,
		}}
	default:
		return nil, fmt.Errorf("AGENTVAULT_TRANSPORT must be mqtt or grpc")
	}
	return agentvault.NewFromConfig(settings)
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
