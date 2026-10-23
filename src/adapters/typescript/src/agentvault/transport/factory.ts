import type { VaultTransportConfig } from '../config/config.js';
import { GrpcVaultTransport } from './grpc.js';
import { MqttVaultTransport } from './mqtt.js';
import type { OwnedVaultTransport } from './transport.js';

/** Creates a fresh owned provider for each typed configuration. */
export class VaultTransportFactory {
  create(config: VaultTransportConfig): OwnedVaultTransport {
    if (config.provider === 'grpc') return new GrpcVaultTransport(config.grpc);
    if (config.provider === 'mqtt') return new MqttVaultTransport(config.mqtt);
    throw new Error('unsupported AgentVault transport provider');
  }
}
