import type { Express } from 'express';
import type { FeedHandler } from '../handler/feed.js';

export class FeedRouter {
  constructor(private readonly handler: FeedHandler) {}

  register(application: Express): void {
    application.get('/healthz', this.handler.health);
    application.get('/metrics', this.handler.metrics);
  }
}
