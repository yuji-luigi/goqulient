// VSX-12: read-only table/collection viewer. One panel per (session, object); every `table/page` carries a
// CancellationToken that a newer page, the Cancel button or closing the panel cancels.
import * as vscode from 'vscode';
import { EngineError } from '../engine/client.ts';
import { createPanel, errorMessage, poster, type Engine } from './panel.ts';
import type { ObjectRef, QueryResult, Sort, TableFromWebview, TableToWebview } from './protocol.ts';

export interface TableOptions {
  extensionUri: vscode.Uri;
  /** The profile's page size (`maxRows`). */
  pageSize: number;
}

const panels = new Map<string, vscode.WebviewPanel>();

export function tableTitle(object: ObjectRef): string {
  return `${object.name} — ${object.database}`;
}

/** Opens (or reveals) the viewer for `object` on an open engine session. */
export function openTable(
  engine: Engine,
  sessionId: string,
  object: ObjectRef,
  readOnly: boolean,
  options: TableOptions,
): vscode.WebviewPanel {
  const key = JSON.stringify([sessionId, object.database, object.schema, object.name]);
  const existing = panels.get(key);
  if (existing) {
    existing.reveal();
    return existing;
  }
  const title = tableTitle(object);
  const panel = createPanel('goqulient.table', title, options.extensionUri, 'table');
  panels.set(key, panel);
  const post = poster<TableToWebview>(panel);
  const pageSize = Math.max(1, Math.floor(options.pageSize));
  let seq = 0;
  let pending: { id: number; offset: number; sort?: Sort; cts: vscode.CancellationTokenSource } | undefined;
  // The last answer, sent again when a hidden panel's webview is recreated.
  let last: TableToWebview | undefined;

  const load = (offset: number, sort?: Sort) => {
    pending?.cts.cancel();
    const id = ++seq;
    const cts = new vscode.CancellationTokenSource();
    pending = { id, offset, sort, cts };
    post({ type: 'loading', requestId: id, offset, sort });
    const params = { sessionId, object, offset: Math.max(0, offset), limit: pageSize, ...(sort ? { sort } : {}) };
    engine
      .request<QueryResult>('table/page', params, cts.token)
      .then(
        (result): TableToWebview => ({ type: 'page', requestId: id, offset, sort, result }),
        (e): TableToWebview =>
          e instanceof EngineError && e.cancelled
            ? { type: 'cancelled', requestId: id }
            : { type: 'error', requestId: id, message: errorMessage(e) },
      )
      .then((m) => {
        cts.dispose();
        if (pending?.id !== id) return; // superseded: a newer page owns the view
        pending = undefined;
        if (m.type !== 'cancelled') last = m;
        post(m);
      });
  };

  panel.webview.onDidReceiveMessage((m: TableFromWebview) => {
    switch (m.type) {
      case 'ready':
        post({ type: 'init', title, object, readOnly, pageSize });
        if (pending) post({ type: 'loading', requestId: pending.id, offset: pending.offset, sort: pending.sort });
        else if (last) post(last);
        else load(0);
        break;
      case 'load':
        load(m.offset, m.sort);
        break;
      case 'cancel':
        pending?.cts.cancel();
        break;
      case 'copy':
        void vscode.env.clipboard.writeText(m.text).then(() => vscode.window.setStatusBarMessage('Copied to the clipboard', 2000));
        break;
    }
  });
  panel.onDidDispose(() => {
    pending?.cts.cancel();
    pending = undefined;
    last = undefined;
    panels.delete(key);
  });
  return panel;
}
