package transport_test

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net"
	"strings"
	"testing"
	"time"

	"github.com/eclipse/paho.mqtt.golang/packets"

	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/config"
	"github.com/wl4g-blockchain/agentvault/src/adapters/golang/pkg/transport"
)

func TestMqttVaultTransportValidatesBoundaryAndRedactsSecrets(t *testing.T) {
	unsafe := config.MqttConfig{
		Host: "broker.example", Port: 1883, TLS: false, Username: "client", Password: "secret",
		ConnectionPrefix: "worker", VaultClientID: "client-a",
	}
	if _, err := transport.NewMqttVaultTransport(unsafe); err == nil {
		t.Fatal("remote cleartext MQTT must be rejected")
	}

	safe := unsafe
	safe.Host = "127.0.0.1"
	instance, err := transport.NewMqttVaultTransport(safe)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(fmt.Sprint(instance), "secret") || strings.Contains(safe.String(), "secret") {
		t.Fatal("MQTT configuration leaked a secret")
	}
	var async transport.AsyncTransport = instance
	result := <-async.DoAsync(context.Background(), transport.TransportRequest{
		Method: transport.MethodGet,
		Path:   "/unsupported",
	})
	if result.Err == nil {
		t.Fatal("asynchronous MQTT submission must fail unsupported operations")
	}
}

func TestMqttReconnectReplaysOriginalEnvelopeAndPairsReorderedReplies(t *testing.T) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	peer := &mqttTestPeer{listener: listener}
	peerDone := make(chan error, 1)
	go func() { peerDone <- peer.run() }()
	instance, err := transport.NewMqttVaultTransport(config.MqttConfig{
		Host: "127.0.0.1", Port: uint16(listener.Addr().(*net.TCPAddr).Port), Username: "user", Password: "pass",
		ConnectionPrefix: "worker", VaultClientID: "client-a", RequestTimeout: 8 * time.Second,
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = instance.Close() })
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	first := instance.DoAsync(ctx, transport.TransportRequest{Method: transport.MethodPost, Path: "/api/v1/payee/challenges", Body: []byte(`{"runId":"one"}`)})
	second := instance.DoAsync(ctx, transport.TransportRequest{Method: transport.MethodPost, Path: "/api/v1/payee/challenges", Body: []byte(`{"runId":"two"}`)})
	for index, future := range []<-chan transport.TransportResult{first, second} {
		select {
		case result := <-future:
			if result.Err != nil {
				t.Fatal(result.Err)
			}
			expected := []string{`{"runId":"one"}`, `{"runId":"two"}`}[index]
			if string(result.Response.Body) != expected {
				t.Fatalf("reply %d: %s", index, result.Response.Body)
			}
		case <-ctx.Done():
			t.Fatal(ctx.Err())
		}
	}
	if err := <-peerDone; err != nil {
		t.Fatal(err)
	}
	if err := instance.Close(); err != nil {
		t.Fatal(err)
	}
	if err := instance.Close(); err != nil {
		t.Fatal(err)
	}
	if result := <-instance.DoAsync(ctx, transport.TransportRequest{Method: transport.MethodPost, Path: "/api/v1/payee/challenges"}); result.Err == nil {
		t.Fatal("closed transport accepted work")
	}
}

type mqttTestPeer struct {
	listener net.Listener
	clientID string
}

func (peer *mqttTestPeer) commands(connection net.Conn) (map[string][]byte, error) {
	_ = connection.SetDeadline(time.Now().Add(9 * time.Second))
	packet, err := packets.ReadPacket(connection)
	if err != nil {
		return nil, err
	}
	connect, ok := packet.(*packets.ConnectPacket)
	if !ok {
		return nil, fmt.Errorf("expected CONNECT, got %T", packet)
	}
	if peer.clientID != "" && peer.clientID != connect.ClientIdentifier {
		return nil, fmt.Errorf("reconnect replaced client identity")
	}
	peer.clientID = connect.ClientIdentifier
	if err := packets.NewControlPacket(packets.Connack).Write(connection); err != nil {
		return nil, err
	}
	commands := make(map[string][]byte)
	subscribed := false
	for len(commands) < 2 || !subscribed {
		packet, err = packets.ReadPacket(connection)
		if err != nil {
			return nil, err
		}
		switch packet := packet.(type) {
		case *packets.SubscribePacket:
			if len(packet.Qoss) != 1 || packet.Qoss[0] != 1 {
				return nil, fmt.Errorf("subscription is not QoS 1")
			}
			ack := packets.NewControlPacket(packets.Suback).(*packets.SubackPacket)
			ack.MessageID, ack.ReturnCodes = packet.MessageID, []byte{1}
			if err := ack.Write(connection); err != nil {
				return nil, err
			}
			subscribed = true
		case *packets.PublishPacket:
			if packet.Qos != 1 || packet.Retain {
				return nil, fmt.Errorf("command is not non-retained QoS 1")
			}
			var envelope struct {
				RequestSpanID string `json:"requestSpanId"`
				Request       struct {
					Headers map[string]string `json:"headers"`
				} `json:"request"`
			}
			if err := json.Unmarshal(packet.Payload, &envelope); err != nil {
				return nil, err
			}
			if !strings.Contains(envelope.Request.Headers["traceparent"], "-"+envelope.RequestSpanID+"-") {
				return nil, fmt.Errorf("producer span mismatch")
			}
			commands[envelope.RequestSpanID] = append([]byte(nil), packet.Payload...)
			if err := peer.ack(connection, packet.MessageID); err != nil {
				return nil, err
			}
		default:
			return nil, fmt.Errorf("unexpected packet %T", packet)
		}
	}
	return commands, nil
}

func (peer *mqttTestPeer) ack(connection net.Conn, id uint16) error {
	ack := packets.NewControlPacket(packets.Puback).(*packets.PubackPacket)
	ack.MessageID = id
	return ack.Write(connection)
}

func (peer *mqttTestPeer) run() error {
	first, err := peer.listener.Accept()
	if err != nil {
		return err
	}
	original, err := peer.commands(first)
	_ = first.Close() // Lost application result after broker PUBACK.
	if err != nil {
		return err
	}
	second, err := peer.listener.Accept()
	if err != nil {
		return err
	}
	defer second.Close()
	replay, err := peer.commands(second)
	if err != nil {
		return err
	}
	for span, raw := range original {
		if !bytes.Equal(raw, replay[span]) {
			return fmt.Errorf("reconnect changed envelope")
		}
	}
	// Order by runId descending; reply two arrives before reply one.
	responses := make(map[string]*packets.PublishPacket)
	for span, raw := range original {
		var envelope struct {
			Request struct {
				Body string `json:"body"`
			} `json:"request"`
		}
		if err := json.Unmarshal(raw, &envelope); err != nil {
			return err
		}
		response, err := json.Marshal(map[string]any{"type": "payment.challenge_created", "version": 1, "requestSpanId": span, "status": 200, "headers": map[string]string{}, "body": envelope.Request.Body})
		if err != nil {
			return err
		}
		publish := packets.NewControlPacket(packets.Publish).(*packets.PublishPacket)
		publish.TopicName, publish.Payload, publish.Qos = "agentvault/v1/clients/client-a/results/"+span, response, 1
		responses[envelope.Request.Body] = publish
	}
	keys := make([]string, 0, 2)
	for body := range responses {
		keys = append(keys, body)
	}
	if keys[0] < keys[1] {
		keys[0], keys[1] = keys[1], keys[0]
	}
	for index, body := range keys {
		publish := responses[body]
		publish.MessageID = uint16(index + 1)
		if err := publish.Write(second); err != nil {
			return err
		}
		publish.Dup = true
		if err := publish.Write(second); err != nil {
			return err
		}
	}
	acked := map[uint16]bool{}
	for len(acked) < 2 {
		packet, err := packets.ReadPacket(second)
		if err != nil {
			return err
		}
		switch packet := packet.(type) {
		case *packets.PubackPacket:
			acked[packet.MessageID] = true
		case *packets.PublishPacket:
			if err := peer.ack(second, packet.MessageID); err != nil {
				return err
			}
		default:
			return fmt.Errorf("unexpected reply packet %T", packet)
		}
	}
	return nil
}
