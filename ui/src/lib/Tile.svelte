<script lang="ts">
	import type { Shot } from './types';
	import { reasonText, reasonTone, src } from './api';
	import Badge from './Badge.svelte';

	let {
		shot,
		index,
		pos,
		mark,
		current,
		onselect,
		onopen
	}: {
		shot: Shot;
		index: number;
		/** Place in the group, e.g. "3/7". */
		pos: string;
		/** 'A' or 'B' when marked for compare. */
		mark: string | null;
		current: boolean;
		onselect: (e: MouseEvent) => void;
		onopen: () => void;
	} = $props();

	const num = $derived(shot.file.replace(/\.[^.]+$/, '').split('-').at(-1));
</script>

<button id="tile-{index}" class="tile" class:current onclick={onselect} ondblclick={onopen}>
	<div class="img">
		{#if shot.error}
			<div class="err" title={shot.error}>could not read</div>
		{:else}
			<img src={src(shot.thumb)} alt={shot.file} loading="lazy" draggable="false" />
		{/if}
		<div class="corner"><Badge {shot} /></div>
		{#if mark}<div class="ab">{mark}</div>{/if}
		<div class="num"><span class="pos">{pos}</span> #{num}</div>
	</div>
	<div class="chips">
		{#each shot.verdict.reasons as r (r)}
			<span class="chip {reasonTone(r)}">{reasonText(r)}</span>
		{/each}
		<span class="sharp" title="eye/frame sharpness relative to the group's best">
			{Math.round(shot.verdict.rel_sharp * 100)}%
		</span>
	</div>
</button>

<style>
	.tile {
		all: unset;
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
		padding: 0.4rem;
		border-radius: 8px;
		border: 2px solid transparent;
		cursor: pointer;
		background: var(--panel);
		min-width: 0;
	}
	.tile.current {
		border-color: var(--accent);
		background: var(--panel-hi);
	}
	.img {
		position: relative;
		aspect-ratio: 3 / 2;
		display: grid;
		place-items: center;
		background: #000;
		border-radius: 4px;
		overflow: hidden;
	}
	img {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		object-fit: contain;
	}
	.err {
		color: var(--reject);
		font-size: 0.8rem;
	}
	.corner {
		position: absolute;
		top: 0.35rem;
		left: 0.35rem;
		display: flex;
		align-items: center;
	}
	.num {
		position: absolute;
		bottom: 0.3rem;
		right: 0.4rem;
		font-size: 0.75rem;
		color: #ddd;
		text-shadow: 0 0 3px #000;
	}
	.pos {
		color: var(--accent);
		font-variant-numeric: tabular-nums;
		margin-right: 0.3rem;
	}
	.ab {
		position: absolute;
		top: 0.35rem;
		right: 0.4rem;
		font-size: 0.8rem;
		font-weight: 700;
		padding: 0 0.35rem;
		border-radius: 4px;
		background: var(--accent);
		color: #111;
	}
	.chips {
		display: flex;
		flex-wrap: wrap;
		gap: 0.25rem;
		align-items: center;
		min-height: 1.3rem;
	}
	.chip {
		font-size: 0.7rem;
		padding: 0.1rem 0.4rem;
		border-radius: 99px;
		background: var(--chip);
		color: var(--muted);
	}
	.chip.bad {
		background: color-mix(in srgb, var(--reject) 25%, transparent);
		color: #ffb4ad;
	}
	.chip.good {
		background: color-mix(in srgb, var(--pick) 22%, transparent);
		color: #b6f0c0;
	}
	.sharp {
		margin-left: auto;
		font-size: 0.7rem;
		color: var(--muted);
		font-variant-numeric: tabular-nums;
	}
</style>
