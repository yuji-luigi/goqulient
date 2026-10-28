// Webview HTML shell. It carries no data (rows and schemas arrive by postMessage), only the script and stylesheet
// URIs, a nonce'd CSP and the panel title. Pure, so the Node tests check the CSP.
import { randomBytes } from 'node:crypto';

export function nonce(): string {
  return randomBytes(18).toString('base64');
}

export function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
}

export interface ShellOptions {
  /** `webview.cspSource`. */
  cspSource: string;
  nonce: string;
  scriptUri: string;
  styleUri: string;
  title: string;
}

/** No inline script or style without the nonce, no network; `blob:`/`data:` images are for the PNG export only. */
export function csp(o: Pick<ShellOptions, 'cspSource' | 'nonce'>): string {
  return [
    "default-src 'none'",
    `img-src ${o.cspSource} blob: data:`,
    `style-src ${o.cspSource} 'nonce-${o.nonce}'`,
    `font-src ${o.cspSource}`,
    `script-src 'nonce-${o.nonce}'`,
  ].join('; ');
}

export function shell(o: ShellOptions): string {
  return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy" content="${escapeHtml(csp(o))}">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<link rel="stylesheet" href="${escapeHtml(o.styleUri)}" nonce="${o.nonce}">
<title>${escapeHtml(o.title)}</title>
</head>
<body>
<div id="app"></div>
<script nonce="${o.nonce}" src="${escapeHtml(o.scriptUri)}"></script>
</body>
</html>`;
}
