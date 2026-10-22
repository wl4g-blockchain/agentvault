package protocol_test

import (
	"bytes"
	"encoding/base64"
	"encoding/json"
	"os"
	"testing"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/model"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/protocol"
)

func TestX402AdapterUsesNativeWireHeaders(t *testing.T) {
	wire := map[string]any{
		"x402Version": float64(2),
		"resource":    map[string]any{"url": "https://merchant.example/tool", "mimeType": "application/json"},
		"accepts": []any{map[string]any{
			"scheme": "exact", "network": "eip155:42431", "amount": "1000",
			"payTo": "0x1111111111111111111111111111111111111111", "maxTimeoutSeconds": float64(60),
			"asset": "0x2222222222222222222222222222222222222222",
		}},
	}
	challenge, err := (&protocol.X402Adapter{}).Challenge(&model.PaymentChallenge{
		Protocol: "x402", Payload: map[string]any{"payment_required": wire},
	})
	if err != nil {
		t.Fatal(err)
	}
	decoded, err := base64.StdEncoding.DecodeString(challenge.Headers["Payment-Required"])
	if err != nil {
		t.Fatal(err)
	}
	var actual map[string]any
	if err := json.Unmarshal(decoded, &actual); err != nil || actual["x402Version"] != float64(2) {
		t.Fatalf("invalid x402 challenge: %s", decoded)
	}
	if challenge.Headers["WWW-Authenticate"] != "" {
		t.Fatal("x402 challenge used an MPP header")
	}
}

func TestX402AdapterPreservesChainSpecificV2PayloadsAndRejectsCrossovers(t *testing.T) {
	contents, err := os.ReadFile("../../../fixtures/x402_v2_chain_payloads.json")
	if err != nil {
		t.Fatal(err)
	}
	var fixture map[string]any
	if err := json.Unmarshal(contents, &fixture); err != nil {
		t.Fatal(err)
	}
	cases := fixture["cases"].(map[string]any)
	evm := cases["evm_eip3009"].(map[string]any)["payment_payload"]
	solana := cases["solana_spl_exact"].(map[string]any)["payment_payload"]
	adapter := &protocol.X402Adapter{}

	for _, test := range []struct {
		name    string
		payload any
	}{{"evm_eip3009", evm}, {"solana_spl_exact", solana}} {
		t.Run(test.name, func(t *testing.T) {
			payloadJSON, marshalErr := json.Marshal(test.payload)
			if marshalErr != nil {
				t.Fatal(marshalErr)
			}
			header := base64.StdEncoding.EncodeToString(payloadJSON)
			delivery := model.HTTPCredential{
				BoundOrigin: "https://merchant.example", HeaderName: "PAYMENT-SIGNATURE", HeaderValue: header,
			}
			credential := model.PaymentCredential{
				Protocol: "x402", ChallengeHash: "challenge", CredentialHash: "credential",
				Payload: test.payload, HTTP: &delivery,
			}

			mapped, credentialErr := adapter.Credential(credential)
			if credentialErr != nil || mapped != delivery {
				t.Fatalf("payer HTTP credential mapping failed: value=%v error=%v", mapped, credentialErr)
			}
			metadata, metadataErr := adapter.MCPCredential(credential)
			if metadataErr != nil || metadata.Key != "x402/payment" {
				t.Fatalf("payer MCP credential mapping failed: value=%v error=%v", metadata, metadataErr)
			}

			presented, presentedErr := adapter.PresentedCredential(
				map[string]string{"PAYMENT-SIGNATURE": header}, 64*1024,
			)
			presentedJSON, presentedMarshalErr := json.Marshal(presented)
			if presentedErr != nil || presentedMarshalErr != nil || !bytes.Equal(presentedJSON, payloadJSON) {
				t.Fatalf("payee HTTP credential was not preserved: value=%v error=%v", presented, presentedErr)
			}
			presented, presentedErr = adapter.PresentedMCPCredential(
				map[string]any{metadata.Key: metadata.Value}, 64*1024,
			)
			presentedJSON, presentedMarshalErr = json.Marshal(presented)
			if presentedErr != nil || presentedMarshalErr != nil || !bytes.Equal(presentedJSON, payloadJSON) {
				t.Fatalf("payee MCP credential was not preserved: value=%v error=%v", presented, presentedErr)
			}
		})
	}

	crossoverJSON, err := json.Marshal(solana)
	if err != nil {
		t.Fatal(err)
	}
	var crossover map[string]any
	if err := json.Unmarshal(crossoverJSON, &crossover); err != nil {
		t.Fatal(err)
	}
	crossover["accepted"] = evm.(map[string]any)["accepted"]
	if _, err := adapter.PresentedMCPCredential(map[string]any{"x402/payment": crossover}, 64*1024); err == nil {
		t.Fatal("EVM requirements accepted a Solana transaction payload")
	}

	var expanded map[string]any
	if err := json.Unmarshal(mustJSON(t, evm), &expanded); err != nil {
		t.Fatal(err)
	}
	expanded["payload"].(map[string]any)["unexpected"] = true
	if _, err := adapter.PresentedMCPCredential(map[string]any{"x402/payment": expanded}, 64*1024); err == nil {
		t.Fatal("EIP-3009 payload accepted an unknown field")
	}
}

func mustJSON(t *testing.T, value any) []byte {
	t.Helper()
	body, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	return body
}
