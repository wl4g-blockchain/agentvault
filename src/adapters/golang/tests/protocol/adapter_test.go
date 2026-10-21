package protocol_test

import (
	"encoding/base64"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	mpp "github.com/tempoxyz/mpp-go/pkg/mpp"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/protocol"
	x402 "github.com/x402-foundation/x402/go/v2"
)

func TestFactoryCreatesIsolatedProtocolProviders(t *testing.T) {
	factory := protocol.NewPaymentProtocolFactory()
	first, err := factory.Create([]string{"x402", "mpp"})
	if err != nil {
		t.Fatal(err)
	}
	second, err := factory.Create([]string{"mpp"})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := first.Get("x402"); err != nil {
		t.Fatal(err)
	}
	if _, err := second.Get("x402"); err == nil {
		t.Fatal("factory instances leaked enabled providers")
	}
}

func TestSharedProtocolHTTPFixture(t *testing.T) {
	value, err := os.ReadFile(filepath.Join("..", "..", "..", "fixtures", "payment_protocol_http.json"))
	if err != nil {
		t.Fatal(err)
	}
	var fixture struct {
		Schema    string `json:"schema"`
		Protocols map[string]struct {
			EvidenceHeader string `json:"evidence_header"`
		} `json:"protocols"`
		Matrix []map[string]string `json:"matrix"`
	}
	if err := json.Unmarshal(value, &fixture); err != nil {
		t.Fatal(err)
	}
	if fixture.Schema != "agentvault.sdk.protocol-http.v1" {
		t.Fatalf("unexpected shared protocol schema: %s", fixture.Schema)
	}
	registry, err := protocol.NewPaymentProtocolFactory().Create([]string{"x402", "mpp"})
	if err != nil {
		t.Fatal(err)
	}
	combinedEvidence := make(map[string]string, len(fixture.Protocols))
	for name, contract := range fixture.Protocols {
		adapter, err := registry.Get(name)
		if err != nil {
			t.Fatal(err)
		}
		var wire string
		switch name {
		case "x402":
			body, marshalErr := json.Marshal(x402.SettleResponse{
				Success: true, Payer: "0x2222222222222222222222222222222222222222",
				Transaction: "0x1111", Network: x402.Network("eip155:42431"),
			})
			if marshalErr != nil {
				t.Fatal(marshalErr)
			}
			wire = base64.StdEncoding.EncodeToString(body)
		case "mpp":
			wire = mpp.FormatReceipt(mpp.Success("0x1111", mpp.WithReceiptMethod("tempo")))
		}
		evidence, err := adapter.MerchantEvidence(map[string]string{contract.EvidenceHeader: wire})
		if err != nil || evidence == nil || evidence.HeaderName != contract.EvidenceHeader {
			t.Fatalf("%s evidence mapping does not match the shared contract", name)
		}
		combinedEvidence[contract.EvidenceHeader] = wire
	}
	if _, _, err := registry.MerchantEvidence(combinedEvidence); err == nil {
		t.Fatal("cross-protocol HTTP settlement evidence was accepted")
	}
	if got := strings.Join(registry.CredentialHeaderNames(), ","); got != "payment-signature,payment-authorization,authorization" {
		t.Fatalf("unexpected credential header ownership: %s", got)
	}
	if got := strings.Join(registry.MerchantEvidenceHeaderNames(), ","); got != "payment-response,payment-receipt" {
		t.Fatalf("unexpected evidence header ownership: %s", got)
	}
	expected := map[string]struct{}{
		"grpc/x402/payer": {}, "grpc/x402/payee": {},
		"grpc/mpp/payer": {}, "grpc/mpp/payee": {},
		"mqtt/x402/payer": {}, "mqtt/x402/payee": {},
		"mqtt/mpp/payer": {}, "mqtt/mpp/payee": {},
	}
	seen := make(map[string]struct{}, len(fixture.Matrix))
	for _, row := range fixture.Matrix {
		key := row["transport"] + "/" + row["protocol"] + "/" + row["role"]
		if _, exists := expected[key]; !exists {
			t.Fatalf("unexpected protocol matrix row: %s", key)
		}
		if _, duplicate := seen[key]; duplicate {
			t.Fatalf("duplicate protocol matrix row: %s", key)
		}
		seen[key] = struct{}{}
	}
	if len(seen) != len(expected) {
		t.Fatalf("expected full 2x2x2 matrix, got %d distinct rows", len(seen))
	}
}

func TestSharedMCPFixtureUsesOfficialProtocolShapes(t *testing.T) {
	fixture := readMCPFixture(t)
	registry, err := protocol.NewPaymentProtocolFactory().Create([]string{"x402", "mpp"})
	if err != nil {
		t.Fatal(err)
	}
	combinedCredentials := map[string]any{}
	combinedReceipts := map[string]any{}
	for _, name := range []string{"x402", "mpp"} {
		entry := fixture["protocols"].(map[string]any)[name].(map[string]any)
		selected, challenge, err := registry.MCPChallenge(entry["challenge_response"], 64*1024)
		if err != nil {
			t.Fatalf("%s challenge: %v", name, err)
		}
		if selected == nil || selected.Name() != name || challenge == nil {
			t.Fatalf("%s MCP challenge was not selected", name)
		}
		if name == "mpp" && !strings.Contains(challenge.Headers["www-authenticate"], `header="Payment-Authorization"`) {
			t.Fatalf("MPP credential header selection was lost: %q", challenge.Headers["www-authenticate"])
		}
		meta := entry["credential_meta"].(map[string]any)
		presented, err := selected.PresentedMCPCredential(meta, 64*1024)
		if err != nil || presented == nil {
			t.Fatalf("%s credential: value=%#v error=%v", name, presented, err)
		}
		evidence, err := selected.MCPMerchantEvidence(entry["receipt_meta"], 64*1024)
		if err != nil || evidence == nil || evidence.HeaderValue == "" {
			t.Fatalf("%s receipt: value=%#v error=%v", name, evidence, err)
		}
		for key, value := range meta {
			combinedCredentials[key] = value
		}
		for key, value := range entry["receipt_meta"].(map[string]any) {
			combinedReceipts[key] = value
		}
	}
	if _, _, err := registry.PresentedMCPCredential(combinedCredentials, 64*1024); err == nil {
		t.Fatal("cross-protocol MCP credentials were accepted")
	}
	if _, _, err := registry.MCPMerchantEvidence(combinedReceipts, 64*1024); err == nil {
		t.Fatal("cross-protocol MCP receipts were accepted")
	}
	if got := strings.Join(registry.MCPRequestMetadataKeys(), ","); got != "x402/payment,org.paymentauth/credential" {
		t.Fatalf("unexpected MCP credential key ownership: %s", got)
	}
	if got := strings.Join(registry.MCPRootRequestMetadataKeys(), ","); got != "org.paymentauth/credential" {
		t.Fatalf("unexpected root MCP credential key ownership: %s", got)
	}
	if got := strings.Join(registry.MCPReceiptMetadataKeys(), ","); got != "x402/payment-response,org.paymentauth/receipt" {
		t.Fatalf("unexpected MCP receipt key ownership: %s", got)
	}
}

func TestX402MCPRejectsConflictingStructuredAndTextChallenges(t *testing.T) {
	fixture := readMCPFixture(t)
	response := fixture["protocols"].(map[string]any)["x402"].(map[string]any)["challenge_response"].(map[string]any)
	result := response["result"].(map[string]any)
	result["content"] = []any{map[string]any{
		"type": "text", "text": `{"x402Version":2,"resource":null,"accepts":[{"scheme":"exact","network":"eip155:1","amount":"1","payTo":"0x1","maxTimeoutSeconds":1,"asset":"0x2"}]}`,
	}}
	adapter := &protocol.X402Adapter{}
	if _, err := adapter.MCPChallenge(response, 64*1024); err == nil {
		t.Fatal("conflicting x402 MCP challenge was accepted")
	}
}

func TestX402MCPDoesNotMisclassifyUnrelatedToolErrors(t *testing.T) {
	adapter := &protocol.X402Adapter{}
	challenge, err := adapter.MCPChallenge(map[string]any{
		"jsonrpc": "2.0", "id": 1,
		"result": map[string]any{
			"isError":           true,
			"structuredContent": map[string]any{"error": "permission denied"},
			"content":           []any{map[string]any{"type": "text", "text": "permission denied"}},
		},
	}, 64*1024)
	if err != nil || challenge != nil {
		t.Fatalf("unrelated MCP tool error was treated as x402: challenge=%#v error=%v", challenge, err)
	}
}

func TestMPPMCPRejectsUnsupportedSessionAndPreservesRawCredential(t *testing.T) {
	fixture := readMCPFixture(t)
	entry := fixture["protocols"].(map[string]any)["mpp"].(map[string]any)
	response := entry["challenge_response"].(map[string]any)
	challenge := response["error"].(map[string]any)["data"].(map[string]any)["challenges"].([]any)[0].(map[string]any)
	challenge["intent"] = "session"
	adapter := &protocol.MppAdapter{}
	if _, err := adapter.MCPChallenge(response, 64*1024); err == nil {
		t.Fatal("unsupported MPP session was accepted as charge")
	}

	credentialMeta := entry["credential_meta"].(map[string]any)
	credential := credentialMeta["org.paymentauth/credential"]
	body, err := json.Marshal(credential)
	if err != nil {
		t.Fatal(err)
	}
	authorization := "Payment " + base64.RawURLEncoding.EncodeToString(body)
	delivery, err := adapter.MCPCredential(model.PaymentCredential{
		Protocol: "mpp",
		HTTP: &model.HTTPCredential{
			BoundOrigin: "https://marketpulse.example",
			HeaderName:  "Payment-Authorization",
			HeaderValue: authorization,
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	deliveryChallenge := delivery.Value.(map[string]any)["challenge"].(map[string]any)
	if deliveryChallenge["header"] != "Payment-Authorization" {
		t.Fatalf("MPP MCP credential lost the upstream header extension: %#v", deliveryChallenge)
	}
}

func readMCPFixture(t *testing.T) map[string]any {
	t.Helper()
	value, err := os.ReadFile(filepath.Join("..", "..", "..", "fixtures", "payment_protocol_mcp.json"))
	if err != nil {
		t.Fatal(err)
	}
	var fixture map[string]any
	if err := json.Unmarshal(value, &fixture); err != nil {
		t.Fatal(err)
	}
	if fixture["schema"] != "agentvault.sdk.protocol-mcp.v1" {
		t.Fatalf("unexpected MCP fixture schema: %#v", fixture["schema"])
	}
	return fixture
}
