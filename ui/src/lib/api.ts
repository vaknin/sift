import { convertFileSrc, invoke } from '@tauri-apps/api/core';
import type { Crop, DarktableStatus, Decision, KeptCrop, Mark, Note, Ratio, ReframeView, Report, Shot, View } from './types';

export const initialFolder = () => invoke<string | null>('initial_folder');
/** The folder opened last, for Continue; null when none or it is gone. */
export const lastFolder = () => invoke<string | null>('last_folder');
/** Folder chooser floating over the window; null when cancelled. */
export const pickFolder = (start: string | null) => invoke<string | null>('pick_folder', { start });
export const openFolder = (path: string) => invoke<View>('open_folder', { path });
export const setDecisions = (changes: [string, Decision | null][]) => invoke<void>('set_decisions', { changes });
export const regroup = (file: string, split: boolean) => invoke<View>('regroup', { file, split });
export const sendToDarktable = () => invoke<Report>('send_to_darktable');
export const darktableStatus = () => invoke<DarktableStatus>('darktable_status');
/** Crop suggestions; the first call for a frame takes a second or so. */
export const reframe = (file: string) => invoke<ReframeView>('reframe', { file });
/** Replace a frame's kept crops; returns them with their ids. */
export const setCrops = (file: string, crops: [Crop, Ratio][]) => invoke<KeptCrop[]>('set_crops', { file, crops });

/** Name a person (remembered in every folder); an existing name merges, empty forgets. */
export const namePerson = (id: number, name: string) => invoke<View>('name_person', { id, name });
/** Take a face out of its person (0) or give it to a named person. */
export const assignFace = (file: string, face: number, person: number) =>
	invoke<View>('assign_face', { file, face, person });

export const src = (path: string) => convertFileSrc(path);

/** The user's decision when there is one, else the pre-mark. */
export const effective = (s: Shot): Mark => s.decision?.mark ?? s.verdict.mark;

export const matches = (s: Shot, f: 'all' | 'pending' | 'picked' | 'rejected'): boolean =>
	f === 'all' ||
	(f === 'pending' && s.decision === null) ||
	(f === 'picked' && s.decision?.mark === 'pick') ||
	(f === 'rejected' && s.decision?.mark === 'reject');

const REASON_TEXT: Record<string, string> = {
	'eyes-closed': 'eyes closed',
	'soft-eyes': 'soft eyes',
	'no-face': 'no face',
	'face-hidden': 'face hidden',
	'small-face': 'small face',
	'face-cut': 'face cut',
	sharpest: 'sharpest',
	smile: 'smile',
	'soft-frame': 'soft'
};
export const reasonText = (r: string) => REASON_TEXT[r] ?? r;
export const reasonTone = (r: string): 'bad' | 'good' | 'info' =>
	r === 'eyes-closed' || r === 'soft-eyes' || r === 'soft-frame' || r === 'face-cut'
		? 'bad'
		: r === 'sharpest' || r === 'smile'
			? 'good'
			: 'info';

/** CSS brightness that lifts a low-key face to a readable level for inspection. */
export const lift = (s: Shot): number => {
	const l = s.faces[0]?.faceLuma;
	return l === undefined || l <= 0 ? 1 : Math.min(3, Math.max(1, 110 / l));
};

/** Whether the frame has a face the analysis could judge (eye crop worth showing). */
export const judgeable = (s: Shot): boolean =>
	s.eyes.length > 0 && !s.verdict.reasons.some((r) => r === 'no-face' || r === 'face-hidden' || r === 'small-face');

const NOTE_TEXT: Record<Note, string> = {
	original: 'as shot',
	'eyes-on-third': 'eyes on third',
	'lead-room': 'lead room',
	centered: 'centred',
	'clean-edges': 'clean edges',
	'subject-pops': 'subject pops'
};
export const noteText = (n: Note) => NOTE_TEXT[n] ?? n;
export const ratioText = (r: Ratio) => `${r[0]}:${r[1]}`;

/** Long side of `crop` in pixels of a `width` × `height` frame (`Crop::long_side`). */
export const longSide = (crop: Crop, width: number, height: number): number =>
	Math.round(Math.max(crop.w * width, crop.h * height));
