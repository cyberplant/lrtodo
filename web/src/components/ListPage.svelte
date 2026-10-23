<script lang="ts">
  import { ago, app } from '../lib/app.svelte';
  import TaskRow from './TaskRow.svelte';

  let { id }: { id: string } = $props();

  const list = $derived(app.list(id));
  const tasks = $derived(app.tasks(id));
  const open = $derived(tasks.filter((t) => !t.done));
  const done = $derived(tasks.filter((t) => t.done));

  let title = $state('');
  let showDone = $state(false);
  let input: HTMLInputElement | undefined = $state();

  async function add(e: Event) {
    e.preventDefault();
    const t = title.trim();
    if (!t) return;
    title = '';
    await app.add(id, { title: t });
    input?.focus();
  }

  async function clearDone() {
    for (const t of done) await app.remove(id, t.id);
  }
</script>

<nav class="top">
  <a href="#/" class="back" aria-label="Volver">‹</a>
  <h1>{list?.meta.name ?? id}</h1>
  {#if list?.meta.kind === 'home_assistant'}<span class="badge">Home Assistant</span>{/if}
</nav>

{#if !list}
  <p class="muted">
    {app.status.last_sync_ms ? 'Esta lista no existe.' : 'Todavía no hay datos de esta lista; esperando al servidor…'}
  </p>
{:else}
  {#if list.error}
    <p class="banner warn-banner">
      {list.meta.kind === 'home_assistant'
        ? 'Home Assistant no responde; los cambios quedan guardados y se envían cuando vuelva.'
        : `Problema con el archivo: ${list.error}`}
    </p>
  {/if}
  {#if list.pending_upstream}
    <p class="banner">{list.pending_upstream} cambio(s) esperando a Home Assistant</p>
  {/if}

  <form class="add" onsubmit={add}>
    <input bind:this={input} bind:value={title} placeholder="Agregar…" enterkeyhint="done" data-testid="add-input" />
    <button type="submit" disabled={!title.trim()} aria-label="Agregar">+</button>
  </form>

  <ul class="tasks" data-testid="open-tasks">
    {#each open as t (t.id)}
      <TaskRow task={t} list={list} />
    {/each}
  </ul>
  {#if !open.length}
    <p class="muted center">Nada pendiente 🎉</p>
  {/if}

  {#if done.length}
    <div class="done-header">
      <button class="link" onclick={() => (showDone = !showDone)}>
        {showDone ? '▾' : '▸'} Completadas ({done.length})
      </button>
      {#if showDone}
        <button class="link danger" onclick={clearDone}>Borrar completadas</button>
      {/if}
    </div>
    {#if showDone}
      <ul class="tasks done">
        {#each done as t (t.id)}
          <TaskRow task={t} list={list} />
        {/each}
      </ul>
    {/if}
  {/if}

  <p class="muted small center">Actualizada {ago(list.synced_ms, app.now)}</p>
{/if}
