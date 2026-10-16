package agent

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"strconv"
	"sync"
	"time"

	"github.com/mark3labs/mcp-go/client"
	mcptransport "github.com/mark3labs/mcp-go/client/transport"
	"github.com/mark3labs/mcp-go/mcp"
	agentvault "github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg"
)

const (
	yesterdayTool = "marketpulse.yesterday_news"
	realtimeTool  = "marketpulse.realtime_news"
)

type ToolResult struct {
	Body            []byte
	PaymentIntentID string
}

// MCPHelper owns one real mark3labs MCP client and injects the two remote
// MarketPulse tools discovered during initialization.
type MCPHelper struct {
	client   *client.Client
	payments *paymentRoundTripper
	tools    map[string]struct{}
}

func NewMCPHelperFromEnv(ctx context.Context) (*MCPHelper, error) {
	values := &environment{}
	mqttPort := values.positiveUint("AGENTVAULT_MQTT_PORT", 16)
	mqttTLS := values.boolean("AGENTVAULT_MQTT_TLS")
	decimals := values.positiveOrZeroUint("PAYMENT_DECIMALS", 8)
	mqttTimeout := values.positiveDuration("AGENTVAULT_MQTT_TIMEOUT_SECONDS")
	approvalTimeout := values.positiveDuration("AGENTVAULT_APPROVAL_TIMEOUT_SECONDS")
	mqttHost := values.required("AGENTVAULT_MQTT_HOST")
	mqttUsername := values.required("AGENTVAULT_MQTT_USERNAME")
	mqttPassword := values.required("AGENTVAULT_MQTT_PASSWORD")
	clientID := values.required("QUANTSTREAM_CLIENT_ID")
	scenarioID := values.required("QUANTSTREAM_SCENARIO_ID")
	walletID := values.required("QUANTSTREAM_WALLET_ID")
	network := values.required("PAYMENT_NETWORK")
	asset := values.required("PAYMENT_ASSET")
	tokenContract := values.required("PAYMENT_TOKEN_CONTRACT")
	amount := values.required("PAYMENT_AMOUNT")
	recipient := values.required("PAYMENT_RECIPIENT")
	marketPulseURL := values.required("MARKETPULSE_URL")
	if values.err != nil {
		return nil, values.err
	}
	wire, err := agentvault.NewMqttVaultTransport(agentvault.MqttConfig{
		Host: mqttHost, Port: uint16(mqttPort), TLS: mqttTLS,
		Username: mqttUsername, Password: mqttPassword,
		ConnectionPrefix: "quantstream-go", VaultClientID: clientID,
		RequestTimeout: mqttTimeout,
	})
	if err != nil {
		return nil, err
	}
	core, err := agentvault.New(wire).Payer(agentvault.PayerConfig{
		ScenarioID: scenarioID, WalletID: walletID, Protocol: "x402",
		Asset: agentvault.AssetIdentity{Network: network, Asset: asset,
			TokenContract: tokenContract, Decimals: uint8(decimals)},
		Amount: amount, Recipient: recipient, ApprovalTimeout: approvalTimeout,
		ApprovalPollInterval: time.Second, DeliveryRetries: 1,
	})
	if err != nil {
		return nil, err
	}
	payer := core.Standard(nil)
	capture := &paymentRoundTripper{payer: payer}
	httpClient := &http.Client{Transport: capture, Timeout: 30 * time.Second,
		CheckRedirect: func(_ *http.Request, _ []*http.Request) error { return http.ErrUseLastResponse }}
	mcpClient, err := client.NewStreamableHttpClient(
		marketPulseURL,
		mcptransport.WithHTTPBasicClient(httpClient),
	)
	if err != nil {
		return nil, fmt.Errorf("create mark3labs MCP client: %w", err)
	}
	if err = mcpClient.Start(ctx); err != nil {
		return nil, fmt.Errorf("start mark3labs MCP client: %w", err)
	}
	initialized := false
	defer func() {
		if !initialized {
			_ = mcpClient.Close()
		}
	}()
	if _, err = mcpClient.Initialize(ctx, mcp.InitializeRequest{Params: mcp.InitializeParams{
		ProtocolVersion: mcp.LATEST_PROTOCOL_VERSION,
		Capabilities:    mcp.ClientCapabilities{},
		ClientInfo:      mcp.Implementation{Name: "quantstream-golang", Version: "0.1.0"},
	}}); err != nil {
		return nil, fmt.Errorf("initialize MarketPulse MCP client: %w", err)
	}
	listed, err := mcpClient.ListTools(ctx, mcp.ListToolsRequest{})
	if err != nil {
		return nil, fmt.Errorf("discover MarketPulse MCP tools: %w", err)
	}
	tools := make(map[string]struct{}, len(listed.Tools))
	for _, tool := range listed.Tools {
		tools[tool.Name] = struct{}{}
	}
	for _, required := range []string{yesterdayTool, realtimeTool} {
		if _, ok := tools[required]; !ok {
			return nil, fmt.Errorf("MarketPulse MCP server did not expose %s", required)
		}
	}
	initialized = true
	return &MCPHelper{client: mcpClient, payments: capture, tools: tools}, nil
}

func (helper *MCPHelper) Close() error { return helper.client.Close() }

func (helper *MCPHelper) YesterdayNews(ctx context.Context, runID string) (ToolResult, error) {
	return helper.call(ctx, yesterdayTool, runID, runID+".yesterday", false)
}

func (helper *MCPHelper) RealtimeNews(ctx context.Context, runID string, failAfterSettlement bool) (ToolResult, error) {
	return helper.call(ctx, realtimeTool, runID, runID+".realtime", failAfterSettlement)
}

func (helper *MCPHelper) call(
	ctx context.Context,
	tool string,
	runID string,
	executionKey string,
	failAfterSettlement bool,
) (ToolResult, error) {
	if _, ok := helper.tools[tool]; !ok {
		return ToolResult{}, fmt.Errorf("MarketPulse MCP tool %q is not registered", tool)
	}
	headers := make(http.Header)
	headers.Set("x-run-id", runID)
	headers.Set("mcp-execution-key", executionKey)
	if failAfterSettlement {
		headers.Set("x-e2e-fail-after-settlement", "once")
	}
	if tool == realtimeTool {
		operation, err := agentvault.NewPayerRequestContext(runID, "marketpulse-realtime")
		if err != nil {
			return ToolResult{}, err
		}
		headers.Set("x-agentvault-idempotency-key", string(operation.IdempotencyKey))
	}
	result, err := helper.client.CallTool(ctx, mcp.CallToolRequest{
		Header: headers,
		Params: mcp.CallToolParams{Name: tool, Arguments: map[string]any{}},
	})
	if err != nil {
		return ToolResult{}, fmt.Errorf("call MarketPulse MCP tool %s: %w", tool, err)
	}
	if result.IsError {
		return ToolResult{}, fmt.Errorf("MarketPulse MCP tool %s returned an application error", tool)
	}
	body, err := json.Marshal(result.StructuredContent)
	if err != nil {
		return ToolResult{}, fmt.Errorf("encode MarketPulse MCP result: %w", err)
	}
	intentID := helper.payments.take(executionKey)
	if tool == realtimeTool && intentID == "" {
		return ToolResult{}, errors.New("paid MCP result omitted AgentVault payment metadata")
	}
	return ToolResult{Body: body, PaymentIntentID: intentID}, nil
}

type paymentRoundTripper struct {
	payer    http.RoundTripper
	payments sync.Map
}

func (transport *paymentRoundTripper) RoundTrip(request *http.Request) (*http.Response, error) {
	executionKey := request.Header.Get("mcp-execution-key")
	idempotencyKey := request.Header.Get("x-agentvault-idempotency-key")
	if idempotencyKey != "" {
		operation, err := agentvault.NewPayerRequestContext(request.Header.Get("x-run-id"), "marketpulse-realtime")
		if err != nil || string(operation.IdempotencyKey) != idempotencyKey {
			return nil, errors.New("invalid paid MCP operation context")
		}
		request = agentvault.WithPayment(request, operation)
	}
	response, err := transport.payer.RoundTrip(request)
	if err != nil {
		return nil, err
	}
	if payment, ok := agentvault.PaymentFromResponse(response); ok && executionKey != "" {
		transport.payments.Store(executionKey, payment.Intent.ID)
	}
	return response, nil
}

func (transport *paymentRoundTripper) take(executionKey string) string {
	value, ok := transport.payments.LoadAndDelete(executionKey)
	if !ok {
		return ""
	}
	intentID, _ := value.(string)
	return intentID
}

type environment struct{ err error }

func (values *environment) required(name string) string {
	if values.err != nil {
		return ""
	}
	value := os.Getenv(name)
	if value == "" {
		values.err = fmt.Errorf("%s is required", name)
	}
	return value
}

func (values *environment) positiveUint(name string, bits int) uint64 {
	value, err := strconv.ParseUint(values.required(name), 10, bits)
	if values.err == nil && (err != nil || value == 0) {
		values.err = fmt.Errorf("%s must be a positive integer", name)
	}
	return value
}

func (values *environment) positiveOrZeroUint(name string, bits int) uint64 {
	value, err := strconv.ParseUint(values.required(name), 10, bits)
	if values.err == nil && err != nil {
		values.err = fmt.Errorf("%s must be an unsigned integer", name)
	}
	return value
}

func (values *environment) positiveDuration(name string) time.Duration {
	return time.Duration(values.positiveUint(name, 64)) * time.Second
}

func (values *environment) boolean(name string) bool {
	value, err := strconv.ParseBool(values.required(name))
	if values.err == nil && err != nil {
		values.err = fmt.Errorf("%s must be true or false", name)
	}
	return value
}
