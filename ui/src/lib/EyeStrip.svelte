<script lang="ts">
	import type { EyeState, Shot } from './types';
	import { judgeable, lift, reasonText, src } from './api';
	import Badge from './Badge.svelte';

	let {
		shots,
		current,
		sel,
		boost,
		onselect
	}: {
		shots: { shot: Shot; index: number }[];
		current: number;
		/** Frames marked for compare: [A, B]. */
		sel: number[];
		boost: boolean;
		onselect: (i: number, e: MouseEvent) => void;
	} = $props();

	/** Plain-words verdict on both eyes; closed as `cull::eyes_closed` counts it. */
	function eyesText([r, l]: [EyeState, EyeState]): { text: string; tone: 'good' | 'warn' | 'bad' } {
		const closed = (r === 'closed' ? 1 : 0) + (l === 'closed' ? 1 : 0);
		const open = (r === 'open' ? 1 : 0) + (l === 'open' ? 1 : 0);
		if (closed > 0 && open === 0) return { text: 'eyes closed', tone: 'bad' };
		if (closed > 0) return { text: 'one eye closed', tone: 'warn' };
		if (open < 2) return { text: 'half-closed', tone: 'warn' };
		return { text: 'eyes open', tone: 'good' };
	}
</script>

<div class="strip" aria-label="eye crops at 100%">
	{#each shots as { shot, index } (index)}
		{@const m = sel.indexOf(index)}
		<button id="eyes-{index}" class="eyes" class:current={index === current} onclick={(e) => onselect(index, e)}>
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
			{#if m >= 0}<div class="ab">{m === 0 ? 'A' : 'B'}</div>{/if}
			{#if shot.faces[0] && judgeable(shot)}
				{@const f = shot.faces[0]}
				{@const ey = eyesText(f.eyes)}
				<div
					class="blink {ey.tone}"
					title="blink score, 0 open … 1 closed: right {f.blink[0].toFixed(2)} · left {f.blink[1].toFixed(2)}"
				>
					{ey.text}
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
	.ab {
		position: absolute;
		top: 0.3rem;
		right: 0.35rem;
		font-size: 0.8rem;
		font-weight: 700;
		padding: 0 0.35rem;
		border-radius: 4px;
		background: var(--accent);
		color: #111;
	}
	.blink {
		position: absolute;
		bottom: 0.3rem;
		right: 0.35rem;
		font-size: 0.72rem;
		padding: 0.05rem 0.45rem;
		border-radius: 99px;
		background: rgb(0 0 0 / 0.6);
	}
	.blink.good {
		color: #b6f0c0;
	}
	.blink.warn {
		color: var(--accent);
	}
	.blink.bad {
		color: #fff;
		background: color-mix(in srgb, var(--reject) 80%, transparent);
	}
</style>
