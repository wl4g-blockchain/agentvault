from ..config import VaultTransportConfig
from .http import HttpVaultTransport


class VaultTransportFactory:
    def create(self, config: VaultTransportConfig) -> HttpVaultTransport:
        return HttpVaultTransport(config.http)
