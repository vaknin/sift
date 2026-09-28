<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { listen } from '@tauri-apps/api/event';
	import { homeDir, join } from '@tauri-apps/api/path';
	import { open as openDialog } from '@tauri-apps/plugin-dialog';
	import * as api from '$lib/api';
	import { effective, judgeable, matches, src } from '$lib/api';
	import type { Decision, Filter, Progress, Shot, View } from '$lib/types';
	import Tile from '$lib/Tile.svelte';
	import EyeStrip from '$lib/EyeStrip.svelte';
	import Badge from '$lib/Badge.svelte';
	import ZoomView, { fitZoom, type Zoom } from '$lib/ZoomView.svelte';

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
	let sending = $state(false);
	/** Outcome of the last Send. */
	let sendNote = $state<{ written: number; queued: number; failed: [string, string][]; unsent: number } | null>(null);
	/** Changes waiting for "apply sift decisions" in darktable. */
	let waiting = $state(0);
	/** Files sift.lua couldn't find in darktable. */
	let missing = $state<string[]>([]);

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

	// Keep the current frame visible under the active filter.
	$effect(() => {
		if (flat.length && !flat.includes(cur)) cur = flat.find((i) => i > cur) ?? flat.at(-1)!;
	});
	// Scroll the current tile, eye crop and group into view; reset the zoom.
	$effect(() => {
		const c = cur;
		const g = curGroup;
		zoom = fitZoom();
		tick().then(() => {
			for (const id of [`tile-${c}`, `eyes-${c}`, `group-${g}`])
				document.getElementById(id)?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
		});
	});

	async function load(path: string) {
		error = null;
		view = null;
		sendNote = null;
		waiting = 0;
		missing = [];
		recent = [];
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
		const home = await homeDir();
		const dir = await openDialog({ directory: true, defaultPath: await join(home, 'Photography') });
		if (typeof dir === 'string') await load(dir);
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
		mark: s.decision?.mark === mark ? 'none' : mark,
		...(s.decision?.stars ? { stars: s.decision.stars } : {})
	});
	function stars(s: Shot, n: number): Decision {
		const mark = s.decision?.mark ?? (s.verdict.mark === 'reject' ? 'none' : s.verdict.mark);
		return s.decision?.stars === n ? { mark } : { mark, stars: n };
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

	function onkeydown(e: KeyboardEvent) {
		if (e.ctrlKey && e.key === 'o') {
			e.preventDefault();
			pickFolder();
			return;
		}
		if (!view || !shot || e.ctrlKey || e.altKey || e.metaKey) return;
		const k = e.key;
		if (k === 'ArrowRight') step(1);
		else if (k === 'ArrowLeft') step(-1);
		else if (k === 'ArrowDown') stepGroup(1);
		else if (k === 'ArrowUp') stepGroup(-1);
		else if (k === 'p' || k === 'P') decide([[cur, toggle(shot, 'pick')]]);
		else if (k === 'x' || k === 'X') decide([[cur, toggle(shot, 'reject')]]);
		else if (k === 'u' || k === 'U') decide([[cur, null]]);
		else if (k >= '1' && k <= '5') decide([[cur, stars(shot, Number(k))]]);
		else if (k === 'Enter') acceptGroup();
		else if (k === ' ') mode = mode === 'full' ? 'grid' : 'full';
		else if (k === 'c' || k === 'C') mode = mode === 'compare' || partner === undefined ? 'grid' : 'compare';
		else if (k === 'e' || k === 'E') eyesMode = !eyesMode;
		else if (k === 'b' || k === 'B') boost = !boost;
		else if (k === 's' || k === 'S') regroup(true);
		else if (k === 'j' || k === 'J') regroup(false);
		else if (k === 'Escape') mode = 'grid';
		else return;
		e.preventDefault();
	}

	onMount(async () => {
		const dir = await api.initialFolder();
		if (dir) await load(dir);
	});

	const folderName = $derived(view?.folder.split('/').at(-1) ?? '');
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
	<header>
		<strong class="logo">sift</strong>
		<button onclick={pickFolder} title="Ctrl+O">Open folder…</button>
		{#if view}
			<span class="folder" title={view.folder}>{folderName}</span>
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

	{#if error}
		<div class="error" role="alert">
			{error} <button onclick={() => (error = null)}>dismiss</button>
		</div>
	{/if}

	{#if view && (sendNote || waiting > 0 || missing.length > 0)}
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
			<button onclick={pickFolder}>Open folder…</button>
		</div>
	{:else}
		<div class="body">
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

			<main>
				{#if mode !== 'grid' && shot}
					<div class="zoom">
						<ZoomView
							panes={mode === 'compare' && partner !== undefined ? [shot, view.shots[partner]!] : [shot]}
							eyes={eyesMode}
							{boost}
							bind:zoom
						/>
					</div>
				{:else}
					<div class="tiles">
						{#each tilesOf(curGroup) as i (i)}
							<Tile
								shot={view.shots[i]!}
								index={i}
								current={i === cur}
								onselect={() => (cur = i)}
								onopen={() => {
									cur = i;
									mode = 'full';
								}}
							/>
						{/each}
					</div>
				{/if}
				{#if tilesOf(curGroup).some((i) => judgeable(view!.shots[i]!))}
					<EyeStrip
						shots={tilesOf(curGroup).map((i) => ({ shot: view!.shots[i]!, index: i }))}
						current={cur}
						{boost}
						onselect={(i) => (cur = i)}
					/>
				{/if}
			</main>
		</div>
		<footer>
			{#if shot}<span class="now"><Badge {shot} /> {shot.file}</span>{/if}
			<span class="keys">
				<kbd>←→</kbd> frame <kbd>↑↓</kbd> group <kbd>P</kbd> pick <kbd>X</kbd> reject <kbd>U</kbd> clear
				<kbd>1–5</kbd> stars <kbd>Enter</kbd> accept group <kbd>Space</kbd> full <kbd>C</kbd> compare
				<kbd>E</kbd> eyes {eyesMode ? 'on' : 'off'} <kbd>B</kbd> boost {boost ? 'on' : 'off'} <kbd>S</kbd> split
				<kbd>J</kbd> join
			</span>
		</footer>
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
		--star: #f1c94b;
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
		flex: 1;
		min-height: 0;
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
