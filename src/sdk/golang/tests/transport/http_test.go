package transport_test

import (
	"context"
	"github.com/wl4g-blockchain/agentvault/src/sdk/golang/pkg/sdk/config"
	"github.com/wl4g-blockchain/agentvault/src/sdk/golang/pkg/sdk/transport"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

func TestHttpJsonAuthTraceAndResponseLimit(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/api/v1/test" || r.Header.Get("Authorization") != "Bearer edge-secret" || !strings.HasPrefix(r.Header.Get("traceparent"), "00-") {
			t.Error("missing HTTP request context")
		}
		w.Header().Set("content-type", "application/json")
		w.Write([]byte(`{"ok":true}`))
	}))
	defer server.Close()
	settings := config.HttpConfig{BaseURL: server.URL, AuthguardBearer: "edge-secret"}
	client, e := transport.NewHttpVaultTransport(settings)
	if e != nil {
		t.Fatal(e)
	}
	defer client.Close()
	response, e := client.Do(context.Background(), transport.TransportRequest{Method: transport.MethodGet, Path: "/api/v1/test"})
	if e != nil || response.Status != 200 || string(response.Body) != `{"ok":true}` {
		t.Fatalf("invalid response: %v", e)
	}
	settings.MaxResponseBytes = 4
	small, e := transport.NewHttpVaultTransport(settings)
	if e != nil {
		t.Fatal(e)
	}
	defer small.Close()
	if _, e = small.Do(context.Background(), transport.TransportRequest{Method: transport.MethodGet, Path: "/api/v1/test"}); e == nil {
		t.Fatal("accepted oversized response")
	}
	client.Close()
	if _, e = client.Do(context.Background(), transport.TransportRequest{Method: transport.MethodGet, Path: "/api/v1/test"}); e == nil {
		t.Fatal("accepted request after close")
	}
}
