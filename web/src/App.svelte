<script lang="ts">
  import { app } from './lib/app.svelte';
  import ListPage from './components/ListPage.svelte';
  import Lists from './components/Lists.svelte';
  import StatusBar from './components/StatusBar.svelte';

  const LAST = 'lrtodo:last-list';

  function parse(hash: string): string | null {
    const m = hash.match(/^#\/l\/(.+)$/);
    return m ? decodeURIComponent(m[1]) : null;
  }

  // Al abrir la app sin ruta, ir directo a la última lista usada
  // (en el súper: abrir y ver la lista de compras, sin pasos extra).
  if (!location.hash) {
    const last = localStorage.getItem(LAST);
    if (last) history.replaceState(null, '', `#/l/${encodeURIComponent(last)}`);
  }

  let current = $state(parse(location.hash));
  window.addEventListener('hashchange', () => {
    current = parse(location.hash);
    if (current) localStorage.setItem(LAST, current);
    else localStorage.removeItem(LAST);
  });

  let token = $state('');
</script>

<StatusBar />

<main>
  {#if app.fatal}
    <p class="error">No se pudo iniciar: {app.fatal}</p>
  {:else if !app.ready}
    <p class="muted center">Cargando…</p>
  {:else if current}
    <ListPage id={current} />
  {:else}
    <Lists />
  {/if}
</main>

{#if app.needToken}
  <div class="modal">
    <form
      class="card"
      onsubmit={(e) => {
        e.preventDefault();
        app.setToken(token);
      }}
    >
      <h2>Token del servidor</h2>
      <p class="muted">El servidor lrtodo pide un token (opción <code>token</code> del add-on o <code>serve.token</code>).</p>
      <input type="password" bind:value={token} placeholder="token" autocomplete="current-password" />
      <button type="submit">Guardar</button>
    </form>
  </div>
{/if}

{#if app.status.notices.length}
  <div class="toast" role="status">
    {#each app.status.notices as n (n.ms + n.message)}
      <div>{n.list}: {n.message}</div>
    {/each}
    <button class="link" onclick={() => app.clearNotices()}>Entendido</button>
  </div>
{/if}
