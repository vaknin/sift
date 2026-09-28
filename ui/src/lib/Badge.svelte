<script lang="ts">
	import type { Shot } from './types';
	import { effective } from './api';

	let { shot }: { shot: Shot } = $props();
	const mark = $derived(effective(shot));
	const confirmed = $derived(shot.decision !== null);
	const stars = $derived(shot.decision?.stars ?? 0);
</script>

<span
	class="badge {mark}"
	class:confirmed
	title={confirmed ? 'your decision' : 'suggested, not confirmed yet'}
>
	{mark === 'pick' ? '★' : mark === 'reject' ? '✗' : '?'}
</span>
{#if stars > 0}<span class="stars">{'★'.repeat(stars)}</span>{/if}

<style>
	.badge {
		display: inline-grid;
		place-items: center;
		width: 1.6rem;
		height: 1.6rem;
		border-radius: 50%;
		font-size: 0.95rem;
		font-weight: 700;
		border: 2px dashed currentColor;
		background: rgb(0 0 0 / 0.55);
		color: var(--muted);
	}
	.pick {
		color: var(--pick);
	}
	.reject {
		color: var(--reject);
	}
	.confirmed {
		border-style: solid;
		color: #111;
	}
	.confirmed.pick {
		background: var(--pick);
		border-color: var(--pick);
	}
	.confirmed.reject {
		background: var(--reject);
		border-color: var(--reject);
	}
	.confirmed.none {
		background: var(--muted);
		border-color: var(--muted);
	}
	.stars {
		margin-left: 0.3rem;
		color: var(--star);
		font-size: 0.8rem;
		text-shadow: 0 0 3px #000;
	}
</style>
