// Mirrors the serde shapes in src-tauri/src/lib.rs and sift-engine.

export type Mark = 'pick' | 'reject' | 'none';

export type Reason =
	| 'eyes-closed'
	| 'soft-eyes'
	| 'no-face'
	| 'face-hidden'
	| 'small-face'
	| 'face-cut'
	| 'sharpest'
	| 'smile'
	| 'soft-frame';

export interface Verdict {
	mark: Mark;
	reasons: Reason[];
	score: number;
	rel_sharp: number;
}

export interface Decision {
	mark: Mark;
}

/** One eye, classified with the culling thresholds (`cull::EyeState`). */
export type EyeState = 'open' | 'half' | 'closed';

export interface Face {
	blink: [number, number];
	/** Subject's right, left. */
	eyes: [EyeState, EyeState];
	eyeSharp: [number, number];
	presence: number;
	smile: number;
	faceLuma: number;
}

export interface Shot {
	file: string;
	time: number;
	width: number;
	height: number;
	error: string | null;
	verdict: Verdict;
	decision: Decision | null;
	display: string;
	thumb: string;
	eyes: string[];
	faces: Face[];
}

export interface View {
	folder: string;
	shots: Shot[];
	groups: number[][];
}

export interface Progress {
	done: number;
	total: number;
	thumb: string | null;
}

export type Filter = 'all' | 'pending' | 'picked' | 'rejected';

/** Result of a Send (`export::Report`). */
export interface Report {
	/** Sidecars written for files darktable hasn't imported. */
	written: number;
	/** Changes waiting for sift.lua (files already in darktable). */
	queued: number;
	/** [file, reason] for sidecars that couldn't be written. */
	failed: [string, string][];
	/** Files sift.lua no longer found in darktable. */
	missing: string[];
}

export interface DarktableStatus {
	queued: number;
	missing: string[];
}
