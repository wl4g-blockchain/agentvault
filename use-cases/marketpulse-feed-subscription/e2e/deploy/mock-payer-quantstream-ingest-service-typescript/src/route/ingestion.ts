import express, { type Express } from 'express';
import { IngestionHandler } from '../handler/ingestion.js';

/** IngestionRouter owns only Express route composition. */
export class IngestionRouter {
  constructor(private readonly handler = new IngestionHandler()) {}

  app(): Express {
    const app = express();
    app.disable('x-powered-by');
    app.get('/healthz', this.handler.health);
    app.get('/metrics', this.handler.metrics);
    return app;
  }
}
