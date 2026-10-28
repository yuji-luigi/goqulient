// The connection-string rules of the engine's `redact::uri` (engine/crates/goku-engine/src/redact.rs, itself a port of
// Redaction.swift). Keep the patterns identical; anything the extension logs about a URL goes through `redactUri`.
const SECRET_PARAMS = 'password|passwd|pwd|sslpassword|tlsCertificateKeyFilePassword|tlsCAFilePassword';

const URI_RULES: [RegExp, string][] = [
  // Greedy to the last `@`, so a password with an unencoded `@`, `/`, `?` or `#` is still removed whole.
  [/(:\/\/[^:/\s@'"]+:)[^\s'"]*@/gi, '$1****@'],
  [new RegExp(`((?:^|[?&;\\s])(?:${SECRET_PARAMS})=)[^&\\s'";]+`, 'gi'), '$1****'],
];

/** Passwords in connection strings become `****`. */
export function redactUri(s: string): string {
  return URI_RULES.reduce((acc, [re, template]) => acc.replace(re, template), s);
}
