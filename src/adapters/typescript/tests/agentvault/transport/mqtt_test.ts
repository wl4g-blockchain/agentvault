import assert from 'node:assert/strict';
import { inspect } from 'node:util';
import test from 'node:test';
import { createServer, type Socket } from 'node:net';
import { once } from 'node:events';
import mqttPacket, { type Packet } from 'mqtt-packet';
import { MqttConfig } from '../../../src/agentvault/config/config.js';
import { MqttVaultTransport } from '../../../src/agentvault/transport/mqtt.js';

test('MqttConfig rejects topic injection and redacts credentials', () => {
  assert.throws(() => new MqttConfig('localhost', 1883, false, 'user', 'pass', 'worker', 'a/../b'));
  const config = new MqttConfig('localhost', 1883, false, 'secret-user', 'secret-pass', 'worker', 'client-a');
  assert.doesNotMatch(inspect(config), /secret-user|secret-pass/);
});

test('MqttVaultTransport closes and rejects work before any connection', async () => {
  const transport = new MqttVaultTransport(
    new MqttConfig('localhost', 1883, false, 'user', 'pass', 'worker', 'client-a'),
  );
  await transport.close();
  await transport.close();
  await assert.rejects(
    transport.request('POST', '/api/v1/payee/challenges', { runId: 'run-1' }),
    /closed/,
  );
});

test('MQTT replays exact QoS 1 envelopes after a lost reply and pairs duplicate/out-of-order replies', { timeout: 10000 }, async (context) => {
  const original = new Map<string, Buffer>();
  const sockets = new Set<Socket>();
  let connections = 0;
  let resolvePeer!: () => void;
  let rejectPeer!: (error: unknown) => void;
  const peer = new Promise<void>((resolve, reject) => { resolvePeer = resolve; rejectPeer = reject; });
  // Observe errors immediately, even while the client futures are still waiting.
  void peer.catch(() => {});
  const server = createServer((socket) => {
    sockets.add(socket);
    socket.on('close', () => sockets.delete(socket));
    const generation = ++connections;
    const parser = mqttPacket.parser();
    const received = new Set<string>();
    const acknowledgements = new Set<number>();
    let answered = false;
    const send = (packet: Packet) => socket.write(mqttPacket.generate(packet));
    socket.on('data', (data) => parser.parse(data));
    parser.on('error', rejectPeer);
    parser.on('packet', (packet: Packet) => {
      try {
        if (packet.cmd === 'connect') send({ cmd: 'connack', sessionPresent: false, returnCode: 0 });
        else if (packet.cmd === 'subscribe') {
          assert.equal(packet.subscriptions[0].qos, 1);
          send({ cmd: 'suback', messageId: packet.messageId, granted: [1] });
        } else if (packet.cmd === 'publish') {
          assert.equal(packet.qos, 1);
          assert.equal(packet.retain, false);
          const payload = Buffer.from(packet.payload);
          const envelope = JSON.parse(payload.toString());
          const span: string = envelope.requestSpanId;
          assert.equal(span, envelope.request.headers.traceparent.split('-')[2]);
          send({ cmd: 'puback', messageId: packet.messageId! });
          if (generation === 1) original.set(span, payload);
          else assert.deepEqual(payload, original.get(span));
          received.add(span);
          if (received.size !== 2) return;
          if (generation === 1) { socket.end(); return; }
          if (answered) return;
          answered = true;
          let messageId = 1;
          for (const [id, raw] of [...original].reverse()) {
            const response = JSON.stringify({ type: 'payment.challenge_created', version: 1,
              requestSpanId: id, status: 200, headers: {}, body: JSON.parse(raw.toString()).request.body });
            const responsePacket: Packet = { cmd: 'publish', qos: 1, retain: false, dup: false,
              topic: `agentvault/v1/clients/client-a/results/${id}`, messageId: messageId++, payload: response };
            send(responsePacket);
            send({ ...responsePacket, dup: true });
          }
        } else if (packet.cmd === 'puback') {
          acknowledgements.add(packet.messageId);
          if (acknowledgements.size === 2) resolvePeer();
        }
      } catch (error) { rejectPeer(error); }
    });
  });
  context.after(() => { for (const socket of sockets) socket.destroy(); server.close(); });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const port = (server.address() as { port: number }).port;
  const transport = new MqttVaultTransport(new MqttConfig('127.0.0.1', port, false, 'user', 'pass', 'worker', 'client-a', 8000));
  context.after(() => transport.close());
  const results = await Promise.all([
    transport.request('POST', '/api/v1/payee/challenges', { runId: 'one' }),
    transport.request('POST', '/api/v1/payee/challenges', { runId: 'two' }),
  ]);
  assert.deepEqual(results, [{ runId: 'one' }, { runId: 'two' }]);
  await peer;
  assert.equal(connections, 2);
});

test('MQTT cancellation, timeout and close stay bounded while the broker is unavailable', { timeout: 2000 }, async () => {
  const server = createServer();
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const port = (server.address() as { port: number }).port;
  await new Promise<void>((resolve) => server.close(() => resolve()));
  const transport = new MqttVaultTransport(new MqttConfig('127.0.0.1', port, false, 'user', 'pass', 'worker', 'client-a', 100));
  const controller = new AbortController();
  const cancelled = transport.request('POST', '/api/v1/payee/challenges', { runId: 'cancel' }, controller.signal);
  controller.abort();
  await assert.rejects(cancelled, /cancelled/);
  await assert.rejects(transport.request('POST', '/api/v1/payee/challenges', { runId: 'timeout' }), /timed out/);
  const closed = transport.request('POST', '/api/v1/payee/challenges', { runId: 'close' });
  const rejected = assert.rejects(closed, /closed/);
  await transport.close();
  await rejected;
});
