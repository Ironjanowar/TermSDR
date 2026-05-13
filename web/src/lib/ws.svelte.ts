// WebSocket client with auto-reconnect — Svelte 5 reactive

import {
  parseBinaryFrame,
  parseServerMessage,
  encodeCommand,
  type Command,
  type SpectrumFrame,
  type AudioFrame,
  type StateMessage,
  type ErrorMessage,
} from './protocol';
import { sdrState } from './state.svelte';

/** Callback fired on reconnect so components can clear stale display data. */
export type ReconnectCallback = () => void;

export type ConnectionStatus = 'connecting' | 'connected' | 'disconnected';

class WsClient {
  status = $state<ConnectionStatus>('disconnected');

  private ws: WebSocket | null = null;
  private reconnectDelay = 1000;
  private maxReconnectDelay = 30000;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private intentionalClose = false;

  // Callbacks for binary frames (components wire these up)
  onSpectrum: ((frame: SpectrumFrame) => void) | null = null;
  onAudio: ((frame: AudioFrame) => void) | null = null;
  onError: ((err: ErrorMessage) => void) | null = null;
  onReconnect: ReconnectCallback | null = null;

  /** Number of reconnection attempts since last successful connection. */
  reconnectAttempts = $state(0);

  /** Connect to the server WebSocket. */
  connect(url?: string) {
    if (this.ws) return;

    const wsUrl = url ?? `ws://${window.location.host}/ws`;
    this.status = 'connecting';
    this.intentionalClose = false;

    const ws = new WebSocket(wsUrl);
    ws.binaryType = 'arraybuffer';

    ws.onopen = () => {
      const wasReconnect = this.reconnectAttempts > 0;
      this.status = 'connected';
      this.reconnectDelay = 1000; // reset backoff
      this.reconnectAttempts = 0;

      if (wasReconnect) {
        // Notify components to clear stale spectrum/waterfall data
        this.onReconnect?.();
      }
    };

    ws.onmessage = (ev: MessageEvent) => {
      if (ev.data instanceof ArrayBuffer) {
        const frame = parseBinaryFrame(ev.data);
        if (!frame) return;
        if (frame.type === 'spectrum') {
          this.onSpectrum?.(frame.frame);
        } else if (frame.type === 'audio') {
          this.onAudio?.(frame.frame);
        }
      } else if (typeof ev.data === 'string') {
        const msg = parseServerMessage(ev.data);
        if (!msg) return;
        if (msg.type === 'state') {
          sdrState.updateFromServer(msg as StateMessage);
        } else if (msg.type === 'error') {
          this.onError?.(msg as ErrorMessage);
        }
      }
    };

    ws.onclose = () => {
      this.ws = null;
      this.status = 'disconnected';
      if (!this.intentionalClose) {
        this.scheduleReconnect();
      }
    };

    ws.onerror = () => {
      // onclose will fire after this
    };

    this.ws = ws;
  }

  /** Send a command to the server. */
  sendCommand(cmd: Command) {
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(encodeCommand(cmd));
    }
  }

  /** Disconnect and stop reconnecting. */
  disconnect() {
    this.intentionalClose = true;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.ws?.close();
    this.ws = null;
    this.status = 'disconnected';
  }

  private scheduleReconnect() {
    this.reconnectAttempts++;
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.connect();
    }, this.reconnectDelay);

    // Exponential backoff
    this.reconnectDelay = Math.min(this.reconnectDelay * 2, this.maxReconnectDelay);
  }
}

export const wsClient = new WsClient();
