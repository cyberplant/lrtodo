// Estado de la app: réplica offline-first (core en WASM) persistida en
// IndexedDB + motor de sincronización en segundo plano.
//
// Regla de oro: la UI siempre lee y escribe la réplica local. La red sólo se
// usa en segundo plano; nada espera al servidor.

import init, { WasmReplica } from './wasm/lrtodo_wasm.js';
import { kvGet, kvSet, persistStorage } from './db';

export type Caps = { due: boolean; notes: boolean; tags: boolean; priority: boolean; extra: boolean };

export type ListMeta = { id: string; name: string; kind: 'local' | 'home_assistant'; caps: Caps };

export type ListView = {
  meta: ListMeta;
  rev: string;
  pending_upstream: number;
  error?: string;
  upstream_ms?: number;
  open: number;
  total: number;
  pending: number;
  synced_ms?: number;
};

export type Task = {
  id: string;
  title: string;
  done?: boolean;
  due?: string;
  notes?: string;
  tags?: string[];
  priority?: number;
  created?: string;
  updated?: string;
  completed?: string;
  pending: boolean;
};

export type Field = 'title' | 'done' | 'due' | 'notes' | 'tags' | 'priority';

type Notice = { ms: number; list: string; message: string };
type Status = { device: string; pending: number; last_sync_ms?: number; last_error?: string; notices: Notice[] };

const LOCK = 'lrtodo-replica';
const SYNC_TIMEOUT_MS = 10_000;

/** Ejecuta `f` con exclusión mutua entre pestañas (si el navegador lo soporta). */
function withLock<T>(f: () => Promise<T>): Promise<T> {
  return navigator.locks ? navigator.locks.request(LOCK, f) : f();
}

function seed(): number {
  const [a, b] = crypto.getRandomValues(new Uint32Array(2));
  return a * 2 ** 21 + (b & 0x1fffff);
}

class AppState {
  ready = $state(false);
  lists = $state<ListView[]>([]);
  status = $state<Status>({ device: '', pending: 0, notices: [] });
  syncing = $state(false);
  needToken = $state(false);
  fatal = $state<string | null>(null);
  /** Reloj para los textos "hace X min". */
  now = $state(Date.now());
  /** Se incrementa en cada cambio de la réplica (para recalcular vistas). */
  version = $state(0);

  private replica: WasmReplica | null = null;
  private token: string | null = null;
  private channel = new BroadcastChannel('lrtodo');
  private timer: ReturnType<typeof setTimeout> | undefined;
  private backoff = 2000;
  private again = false;
  private events: EventSource | null = null;

  async start() {
    try {
      await init();
      this.token = (await kvGet<string>('token')) ?? null;
      await withLock(async () => this.swap(await this.load()));
    } catch (e) {
      this.fatal = String(e);
      return;
    }
    this.ready = true;
    persistStorage();

    this.channel.onmessage = () => withLock(async () => this.swap(await this.load()));
    window.addEventListener('online', () => this.syncSoon(100));
    document.addEventListener('visibilitychange', () => {
      if (document.visibilityState === 'visible') this.syncSoon(100);
    });
    setInterval(() => (this.now = Date.now()), 15_000);
    setInterval(() => document.visibilityState === 'visible' && this.syncSoon(0), 60_000);

    this.sync();
    this.connectEvents();
  }

  private async load(): Promise<WasmReplica> {
    const data = await kvGet<string>('replica');
    if (data) return WasmReplica.load(data, seed());
    const device = 'web-' + Math.random().toString(36).slice(2, 8);
    return new WasmReplica(device, seed());
  }

  private swap(r: WasmReplica) {
    this.replica?.free();
    this.replica = r;
    this.lists = JSON.parse(r.lists());
    this.status = JSON.parse(r.status());
    this.version++;
  }

  /** Carga la última versión, aplica `f`, persiste. Exclusivo entre pestañas. */
  private async withReplica<T>(f: (r: WasmReplica) => T): Promise<T> {
    const out = await withLock(async () => {
      const r = await this.load();
      let out: T;
      try {
        out = f(r);
      } catch (e) {
        r.free();
        throw e;
      }
      await kvSet('replica', r.toJson());
      this.swap(r);
      return out;
    });
    this.channel.postMessage('changed');
    return out;
  }

  // ------------------------------------------------------------ lectura

  list(id: string): ListView | undefined {
    return this.lists.find((l) => l.meta.id === id);
  }

  tasks(list: string): Task[] {
    void this.version;
    if (!this.replica) return [];
    try {
      return JSON.parse(this.replica.view(list));
    } catch {
      return [];
    }
  }

  // ------------------------------------------------------------ cambios

  private async mutate<T>(f: (r: WasmReplica) => T): Promise<T> {
    const out = await this.withReplica(f);
    this.syncSoon(300);
    return out;
  }

  add(list: string, task: Partial<Task>) {
    return this.mutate((r) => r.add(list, JSON.stringify(task), Date.now()));
  }

  set(list: string, id: string, field: Field, value: unknown) {
    return this.mutate((r) => r.set(list, id, JSON.stringify({ field, value }), Date.now()));
  }

  remove(list: string, id: string) {
    return this.mutate((r) => r.remove(list, id, Date.now()));
  }

  move(from: string, id: string, to: string) {
    return this.mutate((r) => r.moveTask(from, id, to, Date.now()));
  }

  clearNotices() {
    return this.withReplica((r) => r.clearNotices());
  }

  async setToken(token: string) {
    this.token = token.trim() || null;
    await kvSet('token', this.token ?? '');
    this.needToken = false;
    this.connectEvents();
    this.sync();
  }

  async createList(id: string, name: string) {
    const resp = await this.fetch('api/lists', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ id, name: name || undefined }),
    });
    if (!resp.ok) throw new Error(await resp.text());
    await this.sync();
  }

  // ------------------------------------------------------------ sync

  private fetch(path: string, init: RequestInit = {}) {
    const headers = new Headers(init.headers);
    if (this.token) headers.set('authorization', `Bearer ${this.token}`);
    return fetch(path, { ...init, headers });
  }

  syncSoon(ms: number) {
    clearTimeout(this.timer);
    this.timer = setTimeout(() => this.sync(), ms);
  }

  async sync() {
    if (!this.replica) return;
    if (this.syncing) {
      this.again = true;
      return;
    }
    clearTimeout(this.timer);
    this.syncing = true;
    try {
      const body = this.replica.syncRequest();
      const ctrl = new AbortController();
      const t = setTimeout(() => ctrl.abort(), SYNC_TIMEOUT_MS);
      let resp: Response;
      try {
        resp = await this.fetch('api/sync', {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body,
          signal: ctrl.signal,
        });
      } finally {
        clearTimeout(t);
      }
      if (resp.status === 401) {
        this.needToken = true;
        throw new Error('el servidor pide un token');
      }
      if (!resp.ok) throw new Error(`el servidor respondió ${resp.status}`);
      const text = await resp.text();
      await this.withReplica((r) => r.applyResponse(text, Date.now()));
      this.backoff = 2000;
    } catch (e) {
      const err = e as Error;
      const msg =
        err.name === 'AbortError'
          ? 'el servidor tardó demasiado'
          : !navigator.onLine
            ? 'sin conexión'
            : err.message || String(e);
      await this.withReplica((r) => r.syncFailed(msg)).catch(() => {});
      if (!this.needToken) {
        this.syncSoon(this.backoff);
        this.backoff = Math.min(this.backoff * 2, 60_000);
      }
    } finally {
      this.syncing = false;
      if (this.again) {
        this.again = false;
        this.syncSoon(50);
      }
    }
  }

  private connectEvents() {
    this.events?.close();
    const url = 'api/events' + (this.token ? `?token=${encodeURIComponent(this.token)}` : '');
    this.events = new EventSource(url);
    this.events.addEventListener('changed', () => this.syncSoon(250));
    // Al reconectar puede haber cambios que no vimos.
    this.events.onopen = () => this.syncSoon(250);
  }
}

export const app = new AppState();

export function ago(ms: number | undefined, now: number): string {
  if (!ms) return 'nunca';
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 45) return 'recién';
  if (s < 3600) return `hace ${Math.round(s / 60)} min`;
  if (s < 86400) return `hace ${Math.round(s / 3600)} h`;
  return `hace ${Math.round(s / 86400)} d`;
}
