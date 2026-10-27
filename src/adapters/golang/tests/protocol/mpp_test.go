package protocol_test

import (
	"encoding/base64"
	"encoding/json"
	"strings"
	"testing"

	mpp "github.com/tempoxyz/mpp-go/pkg/mpp"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/protocol"
)

func TestMPPAdapterPreservesBearerAndUsesNativeWireHeaders(t *testing.T) {
	adapter := &protocol.MppAdapter{}
	native := mpp.NewChallenge(
		"0123456789abcdef0123456789abcdef",
		"merchant.example",
		"tempo",
		"charge",
		map[string]any{"amount": "1000", "currency": "USDC"},
	)
	wwwAuthenticate, err := native.ToAuthenticateStrict(native.Realm)
	if err != nil {
		t.Fatal(err)
	}
	wwwAuthenticate += `, header="Payment-Authorization"`
	challenge, err := adapter.Challenge(&model.PaymentChallenge{
		Protocol: "mpp", Payload: map[string]any{"www_authenticate": wwwAuthenticate},
	})
	if err != nil {
		t.Fatal(err)
	}
	if challenge.Headers["WWW-Authenticate"] == "" || challenge.Headers["Payment-Required"] != "" {
		t.Fatalf("MPP challenge was wrapped as x402: %#v", challenge.Headers)
	}
	authorization := withPaymentAuthorizationHeader(t,
		native.NewCredential(map[string]any{"type": "hash", "hash": "0x1111"}).ToAuthorization())
	payload, err := adapter.PresentedCredential(map[string]string{
		"Authorization":         "Bearer application-token",
		"Payment-Authorization": authorization,
	}, 1024)
	if err != nil {
		t.Fatal(err)
	}
	if payload.(map[string]any)["authorization"] != authorization {
		t.Fatal("MPP provider overwrote or selected the Bearer credential")
	}
	if _, err := adapter.PresentedCredential(map[string]string{"Authorization": authorization}, 1024); err == nil {
		t.Fatal("MPP provider accepted a credential from a header other than the challenged header")
	}
	if _, err := adapter.Credential(model.PaymentCredential{
		Protocol: "mpp", ChallengeHash: "challenge", CredentialHash: "credential",
		HTTP: &model.HTTPCredential{
			BoundOrigin: "https://merchant.example", HeaderName: "Authorization", HeaderValue: authorization,
		},
	}); err == nil {
		t.Fatal("MPP payer accepted a credential delivery that conflicts with its challenge")
	}
}

func TestMPPMCPAndA2ACredentialsNormalizeRequestAndPreserveOpaque(t *testing.T) {
	adapter := &protocol.MppAdapter{}
	native := mpp.NewChallenge("0123456789abcdef0123456789abcdef", "merchant.example", "tempo", "charge",
		map[string]any{"amount": "1000", "currency": "USDC"})
	// Server-authenticated opaque bytes must survive even when their JSON key
	// order and whitespace differ from Go's default map encoding.
	opaque := base64.RawURLEncoding.EncodeToString([]byte(`{"z":"scope", "a":"bound"}`))
	envelope := credentialEnvelope(t, native.NewCredential(map[string]any{"type": "hash", "hash": "0x1111"}).ToAuthorization())
	challenge := envelope["challenge"].(map[string]any)
	challenge["opaque"] = opaque
	challenge["header"] = "Payment-Authorization"
	body, err := json.Marshal(envelope)
	if err != nil {
		t.Fatal(err)
	}
	credential := model.PaymentCredential{Protocol: "mpp", HTTP: &model.HTTPCredential{
		HeaderName: "Payment-Authorization", HeaderValue: "Payment " + base64.RawURLEncoding.EncodeToString(body),
	}}
	metadata, err := adapter.MCPCredential(credential)
	if err != nil {
		t.Fatal(err)
	}
	projected := metadata.Value.(map[string]any)["challenge"].(map[string]any)
	if _, ok := projected["request"].(map[string]any); !ok {
		t.Fatalf("MCP must carry an expanded request object: %T", projected["request"])
	}
	if projected["opaque"] != opaque || projected["header"] != "Payment-Authorization" {
		t.Fatal("MCP projection changed authenticated opaque bytes or delivery header")
	}
	payload, err := adapter.PresentedMCPCredential(map[string]any{metadata.Key: metadata.Value}, 65536)
	if err != nil {
		t.Fatal(err)
	}
	assertNativeCredential := func(payload any) {
		t.Helper()
		authorization := payload.(map[string]any)["authorization"].(string)
		roundtrip := credentialEnvelope(t, authorization)["challenge"].(map[string]any)
		if roundtrip["request"] != challenge["request"] || roundtrip["opaque"] != opaque || roundtrip["header"] != "Payment-Authorization" {
			t.Fatal("MCP conversion changed the native challenge echo")
		}
		if _, err := adapter.PresentedCredential(map[string]string{"Payment-Authorization": authorization}, 65536); err != nil {
			t.Fatal(err)
		}
	}
	assertNativeCredential(payload)
	message := map[string]any{"jsonrpc": "2.0", "id": "a2a", "method": "message/send", "params": map[string]any{}}
	if err := adapter.A2AApplyCredential(message, credential); err != nil {
		t.Fatal(err)
	}
	payload, err = adapter.PresentedA2ACredential(message, 65536)
	if err != nil {
		t.Fatal(err)
	}
	assertNativeCredential(payload)
	for _, mutate := range []func(map[string]any){
		func(value map[string]any) { delete(value, "payload") },
		func(value map[string]any) { value["challenge"].(map[string]any)["id"] = "" },
		func(value map[string]any) { value["challenge"].(map[string]any)["header"] = "X-Untrusted" },
	} {
		copyBody, _ := json.Marshal(metadata.Value)
		var invalid map[string]any
		if err := json.Unmarshal(copyBody, &invalid); err != nil {
			t.Fatal(err)
		}
		mutate(invalid)
		if _, err := adapter.PresentedMCPCredential(map[string]any{metadata.Key: invalid}, 65536); err == nil {
			t.Fatal("MCP accepted an invalid credential")
		}
	}
}

func TestMPPMCPChallengePreservesRawOpaque(t *testing.T) {
	adapter := &protocol.MppAdapter{}
	native := mpp.NewChallenge("0123456789abcdef0123456789abcdef", "merchant.example", "tempo", "charge",
		map[string]any{"amount": "1000", "currency": "USDC"})
	raw, err := json.Marshal(native)
	if err != nil {
		t.Fatal(err)
	}
	var challenge map[string]any
	if err := json.Unmarshal(raw, &challenge); err != nil {
		t.Fatal(err)
	}
	opaque := base64.RawURLEncoding.EncodeToString([]byte(`{"z":"scope", "a":"bound"}`))
	challenge["opaque"] = opaque
	response, err := adapter.MCPChallenge(map[string]any{"error": map[string]any{
		"code": -32042, "message": "Payment Required", "data": map[string]any{
			"httpStatus": 402, "challenges": []any{challenge},
		},
	}}, 65536)
	if err != nil {
		t.Fatal(err)
	}
	if response == nil || !strings.Contains(response.Headers["www-authenticate"], `opaque="`+opaque+`"`) {
		t.Fatal("MCP challenge conversion changed authenticated opaque bytes")
	}
	challenge["opaque"] = `invalid"injection`
	if _, err := adapter.MCPChallenge(map[string]any{"error": map[string]any{
		"code": -32042, "data": map[string]any{"httpStatus": 402, "challenges": []any{challenge}},
	}}, 65536); err == nil {
		t.Fatal("MCP challenge accepted an invalid opaque encoding")
	}
}

func credentialEnvelope(t *testing.T, authorization string) map[string]any {
	t.Helper()
	token := strings.TrimSpace(strings.TrimPrefix(authorization, "Payment "))
	raw, err := base64.RawURLEncoding.DecodeString(token)
	if err != nil {
		t.Fatal(err)
	}
	var envelope map[string]any
	if err := json.Unmarshal(raw, &envelope); err != nil {
		t.Fatal(err)
	}
	return envelope
}

func withPaymentAuthorizationHeader(t *testing.T, authorization string) string {
	t.Helper()
	token := strings.TrimSpace(strings.TrimPrefix(authorization, "Payment "))
	raw, err := base64.RawURLEncoding.DecodeString(token)
	if err != nil {
		t.Fatal(err)
	}
	var envelope map[string]any
	if err := json.Unmarshal(raw, &envelope); err != nil {
		t.Fatal(err)
	}
	challenge, ok := envelope["challenge"].(map[string]any)
	if !ok {
		t.Fatal("credential challenge is missing")
	}
	challenge["header"] = "Payment-Authorization"
	raw, err = json.Marshal(envelope)
	if err != nil {
		t.Fatal(err)
	}
	return "Payment " + base64.RawURLEncoding.EncodeToString(raw)
}
