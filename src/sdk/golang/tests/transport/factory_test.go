package transport_test

import (
	"github.com/wl4g-blockchain/agentvault/src/sdk/golang/pkg/sdk/config"
	"github.com/wl4g-blockchain/agentvault/src/sdk/golang/pkg/sdk/transport"
	"testing"
)

func TestFactoryOwnsIndependentHttpClients(t *testing.T) {
	f := transport.NewVaultTransportFactory()
	c := config.VaultTransportConfig{Provider: config.TransportProviderHttp, Http: &config.HttpConfig{BaseURL: "http://127.0.0.1:65535"}}
	a, e := f.Create(c)
	if e != nil {
		t.Fatal(e)
	}
	b, e := f.Create(c)
	if e != nil {
		t.Fatal(e)
	}
	defer b.Close()
	if a == b {
		t.Fatal("reused client")
	}
	a.Close()
	a.Close()
	c.Provider = "unknown"
	if _, e = f.Create(c); e == nil {
		t.Fatal("accepted unknown transport")
	}
}
