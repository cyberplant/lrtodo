<script lang="ts">
  import { app, type ListView, type Task } from '../lib/app.svelte';

  let { task, list }: { task: Task; list: ListView } = $props();

  let editing = $state(false);
  let title = $state('');
  let due = $state('');
  let notes = $state('');
  let tags = $state('');
  let moveTo = $state('');

  const caps = $derived(list.meta.caps);
  const others = $derived(app.lists.filter((l) => l.meta.id !== list.meta.id));

  function startEdit() {
    title = task.title;
    due = task.due ?? '';
    notes = task.notes ?? '';
    tags = (task.tags ?? []).join(', ');
    moveTo = '';
    editing = true;
  }

  async function save(e: Event) {
    e.preventDefault();
    const id = list.meta.id;
    if (title.trim() && title.trim() !== task.title) await app.set(id, task.id, 'title', title.trim());
    if (caps.due && (due || null) !== (task.due ?? null)) await app.set(id, task.id, 'due', due || null);
    if (caps.notes && (notes.trim() || null) !== (task.notes ?? null)) await app.set(id, task.id, 'notes', notes.trim() || null);
    if (caps.tags) {
      const t = tags.split(',').map((s) => s.trim()).filter(Boolean);
      if (t.join(',') !== (task.tags ?? []).join(',')) await app.set(id, task.id, 'tags', t);
    }
    if (moveTo) await app.move(id, task.id, moveTo);
    editing = false;
  }
</script>

<li class="task" class:done={task.done} class:pending={task.pending} data-testid="task">
  <label class="check">
    <input
      type="checkbox"
      checked={!!task.done}
      onchange={(e) => app.set(list.meta.id, task.id, 'done', (e.currentTarget as HTMLInputElement).checked)}
    />
  </label>
  <button class="title" onclick={startEdit}>
    <span>{task.title}</span>
    {#if task.due || task.tags?.length || task.notes}
      <span class="meta">
        {#if task.due}<span class="due">{task.due.slice(0, 10)}</span>{/if}
        {#each task.tags ?? [] as tag (tag)}<span class="tag">#{tag}</span>{/each}
        {#if task.notes}<span class="notes">{task.notes.split('\n')[0]}</span>{/if}
      </span>
    {/if}
  </button>
  {#if task.pending}<span class="pending-dot" title="sin enviar al servidor"></span>{/if}
</li>

{#if editing}
  <li class="editor">
    <form onsubmit={save}>
      <input bind:value={title} aria-label="Título" />
      {#if caps.due}
        <label>Vence <input type="date" bind:value={due} /></label>
      {/if}
      {#if caps.notes}
        <textarea bind:value={notes} placeholder="Notas" rows="2"></textarea>
      {/if}
      {#if caps.tags}
        <input bind:value={tags} placeholder="etiquetas, separadas por coma" />
      {/if}
      {#if others.length}
        <label>
          Mover a
          <select bind:value={moveTo}>
            <option value="">—</option>
            {#each others as o (o.meta.id)}<option value={o.meta.id}>{o.meta.name}</option>{/each}
          </select>
        </label>
      {/if}
      <div class="row">
        <button type="submit">Guardar</button>
        <button type="button" class="link" onclick={() => (editing = false)}>Cancelar</button>
        <span class="spacer"></span>
        <button
          type="button"
          class="link danger"
          onclick={() => {
            editing = false;
            app.remove(list.meta.id, task.id);
          }}>Borrar</button
        >
      </div>
    </form>
  </li>
{/if}
