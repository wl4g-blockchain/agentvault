import type { Request, Response } from 'express';

/** IngestionHandler maps HTTP requests to the ingestion application boundary. */
export class IngestionHandler {
  readonly health = (_request: Request, response: Response): void => {
    response.json({ status: 'ready', service: 'quantstream-typescript' });
  };

  readonly metrics = (_request: Request, response: Response): void => {
    response.type('text/plain; version=0.0.4').send(
      '# HELP quantstream_up QuantStream process readiness.\n' +
      '# TYPE quantstream_up gauge\nquantstream_up 1\n',
    );
  };
}
