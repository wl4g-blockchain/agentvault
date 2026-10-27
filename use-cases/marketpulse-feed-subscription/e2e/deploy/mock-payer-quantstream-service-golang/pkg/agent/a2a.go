package agent

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"

	"github.com/a2aproject/a2a-go/a2a"
)

// callA2A uses the existing SDK HTTP payment state machine and Vault transport.
func (helper *MCPHelper) callA2A(ctx context.Context, runID string, injectFailure bool) (ToolResult, error) {
	message := a2a.NewMessage(a2a.MessageRoleUser, a2a.TextPart{Text: "marketpulse.realtime_news"})
	body, err := json.Marshal(map[string]any{"jsonrpc": "2.0", "id": runID, "method": "message/send", "params": a2a.MessageSendParams{Message: message}})
	if err != nil {
		return ToolResult{}, err
	}
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, helper.a2aURL, bytes.NewReader(body))
	if err != nil {
		return ToolResult{}, err
	}
	request.Header.Set("Content-Type", "application/json")
	request.Header.Set("X-Run-Id", runID)
	if injectFailure {
		request.Header.Set("X-E2e-Fail-After-Settlement", "once")
	}
	response, err := helper.httpClient.Do(request)
	if err != nil {
		return ToolResult{}, err
	}
	defer response.Body.Close()
	raw, err := io.ReadAll(io.LimitReader(response.Body, 1024*1024+1))
	if err != nil || len(raw) > 1024*1024 {
		return ToolResult{}, errors.New("invalid A2A response body")
	}
	var result struct {
		Result *a2a.Task `json:"result"`
	}
	if response.StatusCode != 200 || json.Unmarshal(raw, &result) != nil || result.Result == nil || result.Result.Status.State != a2a.TaskStateCompleted || len(result.Result.Artifacts) != 1 || len(result.Result.Artifacts[0].Parts) != 1 {
		return ToolResult{}, errors.New("paid A2A response omitted a completed Task artifact")
	}
	data, ok := result.Result.Artifacts[0].Parts[0].(a2a.DataPart)
	if !ok {
		return ToolResult{}, errors.New("A2A feed artifact must be a DataPart")
	}
	business, err := json.Marshal(data.Data)
	if err != nil {
		return ToolResult{}, err
	}
	intentID := helper.payments.take(runID)
	if intentID == "" {
		return ToolResult{}, errors.New("paid A2A result omitted AgentVault payment metadata")
	}
	return ToolResult{Body: business, PaymentIntentID: intentID}, nil
}
