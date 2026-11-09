import type { ClientMessage, ServerMessage } from './types';

export class ApiFailure extends Error {
  readonly status: number;
  readonly code: string;
  constructor(status: number, message: string, code = 'error') {
    super(message);
    this.status = status;
    this.code = code;
  }
}

export interface SocketHandlers {
  onOpen(): void;
  onMessage(msg: ServerMessage): void;
  onClose(): void;
}

export interface SocketHandle {
  send(msg: ClientMessage): void;
  close(): void;
}

/** Real server or the in-browser mock: same REST + WS semantics. */
export interface Transport {
  readonly mock: boolean;
  request<T>(method: string, path: string, body?: unknown): Promise<T>;
  /** Raw body upload (audio for speech-to-text). */
  upload<T>(path: string, body: Blob): Promise<T>;
  openSocket(h: SocketHandlers): SocketHandle;
}

// The API always lives at the origin root (/api), whatever page the SPA is on (e.g. /invite/<token>).
const apiUrl = (path: string) => new URL(path, location.origin).toString();

export const httpTransport: Transport = {
  mock: false,
  async request<T>(method: string, path: string, body?: unknown): Promise<T> {
    let res: Response;
    try {
      res = await fetch(apiUrl(path), {
        method,
        credentials: 'same-origin',
        headers: body === undefined ? undefined : { 'Content-Type': 'application/json' },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
    } catch {
      throw new ApiFailure(0, 'Cannot reach the Boba server', 'network');
    }
    if (res.status === 204) return undefined as T;
    const text = await res.text();
    let json: unknown = undefined;
    try {
      json = text ? JSON.parse(text) : undefined;
    } catch {
      /* non-JSON body */
    }
    if (!res.ok) {
      const e = json as { error?: string; code?: string } | undefined;
      throw new ApiFailure(res.status, e?.error ?? (text || res.statusText), e?.code);
    }
    return json as T;
  },
  async upload<T>(path: string, body: Blob): Promise<T> {
    const res = await fetch(apiUrl(path), { method: 'POST', credentials: 'same-origin', headers: { 'Content-Type': body.type || 'application/octet-stream' }, body }).catch(() => {
      throw new ApiFailure(0, 'Cannot reach the Boba server', 'network');
    });
    const text = await res.text();
    const json = (() => {
      try {
        return text ? JSON.parse(text) : undefined;
      } catch {
        return undefined;
      }
    })();
    if (!res.ok) throw new ApiFailure(res.status, json?.error ?? (text || res.statusText), json?.code);
    return json as T;
  },
  openSocket(h: SocketHandlers): SocketHandle {
    const url = new URL('/api/ws', location.origin);
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
    const ws = new WebSocket(url);
    ws.onopen = () => h.onOpen();
    ws.onmessage = (ev) => {
      try {
        h.onMessage(JSON.parse(String(ev.data)) as ServerMessage);
      } catch (err) {
        console.warn('[boba] bad ws message', err);
      }
    };
    ws.onclose = () => h.onClose();
    return {
      send: (m) => {
        if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(m));
      },
      close: () => ws.close(),
    };
  },
};
