package handler

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"io"
	"net/http"

	"github.com/gin-gonic/gin"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/payee"
	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-golang-service/pkg/storage"
)

type FeedHandler struct {
	storage               *storage.FeedSQLiteStorage
	allowFailureInjection bool
}

func NewFeedHandler(database *storage.FeedSQLiteStorage, allowFailureInjection bool) *FeedHandler {
	return &FeedHandler{storage: database, allowFailureInjection: allowFailureInjection}
}

func (handler *FeedHandler) Health(context *gin.Context) {
	context.JSON(http.StatusOK, gin.H{"status": "ready", "service": "marketpulse-golang"})
}

func (handler *FeedHandler) Metrics(context *gin.Context) {
	context.Data(http.StatusOK, "text/plain; version=0.0.4", []byte(
		"# HELP marketpulse_up MarketPulse process readiness.\n# TYPE marketpulse_up gauge\nmarketpulse_up 1\n",
	))
}

func (handler *FeedHandler) MCP(context *gin.Context) {
	body, err := io.ReadAll(io.LimitReader(context.Request.Body, 1024*1024+1))
	if err != nil || len(body) > 1024*1024 {
		context.JSON(http.StatusBadRequest, rpcError(nil, -32600, "invalid request"))
		return
	}
	var request struct {
		JSONRPC string         `json:"jsonrpc"`
		ID      any            `json:"id"`
		Method  string         `json:"method"`
		Params  map[string]any `json:"params"`
	}
	if json.Unmarshal(body, &request) != nil || request.JSONRPC != "2.0" {
		context.JSON(http.StatusBadRequest, rpcError(request.ID, -32600, "invalid request"))
		return
	}
	switch request.Method {
	case "initialize":
		version, _ := request.Params["protocolVersion"].(string)
		if version == "" {
			version = "2025-11-25"
		}
		context.JSON(http.StatusOK, rpcResult(request.ID, gin.H{
			"protocolVersion": version, "capabilities": gin.H{"tools": gin.H{"listChanged": false}},
			"serverInfo": gin.H{"name": "marketpulse-feed-golang", "version": "0.1.0"},
		}))
	case "notifications/initialized":
		context.Status(http.StatusAccepted)
	case "tools/list":
		context.JSON(http.StatusOK, rpcResult(request.ID, gin.H{"tools": toolCatalog()}))
	case "tools/call":
		handler.callTool(context, body, request.ID, request.Params)
	default:
		context.JSON(http.StatusNotFound, rpcError(request.ID, -32601, "method not found"))
	}
}

func (handler *FeedHandler) callTool(context *gin.Context, body []byte, id any, params map[string]any) {
	name, _ := params["name"].(string)
	if name == "marketpulse.yesterday_news" {
		news, err := handler.storage.News("yesterday", 20)
		if err != nil {
			context.JSON(http.StatusInternalServerError, rpcError(id, -32603, err.Error()))
			return
		}
		context.JSON(http.StatusOK, toolResult(id, news, false, ""))
		return
	}
	if name != "marketpulse.realtime_news" {
		context.JSON(http.StatusNotFound, rpcError(id, -32602, "unknown tool"))
		return
	}
	runID := context.GetHeader("X-Run-Id")
	if runID == "" {
		context.JSON(http.StatusBadRequest, rpcError(id, -32602, "x-run-id is required"))
		return
	}
	hash := requestHash(body)
	delivery, err := handler.storage.Delivery(runID, hash)
	if err != nil {
		context.JSON(http.StatusConflict, rpcError(id, -32009, err.Error()))
		return
	}
	if delivery.Response != "" {
		context.Data(http.StatusOK, "application/json", []byte(delivery.Response))
		return
	}
	if delivery.IntentID == "" {
		payment, ok := payee.PaymentFromContext(context)
		if !ok {
			context.JSON(http.StatusPaymentRequired, rpcError(id, -32005, "settled payment is required"))
			return
		}
		if err := handler.storage.RecordSettled(runID, hash, payment); err != nil {
			context.JSON(http.StatusConflict, rpcError(id, -32009, err.Error()))
			return
		}
		delivery.IntentID = payment.Intent.ID
	}
	if handler.allowFailureInjection && context.GetHeader("X-E2e-Fail-After-Settlement") == "once" {
		if handler.storage.RecordFailure(runID, hash) == nil {
			context.JSON(http.StatusServiceUnavailable, rpcError(id, -32004, "injected delivery failure after settlement"))
			return
		}
	}
	news, err := handler.storage.News("realtime", 20)
	if err != nil {
		context.JSON(http.StatusInternalServerError, rpcError(id, -32603, err.Error()))
		return
	}
	encoded, _ := json.Marshal(toolResult(id, news, true, delivery.IntentID))
	if err := handler.storage.RecordDelivered(runID, hash, string(encoded)); err != nil {
		context.JSON(http.StatusInternalServerError, rpcError(id, -32603, err.Error()))
		return
	}
	context.Data(http.StatusOK, "application/json", encoded)
}

func toolCatalog() []gin.H {
	return []gin.H{
		{"name": "marketpulse.yesterday_news", "description": "Free previous-day geopolitical and tariff intelligence", "inputSchema": gin.H{"type": "object", "additionalProperties": false}},
		{"name": "marketpulse.realtime_news", "description": "Paid real-time geopolitical and tariff intelligence via x402", "inputSchema": gin.H{"type": "object", "additionalProperties": false}},
	}
}

func toolResult(id any, news []storage.NewsItem, paid bool, intentID string) gin.H {
	structured := gin.H{"news": news, "paid": paid, "intentId": nil}
	if intentID != "" {
		structured["intentId"] = intentID
	}
	text, _ := json.Marshal(structured)
	return rpcResult(id, gin.H{"content": []gin.H{{"type": "text", "text": string(text)}}, "structuredContent": structured, "isError": false})
}

func rpcResult(id any, result any) gin.H {
	return gin.H{"jsonrpc": "2.0", "id": id, "result": result}
}

func rpcError(id any, code int, message string) gin.H {
	return gin.H{"jsonrpc": "2.0", "id": id, "error": gin.H{"code": code, "message": message}}
}

func requestHash(body []byte) string {
	hash := sha256.New()
	hash.Write([]byte("marketpulse.mcp.request.v1\x00"))
	hash.Write(body)
	return hex.EncodeToString(hash.Sum(nil))
}
