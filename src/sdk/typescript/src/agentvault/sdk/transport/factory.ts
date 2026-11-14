import type { VaultTransportConfig } from "../config/config.js";
import { HttpVaultTransport } from "./http.js";
export class VaultTransportFactory {
  create(config: VaultTransportConfig): HttpVaultTransport {
    return new HttpVaultTransport(config.http);
  }
}
