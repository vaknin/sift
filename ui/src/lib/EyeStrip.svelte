<script lang="ts">
	import type { Shot } from './types';
	import { judgeable, lift, reasonText, src } from './api';
	import Badge from './Badge.svelte';

	let {
		shots,
		current,
		boost,
		onselect
	}: { shots: { shot: Shot; index: number }[]; current: number; boost: boolean; onselect: (i: number) => void } =
		$props();
</script>

<div class="strip" aria-label="eye crops at 100%">
	{#each shots as { shot, index } (index)}
		<button id="eyes-{index}" class="eyes" class:current={index === current} onclick={() => onselect(index)}>
			{#if shot.eyes[0] && judgeable(shot)}
				<img
					src={src(shot.eyes[0])}
					alt="eyes, {shot.file}"
					draggable="false"
					style:filter={boost ? `brightness(${lift(shot)})` : undefined}
				/>
			{:else}
				<div class="none">{reasonText(shot.verdict.reasons.find((r) => r.endsWith('face')) ?? 'no-face')}</div>
			{/if}
			<div class="corner"><Badge {shot} /></div>
			{#if shot.faces[0]}
				<div class="blink" title="eyeBlink score per eye (closed ≥ 0.45)">
					{shot.faces[0].blink.map((b) => b.toFixed(2)).join(' / ')}
				</div>
			{/if}
		</button>
	{/each}
</div>

<style>
	.strip {
		display: flex;
		gap: 0.5rem;
		overflow-x: auto;
		padding: 0.5rem;
		background: #000;
		border-top: 1px solid var(--line);
		height: var(--strip-h);
		box-sizing: border-box;
	}
	.eyes {
		all: unset;
		position: relative;
		flex: none;
		height: 100%;
		cursor: pointer;
		border: 2px solid transparent;
		border-radius: 4px;
		overflow: hidden;
		display: grid;
		place-items: center;
		min-width: 6rem;
		background: #0b0b0c;
	}
	.eyes.current {
		border-color: var(--accent);
	}
	img {
		/* Explicit: a percentage height doesn't resolve inside the grid button. */
		height: calc(var(--strip-h) - 1rem - 4px);
		width: auto;
		display: block;
	}
	.none {
		color: var(--muted);
		font-size: 0.8rem;
		padding: 0 1rem;
	}
	.corner {
		position: absolute;
		top: 0.3rem;
		left: 0.3rem;
	}
	.blink {
		position: absolute;
		bottom: 0.25rem;
		right: 0.35rem;
		font-size: 0.7rem;
		color: #ddd;
		text-shadow: 0 0 3px #000;
		font-variant-numeric: tabular-nums;
	}
</style>
