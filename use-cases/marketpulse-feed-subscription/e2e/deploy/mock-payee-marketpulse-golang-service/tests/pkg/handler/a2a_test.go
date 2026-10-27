package handler_test

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"github.com/a2aproject/a2a-go/a2a"
	"github.com/gin-gonic/gin"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-golang-service/pkg/handler"
	"github.com/wl4g-blockchain/agentvault/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-golang-service/pkg/storage"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"testing"
)

func TestA2AFeedReusesDurableBusinessDelivery(t *testing.T) {
	repository, err := storage.NewFeedSqliteRepository(filepath.Join(t.TempDir(), "a2a.sqlite"))
	if err != nil {
		t.Fatal(err)
	}
	defer repository.Close()
	const runID = "run-a2a-1"
	canonical, _ := json.Marshal(gin.H{"jsonrpc": "2.0", "id": runID, "method": "tools/call", "params": gin.H{"name": "marketpulse.realtime_news", "arguments": gin.H{}}})
	digest := sha256.Sum256(append([]byte("marketpulse.mcp.request.v1\x00"), canonical...))
	hash := hex.EncodeToString(digest[:])
	payment := &model.SettledIncomingPayment{Intent: model.PaymentIntent{ID: "intent-1", RunID: runID}}
	if err = repository.RecordSettled(runID, hash, payment); err != nil {
		t.Fatal(err)
	}
	if err = repository.RecordDelivered(runID, hash, `{"jsonrpc":"2.0","id":"run-a2a-1","result":{"structuredContent":{"news":[],"paid":true,"intentId":"intent-1"}}}`); err != nil {
		t.Fatal(err)
	}
	engine := gin.New()
	engine.POST("/a2a", (&handler.A2AHandler{Feed: handler.NewFeedHandler(repository, false)}).Send)
	body := []byte(`{"jsonrpc":"2.0","id":17,"method":"message/send","params":{"message":{"kind":"message","messageId":"message-1","taskId":"task-1","contextId":"context-1","role":"user","parts":[{"kind":"text","text":"news"}]}}}`)
	request := httptest.NewRequest(http.MethodPost, "/a2a", bytes.NewReader(body))
	request.Header.Set("X-Run-Id", runID)
	response := httptest.NewRecorder()
	engine.ServeHTTP(response, request)
	var result struct {
		ID     int       `json:"id"`
		Result *a2a.Task `json:"result"`
	}
	if response.Code != 200 || json.Unmarshal(response.Body.Bytes(), &result) != nil || result.ID != 17 || result.Result == nil || result.Result.Status.State != a2a.TaskStateCompleted {
		t.Fatalf("invalid task: %d %s", response.Code, response.Body.String())
	}
	data, ok := result.Result.Artifacts[0].Parts[0].(a2a.DataPart)
	if !ok || data.Data["intentId"] != "intent-1" || data.Data["paid"] != true {
		t.Fatalf("business data lost: %+v", data)
	}
	request = httptest.NewRequest(http.MethodPost, "/a2a", bytes.NewReader([]byte(`{"jsonrpc":"2.0","id":18,"method":"message/send","params":{"message":{"kind":"message","messageId":"message-2","role":"user","parts":[{"kind":"text","text":"news"}]}}}`)))
	response = httptest.NewRecorder()
	engine.ServeHTTP(response, request)
	if response.Code != 400 {
		t.Fatalf("unbound task accepted: %d", response.Code)
	}
}
