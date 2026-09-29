<script lang="ts">
	import type { Crop, KeptCrop, Ratio, ReframeView, Shot } from './types';
	import { longSide, noteText, ratioText, src } from './api';

	/** One picked frame: a crop box over the image, and a list of kept crops and suggestions. */
	let {
		shot,
		data,
		error,
		minLong,
		cropOnly = $bindable(false),
		onkeep,
		onnext,
		onprev
	}: {
		shot: Shot;
		/** Suggestions; undefined while they load. */
		data: ReframeView | undefined;
		/** Why the suggestions couldn't be made. */
		error: string | null;
		/** Smallest long side a crop may have, full-resolution pixels. */
		minLong: number;
		/** Show only the crop, with nothing drawn over it; kept from frame to frame. */
		cropOnly?: boolean;
		/** Store the frame's kept crops; false when saving failed. */
		onkeep: (crops: [Crop, Ratio][]) => Promise<boolean>;
		onnext: () => void;
		onprev: () => void;
	} = $props();

	interface Box {
		crop: Crop;
		ratio: Ratio;
	}
	type Card = { kind: 'edited' } | { kind: 'kept'; i: number } | { kind: 'suggestion'; i: number };

	/** Ratios `A` cycles through. */
	const CYCLE: Ratio[] = [
		[4, 5],
		[1, 1],
		[3, 2],
		[16, 9],
		[2, 3]
	];
	/** Snap distance, fraction of the frame. */
	const SNAP = 0.015;
	/** WhatsApp HD sends up to this long side. */
	const FULL_HD = 4096;

	// Full-resolution size, upright.
	const W = $derived(data?.width ?? shot.width);
	const H = $derived(data?.height ?? shot.height);
	const crops = $derived(shot.crops);
	// The frame as shot is left out: hold Space to see it.
	const suggestions = $derived((data?.suggestions ?? []).filter((s) => !s.notes.includes('original')));
	const face = $derived(data?.faces[0]);

	const full = (): Box => ({ crop: { x: 0, y: 0, w: 1, h: 1 }, ratio: simplest(shot.width, shot.height) });
	let box = $state<Box>(full());
	/** What is in the box; null until the suggestions arrive. */
	let sel = $state<Card | null>(null);
	/** The box after a manual change, and the card it started from. */
	let edited = $state<(Box & { from: Card | null }) | null>(null);
	/** Space held: the frame as shot, with nothing drawn over it. */
	let original = $state(false);
	let flash = $state<string | null>(null);
	let flashTimer: ReturnType<typeof setTimeout> | undefined;

	let pane = $state<HTMLDivElement>();
	let clip = $state<HTMLDivElement>();
	let cw = $state(0);
	let ch = $state(0);

	function simplest(w: number, h: number): Ratio {
		const g = (a: number, b: number): number => (b ? g(b, a % b) : a);
		const d = g(w, h) || 1;
		const r: Ratio = [w / d, h / d];
		return r[0] > 32 || r[1] > 32 ? (w >= h ? [3, 2] : [2, 3]) : r;
	}

	// Start on the first kept crop, else the best suggestion, once they are known.
	$effect(() => {
		if (sel !== null || (!data && !error)) return;
		if (crops.length) load({ kind: 'kept', i: 0 });
		else if (suggestions.length) load({ kind: 'suggestion', i: 0 });
	});

	/** Left to right: kept crops, then suggestions; an edit sits just after the card it came from. */
	const cards = $derived.by<Card[]>(() => {
		const list: Card[] = [
			...crops.map((_, i) => ({ kind: 'kept', i }) as const),
			...suggestions.map((_, i) => ({ kind: 'suggestion', i }) as const)
		];
		if (edited) list.splice(list.findIndex((c) => same(c, edited!.from)) + 1, 0, { kind: 'edited' });
		return list;
	});
	const same = (a: Card | null, b: Card | null) =>
		a !== null && b !== null && a.kind === b.kind && (a.kind === 'edited' || a.i === (b as { i: number }).i);
	function boxOf(c: Card): Box | undefined {
		if (c.kind === 'edited') return edited ?? undefined;
		const k = c.kind === 'kept' ? crops[c.i] : suggestions[c.i];
		return k && { crop: k.crop, ratio: k.ratio };
	}
	function load(c: Card) {
		const b = boxOf(c);
		if (!b) return;
		box = { crop: { ...b.crop }, ratio: b.ratio };
		sel = c;
	}
	function stepCard(d: number) {
		const k = cards.findIndex((c) => same(c, sel));
		const next = cards[Math.min(cards.length - 1, Math.max(0, k + d))];
		if (next) load(next);
	}
	/** A manual change: the box becomes the edited card. */
	function edit(b: Box) {
		const from = sel?.kind === 'edited' ? (edited?.from ?? null) : sel;
		edited = { ...b, from };
		box = b;
		sel = { kind: 'edited' };
	}

	function say(text: string) {
		flash = text;
		clearTimeout(flashTimer);
		flashTimer = setTimeout(() => (flash = null), 1600);
	}

	// Geometry in full-resolution pixels, so ratios stay exact.
	const rv = (r: Ratio) => r[0] / r[1];
	/** Largest width of ratio `r` that fits the frame. */
	const maxW = (r: Ratio) => Math.min(W, H * rv(r));
	/** Smallest width the floor allows (never more than fits). */
	const minW = (r: Ratio) => Math.min(maxW(r), rv(r) >= 1 ? minLong : minLong * rv(r));
	const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));
	/** A box `w` px wide centred on (cx, cy), held to the floor and the frame. */
	function place(cx: number, cy: number, w: number, r: Ratio): Box {
		w = clamp(w, minW(r), maxW(r));
		const h = w / rv(r);
		const x = clamp(cx - w / 2, 0, W - w);
		const y = clamp(cy - h / 2, 0, H - h);
		return { crop: { x: x / W, y: y / H, w: w / W, h: h / H }, ratio: r };
	}
	const centre = (c: Crop): [number, number] => [(c.x + c.w / 2) * W, (c.y + c.h / 2) * H];
	/** Same centre and area, another ratio. */
	function reshape(r: Ratio) {
		const area = box.crop.w * W * box.crop.h * H;
		edit(place(...centre(box.crop), Math.sqrt(area * rv(r)), r));
	}
	function cycleRatio() {
		const k = CYCLE.findIndex((r) => r[0] === box.ratio[0] && r[1] === box.ratio[1]);
		reshape(CYCLE[(k + 1) % CYCLE.length]!);
	}
	function scale(f: number) {
		edit(place(...centre(box.crop), box.crop.w * W * f, box.ratio));
	}
	function nudge(dx: number, dy: number) {
		const c = box.crop;
		edit({ crop: { ...c, x: clamp(c.x + dx, 0, 1 - c.w), y: clamp(c.y + dy, 0, 1 - c.h) }, ratio: box.ratio });
	}

	// The subject, for snapping and warnings.
	const eye = $derived<[number, number] | null>(
		face ? [(face.eyes[0][0] + face.eyes[1][0]) / 2, (face.eyes[0][1] + face.eyes[1][1]) / 2] : null
	);
	const faceX = $derived(face ? face.bbox[0] + face.bbox[2] / 2 : 0);
	/** Put the eye line on a horizontal third and the face on a vertical third or the centre, when close. */
	function snap(c: Crop): Crop {
		if (!eye) return c;
		let { x, y } = c;
		for (const t of [1 / 3, 2 / 3]) {
			const s = eye[1] - c.h * t;
			if (Math.abs(y - s) < SNAP && s >= 0 && s + c.h <= 1) {
				y = s;
				break;
			}
		}
		for (const t of [1 / 3, 1 / 2, 2 / 3]) {
			const s = faceX - c.w * t;
			if (Math.abs(x - s) < SNAP && s >= 0 && s + c.w <= 1) {
				x = s;
				break;
			}
		}
		return { ...c, x, y };
	}
	/** Which guide lines the subject sits on. */
	const hot = $derived.by(() => {
		const c = box.crop;
		if (!eye) return { h: 0, v: 0 };
		const fy = (eye[1] - c.y) / c.h;
		const fx = (faceX - c.x) / c.w;
		const near = (a: number, b: number) => Math.abs(a - b) < 0.01;
		return {
			h: near(fy, 1 / 3) ? 1 : near(fy, 2 / 3) ? 2 : 0,
			v: near(fx, 1 / 3) ? 1 : near(fx, 1 / 2) ? 1.5 : near(fx, 2 / 3) ? 2 : 0
		};
	});

	const warnings = $derived.by(() => {
		if (!data || !face) return [];
		const c = box.crop;
		const out: string[] = [];
		const fh = face.chin[1] - face.forehead[1];
		const [ms, mt, mb] = data.margin.map((m) => m * fh);
		const side = ms! * (H / W);
		const [bx, by, bw, bh] = face.bbox;
		// Clamped, so the frame as shot never cuts a face at its edge.
		const keep = [Math.max(0, bx - side), Math.max(0, by - mt!), Math.min(1, bx + bw + side), Math.min(1, by + bh + mb!)];
		const e = 1e-4;
		if (c.x > keep[0]! + e || c.y > keep[1]! + e || c.x + c.w < keep[2]! - e || c.y + c.h < keep[3]! - e) out.push('cuts face');
		const bottom = c.y + c.h;
		if (bottom > face.chin[1] && bottom < face.chin[1] + data.chinBand * fh) out.push('cuts chin');
		for (const f of data.faces.slice(1)) {
			const [fx, fy, fw, fhh] = f.bbox;
			const iw = Math.max(0, Math.min(c.x + c.w, fx + fw) - Math.max(c.x, fx));
			const ih = Math.max(0, Math.min(c.y + c.h, fy + fhh) - Math.max(c.y, fy));
			const part = (iw * ih) / (fw * fhh);
			if (part > 0.02 && part < 0.98) {
				out.push('cuts a face');
				break;
			}
		}
		return out;
	});

	// Readout.
	const px = $derived([Math.round(box.crop.w * W), Math.round(box.crop.h * H)]);
	const long = $derived(longSide(box.crop, W, H));
	const atWall = $derived(box.crop.w * W <= minW(box.ratio) + 0.5);
	/** Why the selected suggestion was made. */
	const notes = $derived(sel?.kind === 'suggestion' ? (suggestions[sel.i]?.notes ?? []) : []);
	const mp = (c: Crop) => ((c.w * W * c.h * H) / 1e6).toFixed(1);

	/** IoU of two crops. */
	function iou(a: Crop, b: Crop) {
		const iw = Math.max(0, Math.min(a.x + a.w, b.x + b.w) - Math.max(a.x, b.x));
		const ih = Math.max(0, Math.min(a.y + a.h, b.y + b.h) - Math.max(a.y, b.y));
		const i = iw * ih;
		return i / (a.w * a.h + b.w * b.h - i);
	}

	/** Keep the box, or put it in place of the kept crop it was edited from. */
	async function keep(): Promise<void> {
		if (sel?.kind === 'kept') return say('already kept');
		if (box.crop.w > 0.999 && box.crop.h > 0.999) return say('the original is always kept');
		const replace = sel?.kind === 'edited' && edited?.from?.kind === 'kept' ? edited.from.i : -1;
		const dup = crops.findIndex((k, i) => i !== replace && iou(k.crop, box.crop) > 0.95);
		if (dup >= 0) {
			say('already kept');
			return load({ kind: 'kept', i: dup });
		}
		const list: [Crop, Ratio][] = crops.map((k) => [k.crop, k.ratio]);
		const b: [Crop, Ratio] = [{ ...box.crop }, box.ratio];
		if (replace >= 0) list[replace] = b;
		else list.push(b);
		if (!(await onkeep(list))) return;
		edited = null;
		sel = { kind: 'kept', i: replace >= 0 ? replace : list.length - 1 };
		say(replace >= 0 ? 'crop replaced' : `kept ✂${list.length}`);
	}

	/** Remove the selected kept crop, or the one the box shows (a suggestion that was kept);
	 * it stays in the box as an edit, so Enter brings it back. */
	async function remove() {
		let i = sel?.kind === 'kept' ? sel.i : sel?.kind === 'edited' && edited?.from?.kind === 'kept' ? edited.from.i : -1;
		if (i < 0) i = crops.findIndex((k) => iou(k.crop, box.crop) > 0.95);
		if (!crops[i]) return say(crops.length ? 'not kept · ← to a kept crop, then Del' : 'no kept crops to remove');
		const list: [Crop, Ratio][] = crops.filter((_, j) => j !== i).map((c) => [c.crop, c.ratio]);
		if (!(await onkeep(list))) return;
		edited = { crop: { ...box.crop }, ratio: box.ratio, from: null };
		sel = { kind: 'edited' };
		say('removed · Enter keeps it again');
	}

	/** Key releases from the page; true when handled. */
	export function handleKeyUp(e: KeyboardEvent): boolean {
		if (e.code !== 'Space') return false;
		original = false;
		return true;
	}

	/** Keys from the page; true when handled. */
	export function handleKey(e: KeyboardEvent): boolean {
		const k = e.key;
		if (e.ctrlKey && k.startsWith('Arrow') && !e.altKey && !e.metaKey) {
			const d = 0.01;
			if (k === 'ArrowLeft') nudge(-d, 0);
			else if (k === 'ArrowRight') nudge(d, 0);
			else if (k === 'ArrowUp') nudge(0, -d);
			else nudge(0, d);
			return true;
		}
		if (e.ctrlKey || e.altKey || e.metaKey) return false;
		if (k === 'ArrowUp') onprev();
		else if (k === 'ArrowDown') onnext();
		else if (k === 'ArrowLeft') stepCard(-1);
		else if (k === 'ArrowRight') stepCard(1);
		else if (k === 'Enter') e.shiftKey ? keep().then(onnext) : keep();
		else if (e.code === 'KeyA') cycleRatio();
		else if (e.code === 'KeyO') reshape([box.ratio[1], box.ratio[0]]);
		else if (e.code === 'Minus') scale(0.95);
		else if (e.code === 'Equal') scale(1.05);
		else if (e.code === 'KeyZ') cropOnly = !cropOnly;
		else if (e.code === 'Space') original = true;
		else if (k === 'Escape' && cropOnly) cropOnly = false;
		else if (k === 'Delete' || k === 'Backspace') remove();
		else return false;
		return true;
	}

	// Pointer: drag inside to move, a handle to resize about the opposite side.
	let drag = $state<{ hx: number; hy: number; x: number; y: number; orig: Crop } | null>(null);
	/** Client point → full-resolution pixels. */
	function framePoint(e: PointerEvent): [number, number] {
		const r = clip!.getBoundingClientRect();
		return [((e.clientX - r.left) / r.width) * W, ((e.clientY - r.top) / r.height) * H];
	}
	function startDrag(e: PointerEvent, hx: number, hy: number) {
		if (e.button !== 0 || cropOnly) return;
		e.stopPropagation();
		const [x, y] = framePoint(e);
		drag = { hx, hy, x, y, orig: { ...box.crop } };
		pane!.setPointerCapture(e.pointerId);
	}
	function onpointermove(e: PointerEvent) {
		if (!drag) return;
		const [px, py] = framePoint(e);
		const o = drag.orig;
		const r = box.ratio;
		if (drag.hx === 0 && drag.hy === 0) {
			const c = {
				...o,
				x: clamp(o.x + (px - drag.x) / W, 0, 1 - o.w),
				y: clamp(o.y + (py - drag.y) / H, 0, 1 - o.h)
			};
			edit({ crop: e.ctrlKey ? c : snap(c), ratio: r });
			return;
		}
		const { hx, hy } = drag;
		const [ox, oy, ow, oh] = [o.x * W, o.y * H, o.w * W, o.h * H];
		// The anchor: the opposite edge along a handle's axis, the centre across it.
		const ax = hx > 0 ? ox : hx < 0 ? ox + ow : ox + ow / 2;
		const ay = hy > 0 ? oy : hy < 0 ? oy + oh : oy + oh / 2;
		const want = Math.max(hx ? (px - ax) * hx : 0, hy ? (py - ay) * hy * rv(r) : 0);
		const roomX = hx > 0 ? W - ax : hx < 0 ? ax : W;
		const roomY = (hy > 0 ? H - ay : hy < 0 ? ay : H) * rv(r);
		const w = clamp(want, minW(r), Math.max(minW(r), Math.min(roomX, roomY, maxW(r))));
		const h = w / rv(r);
		const x = hx > 0 ? ax : hx < 0 ? ax - w : ax - w / 2;
		const y = hy > 0 ? ay : hy < 0 ? ay - h : ay - h / 2;
		edit(place(x + w / 2, y + h / 2, w, r));
	}
	function onwheel(e: WheelEvent) {
		if (cropOnly) return;
		e.preventDefault();
		scale(Math.exp(-e.deltaY * 0.0015));
	}

	/** Where the pane shows `v` (a frame fraction) as large as fits: sizes in CSS px. */
	const view = $derived.by(() => {
		const v = cropOnly && !original ? box.crop : { x: 0, y: 0, w: 1, h: 1 };
		const k = cw && ch ? Math.min(cw / (v.w * W), ch / (v.h * H)) : 0;
		return { w: v.w * W * k, h: v.h * H * k, iw: W * k, ih: H * k, ix: -v.x * W * k, iy: -v.y * H * k };
	});
	const pct = (c: Crop) => `left:${c.x * 100}%;top:${c.y * 100}%;width:${c.w * 100}%;height:${c.h * 100}%`;
	const HANDLES = [
		[-1, -1],
		[0, -1],
		[1, -1],
		[1, 0],
		[1, 1],
		[0, 1],
		[-1, 1],
		[-1, 0]
	] as const;

	/** A card's thumbnail: the display image cropped by CSS to a fixed height. */
	const THUMB_H = 64;
	function thumb(c: Crop) {
		const h = THUMB_H;
		const w = (h * (c.w * W)) / (c.h * H);
		return { w, img: `width:${w / c.w}px;height:${h / c.h}px;left:${(-c.x * w) / c.w}px;top:${(-c.y * h) / c.h}px` };
	}
	const cardLabel = (c: Card) =>
		c.kind === 'edited'
			? 'edited'
			: c.kind === 'kept'
				? `${c.i + 1}`
				: c.i === 0
					? 'best'
					: '';
	$effect(() => {
		const k = cards.findIndex((c) => same(c, sel));
		if (k >= 0) document.getElementById(`card-${k}`)?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
	});
</script>

<svelte:window onblur={() => (original = false)} />

<div class="reframe">
	<div
		class="pane"
		bind:this={pane}
		bind:clientWidth={cw}
		bind:clientHeight={ch}
		{onpointermove}
		onpointerup={() => (drag = null)}
		onpointercancel={() => (drag = null)}
		{onwheel}
		role="img"
		aria-label="crop of {shot.file}"
	>
		<div class="clip" bind:this={clip} style="width:{view.w}px;height:{view.h}px">
			<img
				src={src(shot.display)}
				alt={shot.file}
				draggable="false"
				style="width:{view.iw}px;height:{view.ih}px;left:{view.ix}px;top:{view.iy}px"
			/>
			{#if !cropOnly && !original}
				<!-- svelte-ignore a11y_no_static_element_interactions -->
				<div class="box" class:glide={!drag} style={pct(box.crop)} onpointerdown={(e) => startDrag(e, 0, 0)}>
					<div class="line h" class:hot={hot.h === 1} style="top:33.333%"></div>
					<div class="line h" class:hot={hot.h === 2} style="top:66.667%"></div>
					<div class="line v" class:hot={hot.v === 1} style="left:33.333%"></div>
					<div class="line v" class:hot={hot.v === 2} style="left:66.667%"></div>
					{#if hot.v === 1.5}<div class="line v hot" style="left:50%"></div>{/if}
					{#each HANDLES as [hx, hy] (`${hx},${hy}`)}
						<div
							class="handle"
							class:edge={hx === 0 || hy === 0}
							style="left:{(hx + 1) * 50}%;top:{(hy + 1) * 50}%;transform:translate({-(hx + 1) * 50}%, {-(hy + 1) *
								50}%);cursor:{hx === hy
								? 'nwse'
								: hx === -hy
									? 'nesw'
									: hx === 0
										? 'ns'
										: 'ew'}-resize"
							onpointerdown={(e) => startDrag(e, hx, hy)}
						></div>
					{/each}
				</div>
				{#each crops as k, i (k.id)}
					<div class="outline" class:on={sel?.kind === 'kept' && sel.i === i} style={pct(k.crop)}>
						<span>{i + 1}</span>
					</div>
				{/each}
			{/if}
		</div>
		{#if original}<div class="asshot">as shot</div>{/if}
		<div class="readout" class:gone={cropOnly || original}>
			<span class="pill" class:warn={long < minLong * 1.2} class:hd={long >= FULL_HD}>
				{ratioText(box.ratio)} · {px[0]}×{px[1]} · {mp(box.crop)} MP · {Math.round(box.crop.w * box.crop.h * 100)}% of frame
				{#if atWall && box.crop.w < 1 && box.crop.h < 1}<b>min</b>{/if}
				{#if long >= FULL_HD}<b>full HD</b>{/if}
			</span>
			{#each notes as n (n)}<span class="pill note">{noteText(n)}</span>{/each}
			{#each warnings as w (w)}<span class="pill bad">{w}</span>{/each}
		</div>
		{#if flash}<div class="flash">{flash}</div>{/if}
	</div>

	<div class="strip" aria-label="crops">
		{#if error}
			<p class="msg bad">Couldn't make suggestions: {error}</p>
		{:else if !data}
			<p class="msg"><span class="spin"></span> Finding crops…</p>
		{:else if data.faces.length === 0}
			<p class="msg">No face found, so no suggestions. Drag to crop, or press <kbd>A</kbd> for a ratio.</p>
		{/if}
		{#each cards as c, k (c.kind === 'edited' ? 'e' : `${c.kind}${c.i}`)}
			{@const b = boxOf(c)}
			{#if b}
				{#if c.kind !== 'edited' && c.i === 0}
					<span class="set">{c.kind === 'kept' ? 'kept' : 'suggested'}</span>
				{/if}
				{@const t = thumb(b.crop)}
				{@const label = cardLabel(c)}
				<button
					id="card-{k}"
					class="card"
					class:on={same(c, sel)}
					class:kept={c.kind === 'kept'}
					class:edited={c.kind === 'edited'}
					onclick={() => load(c)}
					title="{mp(b.crop)} MP"
				>
					<span class="thumb" style="width:{t.w}px;height:{THUMB_H}px">
						<img src={src(shot.display)} alt="" draggable="false" style={t.img} />
						{#if label}<span class="tag" class:best={label === 'best'}>{label}</span>{/if}
					</span>
					<span class="meta"><b>{ratioText(b.ratio)}</b> {longSide(b.crop, W, H)} px</span>
				</button>
			{/if}
		{/each}
	</div>
</div>

<style>
	.reframe {
		flex: 1;
		display: flex;
		flex-direction: column;
		min-height: 0;
	}
	.pane {
		position: relative;
		flex: 1;
		min-width: 0;
		min-height: 0;
		overflow: hidden;
		background: #000;
		display: grid;
		place-items: center;
		touch-action: none;
		user-select: none;
	}
	.clip {
		position: relative;
		overflow: hidden;
	}
	.clip img {
		position: absolute;
		max-width: none;
	}
	.box {
		position: absolute;
		box-sizing: border-box;
		border: 1px solid rgba(255, 255, 255, 0.9);
		box-shadow: 0 0 0 100vmax rgba(0, 0, 0, 0.6);
		cursor: move;
	}
	.box.glide {
		transition:
			left 0.15s ease-out,
			top 0.15s ease-out,
			width 0.15s ease-out,
			height 0.15s ease-out;
	}
	.line {
		position: absolute;
		background: rgba(255, 255, 255, 0.28);
		pointer-events: none;
	}
	.line.h {
		left: 0;
		right: 0;
		height: 1px;
	}
	.line.v {
		top: 0;
		bottom: 0;
		width: 1px;
	}
	.line.hot {
		background: var(--accent);
		box-shadow: 0 0 3px var(--accent);
	}
	/* Inside the box, so none is clipped at the frame's edge. */
	.handle {
		position: absolute;
		width: 14px;
		height: 14px;
		box-sizing: border-box;
		background: #fff;
		border: 1px solid #000;
		border-radius: 2px;
	}
	.handle.edge {
		width: 10px;
		height: 10px;
		border-radius: 50%;
	}
	.outline {
		position: absolute;
		box-sizing: border-box;
		border: 1px solid color-mix(in srgb, var(--pick) 80%, transparent);
		pointer-events: none;
	}
	.outline.on {
		border-width: 2px;
	}
	.outline span {
		position: absolute;
		top: 2px;
		left: 4px;
		font-size: 0.7rem;
		font-weight: 700;
		color: var(--pick);
		text-shadow: 0 0 3px #000;
	}
	.readout {
		position: absolute;
		left: 0.6rem;
		bottom: 0.6rem;
		display: flex;
		gap: 0.4rem;
		flex-wrap: wrap;
		pointer-events: none;
	}
	.readout.gone {
		display: none;
	}
	.asshot {
		position: absolute;
		top: 0.6rem;
		left: 0.7rem;
		font-size: 0.75rem;
		color: #ddd;
		opacity: 0.7;
		text-shadow: 0 0 3px #000;
		pointer-events: none;
	}
	.pill {
		font-size: 0.8rem;
		padding: 0.15rem 0.6rem;
		border-radius: 99px;
		background: rgba(20, 21, 24, 0.85);
		color: var(--text);
		font-variant-numeric: tabular-nums;
	}
	.pill b {
		margin-left: 0.4rem;
		font-size: 0.72rem;
		text-transform: uppercase;
	}
	.pill.warn {
		color: var(--accent);
	}
	.pill.hd b {
		color: var(--pick);
	}
	.pill.bad {
		background: color-mix(in srgb, var(--reject) 70%, #000);
		color: #fff;
	}
	.flash {
		position: absolute;
		top: 0.8rem;
		left: 50%;
		transform: translateX(-50%);
		padding: 0.3rem 0.9rem;
		border-radius: 6px;
		background: rgba(20, 21, 24, 0.9);
		border: 1px solid var(--accent);
		pointer-events: none;
	}
	.pill.note {
		color: var(--muted);
	}
	/* Crops left to right, like the filmstrip in Cull: ←→ walk it. */
	.strip {
		flex: none;
		display: flex;
		align-items: flex-end;
		gap: 4px;
		overflow-x: auto;
		padding: 4px 0.5rem;
		background: #0b0b0c;
		border-top: 1px solid var(--line);
		min-height: 5.4rem;
		box-sizing: border-box;
	}
	.set {
		align-self: stretch;
		writing-mode: vertical-rl;
		transform: rotate(180deg);
		text-align: center;
		font-size: 0.62rem;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--muted);
		margin-left: 0.3rem;
	}
	.msg {
		align-self: center;
		margin: 0 0.5rem;
		font-size: 0.8rem;
		color: var(--muted);
	}
	.msg.bad {
		color: #ffb4ad;
	}
	.card {
		flex: none;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 2px;
		padding: 2px;
		background: transparent;
		border: 2px solid transparent;
		border-radius: 4px;
		opacity: 0.7;
	}
	.card.on {
		border-color: var(--accent);
		opacity: 1;
	}
	.card.kept .thumb {
		outline: 1px solid var(--pick);
	}
	.card.edited .thumb {
		outline: 1px dashed var(--accent);
	}
	.thumb {
		position: relative;
		flex: none;
		overflow: hidden;
		border-radius: 3px;
		background: #000;
	}
	.thumb img {
		position: absolute;
		max-width: none;
	}
	.tag {
		position: absolute;
		top: 2px;
		left: 2px;
		font-size: 0.6rem;
		font-weight: 700;
		padding: 0 0.3rem;
		border-radius: 3px;
		background: rgba(20, 21, 24, 0.85);
		text-transform: uppercase;
	}
	.card.kept .tag {
		color: var(--pick);
	}
	.tag.best {
		background: var(--accent);
		color: #111;
	}
	.meta {
		font-size: 0.68rem;
		color: var(--muted);
		white-space: nowrap;
		font-variant-numeric: tabular-nums;
	}
	.meta b {
		color: var(--text);
	}
	.spin {
		display: inline-block;
		width: 0.7rem;
		height: 0.7rem;
		border: 2px solid var(--muted);
		border-top-color: transparent;
		border-radius: 50%;
		animation: spin 0.8s linear infinite;
		vertical-align: -1px;
	}
	@keyframes spin {
		to {
			transform: rotate(360deg);
		}
	}
	kbd {
		background: var(--chip);
		border-radius: 3px;
		padding: 0 0.3rem;
		font: inherit;
	}
</style>
