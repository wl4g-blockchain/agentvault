package agent

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
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
	client         *client.Client
	payments       *paymentRoundTripper
	tools          map[string]struct{}
	closeTransport func() error
}

func NewMCPHelperFromEnv(ctx context.Context) (*MCPHelper, error) {
	values := &environment{}
	mqttTimeout := values.positiveDuration("AGENTVAULT_MQTT_TIMEOUT_SECONDS")
	approvalTimeout := values.positiveDuration("AGENTVAULT_APPROVAL_TIMEOUT_SECONDS")
	scenarioID := values.required("QUANTSTREAM_SCENARIO_ID")
	marketPulseURL := values.required("MARKETPULSE_URL")
	if values.err != nil {
		return nil, values.err
	}
	transportName := os.Getenv("AGENTVAULT_TRANSPORT")
	if transportName == "" {
		transportName = "mqtt"
	}
	var wire agentvault.Transport
	var closeTransport func() error
	var err error
	switch transportName {
	case "mqtt":
		wire, err = agentvault.NewMqttVaultTransport(agentvault.MqttConfig{
			Host:             values.required("AGENTVAULT_MQTT_HOST"),
			Port:             uint16(values.positiveUint("AGENTVAULT_MQTT_PORT", 16)),
			TLS:              values.boolean("AGENTVAULT_MQTT_TLS"),
			Username:         values.required("AGENTVAULT_MQTT_USERNAME"),
			Password:         values.required("AGENTVAULT_MQTT_PASSWORD"),
			ConnectionPrefix: "quantstream-go", VaultClientID: values.required("QUANTSTREAM_CLIENT_ID"),
			RequestTimeout: mqttTimeout,
		})
	case "grpc":
		var grpc *agentvault.GrpcVaultTransport
		grpc, err = agentvault.NewGrpcVaultTransport(agentvault.GrpcConfig{
			Endpoint: values.required("AGENTVAULT_GRPC_ENDPOINT"),
			TLS:      values.boolean("AGENTVAULT_GRPC_TLS"), RequestTimeout: mqttTimeout,
		})
		wire = grpc
		if grpc != nil {
			closeTransport = grpc.Close
		}
	default:
		return nil, errors.New("AGENTVAULT_TRANSPORT must be mqtt or grpc")
	}
	if values.err != nil {
		return nil, values.err
	}
	if err != nil {
		return nil, err
	}
	slog.Info("selected AgentVault Payer transport",
		"event.name", "agentvault.sdk.payer.transport_selected", "agentvault.transport", transportName)
	core, err := agentvault.New(wire).Payer(agentvault.PayerConfig{
		ScenarioID: scenarioID, ApprovalTimeout: approvalTimeout,
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
	return &MCPHelper{client: mcpClient, payments: capture, tools: tools, closeTransport: closeTransport}, nil
}

func (helper *MCPHelper) Close() error {
	clientErr := helper.client.Close()
	if helper.closeTransport != nil {
		if err := helper.closeTransport(); clientErr == nil {
			clientErr = err
		}
	}
	return clientErr
}

func (helper *MCPHelper) YesterdayNews(ctx context.Context, runID string) (ToolResult, error) {
	return helper.call(ctx, yesterdayTool, runID, false)
}

func (helper *MCPHelper) RealtimeNews(ctx context.Context, runID string, failAfterSettlement bool) (ToolResult, error) {
	return helper.call(ctx, realtimeTool, runID, failAfterSettlement)
}

func (helper *MCPHelper) call(
	ctx context.Context,
	tool string,
	runID string,
	failAfterSettlement bool,
) (ToolResult, error) {
	if _, ok := helper.tools[tool]; !ok {
		return ToolResult{}, fmt.Errorf("MarketPulse MCP tool %q is not registered", tool)
	}
	headers := make(http.Header)
	headers.Set("x-run-id", runID)
	if failAfterSettlement {
		headers.Set("x-e2e-fail-after-settlement", "once")
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
	intentID := helper.payments.take(runID)
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
	runID := request.Header.Get("x-run-id")
	if runID != "" {
		operation, err := agentvault.NewPayerRequestContext(runID)
		if err != nil {
			return nil, errors.New("invalid paid MCP operation context")
		}
		request = agentvault.WithPayment(request, operation)
	}
	response, err := transport.payer.RoundTrip(request)
	if err != nil {
		return nil, err
	}
	if payment, ok := agentvault.PaymentFromResponse(response); ok && runID != "" {
		transport.payments.Store(runID, payment.Intent.ID)
	}
	return response, nil
}

func (transport *paymentRoundTripper) take(runID string) string {
	value, ok := transport.payments.LoadAndDelete(runID)
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
