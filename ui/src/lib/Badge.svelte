<script lang="ts">
	import type { Shot } from './types';
	import { effective } from './api';

	let { shot }: { shot: Shot } = $props();
	const mark = $derived(effective(shot));
	const confirmed = $derived(shot.decision !== null);
</script>

<span
	class="badge {mark}"
	class:confirmed
	title={confirmed ? 'your decision' : 'suggested, not confirmed yet'}
>
	{mark === 'pick' ? '★' : mark === 'reject' ? '✗' : '?'}
</span>

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
</style>
