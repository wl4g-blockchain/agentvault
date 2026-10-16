package transport_test

import (
	"testing"
	"time"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/config"
	. "github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/transport"
)

func TestGrpcVaultTransportRejectsRemoteCleartext(t *testing.T) {
	_, err := NewGrpcVaultTransport(config.GrpcConfig{Endpoint: "agentvault.example:50051", RequestTimeout: time.Second})
	if err == nil {
		t.Fatal("remote cleartext gRPC must be rejected")
	}
}
