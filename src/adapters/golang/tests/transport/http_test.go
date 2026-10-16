package transport_test

import (
	"testing"

	. "github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/transport"
)

func TestHttpVaultTransportRejectsAuthorityWithoutScheme(t *testing.T) {
	if _, err := NewHttpVaultTransport("agentvault.internal"); err == nil {
		t.Fatal("unsafe HTTP endpoint must be rejected")
	}
}
