package payee_test

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"sync/atomic"
	"testing"

	"github.com/a2aproject/a2a-go/a2a"
	"github.com/a2aproject/a2a-go/a2asrv"
	"github.com/a2aproject/a2a-go/a2asrv/eventqueue"
	"github.com/gin-gonic/gin"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/config"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/payee"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/payer"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/protocol"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/transport"
)

// This test uses the official A2A HTTP handler, executor and TaskStore contract.
// Vault replies use shared protocol fixtures; it does not claim chain settlement.
func TestA2ABothRolesWithOfficialServer(t *testing.T) {
	for _, ginMode := range []bool{false, true} {
		t.Run(fmt.Sprintf("gin=%v", ginMode), func(t *testing.T) { testA2ABothRoles(t, ginMode) })
	}
}
func testA2ABothRoles(t *testing.T, ginMode bool) {
	fixture := readPayeeMCPFixture(t)
	for _, name := range []string{"x402", "mpp"} {
		for _, existing := range []bool{false, true} {
			for _, failed := range []bool{false, true} {
				t.Run(fmt.Sprintf("%s/existing=%v/failed=%v", name, existing, failed), func(t *testing.T) {
					entry := fixture["protocols"].(map[string]any)[name].(map[string]any)
					payeeCore, hashes, _ := newMCPPayeeCore(t, name, entry, true)
					store := &a2aTestStore{tasks: map[a2a.TaskID]*a2a.Task{}, versions: map[a2a.TaskID]a2a.TaskVersion{}}
					taskID := a2a.TaskID("run-1")
					if existing {
						taskID = "task-1"
						store.tasks[taskID] = &a2a.Task{ID: taskID, ContextID: "context-1", Status: a2a.TaskStatus{State: a2a.TaskStateSubmitted}}
					}
					executor := &a2aTestExecutor{}
					native := a2asrv.NewJSONRPCHandler(a2asrv.NewHandler(executor, a2asrv.WithTaskStore(store)))
					route := payee.StandardPayeeRoute{Method: "POST", Path: "/a2a", ScenarioID: "scenario", PriceID: "price",
						Resource: "mcp://tool/marketpulse.realtime", Origin: "https://marketpulse.example", Protocol: name, A2ATaskStore: store}
					business := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
						if r.Header.Get("Authorization") != "Bearer app-secret" {
							t.Error("Bearer was not preserved")
						}
						if failed {
							executor.calls.Add(1)
							w.Header().Set("Content-Type", "application/json")
							_, _ = w.Write([]byte(`{"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"business failed"}}`))
							return
						}
						native.ServeHTTP(w, r)
					})
					var protected http.Handler = payeeCore.Standard(route).Handler(business)
					if ginMode {
						engine := gin.New()
						ginRoute := payee.GinPayeeRoute{Method: route.Method, Path: route.Path, ScenarioID: route.ScenarioID, PriceID: route.PriceID, Resource: route.Resource, Origin: route.Origin, Protocol: route.Protocol, A2ATaskStore: store}
						engine.POST("/a2a", payeeCore.Gin(ginRoute).Handler(), gin.WrapH(business))
						protected = engine
					}
					server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
						recorder := httptest.NewRecorder()
						protected.ServeHTTP(recorder, r)
						if recorder.Code != 200 {
							t.Logf("A2A merchant failure: %d %s", recorder.Code, recorder.Body.String())
						}
						for key, values := range recorder.Header() {
							w.Header()[key] = values
						}
						w.WriteHeader(recorder.Code)
						_, _ = w.Write(recorder.Body.Bytes())
					}))
					defer server.Close()
					card := &a2a.AgentCard{}
					if err := payeeCore.A2AAgentCard(card); err != nil || len(card.Capabilities.Extensions) != 1 {
						t.Fatal("A2A extension declaration missing", err)
					}
					var reports atomic.Int32
					core := a2aPayerCore(t, name, entry, server.URL, &reports)
					request, _ := http.NewRequest("POST", server.URL+"/a2a", strings.NewReader(`{"jsonrpc":"2.0","id":1,"method":"message/send","params":{"message":{"kind":"message","messageId":"request-1","role":"user","parts":[{"kind":"text","text":"news"}],"metadata":{"app":"preserved"}}}}`))
					if existing {
						raw, _ := io.ReadAll(request.Body)
						var call map[string]any
						_ = json.Unmarshal(raw, &call)
						message := call["params"].(map[string]any)["message"].(map[string]any)
						message["taskId"] = "task-1"
						message["contextId"] = "context-1"
						raw, _ = json.Marshal(call)
						request.Body = io.NopCloser(bytes.NewReader(raw))
						request.ContentLength = int64(len(raw))
						request.GetBody = func() (io.ReadCloser, error) { return io.NopCloser(bytes.NewReader(raw)), nil }
					}
					request.Header.Set("Content-Type", "application/json")
					request.Header.Set("Authorization", "Bearer app-secret")
					operation, _ := model.NewPayerRequestContext("run-1")
					response, err := core.Standard(nil).Client().Do(payer.WithPayment(request, operation))
					if err != nil {
						t.Fatal(err)
					}
					defer response.Body.Close()
					body, _ := io.ReadAll(response.Body)
					if response.StatusCode != 200 || executor.calls.Load() != 1 || reports.Load() != 1 {
						t.Fatalf("A2A flow failed: status=%d executor=%d reports=%d body=%s", response.StatusCode, executor.calls.Load(), reports.Load(), body)
					}
					if len(*hashes) != 2 || (*hashes)[0] != (*hashes)[1] {
						t.Fatalf("A2A credential changed operation hash: %v", *hashes)
					}
					task, _, err := store.Get(context.Background(), taskID)
					expected := a2a.TaskStateCompleted
					if failed {
						expected = a2a.TaskStateFailed
					}
					if err != nil || task.Status.State != expected {
						t.Fatal("A2A task did not complete", err)
					}
					if !bytes.Contains(body, []byte("payment-completed")) && name == "x402" {
						t.Fatal("x402 A2A receipt missing")
					}
					if !bytes.Contains(body, []byte("org.paymentauth/receipt")) && name == "mpp" {
						t.Fatal("MPP JSON-RPC receipt missing")
					}
				})
			}
		}
	}
}

func a2aPayerCore(t *testing.T, name string, entry map[string]any, origin string, reports *atomic.Int32) *payer.PayerMiddleware {
	t.Helper()
	key, headerName := "x402/payment", "payment-signature"
	if name == "mpp" {
		key, headerName = "org.paymentauth/credential", "payment-authorization"
	}
	raw, _ := json.Marshal(entry["credential_meta"].(map[string]any)[key])
	headerValue := base64.StdEncoding.EncodeToString(raw)
	if name == "mpp" {
		native, err := (&protocol.MppAdapter{}).PresentedMCPCredential(entry["credential_meta"], 64*1024)
		if err != nil {
			t.Fatal(err)
		}
		headerValue = native.(map[string]any)["authorization"].(string)
	}
	core, err := payer.NewPayerMiddleware(payeeTransport(func(_ context.Context, request transport.TransportRequest) (transport.TransportResponse, error) {
		if strings.HasSuffix(request.Path, "/outbound-response") {
			reports.Add(1)
			return payeeJSONResponse(t, map[string]any{"accepted": true, "replayed": false, "intent": map[string]any{"id": "intent-1"}}), nil
		}
		if request.Path != "/api/v1/payments/authorize" {
			return transport.TransportResponse{}, errors.New("unexpected Vault request")
		}
		return payeeJSONResponse(t, map[string]any{
			"authorization": map[string]any{"outcome": "authorized", "intent_id": "intent-1"},
			"intent":        map[string]any{"id": "intent-1", "scenario_id": "scenario", "run_id": "run-1", "wallet_id": "wallet", "policy_id": "policy", "direction": "outbound", "protocol": name, "resource": origin + "/a2a", "challenge_hash": "challenge"},
			"credential":    map[string]any{"protocol": name, "challenge_hash": "challenge", "credential_hash": "hash", "payload": map[string]any{}, "http": map[string]any{"bound_origin": origin, "header_name": headerName, "header_value": headerValue}},
		}), nil
	}), config.PayerConfig{ScenarioID: "scenario"})
	if err != nil {
		t.Fatal(err)
	}
	return core
}

type a2aTestExecutor struct{ calls atomic.Int32 }

func (executor *a2aTestExecutor) Execute(ctx context.Context, request *a2asrv.RequestContext, queue eventqueue.Queue) error {
	executor.calls.Add(1)
	if request.Message.Metadata["app"] != "preserved" || request.Message.Metadata["x402.payment.payload"] != nil {
		return errors.New("A2A business metadata corrupted")
	}
	task := *request.StoredTask
	task.Status = a2a.TaskStatus{State: a2a.TaskStateCompleted, Message: a2a.NewMessageForTask(a2a.MessageRoleAgent, request, a2a.TextPart{Text: "news delivered"})}
	return queue.Write(ctx, &task)
}
func (*a2aTestExecutor) Cancel(context.Context, *a2asrv.RequestContext, eventqueue.Queue) error {
	return nil
}

type a2aTestStore struct {
	sync.Mutex
	tasks    map[a2a.TaskID]*a2a.Task
	versions map[a2a.TaskID]a2a.TaskVersion
}

func (store *a2aTestStore) Get(_ context.Context, id a2a.TaskID) (*a2a.Task, a2a.TaskVersion, error) {
	store.Lock()
	defer store.Unlock()
	task := store.tasks[id]
	if task == nil {
		return nil, 0, a2a.ErrTaskNotFound
	}
	return store.clone(task), store.versions[id], nil
}
func (store *a2aTestStore) Save(_ context.Context, task *a2a.Task, _ a2a.Event, _ *a2a.Task, version a2a.TaskVersion) (a2a.TaskVersion, error) {
	store.Lock()
	defer store.Unlock()
	if store.versions[task.ID] != version {
		return 0, errors.New("task version conflict")
	}
	store.versions[task.ID]++
	store.tasks[task.ID] = store.clone(task)
	return store.versions[task.ID], nil
}
func (store *a2aTestStore) clone(task *a2a.Task) *a2a.Task {
	raw, _ := json.Marshal(task)
	var copy a2a.Task
	_ = json.Unmarshal(raw, &copy)
	return &copy
}
func (*a2aTestStore) List(context.Context, *a2a.ListTasksRequest) (*a2a.ListTasksResponse, error) {
	return &a2a.ListTasksResponse{}, nil
}
