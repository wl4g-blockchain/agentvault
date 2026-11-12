import { isIP } from 'node:net';
import { inspect } from 'node:util';
const IDENTIFIER = /^[A-Za-z0-9_.-]{1,128}$/;

function loopback(host: string): boolean {
  if (host.toLowerCase() === 'localhost') return true;
  if (isIP(host) === 4) return host.startsWith('127.');
  return host === '::1' || host.toLowerCase() === '0:0:0:0:0:0:0:1';
}

export interface PayerConfig {
  scenarioId: string;
  approvalTimeoutMs?: number;
  approvalPollIntervalMs?: number;
  deliveryRetries?: number;
  maxChallengeBytes?: number;
  protocols?: readonly string[];
}

export interface PayeeConfig {
  maxBodyBytes?: number;
  protocols?: readonly string[];
}

export class GrpcConfig {
  constructor(
    public readonly endpoint: string,
    public readonly tls = true,
    public readonly authguardBearer?: string,
    public readonly timeoutMs = 30_000,
    public readonly maxMessageBytes = 2 * 1024 * 1024,
  ) {
    const match = /^\[?([^\]]+)\]?:([0-9]+)$/.exec(endpoint);
    if (!match || Number(match[2]) <= 0 || Number(match[2]) > 65_535 || timeoutMs <= 0 ||
        maxMessageBytes <= 0 || (!tls && !loopback(match[1]!)) ||
        (authguardBearer !== undefined &&
          (!authguardBearer.trim() || /[\x00-\x1f\x7f]/.test(authguardBearer)))) {
      throw new Error('invalid or unsafe AgentVault gRPC configuration');
    }
  }

  [inspect.custom](): string {
    return `GrpcConfig { endpoint: ${JSON.stringify(this.endpoint)}, tls: ${this.tls}, authguardBearer: ${this.authguardBearer ? '<redacted>' : 'disabled'} }`;
  }
}

export class MqttConfig {
  constructor(
    public readonly host: string,
    public readonly port: number,
    public readonly tls: boolean,
    public readonly username: string,
    public readonly password: string,
    public readonly connectionPrefix: string,
    public readonly vaultClientId: string,
    public readonly timeoutMs = 30_000,
    public readonly maxResponseBytes = 2 * 1024 * 1024,
  ) {
    if (!host.trim() || !Number.isInteger(port) || port <= 0 || port > 65_535 || timeoutMs <= 0 ||
        maxResponseBytes <= 0 || !IDENTIFIER.test(username) || !password ||
        !IDENTIFIER.test(connectionPrefix) || !IDENTIFIER.test(vaultClientId) ||
        (!tls && !loopback(host))) {
      throw new Error('invalid or unsafe AgentVault MQTT configuration');
    }
  }

  [inspect.custom](): string {
    return `MqttConfig { host: ${JSON.stringify(this.host)}, port: ${this.port}, tls: ${this.tls}, credentials: <redacted>, vaultClientId: ${JSON.stringify(this.vaultClientId)} }`;
  }
}

export type VaultTransportConfig =
  | { provider: 'grpc'; grpc: GrpcConfig; mqtt?: never }
  | { provider: 'mqtt'; mqtt: MqttConfig; grpc?: never };
