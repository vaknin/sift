<script lang="ts">
	import { onMount, tick, untrack } from 'svelte';
	import { SvelteMap } from 'svelte/reactivity';
	import { listen } from '@tauri-apps/api/event';
	import { getCurrentWindow } from '@tauri-apps/api/window';
	import { homeDir, join } from '@tauri-apps/api/path';
	import * as api from '$lib/api';
	import { effective, judgeable, matches, src } from '$lib/api';
	import type { Crop, Decision, Filter, Progress, Ratio, ReframeView, Shot, View } from '$lib/types';
	import Tile from '$lib/Tile.svelte';
	import EyeStrip from '$lib/EyeStrip.svelte';
	import Filmstrip from '$lib/Filmstrip.svelte';
	import Badge from '$lib/Badge.svelte';
	import ZoomView, { fitZoom, type Zoom } from '$lib/ZoomView.svelte';
	import Reframe from '$lib/Reframe.svelte';

	let view = $state<View | null>(null);
	let progress = $state<Progress | null>(null);
	let recent = $state<string[]>([]);
	let error = $state<string | null>(null);
	let cur = $state(0);
	let filter = $state<Filter>('all');
	let mode = $state<'grid' | 'full' | 'compare'>('grid');
	let eyesMode = $state(false);
	let boost = $state(true);
	let zoom = $state<Zoom>(fitZoom());
	/** Image only: no bars or strips, the window fullscreen. Keys keep working. */
	let fullscreen = $state(false);
	/** Frames marked for compare, in marking order: [A, B]. */
	let sel = $state<number[]>([]);
	/** The two frames in compare, and which side keys act on (`cur` is that side). */
	let pair = $state<[number, number]>([0, 0]);
	let active = $state(0);
	/** Mode to return to when compare closes. */
	let back: 'grid' | 'full' = 'grid';
	let sending = $state(false);
	/** Outcome of the last Send. */
	let sendNote = $state<{ written: number; queued: number; failed: [string, string][]; unsent: number } | null>(null);
	/** Changes waiting for "apply sift decisions" in darktable. */
	let waiting = $state(0);
	/** Files sift.lua couldn't find in darktable. */
	let missing = $state<string[]>([]);
	/** Cull decides; Reframe crops the picks. */
	let stage = $state<'cull' | 'reframe'>('cull');
	/** The picks when Reframe opened, so a frame unpicked there stays until you leave. */
	let picks = $state<number[]>([]);
	/** Suggestions per file, or why they failed; kept while the folder is open. */
	const reframes = new SvelteMap<string, ReframeView | { error: string }>();
	/** The file whose suggestions are being made. */
	let reframing = $state<string | null>(null);
	let reframer = $state<{ handleKey: (e: KeyboardEvent) => boolean }>();
	/** Bumped to stop a running prefetch loop. */
	let prefetchGen = 0;

	const groupOf = $derived.by(() => {
		const out: number[] = [];
		view?.groups.forEach((g, gi) => g.forEach((i) => (out[i] = gi)));
		return out;
	});
	const tilesOf = (gi: number): number[] =>
		(view?.groups[gi] ?? []).filter((i) => view !== null && matches(view.shots[i]!, filter));
	const visibleGroups = $derived(view ? view.groups.map((_, gi) => gi).filter((gi) => tilesOf(gi).length > 0) : []);
	const flat = $derived(visibleGroups.flatMap(tilesOf));
	const curGroup = $derived(groupOf[cur] ?? 0);
	const shot = $derived(view?.shots[cur]);
	const counts = $derived.by(() => {
		const c = { pick: 0, reject: 0, pending: 0 };
		for (const s of view?.shots ?? []) {
			const m = effective(s);
			if (m === 'pick') c.pick++;
			if (m === 'reject') c.reject++;
			if (s.decision === null) c.pending++;
		}
		return c;
	});

	/** The frame to compare with: the group's best pick other than the current one. */
	const partner = $derived.by(() => {
		if (!view) return undefined;
		const others = (view.groups[curGroup] ?? []).filter((i) => i !== cur);
		const by = (a: number, b: number) => view!.shots[b]!.verdict.score - view!.shots[a]!.verdict.score;
		const picks = others.filter((i) => effective(view!.shots[i]!) === 'pick').sort(by);
		return picks[0] ?? others.sort(by)[0];
	});

	// Keep the current frame visible under the active filter (compare keeps
	// its two frames even when a decision filters them out, Reframe its picks).
	$effect(() => {
		if (stage === 'cull' && mode !== 'compare' && flat.length && !flat.includes(cur))
			cur = flat.find((i) => i > cur) ?? flat.at(-1)!;
	});
	// Scroll the current tile, frame, eye crop and group into view.
	$effect(() => {
		const c = cur;
		const g = curGroup;
		void stage; // the rail and tiles appear on a stage change
		tick().then(() => {
			for (const id of [`tile-${c}`, `film-${c}`, `eyes-${c}`, `group-${g}`, `pick-${c}`])
				document.getElementById(id)?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
		});
	});
	// A new group resets the zoom and compare; within a burst the zoom stays
	// on the same spot (it is normalised, so it lines up frame to frame).
	let lastGroup = -1;
	$effect(() => {
		const g = curGroup;
		untrack(() => {
			if (g === lastGroup) return;
			lastGroup = g;
			zoom = fitZoom();
			sel = [];
			if (mode === 'compare') mode = 'full';
		});
	});

	async function load(path: string) {
		error = null;
		view = null;
		sendNote = null;
		waiting = 0;
		missing = [];
		recent = [];
		stage = 'cull';
		reframes.clear();
		progress = { done: 0, total: 0, thumb: null };
		const unlisten = await listen<Progress>('progress', (e) => {
			progress = e.payload;
			if (e.payload.thumb) recent = [...recent.slice(-47), e.payload.thumb];
		});
		try {
			view = await api.openFolder(path);
			const firstPending = view.shots.findIndex((s) => s.decision === null);
			cur = Math.max(0, firstPending);
			mode = 'grid';
			await pollDarktable(); // a queue left from an earlier Send
		} catch (e) {
			error = String(e);
		} finally {
			unlisten();
			progress = null;
		}
	}

	async function pickFolder() {
		// Start beside the open shoot, else in ~/Photography.
		const start = view ? view.folder.replace(/\/[^/]+$/, '') : await join(await homeDir(), 'Photography');
		try {
			const dir = await api.pickFolder(start);
			if (dir) await load(dir);
		} catch (e) {
			error = `open folder: ${e}`;
		}
	}

	async function decide(changes: [number, Decision | null][]) {
		if (!view || changes.length === 0) return;
		const before = changes.map(([i]) => view!.shots[i]!.decision);
		for (const [i, d] of changes) view.shots[i]!.decision = d;
		try {
			await api.setDecisions(changes.map(([i, d]) => [view!.shots[i]!.file, d]));
		} catch (e) {
			changes.forEach(([i], k) => (view!.shots[i]!.decision = before[k] ?? null));
			error = `could not save: ${e}`;
		}
	}

	async function send() {
		if (!view || sending) return;
		sending = true;
		try {
			const r = await api.sendToDarktable();
			sendNote = { written: r.written, queued: r.queued, failed: r.failed, unsent: counts.pending };
			waiting = r.queued;
			missing = r.missing;
		} catch (e) {
			error = `send failed: ${e}`;
		} finally {
			sending = false;
		}
	}

	async function pollDarktable() {
		try {
			const s = await api.darktableStatus();
			waiting = s.queued;
			if (s.missing.length) missing = [...missing, ...s.missing];
		} catch (e) {
			waiting = 0;
			error = `darktable: ${e}`;
		}
	}
	// Watch the queue until darktable has applied it.
	$effect(() => {
		if (waiting === 0 || !view) return;
		const id = setInterval(pollDarktable, 2000);
		return () => clearInterval(id);
	});

	const toggle = (s: Shot, mark: 'pick' | 'reject'): Decision => ({
		mark: s.decision?.mark === mark ? 'none' : mark
	});
	/** Mark the whole group; when it already all has that mark, take it off. */
	function markGroup(mark: 'pick' | 'reject') {
		if (!view) return;
		const g = view.groups[curGroup] ?? [];
		const all = g.every((i) => view!.shots[i]!.decision?.mark === mark);
		decide(g.map((i) => [i, { mark: all ? 'none' : mark }]));
	}

	/** Mark or unmark a frame for compare; a third mark replaces the oldest. */
	function toggleSel(i: number) {
		sel = sel.includes(i) ? sel.filter((j) => j !== i) : [...sel, i].slice(-2);
	}
	/** Click on a frame: select it, or with Ctrl/Shift mark it for compare. */
	function pickFrame(i: number, e: MouseEvent) {
		if (e.ctrlKey || e.shiftKey || e.metaKey) toggleSel(i);
		else if (mode === 'compare') setPane(i);
		else cur = i;
	}

	/** Compare the marked frames: A | B, the mark | the current frame, or the current frame | its partner. */
	function openCompare() {
		const [a, b] =
			sel.length === 2
				? sel
				: sel.length === 1 && sel[0] !== cur
					? [sel[0]!, cur]
					: [sel[0] ?? cur, partner];
		if (a === undefined || b === undefined || a === b) return;
		if (mode !== 'compare') back = mode;
		pair = [a, b];
		active = b === cur ? 1 : 0;
		cur = pair[active]!;
		mode = 'compare';
	}
	function setActive(k: number) {
		active = k;
		cur = pair[k]!;
	}
	/** Show frame `i` on the active side. */
	function setPane(i: number) {
		if (i === pair[1 - active]) return setActive(1 - active);
		pair[active] = i;
		cur = i;
	}
	/** Walk the active side through its group, skipping the other side's frame. */
	function stepPane(d: number) {
		const g = tilesOf(curGroup);
		for (let k = g.indexOf(cur) + d; k >= 0 && k < g.length; k += d) {
			if (g[k] !== pair[1 - active]) return setPane(g[k]!);
		}
	}

	function step(d: number) {
		const k = flat.indexOf(cur);
		const next = flat[Math.min(flat.length - 1, Math.max(0, k + d))];
		if (next !== undefined) cur = next;
	}
	function stepGroup(d: number) {
		const k = visibleGroups.indexOf(curGroup);
		const g = visibleGroups[Math.min(visibleGroups.length - 1, Math.max(0, k + d))];
		const first = g === undefined ? undefined : tilesOf(g)[0];
		if (first !== undefined) cur = first;
	}
	async function acceptGroup() {
		if (!view) return;
		const g = view.groups[curGroup] ?? [];
		const next = visibleGroups[visibleGroups.indexOf(curGroup) + 1];
		await decide(g.filter((i) => view!.shots[i]!.decision === null).map((i) => [i, { mark: view!.shots[i]!.verdict.mark }]));
		const first = next === undefined ? undefined : tilesOf(next)[0];
		if (first !== undefined) cur = first;
	}
	async function regroup(split: boolean) {
		if (!view || !shot) return;
		const g = view.groups[curGroup] ?? [];
		if (split && g[0] === cur) return; // already starts its group
		if (!split && curGroup === 0) return;
		const file = split ? shot.file : view.shots[g[0]!]!.file;
		try {
			view = await api.regroup(file, split);
		} catch (e) {
			error = String(e);
		}
	}

	async function setFullscreen(on: boolean) {
		fullscreen = on;
		if (on && mode === 'grid') mode = 'full';
		try {
			await getCurrentWindow().setFullscreen(on);
		} catch (e) {
			error = `fullscreen: ${e}`;
		}
	}
	// Fullscreen always shows an image, even after compare hands back to the grid.
	$effect(() => {
		if (fullscreen && mode === 'grid') mode = 'full';
	});

	/** Enter Reframe on the current frame if it is a pick, else the next pick; leave on the frame being reframed. */
	function toggleStage() {
		if (!view) return;
		if (stage === 'reframe') {
			stage = 'cull';
			return;
		}
		const p = view.shots.flatMap((s, i) => (effective(s) === 'pick' ? [i] : []));
		if (p.length === 0) {
			error = 'Nothing picked yet: pick frames with 2, then press X to reframe them.';
			return;
		}
		picks = p;
		cur = p.find((i) => i >= cur) ?? p.at(-1)!;
		if (mode === 'compare') mode = back;
		stage = 'reframe';
		prefetch();
	}
	function stepPick(d: number) {
		const k = picks.indexOf(cur);
		const next = picks[Math.min(picks.length - 1, Math.max(0, k + d))];
		if (next !== undefined) cur = next;
	}
	/** Make suggestions one frame at a time: the current pick first, then the ones after it, then before. */
	async function prefetch() {
		const folder = view?.folder;
		const gen = ++prefetchGen;
		while (gen === prefetchGen && stage === 'reframe' && view && view.folder === folder) {
			const k = Math.max(0, picks.indexOf(cur));
			const order = [...picks.slice(k), ...picks.slice(0, k).reverse()];
			const next = order
				.map((i) => view!.shots[i]!)
				.find((s) => !s.error && !reframes.has(s.file) && s.file !== reframing);
			if (!next) break;
			reframing = next.file;
			try {
				const r = await api.reframe(next.file);
				if (view?.folder === folder) reframes.set(next.file, r);
			} catch (e) {
				if (view?.folder === folder) reframes.set(next.file, { error: String(e) });
			} finally {
				reframing = null;
			}
		}
	}
	async function keepCrops(i: number, crops: [Crop, Ratio][]): Promise<boolean> {
		const s = view?.shots[i];
		if (!s) return false;
		try {
			s.crops = await api.setCrops(s.file, crops);
			return true;
		} catch (e) {
			error = `could not save crops: ${e}`;
			return false;
		}
	}

	function onkeydown(e: KeyboardEvent) {
		if (e.ctrlKey && e.key === 'o') {
			e.preventDefault();
			pickFolder();
			return;
		}
		if (view && shot && stage === 'reframe') {
			const plain = !e.ctrlKey && !e.altKey && !e.metaKey;
			const toCull = plain && ((e.code === 'Digit2' && !e.shiftKey) || (e.code === 'KeyR' && !e.shiftKey) || (e.code === 'KeyF' && e.shiftKey));
			if (!toCull) {
				if (plain && e.code === 'KeyX') toggleStage();
				else if (reframer?.handleKey(e)) {
					// handled
				} else if (plain && e.key === 'Escape') toggleStage();
				else return;
				e.preventDefault();
				return;
			}
		}
		if (!view || !shot || e.ctrlKey || e.altKey || e.metaKey) return;
		const k = e.key;
		const compare = mode === 'compare';
		// Pick and reject by physical key, so Shift and the keyboard layout don't matter.
		if (e.code === 'Digit2') e.shiftKey ? markGroup('pick') : decide([[cur, toggle(shot, 'pick')]]);
		else if (e.code === 'KeyR') e.shiftKey ? markGroup('reject') : decide([[cur, toggle(shot, 'reject')]]);
		else if (k === 'ArrowRight') compare ? stepPane(1) : step(1);
		else if (k === 'ArrowLeft') compare ? stepPane(-1) : step(-1);
		else if (k === 'ArrowDown') stepGroup(1);
		else if (k === 'ArrowUp') stepGroup(-1);
		else if (k === 'Tab' && compare) setActive(1 - active);
		else if (k === 'u' || k === 'U') decide([[cur, null]]);
		else if (k === 'Enter') acceptGroup();
		else if (e.code === 'KeyF' && e.shiftKey) setFullscreen(!fullscreen);
		else if (e.code === 'KeyF') fullscreen ? setFullscreen(false) : (mode = mode === 'full' ? 'grid' : 'full');
		else if (k === 'm' || k === 'M') toggleSel(cur);
		else if (k === 'c' || k === 'C') compare ? (mode = back) : openCompare();
		else if (k === 'e' || k === 'E') eyesMode = !eyesMode;
		else if (k === 'b' || k === 'B') boost = !boost;
		else if (k === 's' || k === 'S') regroup(true);
		else if (k === 'j' || k === 'J') regroup(false);
		else if (e.code === 'KeyX') toggleStage();
		else if (k === 'Escape') {
			if (fullscreen) setFullscreen(false);
			else if (compare) mode = back;
			else if (mode === 'full') mode = 'grid';
			else sel = [];
		} else return;
		e.preventDefault();
	}

	onMount(async () => {
		const dir = await api.initialFolder();
		if (dir) await load(dir);
	});

	const folderName = $derived(view?.folder.split('/').at(-1) ?? '');
	const keptCount = $derived(picks.reduce((n, i) => n + (view?.shots[i]?.crops.length ?? 0), 0));
	const reframeData = $derived(shot ? reframes.get(shot.file) : undefined);
	const groupFrames = $derived(view?.groups[curGroup] ?? []);
	/** 1-based place of the current frame in its whole group. */
	const posInGroup = $derived(groupFrames.indexOf(cur) + 1);
	const shotName = (i: number) => view?.shots[i]?.file.replace(/\.[^.]+$/, '').split('-').at(-1) ?? '';
	const groupCounts = (gi: number) => {
		const c = { pick: 0, reject: 0, pending: 0 };
		for (const i of view?.groups[gi] ?? []) {
			const s = view!.shots[i]!;
			const m = effective(s);
			if (m === 'pick') c.pick++;
			if (m === 'reject') c.reject++;
			if (s.decision === null) c.pending++;
		}
		return c;
	};
	const cover = (gi: number): Shot | undefined => {
		const g = view?.groups[gi] ?? [];
		const i = g.find((i) => effective(view!.shots[i]!) === 'pick') ?? g[0];
		return i === undefined ? undefined : view?.shots[i];
	};
</script>

<svelte:window {onkeydown} />

<div class="app">
	{#if !fullscreen}
		<header>
			<strong class="logo">sift</strong>
			{#if view}
				<button class="folder" onclick={pickFolder} title="{view.folder}&#10;Click or Ctrl+O to open another folder">
					{folderName} ▾
				</button>
				<div class="filters" role="group" aria-label="stage">
					<button class:on={stage === 'cull'} onclick={() => stage === 'reframe' && toggleStage()} title="X">Cull</button>
					<button class:on={stage === 'reframe'} onclick={() => stage === 'cull' && toggleStage()} title="X">
						Reframe ({stage === 'reframe' ? picks.length : counts.pick})
					</button>
				</div>
				{#if stage === 'reframe'}
					<span class="counts">
						<span><b>{picks.indexOf(cur) + 1}</b> / {picks.length}</span>
						<span class="pick">{keptCount} crop{keptCount === 1 ? '' : 's'} kept</span>
					</span>
				{:else}
					<div class="filters" role="group" aria-label="filter">
						{#each ['all', 'pending', 'picked', 'rejected'] as const as f (f)}
							<button class:on={filter === f} onclick={() => (filter = f)}>{f}</button>
						{/each}
					</div>
					<span class="counts">
						<span class="pick">★ {counts.pick}</span>
						<span class="reject">✗ {counts.reject}</span>
						<span>{counts.pending} to review</span>
						<span class="muted">of {view.shots.length}</span>
					</span>
				{/if}
				<button
					class="send"
					onclick={send}
					disabled={sending}
					title="Send confirmed decisions; {counts.pending} unreviewed frame(s) are left out"
				>
					{sending ? 'Sending…' : 'Send to darktable'}
				</button>
			{/if}
		</header>
	{/if}

	{#if error}
		<div class="error" role="alert">
			{error} <button onclick={() => (error = null)}>dismiss</button>
		</div>
	{/if}

	{#if view && !fullscreen && (sendNote || waiting > 0 || missing.length > 0)}
		<div class="sendbar" role="status">
			{#if sendNote}
				{#if sendNote.written + sendNote.queued + sendNote.failed.length === 0}
					<span>Nothing new to send</span>
				{/if}
				{#if sendNote.written}<span>{sendNote.written} XMP written</span>{/if}
				{#if sendNote.failed.length}
					<span class="reject" title={sendNote.failed.map(([f, why]) => `${f}: ${why}`).join('\n')}>
						{sendNote.failed.length} failed
					</span>
				{/if}
			{/if}
			{#if waiting > 0}
				<span class="wait">
					{waiting} waiting for darktable: press <em>apply sift decisions</em> in the lighttable (sift panel)
				</span>
			{:else if sendNote?.queued}
				<span class="pick">{sendNote.queued} applied in darktable ✓</span>
			{/if}
			{#if missing.length}
				<span class="reject" title={missing.join('\n')}>{missing.length} no longer in darktable</span>
			{/if}
			{#if sendNote?.unsent}<span class="muted">{sendNote.unsent} unreviewed not sent</span>{/if}
			<button
				onclick={() => {
					sendNote = null;
					missing = [];
				}}>dismiss</button
			>
		</div>
	{/if}

	{#if progress}
		<div class="loading">
			<p>Analysing… {progress.done} / {progress.total || '?'}</p>
			<progress max={progress.total || 1} value={progress.done}></progress>
			<div class="recent">
				{#each recent as t (t)}<img src={src(t)} alt="" />{/each}
			</div>
		</div>
	{:else if !view}
		<div class="welcome">
			<p>Open a shoot folder to cull it.</p>
			<button class="open" onclick={pickFolder}>Open folder…</button>
			<p class="muted">or press <kbd>Ctrl+O</kbd></p>
		</div>
	{:else}
		<div class="body">
			{#if stage === 'reframe' && shot}
				{#if !fullscreen}
					<nav class="groups" aria-label="picks">
						{#each picks as i (i)}
							{@const s = view.shots[i]!}
							{@const r = reframes.get(s.file)}
							<button id="pick-{i}" class="group" class:on={i === cur} class:unpicked={effective(s) !== 'pick'} onclick={() => (cur = i)}>
								{#if !s.error}<img src={src(s.thumb)} alt="" loading="lazy" />{/if}
								<span class="gmeta">
									<span class="pname">#{shotName(i)}</span>
									<span class="gcounts">
										{#if s.crops.length}<span class="pick">✂{s.crops.length}</span>{/if}
										{#if r && 'faces' in r && r.faces.length === 0}<span class="muted" title="no face found">∅</span>{/if}
										{#if r && 'error' in r}<span class="reject" title={r.error}>!</span>{/if}
										{#if reframing === s.file}<span class="spin" title="finding crops"></span>{/if}
									</span>
								</span>
							</button>
						{/each}
					</nav>
				{/if}
				<main>
					{#key shot.file}
						<Reframe
							bind:this={reframer}
							{shot}
							data={reframeData && 'faces' in reframeData ? reframeData : undefined}
							error={reframeData && 'error' in reframeData ? reframeData.error : null}
							minLong={view.minLong}
							onkeep={(list) => keepCrops(cur, list)}
							onnext={() => stepPick(1)}
							onprev={() => stepPick(-1)}
						/>
					{/key}
				</main>
			{:else}
			{#if !fullscreen}
				<nav class="groups" aria-label="groups">
					{#each visibleGroups as gi (gi)}
						{@const c = groupCounts(gi)}
						{@const cv = cover(gi)}
						<button
							id="group-{gi}"
							class="group"
							class:on={gi === curGroup}
							class:done={c.pending === 0}
							onclick={() => {
								const f = tilesOf(gi)[0];
								if (f !== undefined) cur = f;
							}}
						>
							{#if cv && !cv.error}<img src={src(cv.thumb)} alt="" loading="lazy" />{/if}
							<span class="gmeta">
								<span>Group {gi + 1} · {view.groups[gi]?.length}</span>
								<span class="gcounts">
									<span class="pick">★{c.pick}</span>
									<span class="reject">✗{c.reject}</span>
									{#if c.pending}<span class="muted">?{c.pending}</span>{:else}<span class="muted">✓</span>{/if}
								</span>
							</span>
						</button>
					{/each}
				</nav>
			{/if}

			<main>
				{#if !fullscreen}
					{#key curGroup}
						{@const c = groupCounts(curGroup)}
						<div class="ghead">
							<strong>Group {curGroup + 1}</strong>
							<span>frame <b>{posInGroup}</b> of {groupFrames.length}</span>
							{#if groupFrames.length > 1 && posInGroup === 1}<span class="edge">first</span>{/if}
							{#if groupFrames.length > 1 && posInGroup === groupFrames.length}<span class="edge">last</span>{/if}
							<span class="gcounts">
								<span class="pick">★{c.pick}</span>
								<span class="reject">✗{c.reject}</span>
								{#if c.pending}<span class="muted">{c.pending} to review</span>{/if}
							</span>
							{#if mode === 'compare'}
								<span class="hint">
									comparing #{shotName(pair[0])} | #{shotName(pair[1])} · <kbd>Tab</kbd> switch side ·
									<kbd>←→</kbd> change the {active === 0 ? 'left' : 'right'} side
								</span>
							{:else if sel.length}
								<span class="hint">
									A #{shotName(sel[0]!)}{#if sel[1] !== undefined} · B #{shotName(sel[1])}{/if} ·
									<kbd>C</kbd> compare {sel.length === 2 ? 'A | B' : 'A | this frame'}
								</span>
							{:else}
								<span class="hint muted">Ctrl-click or <kbd>M</kbd> marks frames to compare</span>
							{/if}
						</div>
					{/key}
				{/if}
				{#if mode !== 'grid' && shot}
					<div class="zoom">
						<ZoomView
							panes={mode === 'compare' ? pair.map((i) => view!.shots[i]!) : [shot]}
							eyes={eyesMode}
							{boost}
							bind:zoom
							{active}
							onactivate={(k) => mode === 'compare' && setActive(k)}
						/>
						{#if fullscreen}
							<div class="fspos">Group {curGroup + 1} · {posInGroup} / {groupFrames.length}</div>
						{/if}
					</div>
					{#if !fullscreen}
						<Filmstrip
							shots={groupFrames.map((i) => ({ shot: view!.shots[i]!, index: i }))}
							current={cur}
							{sel}
							onselect={pickFrame}
						/>
					{/if}
				{:else}
					<div class="tiles">
						{#each tilesOf(curGroup) as i (i)}
							{@const m = sel.indexOf(i)}
							<Tile
								shot={view.shots[i]!}
								index={i}
								pos="{groupFrames.indexOf(i) + 1}/{groupFrames.length}"
								mark={m < 0 ? null : m === 0 ? 'A' : 'B'}
								current={i === cur}
								onselect={(e) => pickFrame(i, e)}
								onopen={() => {
									cur = i;
									mode = 'full';
								}}
							/>
						{/each}
					</div>
				{/if}
				{#if !fullscreen && tilesOf(curGroup).some((i) => judgeable(view!.shots[i]!))}
					<EyeStrip
						shots={tilesOf(curGroup).map((i) => ({ shot: view!.shots[i]!, index: i }))}
						current={cur}
						{sel}
						{boost}
						onselect={pickFrame}
					/>
				{/if}
			</main>
			{/if}
		</div>
		{#if !fullscreen && stage === 'reframe'}
			<footer>
				{#if shot}<span class="now"><Badge {shot} /> {shot.file}</span>{/if}
				<span class="keys">
					<kbd>←→</kbd> pick <kbd>↑↓</kbd> suggestion <kbd>Enter</kbd> keep <kbd>⇧Enter</kbd> keep+next
					<kbd>A</kbd> ratio <kbd>O</kbd> orient <kbd>−</kbd>/<kbd>=</kbd> size <kbd>Ctrl+arrows</kbd> nudge
					<kbd>Z</kbd> result <kbd>Del</kbd> remove <kbd>2</kbd> pick <kbd>X</kbd>/<kbd>Esc</kbd> cull
				</span>
			</footer>
		{:else if !fullscreen}
			<footer>
				{#if shot}<span class="now"><Badge {shot} /> {shot.file}</span>{/if}
				<span class="keys">
					<kbd>←→</kbd> frame <kbd>↑↓</kbd> group <kbd>2</kbd> pick <kbd>R</kbd> reject
					<kbd>⇧2</kbd>/<kbd>⇧R</kbd> group <kbd>U</kbd> clear <kbd>Enter</kbd> accept group
					<kbd>F</kbd> full <kbd>⇧F</kbd> fullscreen <kbd>M</kbd> mark <kbd>C</kbd> compare <kbd>E</kbd> eyes {eyesMode ? 'on' : 'off'}
					<kbd>B</kbd> boost {boost ? 'on' : 'off'} <kbd>S</kbd> split <kbd>J</kbd> join <kbd>X</kbd> reframe
				</span>
			</footer>
		{/if}
	{/if}
</div>

<style>
	:global(:root) {
		--bg: #141518;
		--panel: #1d1f24;
		--panel-hi: #272a31;
		--line: #2c2f36;
		--text: #e6e6e6;
		--muted: #8b8f98;
		--accent: #e8b04a;
		--pick: #4cc26a;
		--reject: #e5534b;
		--chip: #2b2e35;
		--strip-h: clamp(150px, 26vh, 300px);
		color-scheme: dark;
	}
	:global(html, body) {
		margin: 0;
		height: 100%;
		background: var(--bg);
		color: var(--text);
		font: 14px/1.4 system-ui, sans-serif;
		overflow: hidden;
	}
	:global(button) {
		font: inherit;
		color: inherit;
		background: var(--panel-hi);
		border: 1px solid var(--line);
		border-radius: 6px;
		padding: 0.3rem 0.7rem;
		cursor: pointer;
	}
	:global(button:disabled) {
		opacity: 0.45;
		cursor: default;
	}
	.app {
		display: flex;
		flex-direction: column;
		height: 100vh;
	}
	header {
		display: flex;
		gap: 1rem;
		align-items: center;
		padding: 0.5rem 0.8rem;
		border-bottom: 1px solid var(--line);
		background: var(--panel);
	}
	.logo {
		color: var(--accent);
		letter-spacing: 0.05em;
	}
	.folder {
		font-weight: 600;
		background: transparent;
		border-color: transparent;
	}
	.folder:hover {
		border-color: var(--line);
	}
	.welcome .open {
		font-size: 1.3rem;
		padding: 0.9rem 2.6rem;
		border-radius: 10px;
		background: var(--accent);
		border-color: var(--accent);
		color: #111;
		font-weight: 600;
	}
	.welcome .open:hover {
		filter: brightness(1.08);
	}
	.filters {
		display: flex;
	}
	.filters button {
		border-radius: 0;
		text-transform: capitalize;
	}
	.filters button:first-child {
		border-radius: 6px 0 0 6px;
	}
	.filters button:last-child {
		border-radius: 0 6px 6px 0;
	}
	.filters button.on {
		background: var(--accent);
		color: #111;
	}
	.counts {
		display: flex;
		gap: 0.8rem;
		font-variant-numeric: tabular-nums;
	}
	.send {
		margin-left: auto;
	}
	.pick {
		color: var(--pick);
	}
	.reject {
		color: var(--reject);
	}
	.muted {
		color: var(--muted);
	}
	.error {
		background: color-mix(in srgb, var(--reject) 25%, var(--bg));
		padding: 0.4rem 0.8rem;
	}
	.sendbar {
		display: flex;
		flex-wrap: wrap;
		gap: 0.3rem 1rem;
		align-items: center;
		padding: 0.3rem 0.8rem;
		border-bottom: 1px solid var(--line);
		background: var(--panel);
		font-size: 0.85rem;
	}
	.sendbar button {
		margin-left: auto;
		padding: 0 0.5rem;
	}
	.wait {
		color: var(--accent);
	}
	.loading,
	.welcome {
		flex: 1;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 1rem;
	}
	.loading progress {
		width: min(40rem, 80vw);
	}
	.recent {
		display: flex;
		flex-wrap: wrap;
		gap: 4px;
		justify-content: center;
		max-width: 80vw;
	}
	.recent img {
		height: 64px;
	}
	.body {
		flex: 1;
		display: flex;
		min-height: 0;
	}
	.groups {
		width: 15rem;
		flex: none;
		overflow-y: auto;
		border-right: 1px solid var(--line);
		padding: 0.4rem;
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
	}
	.group {
		display: flex;
		gap: 0.5rem;
		align-items: center;
		text-align: left;
		padding: 0.3rem;
		background: transparent;
		border-color: transparent;
	}
	.group.on {
		background: var(--panel-hi);
		border-color: var(--accent);
	}
	.group.done {
		opacity: 0.6;
	}
	.group.unpicked .pname {
		text-decoration: line-through;
		color: var(--muted);
	}
	.spin {
		display: inline-block;
		width: 0.65rem;
		height: 0.65rem;
		border: 2px solid var(--muted);
		border-top-color: transparent;
		border-radius: 50%;
		animation: spin 0.8s linear infinite;
		align-self: center;
	}
	@keyframes spin {
		to {
			transform: rotate(360deg);
		}
	}
	.group img {
		width: 4.5rem;
		height: 3rem;
		object-fit: cover;
		border-radius: 3px;
		flex: none;
	}
	.gmeta {
		display: flex;
		flex-direction: column;
		font-size: 0.8rem;
	}
	.gcounts {
		display: flex;
		gap: 0.5rem;
	}
	main {
		flex: 1;
		display: flex;
		flex-direction: column;
		min-width: 0;
	}
	.tiles {
		flex: 1;
		overflow-y: auto;
		padding: 0.6rem;
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(17rem, 1fr));
		gap: 0.6rem;
		align-content: start;
	}
	.zoom {
		position: relative;
		flex: 1;
		min-height: 0;
	}
	.fspos {
		position: absolute;
		top: 0.5rem;
		right: 0.7rem;
		font-size: 0.8rem;
		color: #ddd;
		text-shadow: 0 0 3px #000;
		font-variant-numeric: tabular-nums;
		pointer-events: none;
	}
	.ghead {
		display: flex;
		flex-wrap: wrap;
		gap: 0.3rem 1rem;
		align-items: center;
		padding: 0.35rem 0.8rem;
		border-bottom: 1px solid var(--line);
		background: var(--panel);
		font-size: 0.85rem;
		font-variant-numeric: tabular-nums;
		animation: arrive 0.6s ease-out;
	}
	/* Replayed on every group change ({#key}), so crossing into a new group shows. */
	@keyframes arrive {
		from {
			background: color-mix(in srgb, var(--accent) 35%, var(--panel));
		}
	}
	.ghead b {
		color: var(--accent);
	}
	.edge {
		font-size: 0.72rem;
		padding: 0 0.45rem;
		border-radius: 99px;
		border: 1px solid var(--accent);
		color: var(--accent);
	}
	.hint {
		margin-left: auto;
		font-size: 0.78rem;
	}
	footer {
		display: flex;
		gap: 1.5rem;
		align-items: center;
		padding: 0.3rem 0.8rem;
		border-top: 1px solid var(--line);
		background: var(--panel);
		font-size: 0.75rem;
		color: var(--muted);
	}
	.now {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		color: var(--text);
		white-space: nowrap;
	}
	kbd {
		background: var(--chip);
		border-radius: 3px;
		padding: 0 0.3rem;
		color: var(--text);
		font: inherit;
	}
</style>
