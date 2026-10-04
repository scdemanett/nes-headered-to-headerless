import { invoke } from '@tauri-apps/api/core';

export interface Settings {
  searchDirs: string[];
  outputDir: string;
  dryRun: boolean;
}

export interface SettingsInfo {
  settings: Settings;
  settingsPath: string;
  portable: boolean;
}

export interface Written {
  name: string;
  source: string;
}

export interface Missing {
  name: string;
  crc: string;
  size: number;
}

export interface DatResult {
  fixdat: string;
  needed: number;
  written: Written[];
  present: string[];
  missing: Missing[];
  problems: string[];
  error: string | null;
}

export interface RunResult {
  dats: DatResult[];
  filesIndexed: number;
  filesCached: number;
  dryRun: boolean;
  elapsedMs: number;
  warnings: string[];
}

export interface Progress {
  done: number;
  total: number;
}

export const api = {
  getSettings: () => invoke<SettingsInfo>('get_settings'),
  saveSettings: (settings: Settings) => invoke<void>('save_settings', { settings }),
  takePendingDats: () => invoke<string[]>('take_pending_dats'),
  classifyPaths: (paths: string[]) =>
    invoke<{ files: string[]; dirs: string[] }>('classify_paths', { paths }),
  clearIndex: () => invoke<void>('clear_index'),
  openFolder: (path: string) => invoke<void>('open_folder', { path }),
  runFixdats: (paths: string[]) => invoke<RunResult>('run_fixdats', { paths }),
};
