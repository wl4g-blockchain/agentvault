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
