<script lang="ts" module>
	/** Shared zoom: `s` is a multiplier on fit-to-pane, (cx, cy) the image point
	 * (0–1) at the pane centre. Normalised, so panes showing different-sized
	 * images stay aligned. */
	export interface Zoom {
		s: number;
		cx: number;
		cy: number;
	}
	export const fitZoom = (): Zoom => ({ s: 1, cx: 0.5, cy: 0.5 });
</script>

<script lang="ts">
	import type { Shot } from './types';
	import { lift, src } from './api';
	import Badge from './Badge.svelte';

	let {
		panes,
		eyes,
		boost,
		zoom = $bindable(),
		active = 0,
		onactivate
	}: {
		panes: Shot[];
		eyes: boolean;
		boost: boolean;
		zoom: Zoom;
		/** With two panes: the one keys act on. */
		active?: number;
		onactivate?: (i: number) => void;
	} = $props();

	// Pane and natural image sizes; at most two panes.
	const blank = () => ({ cw: 0, ch: 0, nw: 0, nh: 0 });
	let sizes = $state([blank(), blank()]);

	const url = (s: Shot) => (eyes ? s.eyes[0] : s.display);
	const fit = (i: number) => {
		const z = sizes[i];
		return z && z.nw && z.cw ? Math.min(z.cw / z.nw, z.ch / z.nh) : 1;
	};
	function box(i: number) {
		const z = sizes[i];
		if (!z || !z.nw) return '';
		const sc = fit(i) * zoom.s;
		return `width:${z.nw * sc}px;height:${z.nh * sc}px;left:${z.cw / 2 - zoom.cx * z.nw * sc}px;top:${z.ch / 2 - zoom.cy * z.nh * sc}px`;
	}

	/** Point in the pane → normalised image point. */
	function imagePoint(i: number, px: number, py: number): [number, number] {
		const z = sizes[i]!;
		const sc = fit(i) * zoom.s;
		return [zoom.cx + (px - z.cw / 2) / (z.nw * sc), zoom.cy + (py - z.ch / 2) / (z.nh * sc)];
	}
	/** Zoom to `s`, keeping the image point under (px, py) in place. */
	function zoomAt(i: number, px: number, py: number, s: number) {
		const z = sizes[i];
		if (!z || !z.nw) return;
		const [u, v] = imagePoint(i, px, py);
		const sc = fit(i) * s;
		zoom = { s, cx: u - (px - z.cw / 2) / (z.nw * sc), cy: v - (py - z.ch / 2) / (z.nh * sc) };
	}

	const local = (e: MouseEvent) => {
		const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
		return [e.clientX - r.left, e.clientY - r.top] as const;
	};
	function onwheel(i: number, e: WheelEvent) {
		e.preventDefault();
		const [px, py] = local(e);
		const s = Math.min(40, Math.max(1, zoom.s * Math.exp(-e.deltaY * 0.0015)));
		zoomAt(i, px, py, s);
	}
	function ondblclick(i: number, e: MouseEvent) {
		const [px, py] = local(e);
		if (zoom.s > 1.01) zoom = fitZoom();
		else zoomAt(i, px, py, Math.max(1, 1 / fit(i))); // 100%: one image pixel per screen pixel
	}
	let drag: { i: number; x: number; y: number } | null = null;
	function onpointerdown(i: number, e: PointerEvent) {
		onactivate?.(i);
		drag = { i, x: e.clientX, y: e.clientY };
		(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
	}
	function onpointermove(e: PointerEvent) {
		if (!drag) return;
		const z = sizes[drag.i];
		if (!z || !z.nw) return;
		const sc = fit(drag.i) * zoom.s;
		zoom = { s: zoom.s, cx: zoom.cx - (e.clientX - drag.x) / (z.nw * sc), cy: zoom.cy - (e.clientY - drag.y) / (z.nh * sc) };
		drag = { i: drag.i, x: e.clientX, y: e.clientY };
	}
</script>

<div class="panes">
	{#each panes as shot, i (i)}
		{@const u = url(shot)}
		<div
			class="pane"
			class:active={panes.length > 1 && i === active}
			role="img"
			aria-label={shot.file}
			bind:clientWidth={() => sizes[i]?.cw ?? 0, (v) => sizes[i] && (sizes[i].cw = v)}
			bind:clientHeight={() => sizes[i]?.ch ?? 0, (v) => sizes[i] && (sizes[i].ch = v)}
			onwheel={(e) => onwheel(i, e)}
			ondblclick={(e) => ondblclick(i, e)}
			onpointerdown={(e) => onpointerdown(i, e)}
			{onpointermove}
			onpointerup={() => (drag = null)}
		>
			{#if u}
				<img
					src={src(u)}
					alt={shot.file}
					draggable="false"
					style={box(i)}
					style:filter={boost && eyes ? `brightness(${lift(shot)})` : undefined}
					onload={(e) => {
						const im = e.currentTarget as HTMLImageElement;
						if (sizes[i]) {
							sizes[i].nw = im.naturalWidth;
							sizes[i].nh = im.naturalHeight;
						}
					}}
				/>
			{:else}
				<div class="none">no face</div>
			{/if}
			<div class="label"><Badge {shot} /> <span>{shot.file}</span></div>
		</div>
	{/each}
</div>

<style>
	.panes {
		display: flex;
		gap: 2px;
		height: 100%;
		background: var(--line);
	}
	.pane {
		position: relative;
		flex: 1;
		overflow: hidden;
		background: #000;
		cursor: grab;
		touch-action: none;
	}
	.pane.active::after {
		content: '';
		position: absolute;
		inset: 0;
		border: 2px solid var(--accent);
		pointer-events: none;
	}
	img {
		position: absolute;
		max-width: none;
		user-select: none;
	}
	.none {
		display: grid;
		place-items: center;
		height: 100%;
		color: var(--muted);
	}
	.label {
		position: absolute;
		top: 0.5rem;
		left: 0.5rem;
		display: flex;
		gap: 0.5rem;
		align-items: center;
		font-size: 0.8rem;
		color: #ddd;
		text-shadow: 0 0 3px #000;
	}
</style>
