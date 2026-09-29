<script lang="ts">
	import { src } from './api';
	import type { Person } from './types';

	let {
		people,
		active = $bindable(null),
		onname
	}: {
		people: Person[];
		/** The person the frames are filtered to, or null for everyone. */
		active: number | null;
		onname: (id: number, name: string) => void;
	} = $props();

	/** The person whose name is being typed. */
	let editing = $state<number | null>(null);
	let draft = $state('');

	export const label = (p: Person) =>
		p.name ?? `Person ${people.filter((q) => q.name === null).indexOf(p) + 1}`;

	/** Start typing a name for `id` (N key or double-click). */
	export function rename(id: number) {
		const p = people.find((q) => q.id === id);
		if (!p) return;
		editing = id;
		draft = p.name ?? '';
	}

	function commit() {
		if (editing === null) return;
		const id = editing;
		editing = null;
		const p = people.find((q) => q.id === id);
		if (p && draft.trim() !== (p.name ?? '')) onname(id, draft.trim());
	}

	function focus(el: HTMLInputElement) {
		el.focus();
		el.select();
	}
</script>

<div class="people" role="group" aria-label="people">
	{#each people as p (p.id)}
		{#if editing === p.id}
			<span class="chip on">
				<img src={src(p.face)} alt="" />
				<input
					use:focus
					bind:value={draft}
					placeholder={label(p)}
					onkeydown={(e) => {
						e.stopPropagation();
						if (e.key === 'Enter') commit();
						else if (e.key === 'Escape') editing = null;
					}}
					onblur={commit}
				/>
			</span>
		{:else}
			<button
				class="chip"
				class:on={active === p.id}
				class:unnamed={p.name === null}
				onclick={() => (active = active === p.id ? null : p.id)}
				ondblclick={() => rename(p.id)}
				title="{label(p)}: {p.photos} photo{p.photos === 1 ? '' : 's'}&#10;Click to show only them · double-click or N to name"
			>
				<img src={src(p.face)} alt="" />
				<span class="name">{label(p)}</span>
				<span class="n">{p.photos}</span>
			</button>
		{/if}
	{/each}
</div>

<style>
	.people {
		display: flex;
		gap: 0.4rem;
		padding: 0.3rem 0.8rem;
		overflow-x: auto;
		scrollbar-width: thin;
		border-bottom: 1px solid var(--line);
		background: var(--panel);
		flex-shrink: 0;
	}
	.chip {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		padding: 0.1rem 0.55rem 0.1rem 0.1rem;
		border-radius: 999px;
		white-space: nowrap;
		flex-shrink: 0;
		border: 1px solid var(--line);
		font-size: 0.85rem;
	}
	.chip.on {
		background: var(--accent);
		border-color: var(--accent);
		color: #111;
	}
	.unnamed .name {
		font-style: italic;
	}
	img {
		width: 1.5rem;
		height: 1.5rem;
		border-radius: 50%;
		object-fit: cover;
	}
	.n {
		opacity: 0.65;
		font-variant-numeric: tabular-nums;
	}
	input {
		width: 8rem;
		font: inherit;
		background: var(--bg);
		color: var(--fg, inherit);
		border: none;
		border-radius: 4px;
		padding: 0.1rem 0.3rem;
	}
</style>
