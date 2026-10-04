import { listen } from '@tauri-apps/api/event';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { open } from '@tauri-apps/plugin-dialog';
import { useCallback, useEffect, useRef, useState } from 'react';
import { api, type DatResult, type Progress, type RunResult, type Settings } from './api';

const EMPTY_SETTINGS: Settings = { searchDirs: [], outputDir: '', dryRun: false };

interface Run extends RunResult {
  id: number;
}

function baseName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

function plural(n: number, word: string): string {
  return `${n} ${word}${n === 1 ? '' : 's'}`;
}

export function App() {
  const [settings, setSettings] = useState<Settings>(EMPTY_SETTINGS);
  const [settingsPath, setSettingsPath] = useState('');
  const [portable, setPortable] = useState(false);
  const [running, setRunning] = useState(false);
  const [progress, setProgress] = useState<Progress | null>(null);
  const [runs, setRuns] = useState<Run[]>([]);
  const [notice, setNotice] = useState<string | null>(null);
  const [dragOver, setDragOver] = useState(false);

  // Event listeners outlive renders, so they read the latest state from refs.
  const settingsRef = useRef(settings);
  const queueRef = useRef<string[]>([]);
  const runningRef = useRef(false);
  const runIdRef = useRef(0);

  const updateSettings = useCallback((next: Settings) => {
    settingsRef.current = next;
    setSettings(next);
    return api.saveSettings(next).catch((e) => setNotice(`Could not save settings: ${e}`));
  }, []);

  const drainQueue = useCallback(async () => {
    if (runningRef.current) return;
    runningRef.current = true;
    setRunning(true);
    try {
      while (queueRef.current.length > 0) {
        const batch = queueRef.current.splice(0);
        setProgress(null);
        const result = await api.runFixdats(batch);
        setRuns((prev) => [{ ...result, id: ++runIdRef.current }, ...prev]);
      }
    } catch (e) {
      queueRef.current = [];
      setNotice(String(e));
    } finally {
      runningRef.current = false;
      setRunning(false);
      setProgress(null);
    }
  }, []);

  // Dropped folders become search folders; dropped files run as fixdats.
  const handlePaths = useCallback(
    async (paths: string[]) => {
      if (paths.length === 0) return;
      setNotice(null);
      const { files, dirs } = await api.classifyPaths(paths);
      if (dirs.length > 0) {
        const current = settingsRef.current;
        const merged = [...new Set([...current.searchDirs, ...dirs])];
        await updateSettings({ ...current, searchDirs: merged });
      }
      if (files.length > 0) {
        queueRef.current.push(...files);
        void drainQueue();
      }
    },
    [drainQueue, updateSettings],
  );

  useEffect(() => {
    let disposed = false;
    const unlisteners: Array<() => void> = [];
    const track = (promise: Promise<() => void>) =>
      promise.then((unlisten) => (disposed ? unlisten() : unlisteners.push(unlisten)));

    track(listen<Progress>('fix-progress', (event) => setProgress(event.payload)));
    track(listen<string[]>('dats-opened', (event) => void handlePaths(event.payload)));
    track(
      getCurrentWebview().onDragDropEvent((event) => {
        if (event.payload.type === 'drop') {
          setDragOver(false);
          void handlePaths(event.payload.paths);
        } else {
          setDragOver(event.payload.type !== 'leave');
        }
      }),
    );

    (async () => {
      const info = await api.getSettings();
      if (disposed) return;
      settingsRef.current = info.settings;
      setSettings(info.settings);
      setSettingsPath(info.settingsPath);
      setPortable(info.portable);
      await handlePaths(await api.takePendingDats());
    })().catch((e) => setNotice(String(e)));

    return () => {
      disposed = true;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, [handlePaths]);

  const addSearchDirs = async () => {
    const picked = await open({ directory: true, multiple: true, title: 'Add search folders' });
    if (!picked) return;
    const dirs = Array.isArray(picked) ? picked : [picked];
    await updateSettings({ ...settings, searchDirs: [...new Set([...settings.searchDirs, ...dirs])] });
  };

  const chooseOutputDir = async () => {
    const picked = await open({ directory: true, multiple: false, title: 'Choose output folder' });
    if (typeof picked === 'string') await updateSettings({ ...settings, outputDir: picked });
  };

  const browseFixdats = async () => {
    const picked = await open({
      multiple: true,
      title: 'Choose fixdats',
      filters: [{ name: 'DAT files', extensions: ['dat', 'xml'] }],
    });
    if (picked) await handlePaths(Array.isArray(picked) ? picked : [picked]);
  };

  const rebuildIndex = async () => {
    try {
      await api.clearIndex();
      setNotice('Index cleared. The next run re-reads every file in the search folders.');
    } catch (e) {
      setNotice(String(e));
    }
  };

  const ready = settings.searchDirs.length > 0 && settings.outputDir !== '';
  const percent = progress && progress.total > 0 ? (progress.done / progress.total) * 100 : 0;

  return (
    <div className="app">
      <section
        className={`dropzone${dragOver ? ' is-over' : ''}${running ? ' is-running' : ''}`}
        aria-live="polite"
      >
        {running ? (
          <>
            <p className="dropzone-title">
              {progress && progress.done < progress.total
                ? `Reading search folders… ${progress.done.toLocaleString()} / ${progress.total.toLocaleString()}`
                : 'Matching and writing…'}
            </p>
            <div className="progress">
              <div className="progress-bar" style={{ width: `${percent}%` }} />
            </div>
          </>
        ) : (
          <>
            <p className="dropzone-title">Drop fixdats here</p>
            <p className="dropzone-hint">
              {ready
                ? 'They run straight away against the folders below.'
                : 'Set at least one search folder and an output folder first.'}
            </p>
            <button type="button" onClick={browseFixdats}>
              Browse…
            </button>
          </>
        )}
      </section>

      {notice && (
        <div className="notice" role="alert">
          <span>{notice}</span>
          <button type="button" className="link" onClick={() => setNotice(null)}>
            Dismiss
          </button>
        </div>
      )}

      <section className="panel">
        <div className="panel-head">
          <h2>Search folders</h2>
          <span className="tag">read only</span>
          <button type="button" onClick={addSearchDirs}>
            Add folder…
          </button>
        </div>
        {settings.searchDirs.length === 0 ? (
          <p className="empty">No search folders yet. Add one, or drop folders onto this window.</p>
        ) : (
          <ul className="paths">
            {settings.searchDirs.map((dir) => (
              <li key={dir}>
                <span className="path" title={dir}>
                  {dir}
                </span>
                <button
                  type="button"
                  className="link"
                  onClick={() =>
                    updateSettings({
                      ...settings,
                      searchDirs: settings.searchDirs.filter((d) => d !== dir),
                    })
                  }
                >
                  Remove
                </button>
              </li>
            ))}
          </ul>
        )}

        <div className="panel-head">
          <h2>Output folder</h2>
          <button type="button" onClick={chooseOutputDir}>
            {settings.outputDir ? 'Change…' : 'Choose…'}
          </button>
        </div>
        {settings.outputDir ? (
          <ul className="paths">
            <li>
              <span className="path" title={settings.outputDir}>
                {settings.outputDir}
              </span>
              <button
                type="button"
                className="link"
                onClick={() => api.openFolder(settings.outputDir).catch((e) => setNotice(String(e)))}
              >
                Open
              </button>
            </li>
          </ul>
        ) : (
          <p className="empty">No output folder yet.</p>
        )}

        <div className="options">
          <label>
            <input
              type="checkbox"
              checked={settings.dryRun}
              onChange={(e) => updateSettings({ ...settings, dryRun: e.target.checked })}
            />
            Dry run (list matches, write nothing)
          </label>
          <button type="button" className="link" onClick={rebuildIndex} disabled={running}>
            Rebuild index
          </button>
        </div>
      </section>

      {runs.map((run) => (
        <RunCard key={run.id} run={run} />
      ))}

      {settingsPath && (
        <footer title={settingsPath}>
          {portable ? 'Portable settings: ' : 'Settings: '}
          {settingsPath}
        </footer>
      )}
    </div>
  );
}

function RunCard({ run }: { run: Run }) {
  const fresh = run.filesIndexed - run.filesCached;
  return (
    <section className="panel">
      <p className="run-meta">
        {run.dryRun && <span className="tag warn">dry run</span>}
        {plural(run.filesIndexed, 'file')} searched in {(run.elapsedMs / 1000).toFixed(1)}s
        {fresh > 0 ? ` (${fresh.toLocaleString()} newly read)` : ' (all from index)'}
      </p>
      {run.warnings.length > 0 && <Details label="Warnings" items={run.warnings} tone="warn" />}
      {run.dats.map((dat) => (
        <DatCard key={dat.fixdat} dat={dat} dryRun={run.dryRun} />
      ))}
    </section>
  );
}

function DatCard({ dat, dryRun }: { dat: DatResult; dryRun: boolean }) {
  return (
    <div className="dat">
      <h3 title={dat.fixdat}>{baseName(dat.fixdat)}</h3>
      {dat.error ? (
        <p className="error">{dat.error}</p>
      ) : (
        <>
          <p className="counts">
            <span className="count good">
              {dat.written.length} {dryRun ? 'would be written' : 'written'}
            </span>
            <span className="count">{dat.present.length} already present</span>
            <span className={`count${dat.missing.length > 0 ? ' bad' : ''}`}>
              {dat.missing.length} still missing
            </span>
            <span className="count muted">of {dat.needed}</span>
          </p>
          <Details
            label={dryRun ? 'Would be written' : 'Written'}
            items={dat.written.map((w) => `${w.name}  ←  ${w.source}`)}
            open
          />
          <Details
            label="Still missing"
            items={dat.missing.map((m) => `${m.name}  (crc ${m.crc}, ${m.size.toLocaleString()} bytes)`)}
            tone="bad"
          />
          <Details label="Already present" items={dat.present} />
          <Details label="Problems" items={dat.problems} tone="warn" />
        </>
      )}
    </div>
  );
}

function Details({
  label,
  items,
  tone,
  open,
}: {
  label: string;
  items: string[];
  tone?: 'bad' | 'warn';
  open?: boolean;
}) {
  if (items.length === 0) return null;
  return (
    <details open={open} className={tone}>
      <summary>
        {label} ({items.length})
      </summary>
      <ul>
        {items.map((item, i) => (
          <li key={i}>{item}</li>
        ))}
      </ul>
    </details>
  );
}
