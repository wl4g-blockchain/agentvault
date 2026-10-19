package transport_test

import (
	"context"
	"errors"
	"testing"

	. "github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/transport"
)

type stubTransport struct {
	response TransportResponse
}

func (stub stubTransport) Do(context.Context, TransportRequest) (TransportResponse, error) {
	return stub.response, nil
}

func TestClientMapsConflictToRunConflict(t *testing.T) {
	client := NewClient(stubTransport{response: TransportResponse{
		Status: 409,
		Body:   []byte(`{"error":{"code":"RUN_CONFLICT","message":"conflict"}}`),
	}})
	var output map[string]any
	err := client.Get(context.Background(), "/api/v1/test", &output)
	if !errors.Is(err, ErrRunConflict) {
		t.Fatalf("expected run conflict, got %v", err)
	}
}
