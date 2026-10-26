package payee

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"github.com/a2aproject/a2a-go/a2a"
	"github.com/a2aproject/a2a-go/a2asrv"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	"net/http"
	"reflect"
	"strings"
)

type a2aPaymentCall struct {
	context  context.Context
	store    a2asrv.TaskStore
	task     *a2a.Task
	version  a2a.TaskVersion
	protocol string
}

// A2AAgentCard declares only implemented extensions on the application's
// official AgentCard. The caller publishes this same card through its A2A server.
func (middleware *PayeeMiddleware) A2AAgentCard(card *a2a.AgentCard) error {
	if card == nil {
		return errors.New("A2A AgentCard is required")
	}
	for _, name := range middleware.config.Protocols {
		adapter, err := middleware.protocols.A2A(name)
		if err != nil || adapter.A2AExtension() == "" {
			continue
		}
		found := false
		for _, extension := range card.Capabilities.Extensions {
			found = found || extension.URI == adapter.A2AExtension()
		}
		if !found {
			card.Capabilities.Extensions = append(card.Capabilities.Extensions, a2a.AgentExtension{URI: adapter.A2AExtension(), Required: true})
		}
	}
	return nil
}

func (middleware *PayeeMiddleware) prepareA2ACall(ctx context.Context, body []byte, runID string, operation model.PayeeOperation, store a2asrv.TaskStore, headers http.Header) (*mcpCall, error) {
	call, err := (model.A2ACall{}).Parse(body, middleware.config.MaxBodyBytes)
	if err != nil || call == nil {
		return nil, err
	}
	if err := model.ValidateIdentifier("run_id", runID); err != nil {
		return nil, err
	}
	adapter, err := middleware.protocols.A2A(operation.Protocol)
	if err != nil {
		return nil, err
	}
	if extension := adapter.A2AExtension(); extension != "" {
		active := false
		for _, value := range strings.Split(headers.Get("X-A2A-Extensions"), ",") {
			active = active || strings.TrimSpace(value) == extension
		}
		if !active {
			return nil, errors.New("required A2A payment extension is not activated")
		}
	}
	if present, err := middleware.hasMCPHTTPPaymentCredential(headers); err != nil {
		return nil, err
	} else if present {
		return nil, errors.New("A2A payment credentials must use protocol message metadata")
	}
	selected, credential, err := middleware.protocols.PresentedA2ACredential(call.Envelope, middleware.config.MaxBodyBytes)
	if err != nil {
		return nil, err
	}
	if selected != "" && selected != operation.Protocol {
		return nil, errors.New("A2A credential protocol mismatch")
	}
	if err := middleware.protocols.StripA2ACredentials(call.Envelope); err != nil {
		return nil, err
	}
	params, err := call.Params()
	if err != nil {
		return nil, err
	}
	taskID := params.Message.TaskID
	if taskID == "" {
		taskID = a2a.TaskID(runID)
	}
	task, version, err := store.Get(ctx, taskID)
	if errors.Is(err, a2a.ErrTaskNotFound) {
		if credential != nil || params.Message.TaskID != "" {
			return nil, errors.New("A2A task must exist before credential delivery or continuation")
		}
		params.Message.TaskID = taskID
		task = a2a.NewSubmittedTask(params.Message, params.Message)
		version, err = store.Save(ctx, task, task, nil, 0)
	}
	if err != nil {
		return nil, err
	}
	if task == nil {
		return nil, errors.New("A2A task store returned no task")
	}
	encoded, err := json.Marshal(task)
	if err != nil {
		return nil, err
	}
	var bound a2a.Task
	if err := json.Unmarshal(encoded, &bound); err != nil {
		return nil, err
	}
	if bound.Metadata == nil {
		bound.Metadata = map[string]any{}
	}
	scope := map[string]string{"agentvault.payment.run_id": runID,
		"agentvault.payment.scenario_id": operation.ScenarioID, "agentvault.payment.price_id": operation.PriceID}
	for key, value := range scope {
		if existing, found := bound.Metadata[key]; found && existing != value {
			return nil, errors.New("A2A task is bound to a different payment scope")
		} else if !found && credential != nil {
			return nil, errors.New("A2A task has no established payment binding")
		}
		bound.Metadata[key] = value
	}
	if !reflect.DeepEqual(bound.Metadata, task.Metadata) {
		version, err = store.Save(ctx, &bound, &bound, task, version)
		if err != nil {
			return nil, err
		}
	}
	task = &bound
	if err := call.BindTask(task); err != nil {
		return nil, err
	}
	canonical, err := call.Bytes()
	if err != nil {
		return nil, err
	}
	return &mcpCall{ID: call.Envelope["id"], CredentialProtocol: selected, CredentialPayload: credential, CanonicalBody: canonical,
		a2a: &a2aPaymentCall{context: ctx, store: store, task: task, version: version, protocol: operation.Protocol}}, nil
}

func (middleware *PayeeMiddleware) persistA2ATask(call *mcpCall, body []byte) error {
	if call.a2a == nil {
		return nil
	}
	var response struct {
		Result *a2a.Task `json:"result"`
	}
	if err := json.Unmarshal(body, &response); err != nil {
		return err
	}
	if response.Result == nil {
		return nil
	} // MPP's payment-required response is a JSON-RPC error.
	if response.Result.ID != call.a2a.task.ID || response.Result.ContextID != call.a2a.task.ContextID {
		return errors.New("A2A response changed task/context")
	}
	previous, version, err := call.a2a.store.Get(call.a2a.context, response.Result.ID)
	if err != nil {
		return err
	}
	// The host owns business task updates. Merge only SDK-owned binding metadata
	// and settlement evidence onto the returned official Task, using its CAS port.
	if response.Result.Metadata == nil {
		response.Result.Metadata = map[string]any{}
	}
	for _, key := range []string{"agentvault.payment.run_id", "agentvault.payment.scenario_id", "agentvault.payment.price_id"} {
		if value, present := response.Result.Metadata[key]; present && value != call.a2a.task.Metadata[key] {
			return errors.New("A2A handler changed payment scope")
		}
		response.Result.Metadata[key] = call.a2a.task.Metadata[key]
	}
	_, err = call.a2a.store.Save(call.a2a.context, response.Result, response.Result, previous, version)
	return err
}

func (middleware *PayeeMiddleware) injectA2AReceipt(call *mcpCall, challenge *model.PaymentChallenge, payment *model.SettledIncomingPayment, body []byte) ([]byte, error) {
	var message map[string]any
	decoder := json.NewDecoder(bytes.NewReader(body))
	decoder.UseNumber()
	if err := decoder.Decode(&message); err != nil || message["jsonrpc"] != "2.0" || !reflect.DeepEqual(message["id"], call.ID) {
		return nil, errors.New("A2A response id mismatch")
	}
	if failure, present := message["error"]; present {
		if _, conflicting := message["result"]; conflicting {
			return nil, errors.New("A2A response has both result and error")
		}
		task := *call.a2a.task
		task.Status = a2a.TaskStatus{State: a2a.TaskStateFailed, Message: a2a.NewMessageForTask(a2a.MessageRoleAgent, &task, a2a.DataPart{Data: map[string]any{"jsonrpc_error": failure}})}
		encoded, err := json.Marshal(&task)
		if err != nil {
			return nil, err
		}
		var result map[string]any
		if err := json.Unmarshal(encoded, &result); err != nil {
			return nil, err
		}
		delete(message, "error")
		message["result"] = result
	}
	result, ok := message["result"].(map[string]any)
	if !ok || result["kind"] != "task" || result["id"] != string(call.a2a.task.ID) || result["contextId"] != call.a2a.task.ContextID {
		return nil, errors.New("paid A2A handler must return its bound Task")
	}
	for _, name := range middleware.config.Protocols {
		adapter, err := middleware.protocols.A2A(name)
		if err != nil {
			continue
		}
		if evidence, err := adapter.A2AMerchantEvidence(message, middleware.config.MaxBodyBytes); err != nil {
			return nil, err
		} else if evidence != nil {
			return nil, errors.New("A2A handler attempted to supply payment evidence")
		}
	}
	adapter, err := middleware.protocols.A2A(payment.Receipt.Protocol)
	if err != nil {
		return nil, err
	}
	if err := adapter.A2AInjectReceipt(message, challenge, payment, middleware.config.MaxBodyBytes); err != nil {
		return nil, err
	}
	encoded, err := json.Marshal(message)
	if err != nil || int64(len(encoded)) > middleware.config.MaxBodyBytes {
		return nil, errors.New("paid A2A response exceeds limit")
	}
	if err := middleware.persistA2ATask(call, encoded); err != nil {
		return nil, err
	}
	return encoded, nil
}
