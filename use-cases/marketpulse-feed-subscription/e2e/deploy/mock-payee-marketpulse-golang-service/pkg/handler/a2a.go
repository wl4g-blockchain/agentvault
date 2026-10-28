package handler

import (
	"bytes"
	"encoding/json"
	"github.com/a2aproject/a2a-go/a2a"
	"github.com/gin-gonic/gin"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	"io"
	"net/http"
)

// A2AHandler maps official message/task carriers onto the same feed delivery.
type A2AHandler struct{ Feed *FeedHandler }

func (handler *A2AHandler) Send(context *gin.Context) {
	body, err := io.ReadAll(io.LimitReader(context.Request.Body, 1024*1024+1))
	call, parseErr := (model.A2ACall{}).Parse(body, 1024*1024)
	if err != nil || parseErr != nil || call == nil {
		context.JSON(http.StatusBadRequest, rpcError(nil, -32600, "invalid A2A message/send"))
		return
	}
	params, err := call.Params()
	if err != nil || params.Message.TaskID == "" || params.Message.ContextID == "" {
		context.JSON(http.StatusBadRequest, rpcError(call.Envelope["id"], -32602, "bound A2A task is required"))
		return
	}
	// Metadata and server Task IDs must not alter the business run hash.
	runID := context.GetHeader("X-Run-Id")
	business, _ := json.Marshal(gin.H{"jsonrpc": "2.0", "id": runID, "method": "tools/call", "params": gin.H{"name": "marketpulse.realtime_news", "arguments": gin.H{}}})
	capture := &a2aResultWriter{ResponseWriter: context.Writer, header: make(http.Header), status: 200}
	original := context.Writer
	context.Writer = capture
	handler.Feed.callTool(context, business, runID, map[string]any{"name": "marketpulse.realtime_news"})
	context.Writer = original
	var result struct {
		Result struct {
			StructuredContent map[string]any `json:"structuredContent"`
		} `json:"result"`
		Error any `json:"error"`
	}
	if json.Unmarshal(capture.body.Bytes(), &result) != nil || capture.status >= 400 || result.Error != nil {
		context.JSON(capture.status, gin.H{"jsonrpc": "2.0", "id": call.Envelope["id"], "error": result.Error})
		return
	}
	task := &a2a.Task{ID: params.Message.TaskID, ContextID: params.Message.ContextID, Status: a2a.TaskStatus{State: a2a.TaskStateCompleted}, Artifacts: []*a2a.Artifact{{ID: a2a.ArtifactID(runID), Parts: a2a.ContentParts{a2a.DataPart{Data: result.Result.StructuredContent}}}}}
	context.JSON(http.StatusOK, rpcResult(call.Envelope["id"], task))
}

type a2aResultWriter struct {
	gin.ResponseWriter
	header http.Header
	body   bytes.Buffer
	status int
}

func (writer *a2aResultWriter) Header() http.Header             { return writer.header }
func (writer *a2aResultWriter) WriteHeader(status int)          { writer.status = status }
func (writer *a2aResultWriter) WriteHeaderNow()                 {}
func (writer *a2aResultWriter) Write(value []byte) (int, error) { return writer.body.Write(value) }
func (writer *a2aResultWriter) WriteString(value string) (int, error) {
	return writer.body.WriteString(value)
}
