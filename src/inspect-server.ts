import http from 'node:http';
import { WebSocketServer, WebSocket } from 'ws';

export interface InspectServerOptions {
  chromeHostPort: string;
  targetId: string;
  chromeWsUrl: string;
}

export class InspectServer {
  private httpServer: http.Server;
  private wss: WebSocketServer;
  private _port: number = 0;

  constructor(private options: InspectServerOptions) {
    this.httpServer = http.createServer(this.handleHttp.bind(this));
    this.wss = new WebSocketServer({ server: this.httpServer, path: '/ws' });
    this.wss.on('connection', this.handleWsConnection.bind(this));
  }

  get port(): number {
    return this._port;
  }

  async start(): Promise<void> {
    return new Promise((resolve, reject) => {
      this.httpServer.listen(0, '127.0.0.1', () => {
        const addr = this.httpServer.address();
        if (addr && typeof addr !== 'string') {
          this._port = addr.port;
        }
        resolve();
      });
      this.httpServer.on('error', reject);
    });
  }

  stop(): void {
    this.wss.close();
    this.httpServer.close();
  }

  private handleHttp(req: http.IncomingMessage, res: http.ServerResponse): void {
    if (req.url === '/' || req.url === '') {
      const location = `http://${this.options.chromeHostPort}/devtools/devtools_app.html?ws=127.0.0.1:${this._port}/ws`;
      res.writeHead(302, { Location: location, 'Content-Type': 'text/html' });
      res.end(`<html><body>Redirecting to <a href="${location}">${location}</a></body></html>`);
      return;
    }
    res.writeHead(404);
    res.end();
  }

  private handleWsConnection(devtoolsWs: WebSocket): void {
    const chromeWs = new WebSocket(this.options.chromeWsUrl);
    let sessionId = '';

    chromeWs.on('open', () => {
      // Create a dedicated CDP session for this DevTools connection so it gets
      // fresh domain enablements (DOM.enable, CSS.enable, etc.)
      const attachMsg = JSON.stringify({
        id: -1,
        method: 'Target.attachToTarget',
        params: { targetId: this.options.targetId, flatten: true },
      });
      chromeWs.send(attachMsg);
    });

    chromeWs.on('message', (data) => {
      try {
        const msg = JSON.parse(data.toString());

        // Intercept the attachToTarget response to capture the session ID
        if (msg.id === -1 && msg.result?.sessionId) {
          sessionId = msg.result.sessionId;
          return;
        }

        if (!sessionId) return;
        if (msg.sessionId !== sessionId) return;
        delete msg.sessionId;
        devtoolsWs.send(JSON.stringify(msg));
      } catch {}
    });

    devtoolsWs.on('message', (data) => {
      if (!sessionId) return;
      try {
        const msg = JSON.parse(data.toString());
        msg.sessionId = sessionId;
        chromeWs.send(JSON.stringify(msg));
      } catch {}
    });

    chromeWs.on('close', () => devtoolsWs.close());
    devtoolsWs.on('close', () => chromeWs.close());
    chromeWs.on('error', () => devtoolsWs.close());
    devtoolsWs.on('error', () => chromeWs.close());
  }
}
