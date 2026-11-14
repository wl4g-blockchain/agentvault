from __future__ import annotations

from ..config import VaultTransportConfig
from .grpc import GrpcVaultTransport
from .mqtt import MqttVaultTransport
from .transport import VaultTransport


class VaultTransportFactory:
    """Creates a new owned provider for each typed configuration."""

    def create(self, config: VaultTransportConfig) -> VaultTransport:
        if config.provider == "grpc" and config.grpc is not None:
            return GrpcVaultTransport(config.grpc)
        if config.provider == "mqtt" and config.mqtt is not None:
            return MqttVaultTransport(config.mqtt)
        raise ValueError("unsupported AgentVault transport provider")
