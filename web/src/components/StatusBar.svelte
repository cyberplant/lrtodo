<script lang="ts">
  import { ago, app } from '../lib/app.svelte';

  const s = $derived(app.status);
  const offline = $derived(!!s.last_error);

  // Si la última sync falló, se sigue mostrando "sin conexión" mientras se
  // reintenta (el punto titila), para que quede claro qué tan viejos son los datos.
  const text = $derived.by(() => {
    if (offline) return `Sin conexión · datos de ${ago(s.last_sync_ms, app.now)}`;
    if (app.syncing) return 'Sincronizando…';
    if (s.pending) return `${s.pending} cambio${s.pending > 1 ? 's' : ''} sin enviar`;
    return 'Al día';
  });
</script>

<header class="status" class:offline class:syncing={app.syncing}>
  <span class="dot" aria-hidden="true"></span>
  <span data-testid="sync-status">{text}</span>
  {#if offline && s.pending}
    <span class="pill">{s.pending} pendiente{s.pending > 1 ? 's' : ''}</span>
  {/if}
  <button class="link retry" onclick={() => app.sync()} disabled={app.syncing} title={s.last_error ?? ''}>
    {app.syncing ? '' : 'Sincronizar'}
  </button>
</header>
