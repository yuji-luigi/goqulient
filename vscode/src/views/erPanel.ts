// VSX-13: ER diagram. Flow: introspect (progress + cancel) → model to the webview → it measures the boxes with the
// editor font → `er/layout` in the engine (only for entities without a saved position) → positions to the webview.
// Dragged positions are saved in workspaceState under `layoutKey`.
import * as vscode from 'vscode';
import { EngineError } from '../engine/client.ts';
import { capEntities, decodeSaved, encodeSaved, layoutEdges, mergeLayout, partition } from './layout.ts';
import { createPanel, errorMessage, poster, type Engine } from './panel.ts';
import type { ErFromWebview, ErToWebview, LayoutRect, Point, SchemaModel, Size } from './protocol.ts';

export interface ErOptions {
  extensionUri: vscode.Uri;
  /** Panel title; defaults to "database — ER diagram". */
  title?: string;
}

const panels = new Map<string, vscode.WebviewPanel>();

export function erTitle(database: string, schemas: readonly string[]): string {
  return `${database}${schemas.length ? ` (${schemas.join(', ')})` : ''} — ER diagram`;
}

/** Opens (or reveals) the diagram of `database` (PostgreSQL: `schemas`, empty = all). */
export function openErDiagram(
  engine: Engine,
  sessionId: string,
  database: string,
  schemas: string[],
  layoutKey: string,
  state: vscode.Memento,
  options: ErOptions,
): vscode.WebviewPanel {
  const key = JSON.stringify([sessionId, layoutKey]);
  const existing = panels.get(key);
  if (existing) {
    existing.reveal();
    return existing;
  }
  const title = options.title ?? erTitle(database, schemas);
  const panel = createPanel('goku.er', title, options.extensionUri, 'er');
  panels.set(key, panel);
  const post = poster<ErToWebview>(panel);
  let model: SchemaModel | undefined;
  let sizes: Record<string, Size> | undefined;
  let positions: Record<string, Point> | undefined;
  let failure: string | undefined;
  let busy: vscode.CancellationTokenSource | undefined;
  let generation = 0;

  const persist = () => {
    if (!model || !positions) return;
    const current: Record<string, Point> = {};
    for (const e of model.entities) if (positions[e.id]) current[e.id] = positions[e.id]!;
    void state.update(layoutKey, encodeSaved(current));
  };

  const fail = (e: unknown) => {
    failure = e instanceof EngineError && e.cancelled ? 'Cancelled.' : errorMessage(e);
    post({ type: 'error', message: failure });
  };

  const introspect = () => {
    busy?.cancel();
    const gen = ++generation;
    const cts = (busy = new vscode.CancellationTokenSource());
    [model, sizes, positions, failure] = [undefined, undefined, undefined, undefined];
    const message = `Reading the schema of ${database}…`;
    post({ type: 'loading', message });
    void vscode.window.withProgress(
      { location: vscode.ProgressLocation.Notification, title: message, cancellable: true },
      async (_progress, token) => {
        const sub = token.onCancellationRequested(() => cts.cancel());
        try {
          const m = await engine.request<SchemaModel>('schema/introspect', { sessionId, database, schemas }, cts.token);
          if (gen !== generation) return;
          model = capEntities({ ...m, warnings: m.warnings ?? [] });
          post({ type: 'model', model });
        } catch (e) {
          if (gen === generation) fail(e);
        } finally {
          sub.dispose();
          cts.dispose();
          if (busy === cts) busy = undefined;
        }
      },
    );
  };

  /** Lays out `ids` (all when `fresh`), merges with what's saved, persists and sends the positions. */
  const layout = async (all: boolean) => {
    if (!model || !sizes) return;
    const gen = generation;
    const ids = model.entities.map((e) => e.id).filter((id) => sizes![id]);
    const saved = all ? {} : decodeSaved(state.get(layoutKey));
    const { fresh } = partition(ids, saved);
    let rects: Record<string, LayoutRect> = {};
    if (fresh.length) {
      const only = new Set(fresh);
      const cts = (busy = new vscode.CancellationTokenSource());
      try {
        const r = await engine.request<{ rects: Record<string, LayoutRect> }>(
          'er/layout',
          { entities: fresh, sizes: Object.fromEntries(fresh.map((id) => [id, sizes![id]])), edges: layoutEdges(model, only) },
          cts.token,
        );
        rects = r.rects;
      } catch (e) {
        if (gen === generation) fail(e);
        return;
      } finally {
        cts.dispose();
        if (busy === cts) busy = undefined;
      }
    }
    if (gen !== generation) return;
    positions = mergeLayout(saved, sizes, rects);
    if (fresh.length) persist();
    post({ type: 'layout', positions });
  };

  const exportFile = async (format: 'svg' | 'png', data: string) => {
    const name = `${database}${schemas.length ? `-${schemas.join('-')}` : ''}-er.${format}`.replace(/[\\/:*?"<>|]/g, '_');
    const folder = vscode.workspace.workspaceFolders?.[0]?.uri;
    const uri = await vscode.window.showSaveDialog({
      defaultUri: folder ? vscode.Uri.joinPath(folder, name) : undefined,
      filters: format === 'svg' ? { 'SVG image': ['svg'] } : { 'PNG image': ['png'] },
      saveLabel: 'Export',
    });
    if (!uri) return;
    try {
      await vscode.workspace.fs.writeFile(uri, Buffer.from(data, format === 'svg' ? 'utf8' : 'base64'));
      post({ type: 'exported', path: uri.fsPath });
    } catch (e) {
      void vscode.window.showErrorMessage(`Couldn't export the diagram: ${errorMessage(e)}`);
    }
  };

  panel.webview.onDidReceiveMessage((m: ErFromWebview) => {
    switch (m.type) {
      case 'ready':
        post({ type: 'init', title, database, schemas });
        if (model) post({ type: 'model', model, ...(positions && sizes ? { positions, sizes } : {}) });
        else if (failure) post({ type: 'error', message: failure });
        else if (busy) post({ type: 'loading', message: `Reading the schema of ${database}…` });
        else introspect();
        break;
      case 'sizes':
        sizes = m.sizes;
        void layout(false);
        break;
      case 'moved':
        positions = { ...positions, ...m.positions };
        persist();
        break;
      case 'autoLayout':
        void layout(true);
        break;
      case 'reload':
        introspect();
        break;
      case 'cancel':
        busy?.cancel();
        break;
      case 'export':
        void exportFile(m.format, m.data);
        break;
    }
  });
  panel.onDidDispose(() => {
    generation++;
    busy?.cancel();
    panels.delete(key);
  });
  return panel;
}
