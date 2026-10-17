package payer_test

import (
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/config"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	. "github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/payer"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/transport"
)

func TestStandardPayerMiddlewareRetriesPaidRequest(t *testing.T) {
	merchant := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		if request.Header.Get("payment-signature") == "" {
			writer.Header().Set("payment-protocol", "x402")
			writer.WriteHeader(http.StatusPaymentRequired)
			_, _ = writer.Write([]byte(`{"accepts":[]}`))
			return
		}
		writer.Header().Set("payment-response", "YWJjZA==")
		writer.WriteHeader(http.StatusOK)
	}))
	defer merchant.Close()

	core, err := NewPayerMiddleware(payerTransport(func(_ context.Context, request transport.TransportRequest) (transport.TransportResponse, error) {
		if request.Path == "/api/v1/payments/authorize" {
			body := `{"authorization":{"outcome":"authorized","intent_id":"intent-1"},"intent":{"id":"intent-1","scenario_id":"scenario","run_id":"run-1","wallet_id":"wallet","policy_id":"policy","direction":"outbound","protocol":"x402","asset":{"network":"eip155:31337","asset":"USDC","decimals":6},"recipient":"0x0000000000000000000000000000000000000001","amount":"1000","resource":"` + merchant.URL + `/mcp","challenge_hash":"challenge"},"credential":{"protocol":"x402","challenge_hash":"challenge","credential_hash":"hash","payload":{},"http":{"bound_origin":"` + merchant.URL + `","header_name":"payment-signature","header_value":"signed"}}}`
			return transport.TransportResponse{Status: 200, Body: []byte(body)}, nil
		}
		if strings.HasSuffix(request.Path, "/outbound-response") {
			return transport.TransportResponse{Status: 200, Body: []byte(`{"intent":{"id":"intent-1"},"accepted":true,"replayed":false}`)}, nil
		}
		return transport.TransportResponse{Status: 500, Body: []byte(`{"error":{"code":"UNEXPECTED","message":"unexpected"}}`)}, nil
	}), config.PayerConfig{ScenarioID: "scenario"})
	if err != nil {
		t.Fatal(err)
	}
	request, _ := http.NewRequest(http.MethodPost, merchant.URL+"/mcp", strings.NewReader(`{}`))
	operation, _ := model.NewPayerRequestContext("run-1")
	response, err := core.Standard(nil).Client().Do(WithPayment(request, operation))
	if err != nil {
		t.Fatal(err)
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		t.Fatalf("unexpected status %d", response.StatusCode)
	}
	if payment, ok := PaymentFromResponse(response); !ok || payment.Intent.ID != "intent-1" {
		t.Fatalf("trusted payment metadata missing: %#v", payment)
	}
}
