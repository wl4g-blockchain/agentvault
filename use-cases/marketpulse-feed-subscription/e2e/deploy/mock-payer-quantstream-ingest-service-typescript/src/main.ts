import { IngestionJob, runOnce } from './job/ingestion_job.js';
import { IngestionRouter } from './route/ingestion.js';

export async function main(): Promise<void> {
  const args = process.argv.slice(2);
  if (args.some((argument) => argument !== '--once')) throw new Error('usage: quantstream-typescript [--once]');
  if (args.includes('--once')) {
    console.log(JSON.stringify(await runOnce()));
    return;
  }
  const stop = new AbortController();
  const server = new IngestionRouter().app().listen(8080, '0.0.0.0');
  const ingestion = await IngestionJob.fromEnvironment();
  const tasks = ingestion.start();
  process.once('SIGINT', () => stop.abort());
  process.once('SIGTERM', () => stop.abort());
  stop.signal.addEventListener('abort', () => {
    for (const task of tasks) task.stop();
    server.close();
    void ingestion.close();
  }, { once: true });
  await new Promise<void>((resolve) => stop.signal.addEventListener('abort', () => resolve(), { once: true }));
}

if (process.argv[1] && import.meta.url === new URL(`file://${process.argv[1]}`).href) {
  main().catch((error: unknown) => {
    console.error(error instanceof Error ? error.message : 'service failed');
    process.exitCode = 1;
  });
}
