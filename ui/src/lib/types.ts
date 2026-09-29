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
	/** Id of the person this face belongs to (`View.people`). */
	person: number | null;
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
	/** Crops kept in Reframe. */
	crops: KeptCrop[];
}

export interface View {
	folder: string;
	shots: Shot[];
	groups: number[][];
	/** Smallest long side a Reframe crop may have, full-resolution pixels. */
	minLong: number;
	/** People in the folder, named first, then by photo count. */
	people: Person[];
}

export interface Person {
	id: number;
	/** Null until named; shown as "Person N". */
	name: string | null;
	/** Photos they are in. */
	photos: number;
	/** Their clearest face, aligned square crop. */
	face: string;
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

/** x, y, w, h as fractions of the upright frame (`reframe::Crop`). */
export interface Crop {
	x: number;
	y: number;
	w: number;
	h: number;
}

/** Width : height. */
export type Ratio = [number, number];

/** Why a suggestion scored well (`reframe::Note`). */
export type Note = 'original' | 'eyes-on-third' | 'lead-room' | 'centered' | 'clean-edges' | 'subject-pops';

export interface Suggestion {
	crop: Crop;
	ratio: Ratio;
	score: number;
	notes: Note[];
}

export interface KeptCrop {
	id: string;
	crop: Crop;
	ratio: Ratio;
}

/** A face in frame fractions. */
export interface FaceBox {
	/** x, y, w, h. */
	bbox: [number, number, number, number];
	/** Subject's right, left. */
	eyes: [[number, number], [number, number]];
	chin: [number, number];
	forehead: [number, number];
}

export interface ReframeView {
	/** Full-resolution size, upright. */
	width: number;
	height: number;
	/** Best first; the last is the frame as shot. */
	suggestions: Suggestion[];
	/** Largest first; the first is the subject. */
	faces: FaceBox[];
	/** Space around the face box, in face heights: sides, top, bottom. */
	margin: [number, number, number];
	/** Band under the chin a bottom edge shouldn't cut, in face heights. */
	chinBand: number;
}
