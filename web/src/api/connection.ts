import type { ClientMessage } from './types';
import { getTransport } from './client';
import type { SocketHandle } from './transport';
import { useBoard } from '../store/store';

/**
 * The single WebSocket: auto-reconnect with backoff, keep-alive ping, and it re-sends
 * run subscriptions + the viewed tab after every reconnect (they live per connection).
 */
class Connection {
  private sock: SocketHandle | null = null;
  private wanted = false;
  private attempts = 0;
  private everOpened = false;
  private retryTimer: ReturnType<typeof setTimeout> | undefined;
  private pingTimer: ReturnType<typeof setInterval> | undefined;
  private subs = new Map<string, number>();
  private viewing: string | null = null;

  start() {
    if (this.wanted) return;
    this.wanted = true;
    this.open();
  }

  stop() {
    this.wanted = false;
    clearTimeout(this.retryTimer);
    clearInterval(this.pingTimer);
    this.sock?.close();
    this.sock = null;
    useBoard.getState().setConn('idle');
  }

  private open() {
    useBoard.getState().setConn('connecting');
    let opened = false;
    this.sock = getTransport().openSocket({
      onOpen: () => {
        opened = true;
        this.attempts = 0;
        useBoard.getState().setConn('online');
        if (this.everOpened) this.send({ type: 'resync' });
        this.everOpened = true;
        const ids = [...this.subs.keys()];
        if (ids.length) this.send({ type: 'subscribe', run_ids: ids });
        this.send({ type: 'viewing', tab_id: this.viewing });
        clearInterval(this.pingTimer);
        this.pingTimer = setInterval(() => this.send({ type: 'ping' }), 25_000);
      },
      onMessage: (m) => useBoard.getState().apply(m),
      onClose: () => {
        clearInterval(this.pingTimer);
        this.sock = null;
        if (!this.wanted) return;
        useBoard.getState().setConn(opened || this.everOpened ? 'offline' : 'connecting');
        const delay = Math.min(15_000, 500 * 2 ** this.attempts) * (0.75 + Math.random() * 0.5);
        this.attempts++;
        this.retryTimer = setTimeout(() => this.open(), delay);
      },
    });
  }

  send(m: ClientMessage) {
    this.sock?.send(m);
  }

  /** Ref-counted run subscription; returns the unsubscribe function. */
  subscribe(runId: string): () => void {
    const n = this.subs.get(runId) ?? 0;
    this.subs.set(runId, n + 1);
    if (n === 0) this.send({ type: 'subscribe', run_ids: [runId] });
    return () => {
      const c = (this.subs.get(runId) ?? 1) - 1;
      if (c <= 0) {
        this.subs.delete(runId);
        this.send({ type: 'unsubscribe', run_ids: [runId] });
      } else this.subs.set(runId, c);
    };
  }

  setViewing(tabId: string | null) {
    if (this.viewing === tabId) return;
    this.viewing = tabId;
    this.send({ type: 'viewing', tab_id: tabId });
  }
}

export const connection = new Connection();
