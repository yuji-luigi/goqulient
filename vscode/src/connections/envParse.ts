// A dotenv parser for connection discovery. Supports `KEY=value`, `export KEY=value`, `#` comment lines, single quotes
// (literal), double quotes (escapes \n \r \t \" \\, may span lines), backticks (literal) and ` #` comments after
// unquoted values. No variable expansion: `$X` stays as written, so a value can't pull in another secret.
// Errors carry line numbers only, never values (they may hold passwords).

export interface EnvParseError {
  line: number;
  message: string;
}

export interface EnvParseResult {
  vars: Map<string, string>;
  errors: EnvParseError[];
}

const KEY = /^[A-Za-z_][A-Za-z0-9_.-]*$/;
const ESCAPES: Record<string, string> = { n: '\n', r: '\r', t: '\t', '"': '"', '\\': '\\' };

export function parseEnv(text: string): EnvParseResult {
  const lines = text.replace(/^﻿/, '').split(/\r?\n/);
  const vars = new Map<string, string>();
  const errors: EnvParseError[] = [];
  for (let i = 0; i < lines.length; i++) {
    const lineNo = i + 1;
    const line = lines[i]!;
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith('#')) continue;
    const eq = line.indexOf('=');
    if (eq < 0) {
      errors.push({ line: lineNo, message: 'Expected KEY=value.' });
      continue;
    }
    const key = line.slice(0, eq).trim().replace(/^export\s+/, '');
    if (!KEY.test(key)) {
      errors.push({ line: lineNo, message: 'Invalid variable name.' });
      continue;
    }
    let rest = line.slice(eq + 1).trimStart();
    const quote = rest[0];
    if (quote !== '"' && quote !== "'" && quote !== '`') {
      vars.set(key, line.slice(eq + 1).replace(/\s#.*$/, '').trim());
      continue;
    }
    // Quoted: read until the closing quote, continuing on the following lines if needed.
    let value = '';
    let j = 1;
    let end = i;
    let closed = false;
    for (;;) {
      while (j < rest.length) {
        const c = rest[j]!;
        if (c === quote) {
          closed = true;
          break;
        }
        if (c === '\\' && quote === '"' && j + 1 < rest.length) {
          const next = rest[j + 1]!;
          value += ESCAPES[next] ?? `\\${next}`;
          j += 2;
          continue;
        }
        value += c;
        j++;
      }
      if (closed || end + 1 >= lines.length) break;
      end++;
      value += '\n';
      rest = lines[end]!;
      j = 0;
    }
    if (!closed) {
      errors.push({ line: lineNo, message: 'Unterminated quoted value.' });
      continue;
    }
    const trailing = rest.slice(j + 1).trim();
    if (trailing && !trailing.startsWith('#')) {
      errors.push({ line: lineNo, message: 'Unexpected text after the closing quote.' });
      i = end;
      continue;
    }
    vars.set(key, value);
    i = end;
  }
  return { vars, errors };
}
