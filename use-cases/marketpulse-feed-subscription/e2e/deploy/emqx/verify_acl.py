#!/usr/bin/env python3
"""Tiny dependency-free MQTT 3.1.1 probe for password auth and tenant ACLs."""

from __future__ import annotations

import os
import socket
import struct
import time


HOST = os.environ.get("MQTT_HOST", "127.0.0.1")
PORT = int(os.environ.get("MQTT_PORT", "28883"))
WORKLOAD_PASSWORD = os.environ["MQTT_WORKLOAD_PASSWORD"]
APPROVER_PASSWORD = os.environ["MQTT_APPROVER_PASSWORD"]
VAULT_PASSWORD = os.environ["MQTT_VAULT_PASSWORD"]
PREFIX = "agentvault/v1/clients"
OWN = f"{PREFIX}/client-quantstream/commands/authorize"
CROSS = f"{PREFIX}/client-forbidden/commands/authorize"
DECISION = f"{PREFIX}/client-quantstream/approvers/principal-quantstream-admin/decisions"
RESULT = f"{PREFIX}/client-quantstream/results/acl-roundtrip"
COMMAND_FILTERS = [
    f"{PREFIX}/+/commands/authorize",
    f"{PREFIX}/+/commands/outbound-response",
    f"{PREFIX}/+/commands/resolve-offer",
    f"{PREFIX}/+/commands/create-challenge",
    f"{PREFIX}/+/commands/verify-payment",
    f"{PREFIX}/+/commands/settle-payment",
]


def variable_length(value: int) -> bytes:
    encoded = bytearray()
    while True:
        digit = value % 128
        value //= 128
        if value:
            digit |= 0x80
        encoded.append(digit)
        if not value:
            return bytes(encoded)


def utf8(value: str) -> bytes:
    raw = value.encode("utf-8")
    return struct.pack("!H", len(raw)) + raw


def packet(kind: int, flags: int, payload: bytes) -> bytes:
    return bytes([(kind << 4) | flags]) + variable_length(len(payload)) + payload


def read_packet(connection: socket.socket, timeout: float = 2.0) -> tuple[int, bytes]:
    connection.settimeout(timeout)
    first = connection.recv(1)
    if not first:
        raise RuntimeError("broker closed the MQTT connection")
    multiplier = 1
    length = 0
    while True:
        digit = connection.recv(1)[0]
        length += (digit & 127) * multiplier
        if digit & 128 == 0:
            break
        multiplier *= 128
    body = bytearray()
    while len(body) < length:
        body.extend(connection.recv(length - len(body)))
    return first[0] >> 4, bytes(body)


def connect(client_id: str, username: str, password: str, accepted: bool = True) -> socket.socket:
    connection = socket.create_connection((HOST, PORT), timeout=3)
    variable = utf8("MQTT") + bytes([4, 0xC2]) + struct.pack("!H", 15)
    payload = utf8(client_id) + utf8(username) + utf8(password)
    connection.sendall(packet(1, 0, variable + payload))
    kind, body = read_packet(connection)
    if kind != 2 or len(body) != 2:
        raise RuntimeError("broker returned a malformed CONNACK")
    if accepted and body[1] != 0:
        raise RuntimeError(f"valid MQTT credentials were rejected ({body[1]})")
    if not accepted and body[1] == 0:
        raise RuntimeError("invalid MQTT password was accepted")
    if not accepted:
        connection.close()
    return connection


def subscribe(connection: socket.socket, topic: str, packet_id: int) -> None:
    payload = struct.pack("!H", packet_id) + utf8(topic) + bytes([1])
    connection.sendall(packet(8, 2, payload))
    kind, body = read_packet(connection)
    if kind != 9 or body[:2] != struct.pack("!H", packet_id) or body[2:] != b"\x01":
        raise RuntimeError("authorized MQTT subscription failed")


def publish(connection: socket.socket, topic: str, message: bytes, packet_id: int) -> None:
    payload = utf8(topic) + struct.pack("!H", packet_id) + message
    connection.sendall(packet(3, 2, payload))
    kind, body = read_packet(connection)
    if kind != 4 or body != struct.pack("!H", packet_id):
        raise RuntimeError("QoS1 publish did not receive PUBACK")


def receive_publish(connection: socket.socket, expected: bytes, timeout: float) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            kind, body = read_packet(connection, deadline - time.monotonic())
        except (TimeoutError, socket.timeout):
            return False
        if kind != 3:
            continue
        topic_length = struct.unpack("!H", body[:2])[0]
        offset = 2 + topic_length
        packet_id = body[offset : offset + 2]
        payload = body[offset + 2 :]
        connection.sendall(packet(4, 0, packet_id))
        if payload == expected:
            return True
    return False


connect("bad-password-probe", "quantstream-workload", "definitely-wrong", accepted=False)
vault = connect("vault-acl-probe", "agentvault-vault", VAULT_PASSWORD)
for packet_id, topic in enumerate(COMMAND_FILTERS, start=1):
    subscribe(vault, topic, packet_id)
subscribe(vault, f"{PREFIX}/+/approvers/+/decisions", 20)
workload = connect("workload-acl-probe", "quantstream-workload", WORKLOAD_PASSWORD)
subscribe(workload, f"{PREFIX}/client-quantstream/results/+", 1)
own_marker = b"allowed-own-client"
publish(workload, OWN, own_marker, 3)
if not receive_publish(vault, own_marker, 2.0):
    raise RuntimeError("authorized client-scoped publish was not delivered")
cross_marker = b"must-not-cross-client"
publish(workload, CROSS, cross_marker, 4)
if receive_publish(vault, cross_marker, 1.0):
    raise RuntimeError("cross-client MQTT publish bypassed the ACL")
forged_decision_marker = b"workload-must-not-approve"
publish(workload, DECISION, forged_decision_marker, 5)
if receive_publish(vault, forged_decision_marker, 1.0):
    raise RuntimeError("workload MQTT identity published an approval decision")
approver = connect("approver-acl-probe", "quantstream-approver", APPROVER_PASSWORD)
decision_marker = b"allowed-registered-approver"
publish(approver, DECISION, decision_marker, 6)
if not receive_publish(vault, decision_marker, 2.0):
    raise RuntimeError("registered approver decision was not delivered")
result_marker = b"allowed-vault-result"
publish(vault, RESULT, result_marker, 30)
if not receive_publish(workload, result_marker, 2.0):
    raise RuntimeError("AgentVault result was not delivered to its client workload")
workload.close()
approver.close()
vault.close()
print("MQTT password authentication, client isolation, and approver ACL checks passed")
