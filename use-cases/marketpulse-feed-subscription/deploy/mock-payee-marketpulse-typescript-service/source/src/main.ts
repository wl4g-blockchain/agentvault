import { MarketPulseServer } from './server.js';

export function main(): void {
  MarketPulseServer.fromEnvironment().run();
}

if (process.argv[1] && new URL(import.meta.url).pathname === process.argv[1]) main();
