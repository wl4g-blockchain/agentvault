package model_test

import (
	"github.com/a2aproject/a2a-go/a2a"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	"testing"
)

func TestA2ACallBindsOfficialTaskAndRejectsStreaming(t *testing.T) {
	call, err := (model.A2ACall{}).Parse([]byte(`{"jsonrpc":"2.0","id":"call","method":"message/send","params":{"message":{"kind":"message","messageId":"msg","role":"user","parts":[{"kind":"text","text":"news"}],"metadata":{"app":"kept"}}}}`), 4096)
	if err != nil || call == nil {
		t.Fatal(err)
	}
	params, err := call.Params()
	if err != nil || params.Message.Metadata["app"] != "kept" {
		t.Fatal("official Message projection lost metadata", err)
	}
	if err := call.BindTask(&a2a.Task{ID: "run", ContextID: "context"}); err != nil {
		t.Fatal(err)
	}
	if err := call.BindTask(&a2a.Task{ID: "other", ContextID: "context"}); err == nil {
		t.Fatal("task changed")
	}
	for _, method := range []string{"message/stream", "SendStreamingMessage", "SendMessage"} {
		if _, err := (model.A2ACall{}).Parse([]byte(`{"jsonrpc":"2.0","id":1,"method":"`+method+`"}`), 4096); err == nil {
			t.Fatal("unsupported A2A method accepted", method)
		}
	}
}
