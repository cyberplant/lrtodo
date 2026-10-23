<script lang="ts">
  import { app } from '../lib/app.svelte';

  let creating = $state(false);
  let name = $state('');
  let error = $state('');

  function slug(s: string) {
    return s
      .normalize('NFD')
      .replace(/[\u0300-\u036f]/g, '')
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, '-')
      .replace(/^-+|-+$/g, '');
  }

  async function create(e: Event) {
    e.preventDefault();
    error = '';
    try {
      await app.createList(slug(name), name.trim());
      creating = false;
      name = '';
    } catch (err) {
      error = navigator.onLine ? String((err as Error).message) : 'Crear listas requiere conexión con el servidor';
    }
  }
</script>

<h1>Listas</h1>

{#if !app.lists.length}
  <p class="muted">
    {app.status.last_sync_ms ? 'No hay listas todavía.' : 'Esperando la primera sincronización con el servidor…'}
  </p>
{/if}

<ul class="lists">
  {#each app.lists as l (l.meta.id)}
    <li>
      <a href={`#/l/${encodeURIComponent(l.meta.id)}`} data-testid="list-link">
        <span class="name">{l.meta.name}</span>
        {#if l.meta.kind === 'home_assistant'}<span class="badge">HA</span>{/if}
        {#if l.pending}<span class="pending-dot" title="cambios sin enviar"></span>{/if}
        {#if l.error}<span class="warn" title={l.error}>!</span>{/if}
        <span class="count">{l.open}</span>
      </a>
    </li>
  {/each}
</ul>

{#if creating}
  <form class="row" onsubmit={create}>
    <!-- svelte-ignore a11y_autofocus -->
    <input bind:value={name} placeholder="Nombre de la lista" autofocus />
    <button type="submit" disabled={!slug(name)}>Crear</button>
    <button type="button" class="link" onclick={() => (creating = false)}>Cancelar</button>
  </form>
  {#if error}<p class="error">{error}</p>{/if}
{:else}
  <button class="link" onclick={() => (creating = true)}>+ Nueva lista</button>
{/if}
