<script lang="ts">
	import type { Shot } from './types';
	import { src } from './api';
	import Badge from './Badge.svelte';

	/** The whole group as small thumbnails, so full view shows where the frame sits in it. */
	let {
		shots,
		current,
		sel,
		onselect
	}: {
		shots: { shot: Shot; index: number }[];
		current: number;
		/** Frames marked for compare: [A, B]. */
		sel: number[];
		onselect: (i: number, e: MouseEvent) => void;
	} = $props();
</script>

<div class="film" aria-label="frames in this group">
	{#each shots as { shot, index }, k (index)}
		{@const m = sel.indexOf(index)}
		<button
			id="film-{index}"
			class="frame"
			class:current={index === current}
			class:marked={m >= 0}
			onclick={(e) => onselect(index, e)}
			title="{shot.file} (Ctrl-click: mark for compare)"
		>
			{#if !shot.error}<img src={src(shot.thumb)} alt="" loading="lazy" draggable="false" />{/if}
			<span class="badge"><Badge {shot} /></span>
			<span class="pos">{#if shot.crops.length}<span class="cut">✂{shot.crops.length}</span>{/if}{k + 1}</span>
			{#if m >= 0}<span class="ab">{m === 0 ? 'A' : 'B'}</span>{/if}
		</button>
	{/each}
</div>

<style>
	.film {
		display: flex;
		gap: 4px;
		overflow-x: auto;
		padding: 4px 0.5rem;
		background: #0b0b0c;
		border-top: 1px solid var(--line);
		flex: none;
	}
	.frame {
		all: unset;
		position: relative;
		flex: none;
		height: 56px;
		min-width: 40px;
		border: 2px solid transparent;
		border-radius: 3px;
		cursor: pointer;
		opacity: 0.6;
		overflow: hidden;
	}
	.frame.current {
		border-color: var(--accent);
		opacity: 1;
	}
	.frame.marked {
		opacity: 1;
	}
	img {
		height: 100%;
		display: block;
	}
	.badge {
		position: absolute;
		top: 1px;
		left: 1px;
		transform: scale(0.6);
		transform-origin: top left;
	}
	.pos {
		position: absolute;
		bottom: 1px;
		right: 3px;
		font-size: 0.7rem;
		color: #fff;
		text-shadow: 0 0 3px #000;
		font-variant-numeric: tabular-nums;
	}
	.cut {
		color: var(--pick);
		margin-right: 0.2rem;
	}
	.ab {
		position: absolute;
		top: 2px;
		right: 2px;
		font-size: 0.65rem;
		font-weight: 700;
		padding: 0 0.25rem;
		border-radius: 3px;
		background: var(--accent);
		color: #111;
	}
</style>
