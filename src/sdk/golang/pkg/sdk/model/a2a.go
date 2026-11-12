package model

import (
	"bytes"
	"encoding/json"
	"errors"
	"io"

	"github.com/a2aproject/a2a-go/a2a"
)

// A2ACall retains the original JSON-RPC carrier. Message/task projections use
// the official SDK types; no second copy of their wire fields is maintained.
type A2ACall struct{ Envelope map[string]any }

func (A2ACall) Parse(body []byte, limit int64) (*A2ACall, error) {
	if int64(len(body)) > limit {
		return nil, errors.New("A2A request exceeds configured limit")
	}
	decoder := json.NewDecoder(bytes.NewReader(body))
	decoder.UseNumber()
	var envelope map[string]any
	if err := decoder.Decode(&envelope); err != nil {
		return nil, nil
	}
	var extra any
	if err := decoder.Decode(&extra); !errors.Is(err, io.EOF) {
		return nil, errors.New("JSON-RPC request contains trailing data")
	}
	if envelope["jsonrpc"] != "2.0" {
		return nil, nil
	}
	switch envelope["method"] {
	case "message/stream", "SendStreamingMessage", "SendMessage":
		return nil, errors.New("paid A2A supports only non-streaming v0.3 message/send")
	case "message/send":
	default:
		return nil, nil
	}
	switch envelope["id"].(type) {
	case string, json.Number:
	default:
		return nil, errors.New("paid A2A requires a JSON-RPC request id")
	}
	call := &A2ACall{Envelope: envelope}
	if _, err := call.Params(); err != nil {
		return nil, err
	}
	return call, nil
}

func (call *A2ACall) Params() (*a2a.MessageSendParams, error) {
	params, ok := call.Envelope["params"].(map[string]any)
	if !ok {
		return nil, errors.New("A2A params must be an object")
	}
	message, ok := params["message"].(map[string]any)
	if !ok || message["kind"] != "message" {
		return nil, errors.New("A2A params.message must be a Message")
	}
	raw, err := json.Marshal(params)
	if err != nil {
		return nil, err
	}
	var result a2a.MessageSendParams
	if err := json.Unmarshal(raw, &result); err != nil || result.Message == nil ||
		result.Message.ID == "" || result.Message.Role != a2a.MessageRoleUser || len(result.Message.Parts) == 0 {
		return nil, errors.New("invalid A2A MessageSendParams")
	}
	return &result, nil
}

func (call *A2ACall) BindTask(task *a2a.Task) error {
	params, err := call.Params()
	if err != nil {
		return err
	}
	if task == nil || task.ID == "" || task.ContextID == "" ||
		(params.Message.TaskID != "" && params.Message.TaskID != task.ID) ||
		(params.Message.ContextID != "" && params.Message.ContextID != task.ContextID) {
		return errors.New("A2A task/context does not match the request")
	}
	message := call.Envelope["params"].(map[string]any)["message"].(map[string]any)
	message["taskId"], message["contextId"] = string(task.ID), task.ContextID
	return nil
}

func (call *A2ACall) Bytes() ([]byte, error) { return json.Marshal(call.Envelope) }
