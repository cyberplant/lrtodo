<script lang="ts">
  import { app, type ListView, type Task, type TaskLink } from '../lib/app.svelte';

  let { task, list, depth = 0, tasks = [] }: { task: Task; list: ListView; depth?: number; tasks?: Task[] } = $props();

  let editing = $state(false);
  let title = $state('');
  let due = $state('');
  let notes = $state('');
  let tags = $state('');
  let moveTo = $state('');
  let linkKind = $state<TaskLink['kind']>('subtask');
  let linkTo = $state('');

  const caps = $derived(list.meta.caps);
  const others = $derived(app.lists.filter((l) => l.meta.id !== list.meta.id));
  const linkTargets = $derived(tasks.filter((t) => t.id !== task.id));

  function startEdit() {
    title = task.title;
    due = task.due ?? '';
    notes = task.notes ?? '';
    tags = (task.tags ?? []).join(', ');
    moveTo = '';
    linkKind = 'subtask';
    linkTo = '';
    editing = true;
  }

  function linkLabel(l: TaskLink): string {
    const kind = { subtask: 'subtarea de', blocks: 'bloquea a', related: 'relacionada con' }[l.kind];
    const target = l.to.includes('/') ? l.to : (tasks.find((t) => t.id === l.to)?.title ?? l.to);
    return `${kind} ${target}`;
  }

  async function addLink() {
    if (!linkTo) return;
    await app.link(list.meta.id, task.id, linkTo, linkKind);
    linkTo = '';
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

<li class="task" class:done={task.done} class:pending={task.pending} class:blocked={task.blocked && !task.done} style:padding-left="{depth * 1.5}rem" data-testid="task">
  <label class="check">
    <input
      type="checkbox"
      checked={!!task.done}
      onchange={(e) => app.set(list.meta.id, task.id, 'done', (e.currentTarget as HTMLInputElement).checked)}
    />
  </label>
  <button class="title" onclick={startEdit}>
    {#if task.blocked && !task.done}<span class="blocked-mark" title="bloqueada por otra tarea">⊘</span>{/if}
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
      {#each task.links ?? [] as l (l.kind + l.to)}
        <div class="row link-row">
          <span class="muted">{linkLabel(l)}</span>
          <span class="spacer"></span>
          <button type="button" class="link danger" onclick={() => app.unlink(list.meta.id, task.id, l.to, l.kind)}>quitar</button>
        </div>
      {/each}
      {#if linkTargets.length}
        <div class="row">
          <select bind:value={linkKind} aria-label="Tipo de link">
            <option value="subtask">subtarea de…</option>
            <option value="blocks">bloquea a…</option>
            <option value="related">relacionada con…</option>
          </select>
          <select bind:value={linkTo} aria-label="Destino">
            <option value="">—</option>
            {#each linkTargets as t (t.id)}<option value={t.id}>{t.title}</option>{/each}
          </select>
          <button type="button" class="link" onclick={addLink} disabled={!linkTo}>linkear</button>
        </div>
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
